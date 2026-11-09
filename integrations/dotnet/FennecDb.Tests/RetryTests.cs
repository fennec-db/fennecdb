using System.Diagnostics;
using System.Net;
using System.Net.Sockets;
using System.Text;
using Xunit;

namespace FennecDb.Tests;

/// <summary>
/// What the client sends again (<see cref="Retry"/>): only where it cannot have run or may run twice, and nothing
/// after its own Timeout. Against a stub that answers its n-th request by a script over real connections: a reset,
/// a close and a refusal are the network's, not a handler's.
/// </summary>
public sealed class RetryTests
{
    /// <summary>How the stub answers one request, read whole: a status, headers and a body -- in pieces, gap
    /// apart, where pieces are given -- or a reset (linger 0), a close before a byte, or nothing until the client
    /// lets go.</summary>
    sealed record Step(int Status = 200, string Body = "", string[]? Headers = null, string[]? Pieces = null,
        int GapMs = 0, bool Reset = false, bool Close = false, bool Hold = false);

    static Step Ok(string body) => new(Body: body, Headers: ["Content-Type: application/json"]);

    /// <summary>A server on a port of its own answering its n-th request (from 1) by a script, over keep-alive
    /// connections as fennec-server's are.</summary>
    sealed class Stub : IDisposable
    {
        readonly TcpListener _listener = new(IPAddress.Loopback, 0);
        readonly Func<int, Step> _script;
        readonly List<Socket> _open = [];
        int _requests;

        public Stub(Func<int, Step> script)
        {
            _script = script;
            _listener.Start();
            _ = Task.Run(AcceptAll);
        }

        public int Port => ((IPEndPoint)_listener.LocalEndpoint).Port;
        public string Url => $"http://127.0.0.1:{Port}";
        public int Requests => Volatile.Read(ref _requests);

        async Task AcceptAll()
        {
            while (true)
            {
                Socket s;
                try { s = await _listener.AcceptSocketAsync(); }
                catch (Exception) { return; }
                lock (_open) _open.Add(s);
                _ = Task.Run(() => Answer(s));
            }
        }

        async Task Answer(Socket s)
        {
            try
            {
                using var stream = new NetworkStream(s, ownsSocket: true);
                var buf = new byte[1 << 20];
                var have = 0;
                while (true)
                {
                    int end;
                    while ((end = HeadEnd(buf, have)) < 0)
                    {
                        var got = await stream.ReadAsync(buf.AsMemory(have));
                        if (got == 0) return;
                        have += got;
                    }
                    var head = Encoding.ASCII.GetString(buf, 0, end);
                    var length = head.Split("\r\n")
                        .Where(l => l.StartsWith("Content-Length:", StringComparison.OrdinalIgnoreCase))
                        .Select(l => int.Parse(l[15..].Trim())).FirstOrDefault();
                    var total = end + 4 + length;
                    while (have < total)
                    {
                        var got = await stream.ReadAsync(buf.AsMemory(have));
                        if (got == 0) return;
                        have += got;
                    }
                    Buffer.BlockCopy(buf, total, buf, 0, have - total);
                    have -= total;
                    var step = _script(Interlocked.Increment(ref _requests));
                    if (step.Reset)
                    {
                        s.LingerState = new LingerOption(true, 0);
                        s.Close();
                        return;
                    }
                    if (step.Close)
                    {
                        s.Shutdown(SocketShutdown.Both);
                        return;
                    }
                    if (step.Hold)
                    {
                        while (await stream.ReadAsync(buf) > 0) { }
                        return;
                    }
                    var body = step.Pieces is null ? step.Body : string.Concat(step.Pieces);
                    var answer = new StringBuilder(
                        $"HTTP/1.1 {step.Status} {(HttpStatusCode)step.Status}\r\nContent-Length: {Encoding.UTF8.GetByteCount(body)}\r\n");
                    foreach (var h in step.Headers ?? []) answer.Append(h).Append("\r\n");
                    await stream.WriteAsync(Encoding.UTF8.GetBytes(answer.Append("\r\n").ToString()));
                    var pieces = step.Pieces ?? [body];
                    for (var i = 0; i < pieces.Length; i++)
                    {
                        if (i > 0) await Task.Delay(step.GapMs);
                        await stream.WriteAsync(Encoding.UTF8.GetBytes(pieces[i]));
                    }
                }
            }
            catch (Exception e) when (e is IOException or SocketException or ObjectDisposedException) { }
        }

        static int HeadEnd(byte[] buf, int have)
        {
            for (var i = 0; i + 3 < have; i++)
                if (buf[i] == '\r' && buf[i + 1] == '\n' && buf[i + 2] == '\r' && buf[i + 3] == '\n') return i;
            return -1;
        }

        public void Dispose()
        {
            _listener.Stop();
            lock (_open)
                foreach (var s in _open) s.Dispose();
        }
    }

    // A 503 naming its wait ran nothing, whatever the request: sent again after the wait, never sooner --
    // retry_after_ms before Retry-After's seconds.
    [Fact]
    public async Task A503GoesAgainAfterItsWait()
    {
        using var stub = new Stub(n => n == 1
            ? new(503, "{\"error\":\"busy\",\"retry_after\":1,\"retry_after_ms\":300}", ["Retry-After: 1"])
            : Ok("{\"affected\":1}"));
        using var db = new FennecClient(stub.Url);
        var took = Stopwatch.StartNew();
        Assert.Equal(1, (await db.ExecAsync("put docs {a: 1}")).Affected);
        Assert.True(took.Elapsed >= TimeSpan.FromMilliseconds(300), $"sent again after {took.Elapsed}");
        Assert.Equal(2, stub.Requests);
    }

    // A connection refused sent nothing: a write without a key goes again.
    [Fact]
    public async Task ARefusedConnectionGoesAgain()
    {
        using var stub = new Stub(_ => Ok("{\"affected\":1}"));
        // A port nothing listens on any more, which refuses.
        var gone = new TcpListener(IPAddress.Loopback, 0);
        gone.Start();
        var closed = ((IPEndPoint)gone.LocalEndpoint).Port;
        gone.Stop();
        var dials = 0;
        var handler = new SocketsHttpHandler
        {
            ConnectCallback = async (_, ct) =>
            {
                var s = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp) { NoDelay = true };
                try
                {
                    await s.ConnectAsync(IPAddress.Loopback, Interlocked.Increment(ref dials) == 1 ? closed : stub.Port, ct);
                    return new NetworkStream(s, ownsSocket: true);
                }
                catch
                {
                    s.Dispose();
                    throw;
                }
            },
        };
        using var db = new FennecClient(stub.Url, new() { HttpClient = new HttpClient(handler) });
        Assert.Equal(1, (await db.ExecAsync("put docs {a: 1}")).Affected);
        Assert.Equal(2, dials);
    }

    // A write without a key, reset after the server read it, may have landed: it goes once -- sent again, the third
    // request's answer would have been its own. The caller's AsRead sent home is a claim nothing checks, and goes
    // once too; a read the builder made goes again.
    [Fact]
    public async Task AResetWriteOrCallersReadGoesOnceAndABuildersReadAgain()
    {
        using var stub = new Stub(n => n is >= 2 and <= 5 ? new(Reset: true) : Ok("[{\"a\":1}]"));
        using var db = new FennecClient(stub.Url);
        await db.QueryAsync("get docs");
        await Assert.ThrowsAsync<HttpRequestException>(() => db.ExecAsync("put docs {a: 2}"));
        await Assert.ThrowsAsync<HttpRequestException>(() => db.AsRead().QueryAsync("get docs"));
        await Assert.ThrowsAsync<HttpRequestException>(() => db.CallAsync("history", read: true));
        Assert.Single(await db.From("docs").RowsAsync());
        Assert.Equal(6, stub.Requests);
    }

    // A 503 whose body says statements of a batch were made is no answer that ran nothing: sent again whole, they
    // were made twice. Nor does a refusal naming no wait go again, a 500 whose outcome is unknown among them, under
    // a key or not.
    [Fact]
    public async Task ABatchThatMadeStatementsOrAnUnknownOutcomeGoesOnce()
    {
        using var stub = new Stub(n => n switch
        {
            1 => new(503, "{\"error\":\"stopped\",\"at\":1,\"completed\":1,\"retry_after\":1}"),
            2 => new(500, "{\"error\":\"lease lapsed\",\"outcome\":\"unknown\",\"seq\":7}"),
            _ => Ok("{\"ok\":2,\"results\":[]}"),
        });
        using var db = new FennecClient(stub.Url);
        var e = await Assert.ThrowsAsync<FennecException>(() =>
            db.BatchAsync([new("compact"), new("put docs {a: 1}")]));
        Assert.Equal((503, 1), (e.Status, e.Completed));
        e = await Assert.ThrowsAsync<FennecException>(() =>
            db.WithIdempotencyKey("k1").ExecAsync("put docs {a: 1}"));
        Assert.Equal("outcome_unknown", e.Code);
        Assert.Equal(2, stub.Requests);
    }

    // Nothing goes again after the client's own Timeout, past which the server may still run it -- a read neither.
    // The Timeout bounds the head and each wait between the body's pieces, not the whole.
    [Fact]
    public async Task TheClientsOwnTimeoutSendsNothingAgain()
    {
        using var stub = new Stub(n => n switch
        {
            1 => new(Hold: true),
            // Six pieces 100 ms apart: 500 ms, past the Timeout whole.
            3 => new(Pieces: ["[{\"a\"", ":1", "},", "{\"a\":", "2", "}]"], GapMs: 100),
            _ => Ok("{\"affected\":1}"),
        });
        using var db = new FennecClient(stub.Url, new() { Timeout = TimeSpan.FromMilliseconds(400) });
        var took = Stopwatch.StartNew();
        var late = await Assert.ThrowsAsync<TaskCanceledException>(() => db.From("docs").RowsAsync());
        Assert.IsType<TimeoutException>(late.InnerException);
        Assert.True(took.Elapsed >= TimeSpan.FromMilliseconds(400), $"gave up after {took.Elapsed}");
        Assert.Equal(1, stub.Requests);
        await db.ExecAsync("put docs {a: 1}");
        Assert.Equal(2, (await db.From("docs").RowsAsync()).Count);
    }

    // Retry = null sends nothing again: a 503 naming its wait is the caller's at once.
    [Fact]
    public async Task WithoutRetryNothingGoesAgain()
    {
        using var stub = new Stub(n => n == 1 ? new(503, "{\"error\":\"busy\",\"retry_after_ms\":50}") : Ok("[]"));
        using var db = new FennecClient(stub.Url, new() { Retry = null });
        var e = await Assert.ThrowsAsync<FennecException>(() => db.From("docs").RowsAsync());
        Assert.Equal((503, (TimeSpan?)TimeSpan.FromMilliseconds(50)), (e.Status, e.RetryAfter));
        Assert.Throws<ArgumentOutOfRangeException>(() =>
            new FennecClient(stub.Url, new() { Retry = new Retry { Attempts = 0 } }));
    }

    // A keyed write goes again after a reset, within its tries: .NET's handler sends no request with content again
    // on its own.
    [Fact]
    public async Task AKeyedWriteGoesAgainWithinItsTries()
    {
        using var stub = new Stub(n => n is 2 or 3 ? new(Reset: true) : Ok("{\"affected\":1}"));
        using var db = new FennecClient(stub.Url, new() { Retry = new Retry { Attempts = 2 } });
        await db.ExecAsync("put docs {a: 1}");
        await Assert.ThrowsAsync<HttpRequestException>(() => db.WithIdempotencyKey("k1").ExecAsync("put docs {a: 2}"));
        Assert.Equal(3, stub.Requests);
        Assert.Equal(1, (await db.WithIdempotencyKey("k1").ExecAsync("put docs {a: 2}")).Affected);
    }

    // .NET's handler sends a request with no content -- a GET -- again itself where the server closed the
    // connection before a byte of the answer, three times at most: the client sends it no more after that, or it
    // went out up to twelve times.
    [Fact]
    public async Task AGetTheHandlerSentAgainIsNotSentAgainHere()
    {
        using var stub = new Stub(n => n <= 4 ? new(Close: true) : Ok(""));
        using var db = new FennecClient(stub.Url);
        await Assert.ThrowsAsync<HttpRequestException>(() => db.ChangesAsync(0));
        // The handler's own policy sets how many; the client adds none of its own (twelve), and the handler
        // did send it again.
        Assert.InRange(stub.Requests, 2, 4);
    }

    // A cancel during the pause before a retry ends the request: the stub sees the one request.
    [Fact]
    public async Task ACancelDuringThePauseEndsTheRequest()
    {
        var seen = new TaskCompletionSource();
        using var stub = new Stub(n =>
        {
            if (n == 1) seen.TrySetResult();
            return n == 1 ? new(503, "{\"error\":\"busy\",\"retry_after_ms\":5000}") : Ok("[]");
        });
        using var db = new FennecClient(stub.Url);
        using var cts = new CancellationTokenSource();
        var call = db.ExecAsync("put docs {a: 1}", cancellationToken: cts.Token);
        await seen.Task;
        cts.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => call);
        Assert.Equal(1, stub.Requests);
    }

    // A 429 naming its wait in retry_after_ms alone is sent again after it.
    [Fact]
    public async Task A429GoesAgainAfterItsNamedWait()
    {
        using var stub = new Stub(n => n == 1
            ? new(429, "{\"error\":\"too many\",\"code\":\"too_many\",\"retry_after_ms\":300}")
            : Ok("{\"affected\":1}"));
        using var db = new FennecClient(stub.Url);
        var took = Stopwatch.StartNew();
        Assert.Equal(1, (await db.ExecAsync("put docs {a: 1}")).Affected);
        Assert.True(took.Elapsed >= TimeSpan.FromMilliseconds(300), $"sent again after {took.Elapsed}");
        Assert.Equal(2, stub.Requests);
    }

    // A subscription's head is bounded by the Timeout, its body for as long as it is read.
    [Fact]
    public async Task ASubscriptionsTimeoutIsItsHeadAlone()
    {
        using var stub = new Stub(n => n == 1
            ? new(Hold: true)
            : new(Headers: ["Content-Type: text/event-stream"], GapMs: 900,
                Pieces: [": hi\n\n", "event: seed\ndata: {\"seq\":1,\"rows\":[]}\n\n"]));
        using var db = new FennecClient(stub.Url, new() { Timeout = TimeSpan.FromMilliseconds(300) });
        await Assert.ThrowsAsync<TaskCanceledException>(async () =>
        {
            await foreach (var _ in db.SubscribeAsync("docs")) break;
        });
        await foreach (var ev in db.SubscribeAsync("docs"))
        {
            Assert.Equal("seed", ev.Type);
            break;
        }
    }
}
