using FluentValidation;
using Insight.Identity.Api.Auth;
using Insight.Identity.Api.Configuration;
using Insight.Identity.Api.Contracts;
using Insight.Identity.Domain.Services;
using Insight.Identity.Infrastructure.MariaDb;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.Routing;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Options;

namespace Insight.Identity.Api.Endpoints;

public static class PersonsEndpoints
{
    public static IEndpointRouteBuilder MapPersonsEndpoints(this IEndpointRouteBuilder app)
    {
        ArgumentNullException.ThrowIfNull(app);

        app.MapGet("/v1/persons/{email}", async (
            string email,
            HttpContext http,
            ITenantContext tenants,
            ICallerContext callers,
            PersonLookupService lookup,
            IOptions<AppOptions> options,
            CancellationToken cancellationToken) =>
        {
            // The {email} path segment is retained for backward
            // compatibility but intentionally ignored: the response is
            // always the caller's own accessible org forest — their
            // subtree unioned with every subtree they hold a visibility
            // grant on (whole-tenant grant => the entire tenant tree).
            // The visible set is exactly the same one the visibility CTE
            // gates POST /v1/profiles by, just materialized as a tree, so
            // no record outside the caller's visibility is ever returned.
            var tenantId = tenants.Resolve(http);
            if (tenantId is null)
            {
                return Results.Json(new ProblemResponse(
                    Type: "urn:insight:error:tenant_unresolved",
                    Title: "Bad Request",
                    Status: StatusCodes.Status400BadRequest,
                    Detail: $"Tenant not provided. Send the {HeaderTenantContext.HeaderName} header or configure identity.tenant_default_id."),
                    statusCode: StatusCodes.Status400BadRequest);
            }

            var callerPersonId = await callers.ResolveAsync(http, cancellationToken).ConfigureAwait(false);
            if (callerPersonId is null)
            {
                return Results.Json(new ProblemResponse(
                    Type: "urn:insight:error:caller_unresolved",
                    Title: "Unauthorized",
                    Status: StatusCodes.Status401Unauthorized,
                    Detail: $"Caller not identified. Send the {HeaderCallerContext.HeaderName} header."),
                    statusCode: StatusCodes.Status401Unauthorized);
            }

            var lookupOptions = BuildLookupOptions(options.Value);

            var forest = await lookup.GetVisibleForestAsync(
                    tenantId.Value, callerPersonId.Value, lookupOptions, cancellationToken)
                .ConfigureAwait(false);
            if (forest is null)
            {
                // Caller has no observations of their own — nothing to root
                // the forest on. 404 with the requested email keeps the
                // response shape stable for existing clients.
                return NotFoundByEmail(email);
            }

            return Results.Ok(PersonResponse.From(forest));
        });

        app.MapPost("/v1/profiles", async (
            ResolveProfileCommandModel body,
            HttpContext http,
            ITenantContext tenants,
            ICallerContext callers,
            ProfileLookupService lookup,
            VisibilityService visibility,
            IValidator<ResolveProfileCommandModel> validator,
            IOptions<AppOptions> options,
            CancellationToken cancellationToken) =>
        {
            var tenantId = tenants.Resolve(http);
            if (tenantId is null)
            {
                return Results.Json(new ProblemResponse(
                    Type: "urn:insight:error:tenant_unresolved",
                    Title: "Bad Request",
                    Status: StatusCodes.Status400BadRequest,
                    Detail: $"Tenant not provided. Send the {HeaderTenantContext.HeaderName} header or configure identity.tenant_default_id."),
                    statusCode: StatusCodes.Status400BadRequest);
            }

            var callerPersonId = await callers.ResolveAsync(http, cancellationToken).ConfigureAwait(false);
            if (callerPersonId is null)
            {
                return Results.Json(new ProblemResponse(
                    Type: "urn:insight:error:caller_unresolved",
                    Title: "Unauthorized",
                    Status: StatusCodes.Status401Unauthorized,
                    Detail: $"Caller not identified. Send the {HeaderCallerContext.HeaderName} header."),
                    statusCode: StatusCodes.Status401Unauthorized);
            }

            var validation = await validator.ValidateAsync(body, cancellationToken).ConfigureAwait(false);
            if (!validation.IsValid)
            {
                // First-error-wins for the URN to keep the response shape
                // simple; client gets one urn:insight:error:* per call.
                var first = validation.Errors[0];
                return Results.Json(new ProblemResponse(
                    Type: string.IsNullOrEmpty(first.ErrorCode) ? "urn:insight:error:invalid_request" : first.ErrorCode,
                    Title: "Bad Request",
                    Status: StatusCodes.Status400BadRequest,
                    Detail: first.ErrorMessage),
                    statusCode: StatusCodes.Status400BadRequest);
            }

            var kind = body.ValueType == "id" ? ResolveProfileKind.SourceId : ResolveProfileKind.Email;
            var query = new ResolveProfileQuery(
                Kind: kind,
                Value: body.Value!,
                SourceType: body.InsightSourceType,
                SourceId: body.InsightSourceId);

            var lookupOptions = BuildLookupOptions(options.Value);
            var result = await lookup.ResolveAsync(tenantId.Value, query, lookupOptions, cancellationToken).ConfigureAwait(false);
            switch (result)
            {
                case ProfileLookupResult.Found f:
                    var canSee = await visibility.CanSeeAsync(
                            tenantId.Value, callerPersonId.Value, f.Profile.PersonId,
                            lookupOptions.OrgChartSourceType, cancellationToken)
                        .ConfigureAwait(false);
                    if (!canSee)
                    {
                        return ProfileNotFound(body);
                    }
                    return Results.Ok(ProfileResponse.From(f.Profile));
                case ProfileLookupResult.NotFound:
                    return ProfileNotFound(body);
                case ProfileLookupResult.Ambiguous a:
                    return Results.Json(new AmbiguousProfileProblemResponse(
                            Type: "urn:insight:error:ambiguous_profile",
                            Title: "Data Invariant Violated",
                            Status: StatusCodes.Status422UnprocessableEntity,
                            Detail: $"lookup matched {a.PersonIds.Count} distinct person_ids; invariant requires exactly 1",
                            Lookup: body,
                            PersonIds: a.PersonIds),
                        statusCode: StatusCodes.Status422UnprocessableEntity);
                default:
                    return Results.Problem("unexpected lookup result", statusCode: StatusCodes.Status500InternalServerError);
            }
        });

        app.MapGet("/health", async (PersonsRepository repo, CancellationToken cancellationToken) =>
        {
            var ok = await repo.PingAsync(cancellationToken).ConfigureAwait(false);
            return ok
                ? Results.Ok(new { status = "healthy" })
                : Results.Json(new { status = "unhealthy" }, statusCode: StatusCodes.Status503ServiceUnavailable);
        });

        app.MapGet("/healthz", () => Results.Text("ok", "text/plain"));

        return app;
    }

    /// <summary>Translate the config block into the domain-layer lookup options.</summary>
    private static LookupOptions BuildLookupOptions(AppOptions config) =>
        new(
            ExpandSubordinates: config.ExpandSubordinates,
            MaxDepth: config.MaxSubordinateDepth,
            OrgChartSourceType: config.OrgChartSourceType);

    private static IResult NotFoundByEmail(string email) =>
        Results.Json(new ProblemResponse(
            Type: "urn:insight:error:person_not_found",
            Title: "Not Found",
            Status: StatusCodes.Status404NotFound,
            Detail: $"person with email '{email}' not found"),
            statusCode: StatusCodes.Status404NotFound);

    private static IResult ProfileNotFound(ResolveProfileCommandModel body) =>
        Results.Json(new ProblemResponse(
            Type: "urn:insight:error:person_not_found",
            Title: "Not Found",
            Status: StatusCodes.Status404NotFound,
            Detail: body.ValueType == "email"
                ? $"no current observation matches email '{body.Value}' for the tenant"
                : $"no current observation matches value_type='id' value='{body.Value}' within the given source instance"),
            statusCode: StatusCodes.Status404NotFound);
}
