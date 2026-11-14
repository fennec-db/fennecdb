package com.fennecdb

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import java.io.BufferedInputStream
import java.io.IOException
import java.io.InputStream
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.net.StandardSocketOptions
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue

/**
 * What a connection sends again, and when it gives a request up: only what
 * cannot have run -- an answer naming its wait, a connection refused -- or
 * may run twice -- a read the builder made or sent near, a write under a key
 * -- never after the connection's own timeout
 * (docs/design/js-client.md#retries). Against a scripted server on loopback
 * ([Stub]), no fennec-server needed. A reset is what the JVM's
 * HttpURLConnection sends a buffered POST again after on its own: the tests
 * of a write without a key would see it twice.
 */
class RetryTest {
    private val wrote = """{"affected":1}"""

    /**
     * A 503 naming its wait ran nothing: any request goes again, a write
     * without a key too, after that wait and never sooner -- `retry_after_ms`
     * before `Retry-After` -- from a coroutine or the calling thread alike.
     */
    @Test
    fun a503NamingItsWaitIsSentAgainNoSoonerThanTheWait() = runBlocking<Unit> {
        val moved = Stub.Act.Answer(
            503, """{"error":"the tenant is being moved; retry shortly","retry_after":1,"retry_after_ms":300}""",
            mapOf("retry-after" to "1"),
        )
        Stub(moved, Stub.Act.Answer(200, wrote)).use { s ->
            assertEquals(1L, Fennec.connect(s.url).execute("put notes {n: 1}"))
            val seen = s.seen
            assertEquals(2, seen.size)
            assertTrue(seen[1].at - seen[0].at >= 300_000_000L, "sent again ${(seen[1].at - seen[0].at) / 1_000_000} ms after")
        }
        Stub(moved, Stub.Act.Answer(200, wrote)).use { s ->
            assertEquals(1L, Fennec.connect(s.url).runBlocking("put notes {n: 1}").affected)
            assertEquals(2, s.seen.size)
            assertTrue(s.seen[1].at - s.seen[0].at >= 300_000_000L)
        }
    }

    /** A 429 that names its wait only in `Retry-After` is sent again after that wait. */
    @Test
    fun a429NamingItsWaitOnlyInTheHeaderIsSentAgainAfterIt() = runBlocking<Unit> {
        val slow = Stub.Act.Answer(429, """{"error":"slow down"}""", mapOf("retry-after" to "1"))
        Stub(slow, Stub.Act.Answer(200, wrote)).use { s ->
            assertEquals(1L, Fennec.connect(s.url).execute("put notes {n: 1}"))
            assertEquals(2, s.seen.size)
            assertTrue(s.seen[1].at - s.seen[0].at >= 1_000_000_000L)
        }
    }

    /** Cancelled in the pause before a retry, a request ends there and nothing goes again. */
    @Test
    fun aCancelInThePauseBeforeARetryEndsTheRequest() = runBlocking<Unit> {
        val busy = Stub.Act.Answer(503, """{"error":"busy","retry_after_ms":5000}""")
        Stub(busy, Stub.Act.Answer(200, "[]")).use { s ->
            val db = Fennec.connect(s.url)
            val paused = CompletableDeferred<Unit>()
            db.pausing = { paused.complete(Unit) }
            val job = launch(Dispatchers.IO) { db.from("docs").rows() }
            paused.await()
            job.cancelAndJoin()
            assertTrue(job.isCancelled)
            assertEquals(1, s.seen.size)
        }
    }

    /** A connection refused sent nothing: any request goes again, a write without a key too. */
    @Test
    fun aConnectionRefusedIsTriedAgainWhateverTheRequest() = runBlocking<Unit> {
        Stub(Stub.Act.Answer(200, wrote), listening = false).use { s ->
            val db = Fennec.connect(s.url)
            db.pausing = { s.pausing() }
            assertEquals(1L, db.execute("put notes {n: 1}"))
            assertEquals(1, s.pauses)
            assertEquals(1, s.seen.size)
        }
    }

    /**
     * A write without a key may have landed before a reset: it is never sent
     * again -- by the client, nor by HttpURLConnection on its own, over the
     * connection it kept or a new one -- and the client makes no key. Under a
     * key it goes again, answered from the key.
     */
    @Test
    fun aWriteIsSentAgainAfterAResetOnlyUnderAKey() = runBlocking<Unit> {
        Stub(Stub.Act.Answer(200, wrote), Stub.Act.Reset, Stub.Act.Reset, Stub.Act.Answer(200, wrote)).use { s ->
            val db = Fennec.connect(s.url)
            assertEquals(1L, db.execute("put notes {n: 0}"))
            assertEquals(FennecException.Code.IO, assertFailsWith<FennecException> { db.execute("put notes {n: 1}") }.code)
            assertEquals(2, s.seen.size, "a write that may have landed is never made twice")
            assertEquals(FennecException.Code.IO, assertFailsWith<FennecException> { db.runBlocking("put notes {n: 2}") }.code)
            assertEquals(3, s.seen.size)
            assertEquals(listOf<String?>(null, null, null), s.keys, "the client makes no key of its own")
        }
        Stub(Stub.Act.Reset, Stub.Act.Answer(200, wrote, mapOf("idempotent-replayed" to "true"))).use { s ->
            assertEquals(1L, Fennec.connect(s.url).withIdempotencyKey("n-1").from("notes").insert(mapOf("n" to 1)))
            assertEquals(listOf<String?>("n-1", "n-1"), s.keys)
        }
    }

    /**
     * A read the builder made goes again after a reset; a text the caller
     * says reads ([FennecRemote.asRead]), sent home, is a claim nothing
     * checks, and goes once. Sent near it is marked (`Fennec-Read: replica`),
     * which every server runs read-only: again.
     */
    @Test
    fun aBuilderReadIsSentAgainAfterAResetAndATextSentHomeIsNot() = runBlocking<Unit> {
        val rows = Stub.Act.Answer(200, """[{"id":1}]""")
        Stub(Stub.Act.Reset, rows, Stub.Act.Reset, rows).use { s ->
            val db = Fennec.connect(s.url)
            assertEquals(listOf(1L), db.from("docs").rows().map { it.long("id") })
            assertEquals(2, s.seen.size)
            assertFailsWith<FennecException> { db.asRead().query("get docs") }
            assertEquals(3, s.seen.size)
        }
        Stub(Stub.Act.Reset, Stub.Act.Answer(200, """[{"id":2}]""")).use { s ->
            val near = Fennec.connect(s.url, reads = FennecRemote.Reads.NEAREST).asRead()
            assertEquals(listOf(2L), near.query("get docs").map { it.long("id") })
            assertEquals(listOf<String?>("replica", "replica"), s.seen.map { it.head["fennec-read"] })
        }
    }

    /**
     * A batch stopped after some of its statements landed (`completed` above
     * 0) is never sent again, whatever wait it names: sent again whole, they
     * were made twice.
     */
    @Test
    fun aStopAfterStatementsLandedIsNeverSentAgain() = runBlocking<Unit> {
        Stub(
            Stub.Act.Answer(503, """{"error":"moved","completed":1,"at":1,"retry_after_ms":50}""", mapOf("retry-after" to "1")),
            Stub.Act.Answer(200, """{"results":[]}"""),
        ).use { s ->
            val e = assertFailsWith<FennecException> {
                Fennec.connect(s.url).batch(listOf(Statement("put notes {n: 1}", emptyList()), Statement("compact", emptyList())))
            }
            assertEquals(503, e.status)
            assertEquals(1, e.completed)
            assertEquals(1, s.seen.size)
        }
    }

    /**
     * The timeout bounds the wait for the head; past it nothing goes again,
     * a read the builder made neither: the server may still be running it.
     */
    @Test
    fun nothingGoesAgainAfterTheConnectionsOwnTimeout() = runBlocking<Unit> {
        Stub(Stub.Act.Hold, Stub.Act.Answer(200, "[]")).use { s ->
            val db = Fennec.connect(s.url, timeout = 500)
            val started = System.nanoTime()
            val e = assertFailsWith<FennecException> { db.from("docs").rows() }
            assertTrue(e.message!!.contains("timeout"), e.message)
            assertTrue(System.nanoTime() - started >= 500_000_000L)
            assertEquals(1, s.seen.size)
        }
    }

    /** Three tries unless told, the last answer the caller's; `retry = null` sends nothing again. */
    @Test
    fun theTriesAreBoundedAndRetryNullSendsNothingAgain() = runBlocking<Unit> {
        val busy = Stub.Act.Answer(503, """{"error":"too many connections","retry_after":1,"retry_after_ms":5}""")
        Stub(busy, busy, busy, Stub.Act.Answer(200, "[]")).use { s ->
            val e = assertFailsWith<FennecException> { Fennec.connect(s.url).from("docs").rows() }
            assertEquals(503, e.status)
            assertEquals(0.005, e.retryAfter)
            assertEquals(3, s.seen.size)
        }
        Stub(busy, busy, busy, Stub.Act.Answer(200, """[{"id":1}]""")).use { s ->
            val db = Fennec.connect(s.url, retry = FennecRemote.Retry(attempts = 4))
            assertEquals(listOf(1L), db.from("docs").rows().map { it.long("id") })
            assertEquals(4, s.seen.size)
        }
        Stub(busy, Stub.Act.Answer(200, "[]")).use { s ->
            assertEquals(503, assertFailsWith<FennecException> { Fennec.connect(s.url, retry = null).from("docs").rows() }.status)
            assertEquals(1, s.seen.size)
        }
        assertFailsWith<IllegalArgumentException> { FennecRemote.Retry(attempts = 0) }
    }

    /**
     * The code the server named is the error's `errorCode` -- never `code`,
     * the library's kind -- and a refusal naming none has none; neither is
     * sent again.
     */
    @Test
    fun aRefusalNamesTheServersCode() = runBlocking<Unit> {
        Stub(
            Stub.Act.Answer(401, """{"error":"the token has expired","code":"token_expired"}"""),
            Stub.Act.Answer(401, """{"error":"invalid or missing token"}"""),
        ).use { s ->
            val db = Fennec.connect(s.url, "t")
            val e = assertFailsWith<FennecException> { db.from("docs").rows() }
            assertEquals(401, e.status)
            assertEquals(FennecException.Code.DENIED, e.code)
            assertEquals("token_expired", e.errorCode)
            val none = assertFailsWith<FennecException> { db.from("docs").rows() }
            assertEquals(401, none.status)
            assertEquals(null, none.errorCode)
            assertEquals(2, s.seen.size)
        }
    }
}

/**
 * A server on loopback answering from a script, one act a request it reads:
 * an answer over the kept connection; a reset, the connection closed with a
 * RST once the request is read whole; or a hold, no answer until the client
 * lets the connection go. Past the script's end it answers `[]`. Each
 * request read is noted with the time it came and its head ([seen]). Made
 * with `listening = false` it refuses every connection until [listen].
 */
class Stub(vararg plan: Act, listening: Boolean = true) : AutoCloseable {
    sealed class Act {
        data class Answer(val status: Int, val body: String, val headers: Map<String, String> = emptyMap()) : Act()
        object Reset : Act()
        object Hold : Act()
    }

    /** A request read: when ([System.nanoTime]), and its head, names in lower case. */
    data class Seen(val at: Long, val head: Map<String, String>)

    private val loopback = InetAddress.getLoopbackAddress()
    private val plan = ArrayDeque(plan.toList())
    private val heads = mutableListOf<Seen>()
    private val conns = mutableSetOf<Socket>()
    private var server: ServerSocket? = null
    private var paused = 0

    /**
     * Where a stub made with `listening = false` holds its port: the local
     * end of a live connection to a listener of its own. A connect to the
     * port is refused until [listen] binds beside it (`SO_REUSEPORT`), the
     * port never free in between for another to take -- and macOS answers a
     * connect to a socket bound but not listening with nothing at all.
     */
    private val held = mutableListOf<AutoCloseable>()

    val port: Int

    val url get() = "http://127.0.0.1:$port"

    init {
        if (listening) {
            val s = bound(0)
            port = s.localPort
            serve(s)
        } else {
            val l = ServerSocket(0, 1, loopback)
            val h = Socket()
            h.reuseAddress = true
            h.setOption(StandardSocketOptions.SO_REUSEPORT, true)
            h.connect(InetSocketAddress(loopback, l.localPort))
            held += listOf(h, l.accept(), l)
            port = h.localPort
        }
    }

    private fun bound(port: Int): ServerSocket = ServerSocket().apply {
        reuseAddress = true
        if (supportedOptions().contains(StandardSocketOptions.SO_REUSEPORT)) setOption(StandardSocketOptions.SO_REUSEPORT, true)
        bind(InetSocketAddress(loopback, port), 16)
    }

    /** Every request read so far. */
    val seen: List<Seen> @Synchronized get() = heads.toList()

    /** The `Idempotency-Key` each request carried, null for none. */
    val keys: List<String?> get() = seen.map { it.head["idempotency-key"] }

    /** How often the client's probe said it was about to wait. */
    val pauses: Int @Synchronized get() = paused

    /** The client's probe: about to wait before a request goes again -- a refused connection's, where this starts listening. */
    fun pausing() {
        synchronized(this) { paused++ }
        listen()
    }

    /** Takes connections from here on: until now each was refused. */
    @Synchronized fun listen() {
        if (server == null) serve(bound(port))
    }

    private fun serve(s: ServerSocket) {
        synchronized(this) { server = s }
        Thread {
            while (true) {
                val c = try {
                    s.accept()
                } catch (e: IOException) {
                    return@Thread
                }
                synchronized(this) { conns += c }
                Thread { answer(c) }.apply { isDaemon = true }.start()
            }
        }.apply { isDaemon = true }.start()
    }

    private fun answer(c: Socket) {
        try {
            val input = BufferedInputStream(c.getInputStream())
            while (true) {
                val head = request(input) ?: return
                val act = synchronized(this) {
                    heads += Seen(System.nanoTime(), head)
                    plan.removeFirstOrNull() ?: Act.Answer(200, "[]")
                }
                when (act) {
                    is Act.Answer -> {
                        val body = act.body.encodeToByteArray()
                        val text = StringBuilder("HTTP/1.1 ${act.status} X\r\ncontent-type: application/json\r\ncontent-length: ${body.size}\r\n")
                        act.headers.forEach { (k, v) -> text.append("$k: $v\r\n") }
                        c.getOutputStream().apply {
                            write((text.append("\r\n")).toString().encodeToByteArray() + body)
                            flush()
                        }
                    }
                    Act.Reset -> {
                        c.setSoLinger(true, 0)
                        return
                    }
                    Act.Hold -> {
                        while (input.read() >= 0) Unit
                        return
                    }
                }
            }
        } catch (e: IOException) {
            // The client let the connection go.
        } finally {
            synchronized(this) { conns -= c }
            c.close()
        }
    }

    /** One request read whole -- its head and the body its length names -- its head as names in lower case; null at the connection's end. */
    private fun request(input: InputStream): Map<String, String>? {
        val head = StringBuilder()
        while (!head.endsWith("\r\n\r\n")) {
            val b = input.read()
            if (b < 0) return null
            head.append(b.toChar())
        }
        val lines = head.split("\r\n")
        val fields = lines.drop(1).filter { ':' in it }
            .associate { it.substringBefore(':').trim().lowercase() to it.substringAfter(':').trim() }
        val length = fields["content-length"]?.toInt() ?: 0
        if (input.readNBytes(length).size < length) return null
        return fields + ("" to lines.first())
    }

    /** Lets every connection go and stops listening. */
    override fun close() {
        val (s, open) = synchronized(this) { server to conns.toList() }
        s?.close()
        open.forEach { runCatching { it.close() } }
        held.forEach { runCatching { it.close() } }
    }
}
