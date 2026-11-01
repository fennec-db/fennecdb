using System.Net;
using System.Text.Json;
using Xunit;

namespace FennecDb.Tests;

/// <summary>
/// <see cref="FennecClient.CallAsync"/> against a real fennec-server: statements defined through the client with the
/// server's token, then called by name -- the answer, the refusals, the key, the claims, the mark of a read and
/// the options a statement takes.
/// </summary>
[Collection("servers")]
public sealed class CallTests(Servers servers)
{
    static int _names;

    // A name no other test uses, for a collection or a statement alike (ASCII, at most 59 bytes).
    static string Fresh(string prefix) => $"{prefix}_{Environment.ProcessId}_{Interlocked.Increment(ref _names)}";

    FennecClient Root() => new(servers.Primary, new FennecClientOptions { Token = Servers.RootToken });

    static long Balance(ExecResult r) => r.Rows!.Single().GetProperty("balance").GetInt64();

    /// <summary>The ledger the first tests move money in: two accounts and a transfer between them, which says why
    /// it refused.</summary>
    async Task<(FennecClient Db, string Accounts, string Transfer)> Ledger()
    {
        var db = Root();
        var accounts = Fresh("accounts");
        var transfer = Fresh("transfer");
        await db.ExecAsync($"create collection {accounts} (owner text, balance int)");
        await db.From(accounts).InsertAsync(new object[] { new { owner = "a", balance = 100 }, new { owner = "b", balance = 50 } });
        await db.ExecAsync($"create statement {transfer}($from int, $to int, $amount int) public as begin "
            + $"set {accounts} {{balance: balance - $amount}} where id = $from and balance >= $amount require 1 else 'insufficient funds'; "
            + $"set {accounts} {{balance: balance + $amount}} where id = $to require 1 else 'no such account'; "
            + $"get {accounts} select id, balance where id = $from end");
        return (db, accounts, transfer);
    }

    [Fact]
    public async Task ACallRunsItsStatementAndAnswersTheLastOne()
    {
        var (db, accounts, transfer) = await Ledger();
        using var _ = db;
        // The arguments as an object, a dictionary or a record: values by name, ints and all.
        var moved = await db.CallAsync(transfer, new { from = 1, to = 2, amount = 30 });
        Assert.Equal(1, moved.Affected);
        Assert.Equal(70, Balance(moved));
        Assert.True(moved.Seq > 0 && db.Seq >= moved.Seq);
        Assert.False(moved.Replayed);
        moved = await db.CallAsync(transfer, new Dictionary<string, object?> { ["from"] = 1L, ["to"] = 2L, ["amount"] = 10L });
        Assert.Equal(60, Balance(moved));
        Assert.Equal(1, moved.Rows!.Single().GetProperty("id").GetInt64());
        var both = await db.QueryAsync($"get {accounts} select balance order id");
        Assert.Equal([60L, 90L], both.Select(r => r.GetProperty("balance").GetInt64()));
    }

    [Fact]
    public async Task ARefusedRequireSaysWhyAndPutsTheBlockBack()
    {
        var (db, accounts, transfer) = await Ledger();
        using var _ = db;
        var e = await Assert.ThrowsAsync<FennecException>(() => db.CallAsync(transfer, new { from = 1, to = 2, amount = 500 }));
        Assert.Equal((412, "unmet", "insufficient funds"), (e.Status, e.Code, e.Reason));
        // The second guard refuses after the first set landed in the block: both are put back.
        e = await Assert.ThrowsAsync<FennecException>(() => db.CallAsync(transfer, new { from = 1, to = 99, amount = 10 }));
        Assert.Equal((412, "no such account"), (e.Status, e.Reason));
        var rows = await db.QueryAsync($"get {accounts} select balance order id");
        Assert.Equal([100L, 50L], rows.Select(r => r.GetProperty("balance").GetInt64()));
    }

    [Fact]
    public async Task ARefusalIsTheClientsErrorAndRunsNothing()
    {
        var (db, accounts, transfer) = await Ledger();
        using var _ = db;
        // No such statement: the same 404 as one not granted.
        var e = await Assert.ThrowsAsync<FennecException>(() => db.CallAsync(Fresh("nothing"), new { x = 1 }));
        Assert.Equal((404, "not_found"), (e.Status, e.Code));
        // An argument missing, extra, of another type, or none at all where a statement takes three.
        e = await Assert.ThrowsAsync<FennecException>(() => db.CallAsync(transfer, new { from = 1, to = 2 }));
        Assert.Equal((400, "bad_request"), (e.Status, e.Code));
        Assert.Contains("amount", e.Message);
        e = await Assert.ThrowsAsync<FennecException>(() => db.CallAsync(transfer, new { from = 1, to = 2, amount = 5, more = 1 }));
        Assert.Equal(400, e.Status);
        Assert.Contains("more", e.Message);
        e = await Assert.ThrowsAsync<FennecException>(() => db.CallAsync(transfer, new { from = 1, to = 2, amount = "ten" }));
        Assert.Equal(400, e.Status);
        e = await Assert.ThrowsAsync<FennecException>(() => db.CallAsync(transfer));
        Assert.Equal(400, e.Status);
        var rows = await db.QueryAsync($"get {accounts} select balance order id");
        Assert.Equal([100L, 50L], rows.Select(r => r.GetProperty("balance").GetInt64()));
        // What the builder refuses is never sent: the client's other error, a FennecQueryException.
        await Assert.ThrowsAsync<FennecQueryException>(() => db.CallAsync("transfer; del x", new { a = 1 }));
        await Assert.ThrowsAsync<FennecQueryException>(() => db.CallAsync(transfer, new[] { 1, 2, 3 }));
        await Assert.ThrowsAsync<FennecQueryException>(() => db.CallAsync(transfer, "from 1"));
        await Assert.ThrowsAsync<FennecQueryException>(() => db.CallAsync(transfer, new { from = 1 }, new[] { "sub" }));
    }

    [Fact]
    public async Task EveryKindOfArgumentGoesInAsAParameter()
    {
        using var db = Root();
        var kinds = Fresh("kinds");
        var echo = Fresh("echo");
        await db.ExecAsync($"create collection {kinds} (n int, t text, f float, b bool, at timestamp, meta json, v vector<3>)");
        await db.ExecAsync($"create statement {echo}($n int, $t text, $f float, $b bool, $at timestamp, $meta json, $v vector<3>) public as "
            + $"insert {kinds} {{n: $n, t: $t, f: $f, b: $b, at: $at, meta: $meta, v: $v}} returning id, n, t, f, b, at, meta, v");
        // A text that would be an injection in a spliced statement is a value here.
        const string text = "çağ \"quoted\" \\ '; del x --";
        var at = new DateTimeOffset(2026, 9, 19, 12, 34, 56, 789, TimeSpan.Zero);
        var r = (await db.CallAsync(echo, new Dictionary<string, object?>
        {
            ["n"] = -42,
            ["t"] = text,
            ["f"] = 2.5,
            ["b"] = true,
            ["at"] = at,
            ["meta"] = new Dictionary<string, object?> { ["lang"] = "tr", ["rank"] = 2, ["tags"] = new[] { "a", "b" }, ["gone"] = null },
            ["v"] = new[] { 0.5f, -0.25f, 1f },
        })).Rows!.Single();
        Assert.Equal(-42, r.GetProperty("n").GetInt64());
        Assert.Equal(text, r.GetProperty("t").GetString());
        Assert.Equal(2.5, r.GetProperty("f").GetDouble());
        Assert.True(r.GetProperty("b").GetBoolean());
        Assert.Equal(at.ToUnixTimeMilliseconds(), r.GetProperty("at").ValueKind == JsonValueKind.Number
            ? r.GetProperty("at").GetInt64()
            : DateTimeOffset.Parse(r.GetProperty("at").GetString()!, System.Globalization.CultureInfo.InvariantCulture).ToUnixTimeMilliseconds());
        Assert.Equal("tr", r.GetProperty("meta").GetProperty("lang").GetString());
        Assert.Equal(["a", "b"], r.GetProperty("meta").GetProperty("tags").EnumerateArray().Select(x => x.GetString()));
        Assert.Equal([0.5f, -0.25f, 1f], r.GetProperty("v").Deserialize<float[]>()!);
        Assert.Equal(1, await db.From(kinds).CountAsync());
    }

    [Fact]
    public async Task AWriteThatAnswersNoRowsAnswersItsCount()
    {
        using var db = Root();
        var notes = Fresh("notes");
        var purge = Fresh("purge");
        await db.ExecAsync($"create collection {notes} (body text @hash)");
        await db.From(notes).InsertAsync(new object[] { new { body = "x" }, new { body = "y" }, new { body = "x" } });
        await db.ExecAsync($"create statement {purge}($w text) public as del {notes} where body = $w");
        var r = await db.CallAsync(purge, new { w = "x" });
        Assert.Equal(2, r.Affected);
        Assert.Null(r.Rows);
        Assert.Equal(1, await db.From(notes).CountAsync());
    }

    [Fact]
    public async Task AnIdempotencyKeyMakesACallOnce()
    {
        using var db = Root();
        var notes = Fresh("notes");
        var stamp = Fresh("stamp");
        await db.ExecAsync($"create collection {notes} (body text)");
        await db.ExecAsync($"create statement {stamp}() public as insert {notes} {{body: 'z'}} returning id");
        var first = await db.CallAsync(stamp, idempotencyKey: "stamp-" + notes);
        var again = await db.CallAsync(stamp, idempotencyKey: "stamp-" + notes);
        Assert.False(first.Replayed);
        Assert.True(again.Replayed);
        Assert.Equal(first.Rows!.Single().GetProperty("id").GetInt64(), again.Rows!.Single().GetProperty("id").GetInt64());
        Assert.Equal(1, await db.From(notes).CountAsync());
        // The key with another request is 422, the first answer kept; and a client's own key counts as the option.
        var e = await Assert.ThrowsAsync<FennecException>(() =>
            db.CallAsync(stamp, new Dictionary<string, object?>(), idempotencyKey: "stamp-" + notes));
        Assert.Equal((422, "key_reused"), (e.Status, e.Code));
        var viaClient = await db.WithIdempotencyKey("own-" + notes).CallAsync(stamp);
        Assert.True((await db.WithIdempotencyKey("own-" + notes).CallAsync(stamp)).Replayed);
        Assert.False(viaClient.Replayed);
        Assert.Equal(2, await db.From(notes).CountAsync());
        // Without a key, a call is made each time.
        await db.CallAsync(stamp);
        await db.CallAsync(stamp);
        Assert.Equal(4, await db.From(notes).CountAsync());
    }

    [Fact]
    public async Task ClaimsAreTheServersTokensAsAndAScopedTokensOwn()
    {
        using var db = Root();
        var notes = Fresh("notes");
        var mine = Fresh("mine");
        await db.ExecAsync($"create collection {notes} (body text, by text)");
        await db.From(notes).InsertAsync(new object[]
        {
            new { body = "a1", by = "alice" }, new { body = "b1", by = "bob" }, new { body = "a2", by = "alice" },
        });
        await db.ExecAsync($"create statement {mine}() claims (sub text) as get {notes} select body where by = $jwt.sub order id");
        var rows = (await db.CallAsync(mine, claims: new { sub = "alice" })).Rows!;
        Assert.Equal(["a1", "a2"], rows.Select(r => r.GetProperty("body").GetString()));
        // The claims are one more parameter, in the same call as arguments: dictionaries do as objects do.
        rows = (await db.CallAsync(mine, null, new Dictionary<string, object?> { ["sub"] = "bob" })).Rows!;
        Assert.Equal(["b1"], rows.Select(r => r.GetProperty("body").GetString()));
        // The server's token carries none: a claim the statement reads and the call lacks is 403.
        var e = await Assert.ThrowsAsync<FennecException>(() => db.CallAsync(mine));
        Assert.Equal((403, "forbidden"), (e.Status, e.Code));
        // A claim of another type than declared is as missing.
        e = await Assert.ThrowsAsync<FennecException>(() => db.CallAsync(mine, claims: new { sub = 7 }));
        Assert.Equal(403, e.Status);
    }

    [Fact]
    public async Task ACallThatReadsIsMarkedAndOneThatWritesIsRefusedNeverLanded()
    {
        var (root, accounts, transfer) = await Ledger();
        using var _ = root;
        var history = Fresh("history");
        await root.ExecAsync($"create statement {history}($min int) as get {accounts} select id, balance where balance >= $min order id");
        // A client reading near marks what it knows reads: answered, the primary here running it read-only.
        using var db = new FennecClient(servers.Primary,
            new FennecClientOptions { Token = Servers.RootToken, ReadsNearest = true });
        await db.ExecAsync($"get {accounts} count");
        var read = await db.CallAsync(history, new { min = 60 }, read: true);
        Assert.Equal([1L], read.Rows!.Select(r => r.GetProperty("id").GetInt64()));
        // Sent unmarked, it is a call like any other.
        Assert.Equal(2, (await db.CallAsync(history, new { min = 0 })).Rows!.Count);
        // A call that writes, sent as a read, is refused before it runs: 403, and no money moved.
        var e = await Assert.ThrowsAsync<FennecException>(() =>
            db.CallAsync(transfer, new { from = 1, to = 2, amount = 10 }, read: true));
        Assert.Equal((403, "forbidden"), (e.Status, e.Code));
        Assert.Contains("only reads", e.Message);
        var rows = await db.QueryAsync($"get {accounts} select balance order id");
        Assert.Equal([100L, 50L], rows.Select(r => r.GetProperty("balance").GetInt64()));
        // The client's own mark: AsRead() copies take the option's place.
        Assert.Equal(2, (await db.AsRead().CallAsync(history, new { min = 0 })).Rows!.Count);
        // The same statement sent unmarked writes.
        Assert.Equal(90, Balance(await db.CallAsync(transfer, new { from = 1, to = 2, amount = 10 })));
    }

    [Fact]
    public async Task AHeldCallWaitsForTheWritesItsBodyCanMake()
    {
        using var db = Root();
        var jobs = Fresh("jobs");
        var claim = Fresh("claim");
        await db.ExecAsync($"create collection {jobs} (kind text, run_at timestamp @sorted, owner text)");
        await db.ExecAsync($"create statement {claim}($w text) public as set {jobs} {{owner: $w, run_at: now() + 60000}} "
            + "where run_at <= now() order run_at limit 1 returning kind");
        var held = db.CallAsync(claim, new { w = "w1" }, wait: TimeSpan.FromSeconds(10));
        await Task.Delay(200);
        await db.From(jobs).InsertAsync(new Dictionary<string, object?>
        {
            ["kind"] = "mail",
            ["run_at"] = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
        });
        var got = await held.WaitAsync(TimeSpan.FromSeconds(5));
        Assert.Equal("mail", got.Rows!.Single().GetProperty("kind").GetString());
        // Nothing to take: the wait ends on time with no row.
        var began = DateTime.UtcNow;
        Assert.Empty((await db.CallAsync(claim, new { w = "w2" }, wait: TimeSpan.FromMilliseconds(300))).Rows!);
        Assert.True(DateTime.UtcNow - began >= TimeSpan.FromMilliseconds(290));
        // The client's own Timeout is the wait longer for it, and its token gives the call up as ever.
        using var shortly = new FennecClient(servers.Primary,
            new FennecClientOptions { Token = Servers.RootToken, Timeout = TimeSpan.FromSeconds(1) });
        Assert.Empty((await shortly.CallAsync(claim, new { w = "w3" }, wait: TimeSpan.FromMilliseconds(1500))).Rows!);
        using var gone = new CancellationTokenSource(TimeSpan.FromMilliseconds(100));
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() =>
            db.CallAsync(claim, new { w = "w4" }, wait: TimeSpan.FromSeconds(10), cancellationToken: gone.Token));
        Assert.Throws<ArgumentOutOfRangeException>(() => db.WithWait(TimeSpan.FromSeconds(-1)));
        await Assert.ThrowsAsync<ArgumentOutOfRangeException>(() => db.CallAsync(claim, new { w = "w5" }, wait: TimeSpan.FromSeconds(-1)));
    }

    /// <summary>Answers nothing until the request is cancelled, whether by its token or the client's Timeout.</summary>
    sealed class Silent : HttpMessageHandler
    {
        public int Sent;

        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct)
        {
            Interlocked.Increment(ref Sent);
            await Task.Delay(Timeout.Infinite, ct);
            return new HttpResponseMessage(HttpStatusCode.OK);
        }
    }

    [Fact]
    public async Task ACallIsBoundByTheClientsTimeoutAndItsToken()
    {
        var silent = new Silent();
        using var db = new FennecClient("http://silent.invalid",
            new() { HttpClient = new HttpClient(silent), Timeout = TimeSpan.FromMilliseconds(150) });
        // The Timeout bounds it as it bounds a text, not the HttpClient's.
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => db.CallAsync("stamp"));
        using var cts = new CancellationTokenSource();
        using var slow = new FennecClient("http://silent.invalid",
            new() { HttpClient = new HttpClient(silent), Timeout = Timeout.InfiniteTimeSpan });
        var call = slow.CallAsync("stamp", cancellationToken: cts.Token);
        while (Volatile.Read(ref silent.Sent) < 2) await Task.Delay(10);
        cts.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => call);
        // Cancelled before it starts, nothing is sent.
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => slow.CallAsync("stamp", cancellationToken: cts.Token));
    }
}
