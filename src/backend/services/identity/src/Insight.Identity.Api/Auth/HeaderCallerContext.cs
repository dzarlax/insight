using Microsoft.AspNetCore.Http;

namespace Insight.Identity.Api.Auth;

/// <summary>
/// Reads <c>X-Insight-Person-Id</c> from the request. api-gateway sets
/// the header from the validated JWT subject; downstream services treat
/// it as the authenticated caller id.
/// </summary>
public sealed class HeaderCallerContext : ICallerContext
{
    public const string HeaderName = "X-Insight-Person-Id";

    public Guid? Resolve(HttpContext context)
    {
        ArgumentNullException.ThrowIfNull(context);
        // Reject Guid.Empty — a parseable but non-identity value. A misbehaving
        // gateway sending `00000000-…` (e.g. when the JWT `sub` claim is
        // missing) would otherwise be promoted to a real caller and pollute
        // the audit log with a phantom `author_person_id`.
        if (context.Request.Headers.TryGetValue(HeaderName, out var raw)
            && Guid.TryParse(raw.ToString(), out var personId)
            && personId != Guid.Empty)
        {
            return personId;
        }
        return null;
    }
}
