using Microsoft.AspNetCore.Http;

namespace Insight.Identity.Api.Auth;

/// <summary>
/// Reads <c>X-Insight-Tenant-Id</c> from the request. Operators send this
/// header from internal callers (api-gateway, dbt-runner) until the JWT
/// flow lands and tenants are bound to identity claims.
/// </summary>
public sealed class HeaderTenantContext : ITenantContext
{
    public const string HeaderName = "X-Insight-Tenant-Id";

    public Guid? Resolve(HttpContext context)
    {
        ArgumentNullException.ThrowIfNull(context);
        // Reject Guid.Empty — same reasoning as HeaderCallerContext: a
        // parseable but non-identity value should not pin tenant context.
        if (context.Request.Headers.TryGetValue(HeaderName, out var raw)
            && Guid.TryParse(raw.ToString(), out var tenantId)
            && tenantId != Guid.Empty)
        {
            return tenantId;
        }
        return null;
    }
}
