import FennecFFI
import Foundation

/// The rows of one collection a replica keeps: a filter the server applies,
/// in the JS sync layer's form -- `["status": "open", "priority": ["gte": 3]]`
/// -- the fields it carries, and the business key an optimistic insert is
/// matched with the server's copy by (a `text @hash` field).
public struct Shape: Sendable {
    public let collection: String
    public let condition: Value?
    public let select: [String]?
    public let key: String?

    public init(_ collection: String, where condition: Value? = nil, select: [String]? = nil, key: String? = nil) {
        self.collection = collection
        self.condition = condition
        self.select = select
        self.key = key
    }

    var value: Value {
        var r = Row()
        r["collection"] = .string(collection)
        if let condition { r["where"] = condition }
        if let select { r["select"] = .array(select.map { .string($0) }) }
        if let key { r["key"] = .string(key) }
        return .object(r)
    }
}

/// Why a replica asks its `tokenProvider` for a token: the server answered
/// 401, and nothing is sent until one is handed over.
///
/// Ask the issuer for a fresh token at every one, whatever the code --
/// forcing a refresh at `expired`, where a cached token is the one refused
/// -- and the sync goes on where it stopped. A 401 alone is never a
/// sign-out: the server keeps no list of revoked tokens, so only the
/// issuer knows a session ended; sign out where it refuses to issue one.
/// A replica belongs to one account: its file's path names the account,
/// and it is never opened for another. Signing out keeps it, for that
/// account's next sign-in to send its `pending` writes.
public struct TokenRefusal: Sendable, Equatable {
    /// The code the server named beside its words (its body's `code`), nil
    /// for none: "token_expired" where the token's `exp` passed. A later
    /// server may name others; a 401 naming none is no sign-out by itself.
    public let errorCode: String?
    /// Whether the refused request carried a token: false for one sent
    /// before any was handed over.
    public let hadToken: Bool
    /// How many times in a row the provider is asked since the server last
    /// took the token held: 1, then 2 and on where the token handed over at
    /// the last ask was refused too -- asked after 1 s doubling to 60 s,
    /// since one minted again at once is refused for what minting does not
    /// change (a key the server has not fetched yet, an `nbf` its clock has
    /// not reached). An expiry starts it at 1 again. No reason to sign out.
    public let attempt: Int

    /// The token's `exp` passed (`errorCode` is "token_expired"): refresh
    /// it, and the sync goes on.
    public var expired: Bool { errorCode == "token_expired" }

    public init(errorCode: String? = nil, hadToken: Bool = true, attempt: Int = 1) {
        self.errorCode = errorCode
        self.hadToken = hadToken
        self.attempt = attempt
    }
}

/// A token a replica waits on, as its status says it (`SyncStatus.token`):
/// for an app that hands its tokens over with `setToken` -- which acts at
/// `.asked` -- and for one that shows where its provider stands.
public struct TokenWait: Sendable, Equatable {
    public enum Stage: String, Sendable {
        /// The token handed over was refused too: the provider is asked
        /// after a wait, 1 s doubling to 60 s.
        case delayed
        /// The provider is asked -- with none, the app is -- and no token
        /// has come.
        case asked
        /// The provider threw or handed over none: `resume()` or
        /// `setOnline(true)` asks it again.
        case failed
    }

    public let stage: Stage
    /// What the provider was handed -- or, `.delayed`, will be.
    public let refusal: TokenRefusal

    public init(stage: Stage, refusal: TokenRefusal) {
        self.stage = stage
        self.refusal = refusal
    }
}

/// Where a replica stands with its server.
public struct SyncStatus: Sendable, Equatable {
    public enum State: String, Sendable {
        /// Every shape's stream is open and seeded.
        case online
        /// The server cannot be reached, or the app said the network is gone.
        case offline
        /// Connecting, seeding, or sending the writes left from before.
        case catchingUp = "catching_up"
    }

    public struct Failure: Sendable, Equatable {
        public let message: String
        /// The HTTP status, or 0 for none (the network, or the replica).
        public let status: Int
        /// The tenant's quota the server named, nil for none: a stream
        /// refused by the quota of `streams` leaves the replica `offline`
        /// as the network does, and this tells the two apart.
        public let quota: String?
        /// The code a 401 the sync waits on for a token named (the
        /// server's body's `code`, `TokenRefusal.errorCode`), nil for none:
        /// "token_expired" where the token's `exp` passed.
        public let errorCode: String?
    }

    public struct ShapeState: Sendable, Equatable {
        public let collection: String
        public let cursor: UInt64
        public let seeded: Bool
        public let connected: Bool
    }

    public let state: State
    /// Writes the server has not answered.
    public let pending: Int
    /// The last error: a connection's until it is back, a refused write's
    /// until the next -- the token's while one is wanted, whatever else
    /// failed since (a stream that ended, as streams end offline).
    public let error: Failure?
    /// The token the sync waits on, nil while it waits on none: nothing is
    /// sent meanwhile.
    public let token: TokenWait?
    public let shapes: [ShapeState]

    /// Whether every shape has had its seed.
    public var seeded: Bool { shapes.allSatisfy(\.seeded) }

    static let starting = SyncStatus(state: .catchingUp, pending: 0, error: nil, token: nil, shapes: [])

    init(state: State, pending: Int, error: Failure?, token: TokenWait?, shapes: [ShapeState]) {
        self.state = state
        self.pending = pending
        self.error = error
        self.token = token
        self.shapes = shapes
    }

    init(_ v: Value) {
        state = State(rawValue: v["state"]?.string ?? "") ?? .catchingUp
        pending = v["pending"]?.int ?? 0
        error = v["error"]?.object.map { e in
            Failure(
                message: e["message"]?.string ?? "", status: e["status"]?.int ?? 0, quota: e["quota"]?.string,
                errorCode: e["code"]?.string)
        }
        token = v["token"]?.object.flatMap { t in
            TokenWait.Stage(rawValue: t["stage"]?.string ?? "").map {
                TokenWait(
                    stage: $0,
                    refusal: TokenRefusal(
                        errorCode: t["code"]?.string, hadToken: t["had_token"]?.bool ?? true,
                        attempt: t["attempt"]?.int ?? 1))
            }
        }
        shapes = (v["shapes"]?.array ?? []).map {
            ShapeState(
                collection: $0["collection"]?.string ?? "",
                cursor: UInt64($0["cursor"]?.int ?? 0),
                seeded: $0["seeded"]?.bool ?? false,
                connected: $0["connected"]?.bool ?? false)
        }
    }
}

/// A write the server refused: it was put back on the replica.
public struct Refusal: Sendable, Equatable {
    public let status: Int
    public let message: String
    /// The statement's text, as it was run.
    public let query: String
    /// The tenant's quota that refused it -- "bytes" or "rows", a 507 --
    /// nil for any other refusal: room is made by deleting, and the writes
    /// behind it went on.
    public let quota: String?
    /// What the write's `require n else '<reason>'` said, where one refused
    /// it (a 412): the app says "the seat was taken" rather than
    /// "refused". nil where the statement gave none.
    public let reason: String?
}

extension Fennec {
    /// A replica of `shapes` in the file at `path`, kept in step with the
    /// server at `url`: reads and live queries are the file's, a write to a
    /// shape's collection is applied at once and sent to the server --
    /// queued in the file while it cannot be reached, and sent under an
    /// idempotency key, so it lands once. The same `Fennec`: `from`, `live`
    /// and `query` work as they do over a file of the app's own.
    ///
    ///     let db = try await Fennec.sync(
    ///         url: "https://api.example.com", token: jwt,
    ///         shapes: [Shape("todos", where: ["done": false], key: "key")],
    ///         path: docs.appendingPathComponent("todos.fennec").path)
    ///
    /// The requests and the change stream are made by `URLSession`, with the
    /// system's TLS: iOS's App Transport Security refuses plain `http://`
    /// to anything but a local address, and a server is reached through
    /// TLS in front of it. `tokenProvider` is asked for a token when the
    /// server answers 401, handed why (`TokenRefusal`): it asks the issuer
    /// for a fresh one at every 401, and throws where it gets none -- the
    /// issuer out of reach, or refusing, where the app signs out; nothing is
    /// sent until a token is handed over. A provider that threw, or handed
    /// over an empty token, is named by the status (`token` `.failed`, the
    /// error "the token provider failed: ...") and asked again at
    /// `resume()` or `setOnline(true)`; it is called once at a time.
    /// Nothing bounds its time, since it may wait on a sign-in, so it must
    /// return: time out its own requests to the issuer and throw. One that
    /// never returns holds the sync, `token` `.asked`, until the app hands
    /// a token over itself (`setToken`). `path` is one account's: a replica
    /// is never opened for another.
    public static func sync(
        url: String, token: String? = nil, shapes: [Shape], path: String, options: Options = [],
        tokenProvider: (@Sendable (TokenRefusal) async throws -> String)? = nil
    ) async throws -> Fennec {
        let db = try await open(path: path, options: options)
        do {
            let replica = Replica(db: db, url: url, token: token, provider: tokenProvider)
            var r = Row()
            r["url"] = .string(url)
            if let token { r["token"] = .string(token) }
            var seed = SystemRandomNumberGenerator()
            r["seed"] = .string(String(format: "%016llx%016llx", seed.next() as UInt64, seed.next() as UInt64))
            r["shapes"] = .array(shapes.map(\.value))
            let config = Value.object(r).json
            let handle = db.handle
            let first = try await background {
                try call { out, len in
                    var c = config
                    return c.withUTF8 { fennec_sync_start(handle, $0.baseAddress, $0.count, out, len) }
                } ?? []
            }
            db.attach(replica)
            replica.start(first)
            return db
        } catch {
            try? await db.close()
            throw error
        }
    }

    /// A replica's sync, when the database is one (`Fennec.sync`).
    public var replica: Replica? { syncing }
}

/// A replica's sync with its server: its status, its token, and the
/// network as the app sees it. Its work is on a serial queue of its own,
/// where `URLSession` hands it what comes, so the engine is told of a
/// stream's bytes in their order.
public final class Replica: NSObject, URLSessionDataDelegate, @unchecked Sendable {
    private weak var db: Fennec?
    private let handle: UInt64
    private let url: String
    private let provider: (@Sendable (TokenRefusal) async throws -> String)?
    private let queue = DispatchQueue(label: "dev.fennecdb.sync")
    private var session: URLSession!
    private let lock = NSLock()
    private var token: String?
    private var current = SyncStatus.starting
    private var tasks: [UInt64: URLSessionTask] = [:]
    private var streams: [Int: (id: UInt64, status: Int, body: Data)] = [:]
    private var watchers: [UUID: AsyncStream<SyncStatus>.Continuation] = [:]
    private var refusalWatchers: [UUID: AsyncStream<Refusal>.Continuation] = [:]
    private var stopped = false
    /// The provider is at work, on the queue's word: one call is out at a
    /// time.
    private var providing = false

    init(db: Fennec, url: String, token: String?, provider: (@Sendable (TokenRefusal) async throws -> String)?) {
        self.db = db
        self.handle = db.handle
        self.url = url.hasSuffix("/") ? String(url.dropLast()) : url
        self.token = token
        self.provider = provider
        super.init()
        queue.setSpecific(key: Replica.onQueue, value: true)
        let ops = OperationQueue()
        ops.underlyingQueue = queue
        ops.maxConcurrentOperationCount = 1
        let config = URLSessionConfiguration.default
        // A stream is quiet between changes but for the server's keepalive
        // every 20 s: the idle limit is past it.
        config.timeoutIntervalForRequest = 90
        config.waitsForConnectivity = false
        session = URLSession(configuration: config, delegate: self, delegateQueue: ops)
    }

    // ------------------------------------------------------------ the app's

    /// The status now.
    public var status: SyncStatus {
        lock.lock()
        defer { lock.unlock() }
        return current
    }

    /// The status now and at every change.
    public func statuses() -> AsyncStream<SyncStatus> {
        AsyncStream { cont in
            let id = UUID()
            lock.lock()
            watchers[id] = cont
            let now = current
            lock.unlock()
            cont.yield(now)
            cont.onTermination = { [weak self] _ in
                guard let self else { return }
                self.lock.lock()
                self.watchers[id] = nil
                self.lock.unlock()
            }
        }
    }

    /// The writes the server refused, each put back on the replica.
    public func refusals() -> AsyncStream<Refusal> {
        AsyncStream { cont in
            let id = UUID()
            lock.lock()
            refusalWatchers[id] = cont
            lock.unlock()
            cont.onTermination = { [weak self] _ in
                guard let self else { return }
                self.lock.lock()
                self.refusalWatchers[id] = nil
                self.lock.unlock()
            }
        }
    }

    /// Returns once every shape has had its seed: at once for a replica
    /// opened again, which resumes where it stopped.
    public func ready() async {
        _ = await refresh()
        for await s in statuses() where s.seeded && !s.shapes.isEmpty { return }
    }

    /// Returns once the server has answered every write made so far.
    public func pushed() async {
        _ = await refresh()
        for await s in statuses() where s.pending == 0 { return }
    }

    /// The status once everything told the sync before this call is in
    /// it -- a write just made among its pending ones. `status` is what the
    /// sync last said, and never waits.
    public func refresh() async -> SyncStatus {
        await withCheckedContinuation { cont in
            queue.async {
                self.reload()
                cont.resume(returning: self.status)
            }
        }
    }

    /// A fresh token, for the requests and streams from here on: what a
    /// `401` stopped goes on, whatever the status's `token` says -- a
    /// provider that never returned among them, called no second time
    /// meanwhile.
    public func setToken(_ token: String) {
        lock.lock()
        self.token = token
        lock.unlock()
        signal(["token": .string(token)])
    }

    /// The network as the platform sees it (`NWPathMonitor`): offline closes
    /// the streams and sends nothing; online comes back at once, the
    /// backoff forgotten, and asks the provider again for a token it did
    /// not hand over -- it threw, its issuer out of reach -- but not while
    /// it is still at work, nor before the wait the server's refusal of the
    /// last one began.
    public func setOnline(_ online: Bool) { signal(["online": .bool(online)]) }

    /// What an app calls as it comes back to the foreground: the streams a
    /// suspension cut are opened again at once, from their cursors, and a
    /// provider that failed is asked for a token again.
    public func resume() { setOnline(true) }

    /// The server itself, for what the replica does not hold.
    public var remote: FennecRemote {
        lock.lock()
        defer { lock.unlock() }
        return FennecRemote(url: url, token: token)
    }

    // ---------------------------------------------------------- the engine

    func start(_ first: [UInt8]) {
        queue.async { self.perform(first) }
    }

    /// Asks for what a write left due.
    func poll() {
        queue.async { self.feed(UInt32(FENNEC_SYNC_POLL), 0) }
    }

    /// Stopped from a task (`close()`): the queue is waited for without
    /// holding a thread of Swift's cooperative pool, which has a thread a
    /// core -- what the queue is doing may be a native call waiting for the
    /// database's write lock, or an fsync.
    func stop() async {
        await withCheckedContinuation { (cont: CheckedContinuation<Void, Never>) in
            queue.async {
                self.stopHere()
                cont.resume()
            }
        }
    }

    /// Stopped from the database's `deinit`, which runs on whichever thread
    /// let it go -- the queue's own among them, where `perform` holds it a
    /// moment, and where a `queue.sync` would be a deadlock.
    func stopNow() {
        if DispatchQueue.getSpecific(key: Replica.onQueue) != nil {
            stopHere()
        } else {
            queue.sync { stopHere() }
        }
    }

    private static let onQueue = DispatchSpecificKey<Bool>()

    private func stopHere() {
        guard !stopped else { return }
        feed(UInt32(FENNEC_SYNC_SIGNAL), 0, bytes: Array(#"{"stop":true}"#.utf8))
        stopped = true
        for t in tasks.values { t.cancel() }
        tasks.removeAll()
        session.invalidateAndCancel()
        lock.lock()
        watchers.values.forEach { $0.finish() }
        refusalWatchers.values.forEach { $0.finish() }
        watchers.removeAll()
        refusalWatchers.removeAll()
        lock.unlock()
    }

    private func signal(_ fields: Row) {
        let bytes = Array(Value.object(fields).json.utf8)
        queue.async { self.feed(UInt32(FENNEC_SYNC_SIGNAL), 0, bytes: bytes) }
    }

    /// Tells the engine, on the queue, and performs what it answers.
    private func feed(_ kind: UInt32, _ id: UInt64, status: Int = 0, seq: UInt64 = 0, bytes: [UInt8] = []) {
        if stopped { return }
        let out = try? Fennec.call { out, len in
            bytes.withUnsafeBufferPointer { b in
                fennec_sync_feed(handle, kind, id, Int32(clamping: status), seq, b.baseAddress, b.count, out, len)
            }
        }
        if let out { perform(out) }
    }

    private func perform(_ bytes: [UInt8]) {
        guard let actions = (try? Value.parse(bytes: bytes))?.array else { return }
        for a in actions {
            let id = UInt64(a["id"]?.int ?? 0)
            switch a["do"]?.string {
            case "request": request(id, a)
            case "stream": stream(id, a)
            case "cancel": tasks.removeValue(forKey: id)?.cancel()
            case "wait":
                let ms = a["ms"]?.int ?? 0
                queue.asyncAfter(deadline: .now() + .milliseconds(ms)) { self.feed(UInt32(FENNEC_SYNC_TIMER), id) }
            case "token":
                // Why the core asks: the code the 401 named, whether the
                // refused request carried a token, and the asks in a row.
                // One call out at a time: asked again while at work -- the
                // app's own `setToken` went on and was refused meanwhile --
                // a provider would mint twice for one refusal, and the
                // second token, coming late, end the wait the first one's
                // refusal began. Its answer answers this ask too.
                if let provider, !providing {
                    providing = true
                    let refusal = TokenRefusal(
                        errorCode: a["code"]?.string, hadToken: a["had_token"]?.bool ?? true,
                        attempt: a["attempt"]?.int ?? 1)
                    Task {
                        do {
                            let t = try await provider(refusal)
                            self.provided(t.isEmpty ? nil : t, why: "it handed over no token")
                        } catch {
                            self.provided(nil, why: error.localizedDescription)
                        }
                    }
                }
            case "changed":
                if let db { db.lives.touch(db) }
            case "refused":
                let r = Refusal(
                    status: a["status"]?.int ?? 0, message: a["message"]?.string ?? "", query: a["query"]?.string ?? "",
                    quota: a["quota"]?.string, reason: a["reason"]?.string)
                lock.lock()
                let ws = Array(refusalWatchers.values)
                lock.unlock()
                ws.forEach { $0.yield(r) }
            case "status": reload()
            default: break
            }
        }
    }

    /// The provider's answer: its token handed over, or the engine told
    /// none came and why (`{"token_failed":..}`), which the status's error
    /// says -- `resume()` asks again. On the queue with the call's end, so
    /// an ask the engine makes before the answer reaches it is this call's.
    private func provided(_ token: String?, why: String) {
        let fields: Row
        if let token {
            fields = ["token": .string(token)]
            lock.lock()
            self.token = token
            lock.unlock()
        } else {
            fields = ["token_failed": .string(why)]
        }
        let bytes = Array(Value.object(fields).json.utf8)
        queue.async {
            self.providing = false
            self.feed(UInt32(FENNEC_SYNC_SIGNAL), 0, bytes: bytes)
        }
    }

    private func reload() {
        guard let out = try? Fennec.call({ out, len in fennec_sync_status(handle, out, len) }),
            let v = try? Value.parse(bytes: out)
        else { return }
        let s = SyncStatus(v)
        lock.lock()
        current = s
        let ws = Array(watchers.values)
        lock.unlock()
        ws.forEach { $0.yield(s) }
    }

    private func urlRequest(_ a: Value) -> URLRequest? {
        guard let text = a["url"]?.string, let u = URL(string: text) else { return nil }
        var req = URLRequest(url: u)
        req.httpMethod = a["method"]?.string ?? "GET"
        for (k, v) in a["headers"]?.object ?? Row() {
            req.setValue(v.string, forHTTPHeaderField: k)
        }
        if let body = a["body"]?.string { req.httpBody = Data(body.utf8) }
        return req
    }

    private func request(_ id: UInt64, _ a: Value) {
        guard let req = urlRequest(a) else {
            return feed(UInt32(FENNEC_SYNC_RESPONSE), id, bytes: Array("a request the platform cannot make".utf8))
        }
        let task = session.dataTask(with: req) { [weak self] data, response, error in
            guard let self else { return }
            self.tasks[id] = nil
            let http = response as? HTTPURLResponse
            let status = error == nil ? (http?.statusCode ?? 0) : 0
            let seq = UInt64(http?.value(forHTTPHeaderField: "Fennec-Seq") ?? "") ?? 0
            let body = error.map { Array($0.localizedDescription.utf8) } ?? Array(data ?? Data())
            self.feed(UInt32(FENNEC_SYNC_RESPONSE), id, status: status, seq: seq, bytes: body)
        }
        tasks[id] = task
        task.resume()
    }

    private func stream(_ id: UInt64, _ a: Value) {
        guard let req = urlRequest(a) else {
            return feed(UInt32(FENNEC_SYNC_CLOSED), id, bytes: Array("a stream the platform cannot open".utf8))
        }
        let task = session.dataTask(with: req)
        tasks[id] = task
        streams[task.taskIdentifier] = (id, 0, Data())
        task.resume()
    }

    // ------------------------------------------------- URLSession's events

    public func urlSession(
        _ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
        completionHandler: @escaping (URLSession.ResponseDisposition) -> Void
    ) {
        if var s = streams[dataTask.taskIdentifier] {
            s.status = (response as? HTTPURLResponse)?.statusCode ?? 0
            streams[dataTask.taskIdentifier] = s
            if s.status == 200 { feed(UInt32(FENNEC_SYNC_OPENED), s.id, status: 200) }
        }
        completionHandler(.allow)
    }

    public func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        guard var s = streams[dataTask.taskIdentifier] else { return }
        if s.status == 200 {
            feed(UInt32(FENNEC_SYNC_BYTES), s.id, bytes: Array(data))
        } else {
            s.body.append(data)
            streams[dataTask.taskIdentifier] = s
        }
    }

    public func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        guard let s = streams.removeValue(forKey: task.taskIdentifier) else { return }
        tasks[s.id] = nil
        if s.status != 0 && s.status != 200 {
            feed(UInt32(FENNEC_SYNC_OPENED), s.id, status: s.status, bytes: Array(s.body))
        } else {
            feed(UInt32(FENNEC_SYNC_CLOSED), s.id, bytes: Array((error?.localizedDescription ?? "").utf8))
        }
    }
}
