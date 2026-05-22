using FluentAssertions;
using Insight.Identity.Api.Auth;
using Microsoft.AspNetCore.Http;
using Xunit;

namespace Insight.Identity.Tests.Unit;

public sealed class HeaderCallerContextTests
{
    private static readonly Guid CallerId = Guid.Parse("33333333-3333-3333-3333-333333333333");

    [Fact]
    public void Returns_parsed_guid_when_header_present()
    {
        var context = new DefaultHttpContext();
        context.Request.Headers[HeaderCallerContext.HeaderName] = CallerId.ToString();

        var resolved = new HeaderCallerContext().Resolve(context);

        resolved.Should().Be(CallerId);
    }

    [Fact]
    public void Returns_null_when_header_missing()
    {
        var context = new DefaultHttpContext();

        var resolved = new HeaderCallerContext().Resolve(context);

        resolved.Should().BeNull();
    }

    [Theory]
    [InlineData("")]
    [InlineData("not-a-guid")]
    [InlineData("33333333-3333-3333-3333")]
    public void Returns_null_when_header_value_is_not_a_guid(string raw)
    {
        var context = new DefaultHttpContext();
        context.Request.Headers[HeaderCallerContext.HeaderName] = raw;

        var resolved = new HeaderCallerContext().Resolve(context);

        resolved.Should().BeNull();
    }

    [Fact]
    public void Rejects_guid_empty()
    {
        var context = new DefaultHttpContext();
        context.Request.Headers[HeaderCallerContext.HeaderName] = Guid.Empty.ToString();

        var resolved = new HeaderCallerContext().Resolve(context);

        // Guid.Empty is parseable but is not a real identity — accepting it
        // would let a misbehaving gateway promote `00000000-…` to a valid
        // caller and pollute the audit trail.
        resolved.Should().BeNull();
    }
}
