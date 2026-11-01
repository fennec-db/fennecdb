package com.fennecdb

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNotEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The builder's `call`, where the golden file's chains cannot reach: what the
 * types hold to, and that a call is a value.
 */
class CallBuilderTest {
    @Test
    fun aCallMakesItsTextAndItsParameters() {
        val st = Call.of("transfer", mapOf("from" to 1, "to" to 2, "amount" to 300)).toCall()
        assertEquals("call transfer \$1", st.text)
        assertEquals(listOf<Any?>(mapOf("from" to 1, "to" to 2, "amount" to 300)), st.params)
        // The claims come after the arguments, numbered after them.
        assertEquals(
            Statement("call mine \$1 as \$2", listOf(mapOf("limit" to 10), mapOf("sub" to "alice"))),
            Call.of("mine", mapOf("limit" to 10), mapOf("sub" to "alice")).toCall(),
        )
        assertEquals(Statement("call stamp as \$1", listOf(mapOf("sub" to "alice"))), Call.of("stamp", claims = mapOf("sub" to "alice")).toCall())
        assertEquals(Statement("call stamp", emptyList()), Call.of("stamp").toCall())
        // An empty map is still one argument.
        assertEquals(Statement("call stamp \$1", listOf(emptyMap<String, Any?>())), Call.of("stamp", emptyMap()).toCall())
        assertEquals("stamp", Call.of("stamp").name)
    }

    /** The values are parameters, normalised as a document's are: a `Date` its ISO text, a vector its numbers, in a nested map too. */
    @Test
    fun theValuesAreParametersNeverTheText() {
        val at = java.util.Date(1_789_821_296_789L)
        val st = Call.of("find", mapOf("title" to "\"; del docs; --", "at" to at, "embed" to floatArrayOf(0.5f, -0.25f), "meta" to mapOf("at" to at)), mapOf("sub" to "x' or 1=1 --")).toCall()
        assertEquals("call find \$1 as \$2", st.text)
        val args = st.params[0] as Map<*, *>
        assertEquals("\"; del docs; --", args["title"])
        assertEquals("2026-09-19T12:34:56.789Z", args["at"])
        assertEquals("2026-09-19T12:34:56.789Z", (args["meta"] as Map<*, *>)["at"])
        assertEquals("[0.5,-0.25]", Json.write(args["embed"]))
        assertEquals(mapOf("sub" to "x' or 1=1 --"), st.params[1])
    }

    @Test
    fun aNameIsHeldToTheEnginesRule() {
        for (ok in listOf("a", "Close_day_2", "a" + "b".repeat(58))) assertEquals("call $ok", Call.of(ok).toCall().text)
        for (bad in listOf("", "1st", "_x", "çağ", "stamp \$1; del docs", "a-b", "a b", "stamp\n", "a" + "b".repeat(59))) {
            val e = assertFailsWith<FennecException>(bad) { Call.of(bad) }
            assertEquals(FennecException.Code.BUILDER, e.code)
            assertEquals("invalid statement name: ${Builder.quote(bad)}", e.message)
        }
        // The same function the top-level `call` is.
        assertFailsWith<FennecException> { call("1st") }
    }

    /** What the types cannot say, a list or a text, is refused by its message when it is handed over untyped. */
    @Test
    fun argumentsAndClaimsAreObjects() {
        for (bad in listOf(listOf(1, 2, 300), "from 1", 5L)) {
            val e = assertFailsWith<FennecException> { Call.made("transfer", bad, null) }
            assertEquals("a call's arguments are an object of values by name", e.message)
            val c = assertFailsWith<FennecException> { Call.made("mine", null, bad) }
            assertEquals("a call's claims are an object of values by name", c.message)
        }
        // A name is judged before the arguments.
        assertEquals("invalid statement name: \"1st\"", assertFailsWith<FennecException> { Call.made("1st", listOf(1), null) }.message)
        // A value the library cannot send is the builder's refusal at the call, not a failure at the server.
        assertEquals(FennecException.Code.BUILDER, assertFailsWith<FennecException> { Call.of("f", mapOf("x" to Any())) }.code)
    }

    /** A call is a value: the map it was made of changed afterwards changes nothing, and its text is the same each time. */
    @Test
    fun aCallIsImmutable() {
        val args = linkedMapOf<String, Any?>("n" to 1, "tags" to mutableListOf("a"), "meta" to linkedMapOf<String, Any?>("k" to 1))
        val claims = linkedMapOf<String, Any?>("sub" to "alice")
        val c = Call.of("mine", args, claims)
        val before = c.toCall()
        args["n"] = 2
        args["extra"] = true
        (args["tags"] as MutableList<String>).add("b")
        (args["meta"] as MutableMap<String, Any?>)["k"] = 2
        claims["sub"] = "mallory"
        assertEquals(before, c.toCall())
        assertEquals(before, c.toCall())
        assertEquals(mapOf("n" to 1, "tags" to listOf("a"), "meta" to mapOf("k" to 1)), c.toCall().params[0])
        assertNotEquals(before, Call.of("mine", args, claims).toCall())
    }

    @Test
    fun theGoldenFileHoldsTheCallCases() {
        val calls = GoldenTest.cases.filter { (it.list("steps")!!.first() as Row).string("op") == "call" }
        // Every one of them is a test of GoldenTest.golden; a harness that skipped them would pass with none.
        assertTrue(calls.size >= 20, "${calls.size} call cases")
        assertTrue(calls.any { it.string("error") != null } && calls.any { it.string("text") != null })
    }
}

/**
 * `db.call` on the embedded database: `call <name> $1` through the library,
 * its claims the embedder's `as`.
 */
class CallLocalTest {
    private val statements = listOf(
        "create statement transfer(\$from int, \$to int, \$amount int) public as begin " +
            "set accounts {balance: balance - \$amount} where id = \$from and balance >= \$amount require 1 else 'insufficient funds'; " +
            "set accounts {balance: balance + \$amount} where id = \$to require 1 else 'no such account'; " +
            "get accounts select id, balance where id = \$from end",
        "create statement stamp() public as insert notes {body: 'z'} returning id",
        "create statement mine() claims (sub text) as get notes select body where by = \$jwt.sub",
        "create statement note(\$body text) claims (sub text) as insert notes {body: \$body, by: \$jwt.sub} returning id",
        "create statement kinds(\$n int, \$s text, \$f float, \$b bool, \$at timestamp, \$tags [text], \$meta json, \$v vector<3>) public as " +
            "insert kinds {n: \$n, s: \$s, f: \$f, b: \$b, at: \$at, tags: \$tags, meta: \$meta, v: \$v} returning id, n, s, f, b, at, tags, meta, v",
    )

    private suspend fun bank(): Fennec {
        val db = Fennec.memory()
        db.execute("create collection accounts (balance int)")
        db.execute("create collection notes (body text, by text)")
        db.execute("create collection kinds (n int, s text, f float, b bool, at timestamp, tags [text], meta json, v vector<3>)")
        db.execute("put accounts [{balance: 1000}, {balance: 1000}]")
        for (s in statements) db.execute(s)
        return db
    }

    @Test
    fun aCallRunsAStatementWithItsArguments() = runBlocking {
        val db = bank()
        val moved = db.call("transfer", mapOf("from" to 1, "to" to 2, "amount" to 300))
        assertEquals(listOf(1L), moved.rows.map { it.long("id") })
        assertEquals(700L, moved.rows[0].long("balance"))
        assertEquals(1300L, db.query("get accounts select balance where id = 2").first().long("balance"))
        // A statement of no arguments, the answer of a `returning`.
        assertEquals(1, db.call("stamp").rows.size)
        assertEquals(1L, db.from("notes").count())
        // Arguments of every type a parameter has.
        val at = java.util.Date(1_789_821_296_789L)
        val kinds = db.call(
            "kinds",
            mapOf(
                "n" to 7, "s" to "çağ \"q\"", "f" to 2.5, "b" to true, "at" to at, "tags" to listOf("a", "b"),
                "meta" to mapOf("lang" to "tr", "rank" to 2), "v" to floatArrayOf(0.5f, -0.25f, 1f),
            ),
        ).rows.single()
        assertEquals(7L, kinds.long("n"))
        assertEquals("çağ \"q\"", kinds.string("s"))
        assertEquals(2.5, kinds.double("f"))
        assertEquals(true, kinds["b"])
        assertEquals("2026-09-19T12:34:56.789Z", kinds.string("at"))
        assertEquals(listOf("a", "b"), kinds["tags"])
        assertEquals("tr", kinds.row("meta")!!.string("lang"))
        assertEquals(listOf(0.5f, -0.25f, 1f), kinds.floats("v")!!.toList())
        db.close()
    }

    @Test
    fun aRequireNotMetPutsTheBlockBackAndSaysItsReason() = runBlocking {
        val db = bank()
        val e = assertFailsWith<FennecException> { db.call("transfer", mapOf("from" to 1, "to" to 2, "amount" to 5000)) }
        assertEquals(FennecException.Code.UNMET, e.code)
        assertEquals("insufficient funds", e.reason)
        val no = assertFailsWith<FennecException> { db.call("transfer", mapOf("from" to 1, "to" to 9, "amount" to 10)) }
        assertEquals("no such account", no.reason)
        // The debit before the credit that found no account was put back with it.
        assertEquals(listOf(1000L, 1000L), db.query("get accounts select balance order id").map { it.long("balance") })
        db.close()
    }

    @Test
    fun aRefusedCallIsTheEnginesRefusal() = runBlocking {
        val db = bank()
        assertEquals(FennecException.Code.NOT_FOUND, assertFailsWith<FennecException> { db.call("nothing") }.code)
        // An argument missing, extra, or of another type.
        for (args in listOf(mapOf("from" to 1, "to" to 2), mapOf("from" to 1, "to" to 2, "amount" to 1, "by" to 3), mapOf("from" to 1, "to" to 2, "amount" to "ten"))) {
            val e = assertFailsWith<FennecException> { db.call("transfer", args) }
            assertEquals(FennecException.Code.QUERY, e.code, e.message)
        }
        // Refused by the builder before anything reaches the library.
        assertEquals(FennecException.Code.BUILDER, assertFailsWith<FennecException> { db.call("1st") }.code)
        assertEquals(listOf(1000L, 1000L), db.query("get accounts select balance order id").map { it.long("balance") })
        db.close()
    }

    /** The embedder's claims are `as`: the statement reads `$jwt.sub` of them, and they are held to the declared types as a token's are. */
    @Test
    fun theEmbeddersClaimsAreTheCalls() = runBlocking {
        val db = bank()
        db.call("note", mapOf("body" to "mine"), claims = mapOf("sub" to "alice"))
        db.call("note", mapOf("body" to "bob's"), claims = mapOf("sub" to "bob"))
        assertEquals(listOf("mine"), db.call("mine", claims = mapOf("sub" to "alice")).rows.map { it.string("body") })
        assertEquals(listOf("bob's"), db.call("mine", claims = mapOf("sub" to "bob")).rows.map { it.string("body") })
        // A claim missing, null, of another type: refused, and nothing ran.
        for (claims in listOf<Map<String, Any?>?>(null, emptyMap(), mapOf("sub" to null), mapOf("sub" to 5))) {
            assertFailsWith<FennecException>("$claims") { db.call("note", mapOf("body" to "x"), claims = claims) }
        }
        assertEquals(2L, db.from("notes").count())
        db.close()
    }

    @Test
    fun callBlockingIsCallOnTheCallingThread() {
        val db = Fennec.memory()
        db.executeBlocking("create collection notes (body text)")
        db.executeBlocking("create statement stamp() public as insert notes {body: 'z'} returning id")
        db.executeBlocking("create statement count_by(\$body text) public as get notes select id where body = \$body")
        assertEquals(1, db.callBlocking("stamp").rows.size)
        assertEquals(1, db.callBlocking("count_by", mapOf("body" to "z")).rows.size)
        assertEquals(0, db.callBlocking("count_by", mapOf("body" to "y")).rows.size)
        db.close()
    }
}

/** `call` on a connection to a real fennec-server (`fennec.server`), the statements defined through the SDK itself. */
class CallRemoteTest {
    private val servers = mutableListOf<Server>()

    private fun server(name: String) = Server("call-$name").also { servers.add(it) }

    @AfterTest
    fun stop() = servers.forEach { it.stop() }

    private val transfer =
        "create statement transfer(\$from int, \$to int, \$amount int) public as begin " +
            "set accounts {balance: balance - \$amount} where id = \$from and balance >= \$amount require 1 else 'insufficient funds'; " +
            "set accounts {balance: balance + \$amount} where id = \$to require 1 else 'no such account'; " +
            "get accounts select id, balance where id = \$from end"

    private suspend fun bank(name: String): Pair<Server, FennecRemote> {
        val s = server(name)
        val db = Fennec.connect(s.url)
        db.execute("create collection accounts (balance int)")
        db.execute("create collection notes (body text, by text)")
        db.execute("create collection kinds (n int, s text, f float, b bool, at timestamp, tags [text], meta json, v vector<3>)")
        db.execute("create collection jobs (kind text, run_at timestamp @sorted, owner text)")
        db.execute("put accounts [{balance: 1000}, {balance: 1000}]")
        db.execute(transfer)
        db.execute("create statement stamp() public as insert notes {body: 'z'} returning id")
        db.execute("create statement mine() claims (sub text) as get notes select body where by = \$jwt.sub")
        db.execute("create statement note(\$body text) claims (sub text) as insert notes {body: \$body, by: \$jwt.sub} returning id")
        db.execute("create statement history() public as get notes select body order id")
        db.execute(
            "create statement kinds(\$n int, \$s text, \$f float, \$b bool, \$at timestamp, \$tags [text], \$meta json, \$v vector<3>) public as " +
                "insert kinds {n: \$n, s: \$s, f: \$f, b: \$b, at: \$at, tags: \$tags, meta: \$meta, v: \$v} returning id, n, s, f, b, at, tags, meta, v",
        )
        // Writes nothing while no job is due: what a held call waits on.
        db.execute("create statement claim(\$owner text) public as set jobs {owner: \$owner} where run_at <= now() order run_at limit 1 returning id, kind")
        return s to db
    }

    @Test
    fun aCallIsAnsweredAsATextIs() = runBlocking {
        val (_, db) = bank("answers")
        val moved = db.call("transfer", mapOf("from" to 1, "to" to 2, "amount" to 300))
        assertEquals(700L, moved.rows.single().long("balance"))
        assertEquals(listOf(1300L), db.query("get accounts select balance where id = 2").map { it.long("balance") })
        assertEquals(1, db.call("stamp").rows.size)
        // The same answer the text gives.
        assertEquals(db.query("call history"), db.call("history").rows)
        val at = java.util.Date(1_789_821_296_789L)
        val kinds = db.call(
            "kinds",
            mapOf(
                "n" to 7, "s" to "çağ \"q\"", "f" to 2.5, "b" to true, "at" to at, "tags" to listOf("a", "b"),
                "meta" to mapOf("lang" to "tr", "rank" to 2), "v" to floatArrayOf(0.5f, -0.25f, 1f),
            ),
        ).rows.single()
        assertEquals(7L, kinds.long("n"))
        assertEquals("çağ \"q\"", kinds.string("s"))
        assertEquals(2.5, kinds.double("f"))
        assertEquals("2026-09-19T12:34:56.789Z", kinds.string("at"))
        assertEquals(listOf("a", "b"), kinds["tags"])
        assertEquals("tr", kinds.row("meta")!!.string("lang"))
        assertEquals(listOf(0.5f, -0.25f, 1f), kinds.floats("v")!!.toList())
        // And on the calling thread.
        assertEquals(1000L - 300 - 1, db.callBlocking("transfer", mapOf("from" to 1, "to" to 2, "amount" to 1)).rows.single().long("balance"))
    }

    @Test
    fun aRefusalIsTheSdksUsualError() = runBlocking {
        val (_, db) = bank("refused")
        // `require` not met: 412, its reason, the block put back.
        val e = assertFailsWith<FennecException> { db.call("transfer", mapOf("from" to 1, "to" to 2, "amount" to 5000)) }
        assertEquals(listOf<Any?>(FennecException.Code.UNMET, 412, "insufficient funds"), listOf(e.code, e.status, e.reason))
        val no = assertFailsWith<FennecException> { db.call("transfer", mapOf("from" to 1, "to" to 9, "amount" to 10)) }
        assertEquals(listOf<Any?>(412, "no such account"), listOf(no.status, no.reason))
        assertEquals(listOf(1000L, 1000L), db.query("get accounts select balance order id").map { it.long("balance") })
        // No such statement: 404.
        val missing = assertFailsWith<FennecException> { db.call("nothing") }
        assertEquals(listOf<Any?>(FennecException.Code.NOT_FOUND, 404), listOf(missing.code, missing.status))
        // An argument missing, extra or of another type: 400.
        for (args in listOf(mapOf("from" to 1, "to" to 2), mapOf("from" to 1, "to" to 2, "amount" to 1, "by" to 3), mapOf("from" to 1, "to" to 2, "amount" to "ten"))) {
            val bad = assertFailsWith<FennecException> { db.call("transfer", args) }
            assertEquals(400, bad.status, "$args: ${bad.message}")
        }
        // A claim a statement declares, missing: 403.
        assertEquals(403, assertFailsWith<FennecException> { db.call("mine") }.status)
        // The builder's own refusal comes first, nothing sent.
        assertEquals(FennecException.Code.BUILDER, assertFailsWith<FennecException> { db.call("stamp; del notes") }.code)
        assertEquals(0L, db.from("notes").count())
    }

    @Test
    fun anIdempotencyKeyMakesACallOnce() = runBlocking {
        val (_, db) = bank("keyed")
        val args = mapOf("from" to 1, "to" to 2, "amount" to 10)
        val first = db.call("transfer", args, idempotencyKey = "t-1")
        val again = db.call("transfer", args, idempotencyKey = "t-1")
        assertEquals(first, again)
        assertEquals(listOf(990L, 1010L), db.query("get accounts select balance order id").map { it.long("balance") })
        // The key with another request is refused, and moved nothing.
        val other = assertFailsWith<FennecException> { db.call("transfer", mapOf("from" to 1, "to" to 2, "amount" to 20), idempotencyKey = "t-1") }
        assertEquals(422, other.status)
        // A keyless stamp writes each time, a keyed one once.
        db.call("stamp")
        db.call("stamp")
        db.call("stamp", idempotencyKey = "s-1")
        db.call("stamp", idempotencyKey = "s-1")
        assertEquals(3L, db.from("notes").count())
        // A connection's own key is the call's when it gives none.
        val keyed = db.withIdempotencyKey("s-2")
        keyed.call("stamp")
        keyed.call("stamp")
        assertEquals(4L, db.from("notes").count())
    }

    /** The server's token says whose call it is: `claims` is the `as`. */
    @Test
    fun claimsAreWhatTheServersTokenSays() = runBlocking {
        val (_, db) = bank("claims")
        db.call("note", mapOf("body" to "mine"), claims = mapOf("sub" to "alice"))
        db.call("note", mapOf("body" to "bob's"), claims = mapOf("sub" to "bob"))
        assertEquals(listOf("mine"), db.call("mine", claims = mapOf("sub" to "alice")).rows.map { it.string("body") })
        assertEquals(listOf("bob's"), db.call("mine", claims = mapOf("sub" to "bob")).rows.map { it.string("body") })
        // A claim of another type than declared is refused.
        assertEquals(403, assertFailsWith<FennecException> { db.call("mine", claims = mapOf("sub" to 5)) }.status)
        assertEquals(2L, db.from("notes").count())
    }

    /** `read` marks a call as a text known to read is marked: answered where a read goes, and one that writes refused, never landed. */
    @Test
    fun aReadCallGoesAsAReadAndAWritingOneIsRefused() = runBlocking {
        val (s, home) = bank("read")
        home.call("stamp")
        // The connection reading near sends the mark, to a server that runs a marked request read-only on every route.
        val db = Fennec.connect(s.url, reads = FennecRemote.Reads.NEAREST)
        assertEquals(listOf("z"), db.call("history", read = true).rows.map { it.string("body") })
        assertEquals(listOf("z"), db.callBlocking("history", read = true).rows.map { it.string("body") })
        val refused = assertFailsWith<FennecException> { db.call("stamp", read = true) }
        assertEquals(403, refused.status)
        assertEquals(403, assertFailsWith<FennecException> { db.call("transfer", mapOf("from" to 1, "to" to 2, "amount" to 1), read = true) }.status)
        assertEquals(1L, home.from("notes").count())
        assertEquals(listOf(1000L, 1000L), home.query("get accounts select balance order id").map { it.long("balance") })
        // Without it a call goes home, whatever it does.
        db.call("stamp")
        assertEquals(2L, home.from("notes").count())
        // On a connection reading home `read` changes nothing: it is no mark there.
        home.call("stamp", read = true)
        assertEquals(3L, home.from("notes").count())
    }

    /** A call's statement is what `batch` takes beside a query's: the calls land together, or none does. */
    @Test
    fun aCallIsAStatementABatchTakes() = runBlocking {
        val (_, db) = bank("batch")
        val out = db.batch(listOf(call("stamp").toCall(), call("transfer", mapOf("from" to 1, "to" to 2, "amount" to 5)).toCall(), call("history").toCall()))
        assertEquals(3, out.results.size)
        assertEquals(listOf("z"), out.results[2].rows.map { it.string("body") })
        assertEquals(listOf(995L, 1005L), db.query("get accounts select balance order id").map { it.long("balance") })
        val e = assertFailsWith<FennecException> {
            db.batch(listOf(call("stamp").toCall(), call("transfer", mapOf("from" to 1, "to" to 2, "amount" to 5000)).toCall()))
        }
        assertEquals(listOf<Any?>(412, 1, "insufficient funds"), listOf(e.status, e.at, e.reason))
        assertEquals(1L, db.from("notes").count())
    }

    /** `wait` holds a call that writes nothing, as it holds a text; a coroutine cancelled gives it up at once. */
    @Test
    fun aWaitHoldsACallAndCancellingGivesItUp() = runBlocking {
        val (_, db) = bank("wait")
        val began = System.nanoTime()
        assertTrue(db.call("claim", mapOf("owner" to "w1"), wait = 300).rows.isEmpty())
        assertTrue(System.nanoTime() - began >= 290_000_000L)
        // Held until a job comes due, then taken.
        val held = async(Dispatchers.Default) { db.call("claim", mapOf("owner" to "w2"), wait = 10_000) }
        delay(200)
        db.execute("put jobs {kind: \"mail\", run_at: \$1}", System.currentTimeMillis())
        assertEquals(listOf("mail"), withTimeout(5_000) { held.await() }.rows.map { it.string("kind") })
        db.execute("del jobs where kind = 'mail'")
        val given = async(Dispatchers.Default) { db.call("claim", mapOf("owner" to "w3"), wait = 10_000) }
        delay(100)
        val t = System.nanoTime()
        given.cancelAndJoin()
        assertTrue(System.nanoTime() - t < 2_000_000_000L, "a cancelled call let go at once")
        assertTrue(db.callBlocking("claim", mapOf("owner" to "w4"), wait = 0).rows.isEmpty())
    }

    /** What a call's options put on the wire, against a script: the endpoint, the body, the mark, the key and the wait. */
    @Test
    fun theOptionsAreTheTextsOnTheWire() = runBlocking {
        Wire(null, listOf(Scripted(body = """[{"id":1}]""", seq = "5", history = "1-00000000000000aa"), Scripted(), Scripted(), Scripted())).use { sent ->
            val db = Fennec.connect(sent.url, reads = FennecRemote.Reads.NEAREST)
            db.call("stamp", idempotencyKey = "k-1", wait = 1500)
            assertEquals("k-1", sent.last["idempotency-key"])
            assertEquals("1500", sent.last["fennec-wait"])
            assertNull(sent.last["fennec-read"])
            db.call("history", read = true)
            assertEquals("replica", sent.last["fennec-read"])
            assertEquals("5", sent.last["fennec-after"])
            assertNull(sent.last["idempotency-key"])
            db.call("note", mapOf("body" to "b"), claims = mapOf("sub" to "alice"))
            assertNull(sent.last["fennec-read"])
            // A copy's key and wait are kept by a call.
            db.withWait(700).withIdempotencyKey("k-2").call("stamp")
            assertEquals("k-2", sent.last["idempotency-key"])
            assertEquals("700", sent.last["fennec-wait"])
            assertEquals(4, sent.all.size)
        }
    }
}
