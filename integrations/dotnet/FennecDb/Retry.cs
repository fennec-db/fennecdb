using System.Net;
using System.Net.Sockets;
using System.Text.Json;

namespace FennecDb;

/// <summary>
/// How often, and for how long, a <see cref="FennecClient"/> sends a request again
/// (<see cref="FennecClientOptions.Retry"/>): only where it cannot have run, or may run twice. Cannot have run: a
/// 503, or a quota's 429, naming its wait -- sent after that wait, never sooner, unless its body says statements of a
/// batch were made (<see cref="FennecException.Completed"/>) -- and a connection refused. May run twice, and so goes
/// again after a dropped connection too: a read the builder made, one sent near (an <see cref="FennecClient.AsRead"/>
/// copy of a client with <see cref="FennecClientOptions.ReadsNearest"/>), a write under an idempotency key, and a GET
/// -- <see cref="FennecClient.ChangesAsync"/>, <see cref="FennecClient.SubscribeAsync"/>'s head,
/// <see cref="FennecClient.HealthAsync"/>. Nothing goes again after the client's own Timeout or the token's
/// cancellation, nor a refusal naming no wait; an <c>AsRead</c> copy's statement sent home, or a call's
/// <c>read: true</c>, is a claim nothing checks, and goes once. The client makes no key.
/// </summary>
public sealed record Retry
{
    /// <summary>The tries a request is given, the first among them: 3.</summary>
    public int Attempts { get; init; } = 3;

    /// <summary>The waits between them, all told -- the server's wait, or 250 ms doubling, 30% jitter on top: 10
    /// seconds. A wait past what is left is not waited: the refusal is the caller's at once, its
    /// <see cref="FennecException.RetryAfter"/> saying when to come back.</summary>
    public TimeSpan MaxWait { get; init; } = TimeSpan.FromSeconds(10);
}

public sealed partial class FennecClient
{
    /// <summary>An answer: its response, its body read whole -- but a stream's 200's, open, the caller's to read --
    /// and the request it answers, whose headers say what it was sent after. The caller disposes both.</summary>
    readonly record struct Answered(HttpResponseMessage Res, byte[]? Raw, HttpRequestMessage Req) : IDisposable
    {
        public void Dispose()
        {
            Res.Dispose();
            Req.Dispose();
        }
    }

    /// <summary>
    /// A request, made anew by <paramref name="make"/> for each try, and its answer: its head within the Timeout
    /// (<paramref name="extra"/> and a held write's wait on top) and each piece of its body within the Timeout of
    /// the last -- a stream's head alone -- sent again by the rule the router keeps toward its nodes
    /// (docs/design/js-client.md#retries): what ran nothing -- a 503 or 429 naming its wait, after it, unless
    /// statements of a batch were made (<c>completed</c>); a connection refused -- and, where
    /// <paramref name="twice"/> (a read the builder made or sent near, a write under a key, a GET), after a dropped
    /// connection too. Never after the client's own Timeout, which a request may still run past, nor the token's
    /// cancellation, nor a refusal naming no wait; past the tries or the waits, the last answer or error is the
    /// caller's.
    /// </summary>
    async Task<Answered> AskAsync(Func<HttpRequestMessage> make, TimeSpan extra, bool twice, bool stream,
        CancellationToken ct)
    {
        var waited = TimeSpan.Zero;
        for (var n = 0; ; n++)
        {
            var req = make();
            using var clock = Bounded(extra, ct);
            HttpResponseMessage? res = null;
            var dropped = false;
            TimeSpan pause;
            try
            {
                res = await _http.SendAsync(req, HttpCompletionOption.ResponseHeadersRead, clock.Token)
                    .ConfigureAwait(false);
                // A stream's body is read for as long as the caller reads it: the clock stops at its head, as it
                // is let go of here, and HttpClient binds no content to the token its head was asked under.
                if (stream && res.StatusCode == HttpStatusCode.OK) return new(res, null, req);
                var raw = await BodyAsync(res, clock).ConfigureAwait(false);
                if (res.StatusCode is not (HttpStatusCode.ServiceUnavailable or HttpStatusCode.TooManyRequests)
                    || WaitOf(res, raw) is not { } wait
                    || !Again(n, waited, wait, out pause))
                    return new(res, raw, req);
            }
            catch (Exception e) when (!ct.IsCancellationRequested && clock.IsCancellationRequested)
            {
                res?.Dispose();
                req.Dispose();
                // The client's own: past it a compact, a long read may still be running, and three tries ran a
                // read three times. As HttpClient's own Timeout says it.
                throw new TaskCanceledException($"no answer within the client's Timeout ({_timeout})",
                    new TimeoutException(e.Message, e));
            }
            catch (Exception e) when (!ct.IsCancellationRequested && e is HttpRequestException or IOException)
            {
                // .NET's handler sends a request with no content -- a GET here -- again itself, three times at
                // most, where the connection ended before a byte of the answer: sent again here as well, it went
                // out up to twelve times. Which exception it ends in depends on the handler's own policy, so no
                // content-less request is sent again here after a dropped connection, whatever the exception. A
                // request with content it never sends again.
                var sentAgain = res is null && req.Content is null && !Refused(e);
                res?.Dispose();
                req.Dispose();
                if (!(twice || Refused(e)) || sentAgain || !Again(n, waited, TimeSpan.Zero, out pause))
                    throw;
                dropped = true;
            }
            catch
            {
                res?.Dispose();
                req.Dispose();
                throw;
            }
            // A 503 or 429 to wait out; a dropped connection's were let go of as it was caught.
            if (!dropped)
            {
                res!.Dispose();
                req.Dispose();
            }
            waited += pause;
            await Task.Delay(pause, ct).ConfigureAwait(false);
        }
    }

    /// <summary>Whether try <paramref name="n"/> (from 0) is followed by another, after <paramref name="waited"/> of
    /// waits, and after how long: the server's <paramref name="wait"/>, or 250 ms doubling where it names none, 30%
    /// jitter on top -- so a server back up is not met by every client at once -- within what is left of the
    /// waits.</summary>
    bool Again(int n, TimeSpan waited, TimeSpan wait, out TimeSpan pause)
    {
        pause = TimeSpan.Zero;
        if (_retry is not { } r) return false;
        var doubled = TimeSpan.FromMilliseconds(250 * Math.Pow(2, Math.Min(n, 20)));
        var b = wait > doubled ? wait : doubled;
        if (n + 1 >= r.Attempts || waited + b > r.MaxWait) return false;
        var jittered = b * (1 + Random.Shared.NextDouble() * 0.3);
        pause = jittered < r.MaxWait - waited ? jittered : r.MaxWait - waited;
        return true;
    }

    /// <summary>The body, each piece within the Timeout of the last: a long answer is read for as long as it comes,
    /// one that stops is given up. Bounded whole, a long read failed however steadily it came.</summary>
    async Task<byte[]> BodyAsync(HttpResponseMessage res, CancellationTokenSource clock)
    {
        var bounded = _timeout != System.Threading.Timeout.InfiniteTimeSpan && _timeout > TimeSpan.Zero;
        // The head's clock runs on to the first read, which sets it again: the stream is the response's own.
        using var body = await res.Content.ReadAsStreamAsync(clock.Token).ConfigureAwait(false);
        // A length the server gave is read into one array of it -- the content stream ends there, and one cut
        // short throws -- any other a piece at a time.
        if (res.Content.Headers.ContentLength is { } length and < 1 << 26)
        {
            var all = new byte[length];
            for (var at = 0; at < all.Length;)
            {
                if (bounded) clock.CancelAfter(_timeout);
                var got = await body.ReadAsync(all.AsMemory(at), clock.Token).ConfigureAwait(false);
                if (got == 0) throw new IOException("the answer ended before its length");
                at += got;
            }
            return all;
        }
        using var buf = new MemoryStream();
        var piece = new byte[16384];
        while (true)
        {
            if (bounded) clock.CancelAfter(_timeout);
            var got = await body.ReadAsync(piece, clock.Token).ConfigureAwait(false);
            if (got == 0) return buf.ToArray();
            buf.Write(piece, 0, got);
        }
    }

    /// <summary>The wait a 503 or a 429 names, where it names one: to the millisecond where the body says it
    /// (<c>retry_after_ms</c>) -- Retry-After is whole seconds, and a bucket of 600 rows a second refills a row in
    /// 1.7 ms -- then the header, then the body's <c>retry_after</c>. None where its body says statements of a
    /// batch were made (<c>completed</c>): a /batch holding a compact runs a statement at a time, and sent again
    /// whole its first statements were made twice.</summary>
    static TimeSpan? WaitOf(HttpResponseMessage res, byte[] raw)
    {
        double ms = 0, secs = 0;
        try
        {
            using var doc = JsonDocument.Parse(raw);
            var r = doc.RootElement;
            if (r.ValueKind == JsonValueKind.Object)
            {
                if (r.TryGetProperty("completed", out var c) && c.ValueKind == JsonValueKind.Number
                    && c.GetDouble() > 0)
                    return null;
                if (r.TryGetProperty("retry_after_ms", out var m) && m.ValueKind == JsonValueKind.Number)
                    ms = m.GetDouble();
                if (r.TryGetProperty("retry_after", out var s) && s.ValueKind == JsonValueKind.Number)
                    secs = s.GetDouble();
            }
        }
        catch (JsonException) { }
        if (ms > 0) return Seconds(ms / 1000);
        if (res.Headers.RetryAfter?.Delta is { } d && d > TimeSpan.Zero) return d;
        return secs > 0 ? Seconds(secs) : null;
    }

    // A week at most: past MaxWait either way, and a TimeSpan from a double overflows.
    static TimeSpan Seconds(double s) => TimeSpan.FromSeconds(Math.Min(s, 7 * 24 * 3600));

    /// <summary>Whether connecting was refused, which sent nothing.</summary>
    static bool Refused(Exception e)
    {
        for (var x = e; x is not null; x = x.InnerException)
            if (x is SocketException { SocketErrorCode: SocketError.ConnectionRefused }) return true;
        return false;
    }
}
