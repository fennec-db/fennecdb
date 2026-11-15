package com.fennecdb

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.asExecutor
import kotlinx.coroutines.delay
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import java.io.IOException
import java.io.InterruptedIOException
import java.net.ConnectException
import java.net.HttpURLConnection
import java.net.SocketTimeoutException
import java.net.URL
import kotlin.math.ceil
import kotlin.random.Random

/**
 * A fennec-server over HTTP, with no file on the device: every query goes to
 * the server (`POST /query`), through the same builder. What
 * [Fennec.Companion.connect] makes, as `connect` does in JS.
 *
 * ```kotlin
 * val db = Fennec.connect("https://api.example.com", token = jwt)
 * val open = db.from("todos").where("done", false).rows()
 * ```
 *
 * The requests are `HttpURLConnection`'s, so TLS is the system's. A request
 * goes again only where it cannot have run or may run twice ([Retry]), and
 * the timeout bounds the wait for an answer's head and each wait between the
 * pieces of its body.
 */
class FennecRemote private constructor(
    url: String,
    /**
     * The token, the last `Fennec-Seq` and the furthest position, shared with
     * every copy [withIdempotencyKey], [withWait] and [asRead] make, as Go's
     * and .NET's copies share theirs.
     */
    private val shared: Shared,
    private val idempotencyKey: String?,
    /** How long the server may hold a write that writes nothing, ms ([withWait]). */
    private val held: Long? = null,
    /** This copy's requests only read ([asRead], the builder's reads). */
    private val reading: Boolean = false,
    /**
     * This copy's requests are the builder's reads ([from]): reads the
     * builder made, which may run twice -- unlike an [asRead] text, a claim
     * nothing checks at home.
     */
    private val built: Boolean = false,
) {
    internal constructor(url: String, token: String?, reads: Reads = Reads.HOME, timeout: Long = 30_000, retry: Retry? = Retry()) :
        this(url, Shared(token, reads, timeout, retry), null) {
        require(timeout > 0) { "a timeout is milliseconds, above 0" }
    }

    /**
     * How a connection sends a request again: only where nothing ran, or
     * where running it again changes nothing (docs/design/js-client.md#retries).
     *
     * Nothing ran: a 503, or a quota's 429, naming its wait -- sent after that
     * wait, never sooner, unless its body says statements of a batch were
     * made (`completed`) -- and a connection refused. Running it again changes
     * nothing: a read the builder made, one sent near (`Fennec-Read: replica`,
     * which every server runs read-only), and a write under an
     * `Idempotency-Key` -- these go again after a reset or a dropped connection
     * too. Never after the connection's own timeout, past which the server may
     * still run it, nor once the coroutine is cancelled or the thread
     * interrupted, nor after a refusal naming no wait; an [asRead] text sent
     * home is a claim nothing checks, and goes once.
     *
     * [attempts] tries at most, their waits -- the server's, or 250 ms
     * doubling, 30% jitter on top -- within [maxWait] milliseconds; past
     * either, the last answer or error is the caller's. `retry = null` on
     * `Fennec.connect` sends nothing again.
     */
    data class Retry(val attempts: Int = 3, val maxWait: Long = 10_000) {
        init {
            require(attempts >= 1 && maxWait >= 0) { "bad retry: $attempts attempts, $maxWait ms" }
        }
    }

    /**
     * Where a connection's reads go. [HOME], the default: every request to
     * the tenant's home. [NEAREST]: the builder's reads, and an [asRead]
     * copy's requests, are marked (`Fennec-Read: replica`) so that a
     * fennec-shard router near a region holding the tenant's read replica
     * answers them there, each sent after the furthest position an answer
     * from home named to this connection (`Fennec-After`, `Fennec-History`:
     * the change and its history) -- never older than its own writes, nor a
     * write a failover replaced. Writes go home.
     */
    enum class Reads { HOME, NEAREST }

    private class Shared(@Volatile var token: String?, val reads: Reads, val timeout: Long, val retry: Retry?) {
        /** The tests' probe, called with each wait before a request goes again; null, as it always is outside them. */
        @Volatile var pausing: ((Long) -> Unit)? = null

        /** The furthest position an answer from home named, which a read near is sent after: an answer that arrives late is not the last. */
        private var at: Position? = null

        @Synchronized fun position(): Position? = at

        /** An answer from home named [p]. */
        @Synchronized fun answered(p: Position) {
            if (p.isPast(at)) at = p
        }

        /**
         * Home's position, lower or not, which a read near's 504 names when
         * no copy held the one it was sent after, [sent] -- a write lost in a
         * failover: kept, every read near would wait it out for good. Taken
         * only over the position sent -- a write answered meanwhile is
         * further, and set back the next read near would miss it -- and
         * never of an earlier term of the same lineage: a router whose copy
         * of the directory lags a failover sends the read to the old
         * primary, fenced, which names the branch it abandoned. Another
         * lineage's is a tenant made again under the name, which starts
         * again at term 1.
         */
        @Synchronized fun told(sent: Position, p: Position) {
            if (at == sent && (p.term >= sent.term || p.isOfAnotherLineage(sent))) at = p
        }
    }

    val url: String = url.trimEnd('/')

    /** Where this connection's reads go ([Fennec.Companion.connect]'s `reads`). */
    val reads: Reads get() = shared.reads

    /**
     * Milliseconds to an answer's head, a [withWait] copy's wait on top, and
     * between the pieces of its body ([Fennec.Companion.connect]'s `timeout`).
     */
    val timeout: Long get() = shared.timeout

    /** How a request goes again, null for never ([Fennec.Companion.connect]'s `retry`). */
    val retry: Retry? get() = shared.retry

    /** The tests' probe of each wait before a request goes again. */
    internal var pausing: ((Long) -> Unit)?
        get() = shared.pausing
        set(value) {
            shared.pausing = value
        }

    /** A fresh token for the requests from here on. */
    fun setToken(token: String?) {
        shared.token = token
    }

    /**
     * The furthest change an answer from home -- a write's, a read-only
     * batch's -- left the database at (`Fennec-Seq`), through this
     * connection or a copy of it; null before any. Answers arriving out of
     * order do not move it back, a read near's answer, which may be a
     * replica's, moves it not, and a read near's 504 sets it to where home
     * says it stands, as Go's `Seq()` and .NET's `Seq` are.
     */
    val seq: Long? get() = shared.position()?.seq

    /**
     * A copy whose writes -- [run], [batch] and the builder's -- carry [key]
     * as their `Idempotency-Key`: sent again after a timeout, a write is
     * answered as it was the first time and not made twice. One key a write:
     * the same key with another request is refused (status 422).
     *
     * ```kotlin
     * db.withIdempotencyKey(orderId).from("orders").insert(order)
     * ```
     */
    fun withIdempotencyKey(key: String): FennecRemote {
        require(key.isNotEmpty()) { "an idempotency key is a text, not empty" }
        return FennecRemote(url, shared, key, held, reading)
    }

    /**
     * A copy whose `set` and `del` statements -- [run], [batch] and the
     * builder's `update`, `delete` and their `Returning` forms -- the server
     * holds while they write nothing, until they can or for [millis], at
     * most 30 000 (`Fennec-Wait`): a worker's claim waits there for a job --
     * one enqueued, a delayed one come due, a lease lapsed -- rather than
     * sleeping and claiming again. Cancelling the coroutine closes the
     * connection, which gives the claim up at once.
     *
     * ```kotlin
     * val claimed = db.withWait(30_000).from("jobs").where(Cond.raw("run_at <= now()"))
     *     .order("run_at").limit(10).updateReturning(claim)
     * ```
     */
    fun withWait(millis: Long): FennecRemote {
        require(millis >= 0) { "a wait is milliseconds, from 0" }
        return FennecRemote(url, shared, idempotencyKey, millis, reading)
    }

    /**
     * A copy whose requests only read: on a connection made with `reads =
     * Reads.NEAREST` a router near the tenant's read replica answers them
     * there, after this connection's writes. The builder's reads go so on
     * their own; a text the caller knows reads -- a [run], a [batch] of
     * `toFennecQL()`s -- goes through this, and one that writes is refused
     * (status 403), never landed. On a connection reading home it changes
     * nothing.
     *
     * ```kotlin
     * val n = db.asRead().query("get orders where customer = $1 count", "c-7")
     * ```
     */
    fun asRead(): FennecRemote = FennecRemote(url, shared, idempotencyKey, held, true)

    /** Runs FennecQL on the server with [params] for `$1`, `$2` ... */
    suspend fun run(text: String, vararg params: Any?): Answer = runList(text, params.toList())

    /** [run], its parameters as a list. With [idempotencyKey] a write runs once however often it is sent. */
    suspend fun runList(text: String, params: List<Any?>, idempotencyKey: String? = null): Answer {
        val body = Json.write(mapOf("query" to text, "params" to params.map { Values.normalize(it) }))
        return answer(posting("/query", "application/json", body, idempotencyKey).first)
    }

    /** The rows a statement answers. */
    suspend fun query(text: String, vararg params: Any?): List<Row> = runList(text, params.toList()).rows

    /** A write: how many documents it wrote. */
    suspend fun execute(text: String, vararg params: Any?): Long = runList(text, params.toList()).affected

    /**
     * The query builder, its queries run on the server -- its reads as an
     * [asRead] copy's, near on a connection reading near, and sent again
     * after a reset as reads the builder made ([Retry]).
     */
    fun from(collection: String): Query {
        val reader = FennecRemote(url, shared, idempotencyKey, held, reading = true, built = true)
        return Query.from(collection).bind(
            { text, params -> runList(text, params) },
            { text, params -> reader.runList(text, params) },
        )
    }

    /**
     * Calls the named statement [name] on the server with [args], a map of
     * values by name: the builder's `call <name> $1` ([Call.toCall]) through
     * [runList], so the transport, the errors, the key and the wait are a
     * text's. The answer is a text's too: the rows of a statement ending in a
     * `get` or a `returning`, or how many documents it wrote.
     *
     * [claims] are what the server's token says the call is made as (`as
     * $2`); a scoped token's own are its token's, and it may not say them
     * (403). [idempotencyKey] makes a call that writes run once however often
     * it is sent, as [runList]'s does; [wait] holds a call that writes
     * nothing for at most that many milliseconds, as [withWait]'s copy does
     * (30 000 at most). [read] marks a call known to only read, as [asRead]
     * does: on a connection reading near it is answered by the read replica
     * near the app, and one that writes is refused (403), never landed;
     * without it a call goes home, as every text not known to read does.
     * A refusal is a [FennecException]: status 404 no such statement or not
     * granted, 403 a claim missing, 400 an argument missing, extra or of
     * another type, 409 a broken statement or a unique value taken, and 412 a
     * `require` not met with its [FennecException.reason].
     *
     * ```kotlin
     * db.call("transfer", mapOf("from" to 1, "to" to 2, "amount" to 300), idempotencyKey = "t-9")
     * val mine = db.call("history", read = true).rows
     * ```
     */
    suspend fun call(
        name: String,
        args: Map<String, Any?>? = null,
        claims: Map<String, Any?>? = null,
        idempotencyKey: String? = null,
        wait: Long? = null,
        read: Boolean = false,
    ): Answer {
        val st = Call.of(name, args, claims).toCall()
        return asCalled(wait, read).runList(st.text, st.params, idempotencyKey)
    }

    /** [call], on the calling thread. */
    @JvmOverloads
    fun callBlocking(
        name: String,
        args: Map<String, Any?>? = null,
        claims: Map<String, Any?>? = null,
        idempotencyKey: String? = null,
        wait: Long? = null,
        read: Boolean = false,
    ): Answer {
        val st = Call.of(name, args, claims).toCall()
        return asCalled(wait, read).send(st.text, st.params, idempotencyKey)
    }

    /**
     * The named statements the server holds, as it lists them to its own
     * token (`statements`) -- a scoped one is refused (403): each one's
     * signature, whether it writes, its comment, why the schema broke it,
     * what a binding is bound to, a scheduled one's clock, and a webhook
     * consumer's hook and whether the server holds its secret. [call] sends
     * a webhook consumer a test event, the event its argument, run as a
     * delivery is and keeping no key.
     */
    suspend fun statements(): List<NamedStatement> = query("statements").map(NamedStatement::of)

    /** [statements], on the calling thread. */
    fun statementsBlocking(): List<NamedStatement> = send("statements", emptyList(), null).rows.map(NamedStatement::of)

    /** This connection, or a copy of it that waits and reads as a call asks. */
    private fun asCalled(wait: Long?, read: Boolean): FennecRemote {
        var c = if (read) asRead() else this
        if (wait != null) c = c.withWait(wait)
        return c
    }

    /** [run], on the calling thread. */
    fun runBlocking(text: String, vararg params: Any?): Answer = send(text, params.toList(), null)

    /**
     * What [batch] answers: each statement's answer in order, the change the
     * batch left the database at, and whether the answer is the one kept for
     * its idempotency key ([replayed]) -- a replayed answer carries no [seq].
     */
    data class BatchAnswer(val results: List<Answer>, val seq: Long?, val replayed: Boolean)

    /**
     * `POST /batch`: the statements in order under one write lock, as one
     * block -- their writes all land, or at the first error none of them do,
     * a [FennecException] whose [FennecException.at] is the statement that
     * stopped it ([FennecException.Code.UNMET] for a write's `require` not
     * met, its [FennecException.reason] the one the statement gave, which
     * stays put as statements go in front of it where `at` moves). A
     * statement is what the builder's `toInsert`, `toUpdate`,
     * `toDelete` and `toFennecQL` make. With [idempotencyKey] a retry after a
     * timeout is answered as the first try was and writes nothing twice; a
     * batch of reads alone takes no key.
     */
    suspend fun batch(statements: List<Statement>, idempotencyKey: String? = null): BatchAnswer {
        val lines = statements.joinToString("\n") { s ->
            Json.write(mapOf("query" to s.text, "params" to s.params.map { Values.normalize(it) }))
        }
        val (v, seq, replayed) = posting("/batch", "application/x-ndjson", lines, idempotencyKey)
        val results = ((v as? Row)?.list("results") ?: emptyList()).map { answer(it ?: Row(emptyMap())) }
        return BatchAnswer(results, seq, replayed)
    }

    /**
     * [post] off the calling thread, as a coroutine that cancelled closes its
     * connection: a read blocked in `HttpURLConnection` heeds no interrupt,
     * and a held claim given up went on waiting for the server's answer.
     */
    private suspend fun posting(path: String, type: String, payload: String, key: String?): Triple<Any, Long?, Boolean> {
        val body = payload.encodeToByteArray()
        return sending(key) { sent, twice ->
            asking(twice, {
                val c = withContext(Dispatchers.IO) { open(path, type, key, sent, body.size, twice) }
                suspendCancellableCoroutine { cont ->
                    cont.invokeOnCancellation { c.disconnect() }
                    Dispatchers.IO.asExecutor().execute { cont.resumeWith(runCatching { exchange(c, body) }) }
                }
            }) { delay(it) }
        }
    }

    private fun send(text: String, params: List<Any?>, key: String?): Answer {
        val body = Json.write(mapOf("query" to text, "params" to params.map { Values.normalize(it) }))
        return answer(post("/query", "application/json", body, key).first)
    }

    /**
     * A POST's JSON answer, its `Fennec-Seq` and whether it was replayed; a
     * refusal as a [FennecException] with its status and, for a batch, where
     * it stopped.
     */
    private fun post(path: String, type: String, payload: String, key: String?): Triple<Any, Long?, Boolean> {
        val body = payload.encodeToByteArray()
        return sending(key) { sent, twice ->
            asking(twice, { exchange(open(path, type, key, sent, body.size, twice), body) }) { Thread.sleep(it) }
        }
    }

    /**
     * A request made by [request] -- handed the mark of a read near, or
     * null, and whether it may run twice -- and its answer read. A read near
     * is marked for the tenant's read replica in the router's region, after
     * the furthest position home named to this connection, which the replica
     * waits for: that change of that history.
     */
    private inline fun sending(key: String?, request: (Near?, Boolean) -> Reply): Triple<Any, Long?, Boolean> {
        // What may run twice, sent again after a reset too: a read the
        // builder made, one sent near, which every server runs read-only
        // (`Fennec-Read`), and a write under a key. An [asRead] text sent
        // home is a claim nothing checks: a statement it calls may have been
        // made to write since.
        val twice = built || (key ?: idempotencyKey) != null
        if (!reading || shared.reads != Reads.NEAREST) return finish(request(null, twice), near = false)
        // A position of change 0 is none: nothing to wait for, as Go and
        // .NET send none.
        val sent = shared.position()?.takeIf { it.seq > 0 }
        val reply = request(Near(sent), true)
        // 504: no copy holds that write in the wait -- home had none
        // either, lost in a failover. Home says where it stands, which the
        // connection takes as its position, lower or not (`told`); asked
        // home again with no mark, the read is answered with what home holds.
        if (reply.status == 504 && sent != null) {
            val home = reply.v as? Row
            val at = home?.long("seq")
            if (home != null && at != null) shared.told(sent, Position(at, home.string("history")))
            return finish(request(null, twice), near = false)
        }
        return finish(reply, near = true)
    }

    /**
     * A request made by [once], sent again by the rule
     * (docs/design/js-client.md#retries, [Retry]): what ran nothing -- a 503
     * or 429 naming its wait, after it, unless statements of a batch landed
     * (`completed`); a connection refused -- and, where [twice], a reset or a
     * dropped connection too. Never after the connection's own timeout, which
     * a request may still run past, nor once the coroutine is cancelled (a
     * `CancellationException`, never an `IOException`) or the thread
     * interrupted. Each wait goes through [pause]: `delay` for a suspending
     * call, `Thread.sleep` for a blocking one.
     */
    private inline fun asking(twice: Boolean, once: () -> Reply, pause: (Long) -> Unit): Reply {
        val r = shared.retry
        var waited = 0L
        var n = 0
        while (true) {
            var wait = 0L
            var reply: Reply? = null
            var failure: IOException? = null
            try {
                val got = once()
                reply = got
                if (got.status == 503 || got.status == 429) {
                    val body = got.v as? Row
                    if (!((body?.long("completed") ?: 0L) > 0L)) {
                        wait = waitOf(body, got.c.getHeaderField("retry-after"))?.let { ceil(it * 1000).toLong() } ?: 0L
                    }
                }
                if (wait == 0L) return got
            } catch (e: InterruptedIOException) {
                // The connection's own timeout (`SocketTimeoutException`): a
                // `compact`, a long read may still be running, and three tries
                // ran a read three times. Or the thread interrupted.
                throw unreached(e)
            } catch (e: IOException) {
                if (!twice && e !is ConnectException) throw unreached(e)
                failure = e
            }
            // 250 ms doubling, or the server's wait; 30% jitter on top, so a
            // server back up is not met by every client at once.
            val base = maxOf(wait, 250L shl minOf(n, 30))
            if (r == null || n + 1 >= r.attempts || waited + base > r.maxWait) {
                if (failure != null) throw unreached(failure)
                return reply!!
            }
            val w = minOf((base * (1 + Random.nextDouble() * 0.3)).toLong(), r.maxWait - waited)
            waited += w
            shared.pausing?.invoke(w)
            pause(w)
            n++
        }
    }

    private fun unreached(e: IOException) = FennecException(
        FennecException.Code.IO,
        if (e is SocketTimeoutException) "no answer within the timeout: ${e.message}" else "the server could not be reached: ${e.message}",
    )

    /** A read near's mark: the position it is sent after, null for none. */
    private class Near(val after: Position?)

    /**
     * An answer as it came: its status, its body read as JSON (null where
     * empty or not JSON) and the head it is read by. Read where the
     * exchange ran -- on `Dispatchers.IO` for a suspending call -- so a
     * large answer is never parsed on the caller's thread, an app's main
     * thread among them.
     */
    private class Reply(val status: Int, val v: Any?, val c: HttpURLConnection)

    private fun open(path: String, type: String, key: String?, near: Near?, length: Int, twice: Boolean): HttpURLConnection {
        val c = URL("$url$path").openConnection() as HttpURLConnection
        c.requestMethod = "POST"
        c.connectTimeout = shared.timeout.coerceAtMost(Int.MAX_VALUE.toLong()).toInt()
        // Each wait for a piece of the answer -- its head, then each between
        // the pieces of its body -- not the whole: a long answer is read for
        // as long as it comes. A held request is answered once it writes, or
        // at the end of its wait: the head's wait alone, reset once it is in.
        c.readTimeout = (shared.timeout + (held ?: 0L)).coerceAtMost(Int.MAX_VALUE.toLong()).toInt()
        // A request that must go once goes with its length fixed. Its body
        // buffered, the JVM's HttpURLConnection sends a POST again on its own
        // when the answer's head fails -- a reset, a connection closed before
        // it (`sun.net.http.retryPost`) -- and Android's, OkHttp's, one it
        // can send again: a keyless write made twice. Streamed, neither does,
        // and the JVM's asks a kept connection whether the server closed it
        // before it is used, a wait of a millisecond (`available()`); a
        // request that may run twice is spared it, and may be sent again so.
        if (!twice) c.setFixedLengthStreamingMode(length)
        c.setRequestProperty("content-type", type)
        shared.token?.let { c.setRequestProperty("authorization", "Bearer $it") }
        (key ?: idempotencyKey)?.let { c.setRequestProperty("idempotency-key", it) }
        held?.takeIf { it > 0 }?.let { c.setRequestProperty("fennec-wait", it.toString()) }
        if (near != null) {
            c.setRequestProperty("fennec-read", "replica")
            near.after?.let { at ->
                c.setRequestProperty("fennec-after", at.seq.toString())
                at.history?.let { c.setRequestProperty("fennec-history", it) }
            }
        }
        c.doOutput = true
        return c
    }

    /** The request sent and its answer read; an [IOException] is [asking]'s to judge. */
    private fun exchange(c: HttpURLConnection, payload: ByteArray): Reply {
        c.outputStream.use { it.write(payload) }
        val status = c.responseCode
        // The head is in: a held request's wait is over, each gap in the body
        // is bounded by the timeout alone.
        c.readTimeout = shared.timeout.coerceAtMost(Int.MAX_VALUE.toLong()).toInt()
        val body = (if (status >= 400) c.errorStream else c.inputStream)?.use { String(it.readBytes(), Charsets.UTF_8) } ?: ""
        parsing?.invoke()
        return Reply(status, runCatching { if (body.isEmpty()) null else Json.parse(body) }.getOrNull(), c)
    }

    private fun finish(reply: Reply, near: Boolean): Triple<Any, Long?, Boolean> {
        val status = reply.status
        val v = reply.v
        val c = reply.c
        if (status !in 200..299) throw refused(status, v as? Row, c.getHeaderField("retry-after"))
        v ?: throw FennecException(FennecException.Code.IO, "the server did not answer JSON ($status)")
        val seq = c.getHeaderField("fennec-seq")?.toLongOrNull()
        // A position from home alone: a read near's answer may be a
        // replica's, behind home or on a branch a failover left.
        if (seq != null && !near) shared.answered(Position(seq, c.getHeaderField("fennec-history")))
        return Triple(v, seq, c.getHeaderField("idempotent-replayed") == "true")
    }

    internal companion object {
        /**
         * Called on the thread an answer is about to be parsed on: the tests'
         * probe that a suspending call parses on `Dispatchers.IO`, not on its
         * caller's thread. Null, as it always is outside them.
         */
        @Volatile internal var parsing: (() -> Unit)? = null

        /**
         * A refusal as the error it is: its status and message, where a batch stopped, what a
         * `require n else '<reason>'` that refused it said and the statement bound to the write that
         * refused it, and where a tenant's quota refused it the
         * quota and how many seconds to wait -- to the millisecond where the body says it
         * (`retry_after_ms`), else `Retry-After`, or the same in the body -- and the code the
         * server named (`errorCode`, a text not empty alone).
         */
        fun refused(status: Int, r: Row?, retryAfter: String?): FennecException {
            val quota = r?.string("quota")
            return FennecException(
                if (quota == null) code(status) else FennecException.Code.QUOTA,
                r?.string("error") ?: "HTTP $status", null,
                status = status, at = r?.int("at"), completed = r?.int("completed"),
                quota = quota,
                retryAfter = waitOf(r, retryAfter),
                reason = r?.string("reason"),
                binding = r?.string("binding"),
                errorCode = r?.string("code")?.takeIf { it.isNotEmpty() },
            )
        }

        /**
         * The seconds an answer asks a client to wait, where it names any: to
         * the millisecond where the body says it (`retry_after_ms`) --
         * `Retry-After` is whole seconds, and a bucket of 600 rows a second
         * refills a row in 1.7 ms -- then the header, then the body's
         * `retry_after`, which a 503 of the server's carries.
         */
        fun waitOf(r: Row?, header: String?): Double? {
            r?.double("retry_after_ms")?.takeIf { it > 0 && it.isFinite() }?.let { return it / 1000 }
            return listOf(header?.trim()?.toDoubleOrNull(), r?.double("retry_after")).firstOrNull { it != null && it > 0 && it.isFinite() }
        }

        /** An HTTP status as the error kind the engine would have said; the status itself is the error's `status`. */
        fun code(status: Int) = when (status) {
            400 -> FennecException.Code.QUERY
            401, 403 -> FennecException.Code.DENIED
            404 -> FennecException.Code.NOT_FOUND
            409 -> FennecException.Code.DUPLICATE
            412 -> FennecException.Code.UNMET
            else -> FennecException.Code.IO
        }

        /**
         * The endpoint's answer as the library's: rows come as an array, or
         * -- when the query asked for facets, and as a batch's each read --
         * as `{"rows": [...], "facets": {...}}`.
         */
        fun answer(v: Any): Answer = when {
            v is List<*> -> rowsOf(v, null)
            v is Row && v.list("rows") != null -> rowsOf(v.list("rows")!!, v.row("facets"))
            v is Row && v.long("affected") != null -> Answer.Affected(v.long("affected")!!)
            v is Row && v.list("collections") != null -> Answer.Schemas(v.list("collections")!!.map { it as Row })
            v is Row -> Answer.Ok(v.string("message") ?: Json.write(v))
            else -> Answer.Ok(Json.write(v))
        }

        private fun rowsOf(list: List<*>, facets: Row?): Answer.Rows {
            val rows = list.map { it as Row }
            return Answer.Rows(rows.firstOrNull()?.keys?.toList() ?: emptyList(), rows, facetsOf(facets))
        }
    }
}

/**
 * Where an answer from home left the tenant: its change (`Fennec-Seq`) and the
 * history that change is of (`Fennec-History`, `<term>-<id>`; none for a
 * database that never had a replica).
 */
internal data class Position(val seq: Long, val history: String?) {
    /** The history's term, one more at each promotion. */
    val term: Long get() = history?.substringBefore('-')?.toLongOrNull() ?: 0

    /**
     * The history's lineage: its id's top half, which every fork of the
     * tenant keeps and a tenant made again under its name does not.
     */
    val lineage: String? get() = history?.takeIf { '-' in it }?.substringAfter('-')?.takeIf { it.length >= 8 }?.take(8)

    /** Whether this is of another lineage than [q]: a tenant made again. */
    fun isOfAnotherLineage(q: Position): Boolean {
        val a = lineage
        val b = q.lineage
        return a != null && b != null && a != b
    }

    /**
     * Whether this is past [q]: of another lineage -- home's word on a
     * tenant made again, whose terms start again -- or a later term of the
     * same, or further in one.
     */
    fun isPast(q: Position?): Boolean =
        q == null || isOfAnotherLineage(q) || term > q.term || (term == q.term && seq > q.seq)
}

/**
 * A fennec-server over HTTP, with no local file: every query goes to the
 * server, through the same builder. With `reads = FennecRemote.Reads.NEAREST`
 * the builder's reads go to the tenant's read replica near the app, where a
 * fennec-shard router has one, after the app's own writes ([FennecRemote.Reads]).
 * [timeout] is milliseconds to an answer's head and between the pieces of its
 * body; [retry] how a request goes again where it cannot have run or may run
 * twice, null for never ([FennecRemote.Retry]).
 */
fun Fennec.Companion.connect(
    url: String,
    token: String? = null,
    reads: FennecRemote.Reads = FennecRemote.Reads.HOME,
    timeout: Long = 30_000,
    retry: FennecRemote.Retry? = FennecRemote.Retry(),
): FennecRemote = FennecRemote(url, token, reads, timeout, retry)
