using Xunit;

namespace FennecDb.Tests;

/// <summary>
/// The body's <c>code</c> as <see cref="FennecException.ErrorCode"/> and an error event's: only a non-empty string
/// counts, and <see cref="FennecException.Code"/> stays the status's name. No server: the readers are internal.
/// </summary>
public sealed class ErrorCodeTests
{
    [Fact]
    public void ARefusalsBodyCodeIsItsErrorCode()
    {
        var e = FennecException.From(401, """{"error":"the token has expired","code":"token_expired"}""");
        Assert.Equal("token_expired", e.ErrorCode);
        Assert.Equal("unauthorized", e.Code);
        Assert.Equal(401, e.Status);
    }

    [Theory]
    [InlineData("""{"error":"bad token"}""")]
    [InlineData("""{"error":"bad token","code":7}""")]
    [InlineData("""{"error":"bad token","code":""}""")]
    [InlineData("""{"error":"bad token","code":null}""")]
    [InlineData("not json")]
    public void AnythingButANonEmptyStringIsNone(string body)
    {
        var e = FennecException.From(401, body);
        Assert.Null(e.ErrorCode);
        Assert.Equal("unauthorized", e.Code);
    }

    [Fact]
    public void AnUnknownOutcomeKeepsItsCodeToo()
    {
        var e = FennecException.From(500, """{"error":"landed","outcome":"unknown","seq":7,"code":"x"}""");
        Assert.Equal("outcome_unknown", e.Code);
        Assert.Equal("x", e.ErrorCode);
        Assert.Equal(7, e.Seq);
    }

    [Fact]
    public void AStreamsErrorEventCarriesTheCode()
    {
        var ev = Event.Of("error", """{"error":"the token has expired","status":401,"code":"token_expired"}""");
        Assert.Equal("token_expired", ev.ErrorCode);
        Assert.Equal(401, ev.Status);
        Assert.Null(Event.Of("error", """{"error":"x","status":401,"code":3}""").ErrorCode);
        Assert.Null(Event.Of("error", """{"error":"x","status":401,"code":""}""").ErrorCode);
        Assert.Null(Event.Of("error", """{"error":"x"}""").ErrorCode);
    }
}
