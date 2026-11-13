package com.fennecdb

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.security.SecureRandom
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.TimeUnit
import kotlin.coroutines.resume

/**
 * The rows of one collection a replica keeps: a filter the server applies,
 * in the JS sync layer's form -- `mapOf("status" to "open", "priority" to
 * mapOf("gte" to 3))` -- the fields it carries, and the business key an
 * optimistic insert is matched with the server's copy by (a `text @hash`
 * field).
 */
data class Shape @JvmOverloads constructor(
    val collection: String,
    val where: Map<String, Any?>? = null,
    val select: List<String>? = null,
    val key: String? = null,
) {
    internal fun json(): Map<String, Any?> = buildMap {
        put("collection", collection)
        where?.let { put("where", Values.normalize(it)) }
        select?.let { put("select", it) }
        key?.let { put("key", it) }
    }
}

/** Where a replica stands with its server. */
data class SyncStatus(
    val state: State,
    /** Writes the server has not answered. */
    val pending: Int,
    /** The last error: a connection's until it is back, a refused write's until the next. */
    val error: Failure?,
    val shapes: List<ShapeState>,
) {
    enum class State {
        /** Every shape's stream is open and seeded. */
        ONLINE,

        /** The server cannot be reached, or the app said the network is gone. */
        OFFLINE,

        /** Connecting, seeding, or sending the writes left from before. */
        CATCHING_UP,
    }

    /**
     * An error, and its HTTP status (0: the network, or the replica). [quota] is the tenant's quota the
     * server named, null for none: a stream refused by the quota of `streams` leaves the replica OFFLINE
     * as the network does, and this tells the two apart. [errorCode] is the code a 401 the sync waits on
     * for a token named (the server's body's `code`, [TokenRefusal.errorCode]), null for none:
     * "token_expired" where the token's `exp` passed. [tokenRefusal] is what the provider was handed for
     * the token the sync waits on, null while it waits on none and while it waits before asking again --
     * both for an app that hands its tokens over with `setToken` rather than a provider.
     */
    data class Failure @JvmOverloads constructor(
        val message: String,
        val status: Int,
        val quota: String? = null,
        val errorCode: String? = null,
        val tokenRefusal: TokenRefusal? = null,
    )

    data class ShapeState(val collection: String, val cursor: Long, val seeded: Boolean, val connected: Boolean)

    /** Whether every shape has had its seed. */
    val seeded: Boolean get() = shapes.isNotEmpty() && shapes.all { it.seeded }

    internal companion object {
        val STARTING = SyncStatus(State.CATCHING_UP, 0, null, emptyList())

        fun of(v: Row) = SyncStatus(
            when (v.string("state")) {
                "online" -> State.ONLINE
                "offline" -> State.OFFLINE
                else -> State.CATCHING_UP
            },
            v.int("pending") ?: 0,
            v.row("error")?.let {
                Failure(
                    it.string("message") ?: "", it.int("status") ?: 0, it.string("quota"), it.string("code"),
                    it.int("attempt")?.let { attempt -> TokenRefusal(it.string("code"), it.bool("had_token") ?: true, attempt) },
                )
            },
            (v.list("shapes") ?: emptyList()).map {
                val s = it as Row
                ShapeState(s.string("collection") ?: "", s.long("cursor") ?: 0, s.bool("seeded") == true, s.bool("connected") == true)
            },
        )
    }
}

/**
 * Why a replica asks its `tokenProvider` for a token: the server answered
 * 401, and nothing is sent until one is handed over.
 *
 * Ask the issuer for a fresh token at every one, whatever the code --
 * forcing a refresh at [expired], where a cached token is the one refused --
 * and the sync goes on where it stopped. A 401 alone is never a sign-out:
 * the server keeps no list of revoked tokens, so only the issuer knows a
 * session ended; sign out where it refuses to issue one. A replica belongs
 * to one account: its file's path names the account, and it is never opened
 * for another. Signing out keeps it, for that account's next sign-in to send
 * its `pending` writes.
 *
 * A plain class, not a data class: what a later server says can be added to
 * it without a `copy` or a `componentN` whose shape callers compiled against.
 */
class TokenRefusal @JvmOverloads constructor(
    /**
     * The code the server named beside its words (its body's `code`), null
     * for none: "token_expired" where the token's `exp` passed. A later
     * server may name others; a 401 naming none is no sign-out by itself.
     */
    val errorCode: String? = null,
    /** Whether the refused request carried a token: false for one sent before any was handed over. */
    val hadToken: Boolean = true,
    /**
     * How many times in a row the provider is asked since the server last
     * took the token held: 1, then 2 and on where the token handed over at
     * the last ask was refused too -- asked after 1 s doubling to 60 s, since
     * one minted again at once is refused for what minting does not change
     * (a key the server has not fetched yet, an `nbf` its clock has not
     * reached). An expiry starts it at 1 again. No reason to sign out.
     */
    val attempt: Int = 1,
) {
    /** The token's `exp` passed ([errorCode] is "token_expired"): refresh it, and the sync goes on. */
    val expired: Boolean get() = errorCode == TOKEN_EXPIRED

    override fun equals(other: Any?) =
        other is TokenRefusal && other.errorCode == errorCode && other.hadToken == hadToken && other.attempt == attempt

    override fun hashCode() = (errorCode.hashCode() * 31 + hadToken.hashCode()) * 31 + attempt

    override fun toString() = "TokenRefusal(errorCode=$errorCode, hadToken=$hadToken, attempt=$attempt)"

    companion object {
        /** The code fennec-server tells a token past its `exp` by. */
        const val TOKEN_EXPIRED = "token_expired"
    }
}

/**
 * A write the server refused: it was put back on the replica. [quota] is the tenant's quota that
 * refused it -- "bytes" or "rows", a 507 -- null for any other refusal: room is made by deleting,
 * and the writes behind it went on. [reason] is what the write's `require n else '<reason>'` said,
 * where one refused it (a 412): the app says "the seat was taken" rather than "refused"; null where
 * the statement gave none.
 */
data class Refusal @JvmOverloads constructor(
    val status: Int,
    val message: String,
    val query: String,
    val quota: String? = null,
    val reason: String? = null,
)

/**
 * A replica of [shapes] in the file at [path], kept in step with the server
 * at [url]: reads and live queries are the file's, a write to a shape's
 * collection is applied at once and sent to the server -- queued in the
 * file while it cannot be reached, and sent under an idempotency key, so it
 * lands once. The same [Fennec]: `from`, `live` and `query` work as they do
 * over a file of the app's own.
 *
 * ```kotlin
 * val db = Fennec.sync(
 *     url = "https://api.example.com", token = jwt,
 *     shapes = listOf(Shape("todos", where = mapOf("done" to false), key = "key")),
 *     path = File(context.filesDir, "todos.fennec").path,
 * )
 * ```
 *
 * The requests and the change stream are `HttpURLConnection`'s, the JDK's
 * and Android's own, with the system's TLS: Android refuses cleartext HTTP
 * by default, and a server is reached through TLS in front of it.
 * [tokenProvider] is asked for a token when the server answers 401, handed
 * why ([TokenRefusal]): it asks the issuer for a fresh one at every 401,
 * and throws where it gets none -- the issuer out of reach, or refusing,
 * where the app signs out; nothing is sent until a token is handed over.
 * A provider that threw, or handed over an empty token, is named by the
 * status's error ("the token provider failed: ...") and asked again at
 * `resume()` or `setOnline(true)`; it is called once at a time. [path] is
 * one account's: a replica is never opened for another. A lambda asking
 * nothing, `{ refresh() }`, still takes; a reference to a function of none,
 * `::refresh`, no longer does: `{ refresh() }` for it.
 */
suspend fun Fennec.Companion.sync(
    url: String,
    token: String? = null,
    shapes: List<Shape>,
    path: String,
    flags: Int = 0,
    tokenProvider: (suspend (TokenRefusal) -> String)? = null,
): Fennec = withContext(Dispatchers.IO) {
    val db = Fennec.open(path, flags)
    try {
        val replica = Replica(db, url, token, tokenProvider)
        val seed = SecureRandom().let { r -> "%016x%016x".format(r.nextLong(), r.nextLong()) }
        val config = Json.write(
            buildMap {
                put("url", url)
                token?.let { put("token", it) }
                put("seed", seed)
                put("shapes", shapes.map { it.json() })
            },
        )
        val first = FennecNative.answer(FennecNative.syncStart(db.handle, config.encodeToByteArray()))
        db.attach(replica)
        replica.start(first)
        db
    } catch (e: Throwable) {
        db.close()
        throw e
    }
}

/**
 * A replica's sync with its server: its status as a [StateFlow], its
 * token, and the network as the app sees it. Its work is on a thread of its
 * own, where every request's answer, every piece of a stream and every
 * timer is handed in turn, so the engine is told of a stream's bytes in
 * their order and an action is never performed before the one it cancels.
 */
class Replica internal constructor(
    private val db: Fennec,
    url: String,
    @Volatile private var token: String?,
    private val provider: (suspend (TokenRefusal) -> String)?,
) {
    private val url = url.trimEnd('/')
    private val handle = db.handle
    private val queue: ScheduledExecutorService = Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "fennecdb-sync").apply { isDaemon = true }
    }
    private val connections = HashMap<Long, HttpURLConnection>()
    private val current = MutableStateFlow(SyncStatus.STARTING)
    private val refused = MutableSharedFlow<Refusal>(extraBufferCapacity = 64)

    @Volatile private var stopped = false

    /** The provider is at work, on the queue's word: one call is out at a time. */
    private var providing = false

    /** The status now and at every change; `status.value` never waits. */
    val status: StateFlow<SyncStatus> = current.asStateFlow()

    /** The writes the server refused, each put back on the replica. */
    val refusals: SharedFlow<Refusal> = refused.asSharedFlow()

    /** Returns once every shape has had its seed: at once for a replica opened again. */
    suspend fun ready() {
        refresh()
        current.first { it.seeded }
    }

    /** Returns once the server has answered every write made so far. */
    suspend fun pushed() {
        refresh()
        current.first { it.pending == 0 }
    }

    /** The status once everything told the sync before this call is in it -- a write just made among its pending ones. */
    suspend fun refresh(): SyncStatus = suspendCancellableCoroutine { cont ->
        queue.execute {
            reload()
            cont.resume(current.value)
        }
    }

    /** A fresh token, for the requests and streams from here on. */
    fun setToken(token: String) {
        this.token = token
        signal(mapOf("token" to token))
    }

    /**
     * The network as the platform sees it (`ConnectivityManager`): offline
     * closes the streams and sends nothing; online comes back at once, the
     * backoff forgotten, and asks the provider again for a token it did not
     * hand over -- it threw, its issuer out of reach -- but not while it is
     * still at work, nor before the wait the server's refusal of the last
     * one began.
     */
    fun setOnline(online: Boolean) = signal(mapOf("online" to online))

    /**
     * What an app calls in `onStart`: the streams a stop cut are opened again at once, from their
     * cursors, and a provider that failed is asked for a token again.
     */
    fun resume() = setOnline(true)

    /** The server itself, for what the replica does not hold. */
    val remote: FennecRemote get() = FennecRemote(url, token)

    // ------------------------------------------------------------- the engine

    internal fun start(first: String) = queue.execute { perform(first) }

    /** Asks for what a write left due. */
    internal fun poll() {
        if (!stopped) queue.execute { feed(KIND_POLL, 0) }
    }

    internal fun stop() {
        if (stopped) return
        runCatching {
            queue.submit {
                feed(KIND_SIGNAL, 0, bytes = """{"stop":true}""".encodeToByteArray())
                stopped = true
                connections.values.forEach { it.disconnect() }
                connections.clear()
            }.get()
        }
        queue.shutdownNow()
    }

    private fun signal(fields: Map<String, Any?>) {
        val bytes = Json.write(fields).encodeToByteArray()
        if (!stopped) queue.execute { feed(KIND_SIGNAL, 0, bytes = bytes) }
    }

    private fun later(f: () -> Unit) {
        if (!stopped) runCatching { queue.execute(f) }
    }

    /** Tells the engine, on the queue, and performs what it answers. */
    private fun feed(kind: Int, id: Long, status: Int = 0, seq: Long = 0, bytes: ByteArray? = null) {
        if (stopped) return
        val out = runCatching { FennecNative.answer(FennecNative.syncFeed(handle, kind, id, status, seq, bytes)) }.getOrNull()
        if (out != null) perform(out)
    }

    private fun perform(json: String) {
        val actions = runCatching { Json.parse(json) as List<*> }.getOrNull() ?: return
        for (a in actions) {
            val r = a as Row
            val id = r.long("id") ?: 0
            when (r.string("do")) {
                "request" -> request(id, r)
                "stream" -> stream(id, r)
                "cancel" -> connections.remove(id)?.let { c -> Thread { c.disconnect() }.start() }
                "wait" -> queue.schedule({ feed(KIND_TIMER, id) }, r.long("ms") ?: 0, TimeUnit.MILLISECONDS)
                // Why the core asks: the code the 401 named, whether the
                // refused request carried a token, and the asks in a row.
                // One call out at a time: asked again while at work -- the
                // app's own `setToken` went on and was refused meanwhile --
                // a provider would mint twice for one refusal, and the
                // second token, coming late, end the wait the first one's
                // refusal began. Its answer answers this ask too.
                "token" -> provider?.takeIf { !providing }?.let { p ->
                    providing = true
                    val refusal = TokenRefusal(r.string("code"), r.bool("had_token") ?: true, r.int("attempt") ?: 1)
                    Thread {
                        val got = runCatching { kotlinx.coroutines.runBlocking { p(refusal) } }
                        val token = got.getOrNull()?.takeIf { it.isNotEmpty() }
                        val why = got.exceptionOrNull()?.let { it.message ?: it.toString() } ?: "it handed over no token"
                        provided(token, why)
                    }.apply { isDaemon = true }.start()
                }
                "changed" -> db.lives.touch()
                "refused" -> refused.tryEmit(
                    Refusal(
                        r.int("status") ?: 0, r.string("message") ?: "", r.string("query") ?: "", r.string("quota"),
                        r.string("reason"),
                    ),
                )
                "status" -> reload()
            }
        }
    }

    /**
     * The provider's answer: its token handed over, or the engine told none
     * came and why (`{"token_failed":..}`), which the status's error says --
     * `resume()` asks again. On the queue with the call's end, so an ask the
     * engine makes before the answer reaches it is this call's.
     */
    private fun provided(token: String?, why: String) {
        if (token != null) this.token = token
        val bytes = Json.write(if (token != null) mapOf("token" to token) else mapOf("token_failed" to why)).encodeToByteArray()
        later {
            providing = false
            feed(KIND_SIGNAL, 0, bytes = bytes)
        }
    }

    private fun reload() {
        val out = runCatching { FennecNative.answer(FennecNative.syncStatus(handle)) }.getOrNull() ?: return
        current.value = SyncStatus.of(Json.parse(out) as Row)
    }

    private fun connection(r: Row, readTimeout: Int): HttpURLConnection {
        val c = URL(r.string("url")).openConnection() as HttpURLConnection
        c.requestMethod = r.string("method") ?: "GET"
        c.connectTimeout = 10_000
        c.readTimeout = readTimeout
        c.useCaches = false
        r.row("headers")?.forEach { (k, v) -> c.setRequestProperty(k, v as String) }
        return c
    }

    /** A request on a thread of its own, its answer handed to the queue. */
    private fun request(id: Long, r: Row) {
        val c = connection(r, 30_000)
        connections[id] = c
        Thread {
            var status = 0
            var seq = 0L
            val body: ByteArray = try {
                r.string("body")?.let { b ->
                    c.doOutput = true
                    c.outputStream.use { it.write(b.encodeToByteArray()) }
                }
                status = c.responseCode
                seq = c.getHeaderField("Fennec-Seq")?.toLongOrNull() ?: 0
                (if (status >= 400) c.errorStream else c.inputStream)?.use { it.readBytes() } ?: ByteArray(0)
            } catch (e: IOException) {
                status = 0
                (e.message ?: e.toString()).encodeToByteArray()
            }
            later {
                connections.remove(id)
                feed(KIND_RESPONSE, id, status, seq, body)
            }
        }.apply { isDaemon = true }.start()
    }

    /** A stream on a thread of its own, each piece handed to the queue as it comes. */
    private fun stream(id: Long, r: Row) {
        // The server's keepalive comes every 20 s: a silence past it is a
        // connection gone.
        val c = connection(r, 90_000)
        connections[id] = c
        Thread {
            var why = ""
            try {
                val status = c.responseCode
                if (status != 200) {
                    val body = c.errorStream?.use { it.readBytes() } ?: ByteArray(0)
                    later { feed(KIND_OPENED, id, status, bytes = body) }
                    return@Thread
                }
                later { feed(KIND_OPENED, id, 200) }
                c.inputStream.use { input ->
                    val buf = ByteArray(16 * 1024)
                    while (true) {
                        val n = input.read(buf)
                        if (n < 0) break
                        val piece = buf.copyOf(n)
                        later { feed(KIND_BYTES, id, bytes = piece) }
                    }
                }
            } catch (e: IOException) {
                why = e.message ?: e.toString()
            }
            later { feed(KIND_CLOSED, id, bytes = why.encodeToByteArray()) }
        }.apply { isDaemon = true }.start()
    }

    private companion object {
        const val KIND_POLL = 0
        const val KIND_RESPONSE = 1
        const val KIND_OPENED = 2
        const val KIND_BYTES = 3
        const val KIND_CLOSED = 4
        const val KIND_TIMER = 5
        const val KIND_SIGNAL = 6
    }
}

/** A replica's sync, when the database is one ([Fennec.Companion.sync]). */
val Fennec.replica: Replica? get() = syncing
