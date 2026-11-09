import Foundation

/// A fennec-server over HTTP, with no file on the device: every query goes
/// to the server (`POST /query`), through the same builder. What
/// `Fennec.connect` makes, as `connect` does in JS.
///
///     let db = Fennec.connect(url: "https://api.example.com", token: jwt)
///     let open = try await db.from("todos").where("done", false).rows()
///
/// The requests are `URLSession`'s, so TLS is the system's. A request goes
/// again only where it cannot have run or may run twice (`Retry`), and
/// `timeout` bounds the wait for an answer's head and each wait between
/// the pieces of its body.
public final class FennecRemote: @unchecked Sendable {
    /// How a connection sends a request again: only where nothing ran, or
    /// where running it again changes nothing
    /// (docs/design/js-client.md#retries).
    ///
    /// Nothing ran: a 503, or a quota's 429, naming its wait -- sent after
    /// that wait, never sooner, unless its body says statements of a batch
    /// were made (`completed`) -- and a connection refused. Running it
    /// again changes nothing: a read the builder made, one sent near
    /// (`Fennec-Read: replica`, which every server runs read-only), and a
    /// write under an `Idempotency-Key` -- these go again after a reset or
    /// a dropped connection too. Never after the connection's own
    /// `timeout`, past which the server may still run it, nor once the
    /// task is cancelled, nor after a refusal naming no wait; an
    /// `asRead()` text sent home is a claim nothing checks, and goes once.
    ///
    /// `attempts` tries at most, their waits -- the server's, or 0.25 s
    /// doubling, 30% jitter on top -- within `maxWait` seconds; past
    /// either, the last answer or error is the caller's. `retry: nil` on
    /// `Fennec.connect` sends nothing again.
    public struct Retry: Sendable, Equatable {
        public let attempts: Int
        public let maxWait: Double

        public init(attempts: Int = 3, maxWait: Double = 10) {
            precondition(attempts >= 1 && maxWait >= 0, "bad retry: \(attempts) attempts, \(maxWait) s")
            self.attempts = attempts
            self.maxWait = maxWait
        }
    }

    /// Where a connection's reads go. `.home`, the default: every request
    /// to the tenant's home. `.nearest`: the builder's reads, and an
    /// `asRead()` copy's requests, are marked (`Fennec-Read: replica`) so
    /// that a fennec-shard router near a region holding the tenant's read
    /// replica answers them there, each sent after the furthest position an
    /// answer from home named to this connection (`Fennec-After`,
    /// `Fennec-History`: the change and its history) -- never older than its
    /// own writes, nor a write a failover replaced. Writes go home.
    public enum Reads: Sendable { case home, nearest }

    public let url: String
    /// Where this connection's reads go (`Fennec.connect`'s `reads`).
    public let reads: Reads
    /// Seconds to an answer's head, a `withWait` copy's wait on top, and
    /// between the pieces of its body (`Fennec.connect`'s `timeout`).
    public let timeout: Double
    /// How a request goes again, nil for never (`Fennec.connect`'s `retry`).
    public let retry: Retry?
    /// The token, the last `Fennec-Seq` and the furthest position, shared
    /// with every copy `withIdempotencyKey`, `withWait` and `asRead` make,
    /// as Go's and .NET's copies share theirs.
    private let shared: Shared
    private let session: URLSession
    private let idempotencyKey: String?
    /// How long the server may hold a write that writes nothing (`withWait`).
    private let held: Double?
    /// This copy's requests only read (`asRead`, the builder's reads).
    private let reading: Bool
    /// This copy's requests are the builder's reads (`from`): reads the
    /// builder made, which may run twice -- unlike an `asRead()` text, a
    /// claim nothing checks at home.
    private let built: Bool

    private final class Shared: @unchecked Sendable {
        let lock = NSLock()
        var token: String?
        /// The furthest position an answer from home named, which a read
        /// near is sent after: an answer that arrives late is not the last.
        var at: Position?
        /// The tests' probe, called with each wait before a request goes
        /// again; nil, as it always is outside them.
        var pausing: (@Sendable (Double) -> Void)?
        init(token: String?) { self.token = token }
    }

    init(
        url: String, token: String?, reads: Reads = .home, timeout: Double = 30, retry: Retry? = Retry(),
        session: URLSession = .shared
    ) {
        precondition(timeout > 0, "bad timeout: \(timeout)")
        self.url = url.hasSuffix("/") ? String(url.dropLast()) : url
        self.reads = reads
        self.timeout = timeout
        self.retry = retry
        self.shared = Shared(token: token)
        self.session = session
        self.idempotencyKey = nil
        self.held = nil
        self.reading = false
        self.built = false
    }

    private init(_ of: FennecRemote, key: String?, held: Double?, reading: Bool, built: Bool = false) {
        url = of.url
        reads = of.reads
        timeout = of.timeout
        retry = of.retry
        shared = of.shared
        session = of.session
        idempotencyKey = key
        self.held = held
        self.reading = reading
        self.built = built
    }

    /// The tests' probe of each wait before a request goes again.
    var pausing: (@Sendable (Double) -> Void)? {
        get {
            shared.lock.lock()
            defer { shared.lock.unlock() }
            return shared.pausing
        }
        set {
            shared.lock.lock()
            shared.pausing = newValue
            shared.lock.unlock()
        }
    }

    /// A fresh token for the requests from here on.
    public func setToken(_ token: String?) {
        shared.lock.lock()
        shared.token = token
        shared.lock.unlock()
    }

    private func currentToken() -> String? {
        shared.lock.lock()
        defer { shared.lock.unlock() }
        return shared.token
    }

    /// The furthest change an answer from home -- a write's, a read-only
    /// batch's -- left the database at (`Fennec-Seq`), through this
    /// connection or a copy of it; nil before any. Answers arriving out of
    /// order do not move it back, a read near's answer, which may be a
    /// replica's, moves it not, and a read near's 504 sets it to where home
    /// says it stands, as Go's `Seq()` and .NET's `Seq` are.
    public var seq: Int? { position()?.seq }

    /// A copy whose writes -- `run`, `batch` and the builder's -- carry
    /// `key` as their `Idempotency-Key`: sent again after a timeout, a write
    /// is answered as it was the first time and not made twice. One key a
    /// write: the same key with another request is refused (status 422).
    ///
    ///     try await db.withIdempotencyKey(orderID).from("orders").insert(order)
    public func withIdempotencyKey(_ key: String) -> FennecRemote {
        FennecRemote(self, key: key, held: held, reading: reading)
    }

    /// A copy whose `set` and `del` statements -- `run`, `batch` and the
    /// builder's `update`, `delete` and their `Returning` forms -- the
    /// server holds while they write nothing, until they can or for
    /// `seconds`, at most 30 (`Fennec-Wait`): a worker's claim waits there
    /// for a job -- one enqueued, a delayed one come due, a lease lapsed --
    /// rather than sleeping and claiming again. Cancelling the task gives
    /// it up.
    ///
    ///     let claimed = try await db.withWait(30).from("jobs").where(.raw("run_at <= now()"))
    ///         .order("run_at").limit(10).updateReturning(claim)
    public func withWait(_ seconds: Double) -> FennecRemote {
        FennecRemote(self, key: idempotencyKey, held: seconds, reading: reading)
    }

    /// A copy whose requests only read: on a connection made with `reads:
    /// .nearest` a router near the tenant's read replica answers them
    /// there, after this connection's writes. The builder's reads go so on
    /// their own; a text the caller knows reads -- a `run`, a `batch` of
    /// `toFennecQL()`s -- goes through this, and one that writes is refused
    /// (status 403), never landed. On a connection reading home it changes
    /// nothing.
    ///
    ///     let n = try await db.asRead().query("get orders where customer = $1 count", "c-7")
    public func asRead() -> FennecRemote {
        FennecRemote(self, key: idempotencyKey, held: held, reading: true)
    }

    /// Runs FennecQL on the server with `params` for `$1`, `$2` ...
    public func run(_ text: String, _ params: any FennecValue...) async throws -> Answer {
        try await run(text, params: try params.map { try $0.fennecValue() })
    }

    /// `run`, its parameters as values. With `idempotencyKey` a write runs
    /// once however often it is sent.
    public func run(_ text: String, params: [Value], idempotencyKey: String? = nil) async throws -> Answer {
        var body = Row()
        body["query"] = .string(text)
        body["params"] = .array(params)
        let (v, _, _) = try await post(
            "/query", type: "application/json", body: Data(Value.object(body).json.utf8),
            key: idempotencyKey)
        return FennecRemote.answer(v)
    }

    /// What `batch` answers: each statement's answer in order, the change
    /// the batch left the database at, and whether the answer is the one
    /// kept for its idempotency key (`replayed`) -- a replayed answer
    /// carries no `seq`.
    public struct BatchAnswer: Sendable, Equatable {
        public let results: [Answer]
        public let seq: Int?
        public let replayed: Bool
    }

    /// `POST /batch`: the statements in order under one write lock, as one
    /// block -- their writes all land, or at the first error none of them
    /// do, a `FennecError` whose `at` is the statement that stopped it
    /// (`unmet` for a write's `require` not met, its `reason` the one the
    /// statement gave, which stays put as statements go in front of it
    /// where `at` moves). A statement is what the builder's `toInsert`,
    /// `toUpdate`, `toDelete` and `toFennecQL` make.
    /// With `idempotencyKey` a retry after a timeout is answered as the
    /// first try was and writes nothing twice; a batch of reads alone takes
    /// no key.
    public func batch(
        _ statements: [(text: String, params: [Value])], idempotencyKey: String? = nil
    ) async throws -> BatchAnswer {
        let lines = statements.map { s -> String in
            var line = Row()
            line["query"] = .string(s.text)
            line["params"] = .array(s.params)
            return Value.object(line).json
        }
        let (v, seq, replayed) = try await post(
            "/batch", type: "application/x-ndjson", body: Data(lines.joined(separator: "\n").utf8),
            key: idempotencyKey)
        let results = (v["results"]?.array ?? []).map(FennecRemote.answer)
        return BatchAnswer(results: results, seq: seq, replayed: replayed)
    }

    /// A POST's JSON answer, its `Fennec-Seq` and whether it was replayed;
    /// a refusal as a `FennecError` with its status and, for a batch, where
    /// it stopped.
    private func post(_ path: String, type: String, body: Data, key: String?) async throws -> (Value, Int?, Bool) {
        guard let u = URL(string: "\(url)\(path)") else {
            throw FennecError(code: .misuse, message: "not a URL: \(url)")
        }
        var req = URLRequest(url: u)
        req.httpMethod = "POST"
        req.setValue(type, forHTTPHeaderField: "content-type")
        if let token = currentToken() { req.setValue("Bearer \(token)", forHTTPHeaderField: "authorization") }
        let key = key ?? idempotencyKey
        if let key { req.setValue(key, forHTTPHeaderField: "idempotency-key") }
        // URLSession's is a bound on each wait for a piece of the answer --
        // its head, then each between the pieces of its body -- not on the
        // whole: a long answer is read for as long as it comes.
        req.timeoutInterval = timeout
        if let held, held > 0 {
            req.setValue(String(Int(held * 1000)), forHTTPHeaderField: "fennec-wait")
            // Held that long before the first byte of its answer: the head's
            // wait alone (`read`), each gap in the body is bounded by
            // `timeout` again.
            req.timeoutInterval = timeout + held
        }
        // A read near: marked for the tenant's read replica in the router's
        // region, after the furthest position home named to this
        // connection, which the replica waits for -- that change of that
        // history.
        var near = reading && reads == .nearest
        // A position of change 0 is none: nothing to wait for, as Go and
        // .NET send none.
        let sent = near ? position().flatMap { $0.seq > 0 ? $0 : nil } : nil
        if near {
            req.setValue("replica", forHTTPHeaderField: "fennec-read")
            if let sent {
                req.setValue(String(sent.seq), forHTTPHeaderField: "fennec-after")
                if let h = sent.history { req.setValue(h, forHTTPHeaderField: "fennec-history") }
            }
        }
        req.httpBody = body
        // What may run twice, sent again after a reset too: a read the
        // builder made, one sent near, which every server runs read-only
        // (`Fennec-Read`), and a write under a key. An `asRead()` text sent
        // home is a claim nothing checks: a statement it calls may have
        // been made to write since.
        var (data, http) = try await ask(req, twice: built || key != nil || near)
        // 504: no copy holds that write in the wait -- home had none
        // either, lost in a failover. Home says where it stands, which the
        // connection takes as its position, lower or not (`told`); asked
        // home again with no mark, the read is answered with what home
        // holds.
        if near, let sent, http?.statusCode == 504 {
            if let home = try? Value.parse(bytes: Array(data)), let at = home["seq"]?.int {
                told(sent: sent, Position(seq: at, history: home["history"]?.string))
            }
            for name in ["fennec-read", "fennec-after", "fennec-history"] { req.setValue(nil, forHTTPHeaderField: name) }
            near = false
            (data, http) = try await ask(req, twice: built || key != nil)
        }
        let status = http?.statusCode ?? 0
        let v = data.isEmpty ? Value.null : (try? Value.parse(bytes: Array(data)))
        guard (200..<300).contains(status) else {
            throw FennecRemote.refused(status, v, retryAfter: http?.value(forHTTPHeaderField: "retry-after"))
        }
        guard let v else {
            throw FennecError(code: .io, message: "the server did not answer JSON (\(status))")
        }
        let seq = (http?.value(forHTTPHeaderField: "fennec-seq")).flatMap { Int($0) }
        // A position from home alone: a read near's answer may be a
        // replica's, behind home or on a branch a failover left.
        if let seq, !near { note(Position(seq: seq, history: http?.value(forHTTPHeaderField: "fennec-history"))) }
        return (v, seq, http?.value(forHTTPHeaderField: "idempotent-replayed") == "true")
    }

    /// The request sent, and its answer's body and head, sent again by the
    /// rule (docs/design/js-client.md#retries, `Retry`): what ran nothing --
    /// a 503 or 429 naming its wait, after it, unless statements of a batch
    /// landed (`completed`); a connection refused -- and, where `twice`, a
    /// reset or a dropped connection too. Never after the connection's own
    /// timeout, which a request may still run past, nor once the task is
    /// cancelled. URLSession sends a POST once, a reset and a dropped
    /// connection answered as `networkConnectionLost` (it sends a GET
    /// again on its own on a kept connection, which this client sends
    /// none of).
    private func ask(_ req: URLRequest, twice: Bool) async throws -> (Data, HTTPURLResponse?) {
        var waited = 0.0
        var n = 0
        while true {
            var wait = 0.0
            var answer: (Data, HTTPURLResponse?)?
            var failure: Error?
            do {
                let (data, response) = try await read(req)
                let http = response as? HTTPURLResponse
                if let s = http?.statusCode, s == 503 || s == 429 {
                    let v = try? Value.parse(bytes: Array(data))
                    if !((v?["completed"]?.int ?? 0) > 0) {
                        wait = FennecRemote.waitOf(v, header: http?.value(forHTTPHeaderField: "retry-after")) ?? 0
                    }
                }
                if wait == 0 { return (data, http) }
                answer = (data, http)
            } catch {
                if Task.isCancelled { throw CancellationError() }
                let code = (error as? URLError)?.code
                // The connection's own timeout: a `compact`, a long read may
                // still be running, and three tries ran a read three times.
                if code == .cancelled { throw FennecRemote.unreached(error) }
                if code == .timedOut || !(twice || code == .cannotConnectToHost) { throw FennecRemote.unreached(error) }
                failure = error
            }
            // 250 ms doubling, or the server's wait; 30% jitter on top, so a
            // server back up is not met by every client at once.
            let base = max(wait, 0.25 * Double(1 << min(n, 30)))
            guard let r = retry, n + 1 < r.attempts, waited + base <= r.maxWait else {
                if let failure { throw FennecRemote.unreached(failure) }
                return answer!
            }
            let w = min(base * (1 + Double.random(in: 0..<0.3)), r.maxWait - waited)
            waited += w
            pausing?(w)
            try await Task.sleep(nanoseconds: UInt64(w * 1_000_000_000))
            n += 1
        }
    }

    /// The answer, the head waited for as long as `req` says (a held
    /// write's wait on top of `timeout`), then each gap between the pieces
    /// of its body for `timeout` alone -- URLSession has one interval for
    /// both, so the body's gaps are watched here.
    private func read(_ req: URLRequest) async throws -> (Data, URLResponse) {
        let (bytes, response) = try await session.bytes(for: req)
        let gap = timeout
        let beat = Beat()
        return try await withThrowingTaskGroup(of: Data?.self) { group in
            group.addTask {
                var data = Data()
                for try await b in bytes {
                    data.append(b)
                    beat.touch()
                }
                return data
            }
            group.addTask {
                while true {
                    try await Task.sleep(nanoseconds: UInt64(min(gap, 0.05) * 1_000_000_000))
                    if beat.idle >= gap { throw URLError(.timedOut) }
                }
            }
            defer { group.cancelAll() }
            while let r = try await group.next() {
                if let r { return (r, response) }
            }
            throw URLError(.unknown)
        }
    }

    private static func unreached(_ error: Error) -> FennecError {
        if (error as? URLError)?.code == .timedOut {
            return FennecError(code: .io, message: "no answer within the timeout: \(error.localizedDescription)")
        }
        return FennecError(code: .io, message: "the server could not be reached: \(error.localizedDescription)")
    }

    /// The seconds an answer asks a client to wait, where it names any: to
    /// the millisecond where the body says it (`retry_after_ms`) --
    /// `Retry-After` is whole seconds, and a bucket of 600 rows a second
    /// refills a row in 1.7 ms -- then the header, then the body's
    /// `retry_after`, which a 503 of the server's carries.
    static func waitOf(_ v: Value?, header: String?) -> Double? {
        if let ms = v?["retry_after_ms"]?.double, ms > 0, ms.isFinite { return ms / 1000 }
        for said in [header.flatMap { Double($0.trimmingCharacters(in: .whitespaces)) }, v?["retry_after"]?.double] {
            if let s = said, s > 0, s.isFinite { return s }
        }
        return nil
    }

    // Out of the async function: an NSLock is not to be taken in one.
    private func note(_ p: Position) {
        shared.lock.lock()
        if p.isPast(shared.at) { shared.at = p }
        shared.lock.unlock()
    }

    private func position() -> Position? {
        shared.lock.lock()
        defer { shared.lock.unlock() }
        return shared.at
    }

    /// Home's position, lower or not, which a read near's 504 names when no
    /// copy held the one it was sent after -- a write lost in a failover:
    /// kept, every read near would wait it out for good. Taken only over
    /// the position sent -- a write answered meanwhile is further, and set
    /// back the next read near would miss it -- and never of an earlier
    /// term of the same lineage: a router whose copy of the directory lags
    /// a failover sends the read to the old primary, fenced, which names
    /// the branch it abandoned. Another lineage's is a tenant made again
    /// under the name, which starts again at term 1.
    private func told(sent: Position, _ p: Position) {
        shared.lock.lock()
        if shared.at == sent && (p.term >= sent.term || p.isOfAnotherLineage(than: sent)) { shared.at = p }
        shared.lock.unlock()
    }

    /// The rows a statement answers.
    public func query(_ text: String, _ params: any FennecValue...) async throws -> [Row] {
        try await run(text, params: try params.map { try $0.fennecValue() }).rows
    }

    /// A write: how many documents it wrote.
    @discardableResult
    public func execute(_ text: String, _ params: any FennecValue...) async throws -> Int {
        try await run(text, params: try params.map { try $0.fennecValue() }).affected
    }

    /// The query builder, its queries run on the server -- its reads as an
    /// `asRead()` copy's, near on a connection reading near, and sent again
    /// after a reset as reads the builder made (`Retry`).
    public func from(_ collection: String) throws -> Query {
        let reader = FennecRemote(self, key: idempotencyKey, held: held, reading: true, built: true)
        return try Query.from(collection).bind(
            { [self] text, params in try await self.run(text, params: params) },
            read: { text, params in try await reader.run(text, params: params) })
    }

    /// A refusal as the error it is: its status and message, where a batch
    /// stopped, what a `require n else '<reason>'` that refused it said and
    /// the statement bound to the write that refused it,
    /// and where a tenant's quota refused it the quota and how
    /// many seconds to wait -- to the millisecond where the body says it
    /// (`retry_after_ms`), else `Retry-After`, or the same in the body.
    static func refused(_ status: Int, _ v: Value?, retryAfter: String?) -> FennecError {
        let quota = v?["quota"]?.string
        let wait = waitOf(v, header: retryAfter)
        return FennecError(
            code: quota == nil ? FennecRemote.code(status) : .quota,
            message: v?["error"]?.string ?? "HTTP \(status)",
            status: status, at: v?["at"]?.int, completed: v?["completed"]?.int,
            quota: quota, retryAfter: wait, reason: v?["reason"]?.string,
            binding: v?["binding"]?.string)
    }

    /// An HTTP status as the error kind the engine would have said; the
    /// status itself is the error's `status`.
    static func code(_ status: Int) -> FennecError.Code {
        switch status {
        case 400: return .query
        case 401, 403: return .denied
        case 404: return .notFound
        case 409: return .duplicate
        case 412: return .unmet
        default: return .io
        }
    }

    /// The endpoint's answer as the library's: rows come as an array, or --
    /// when the query asked for facets, and as a batch's each read -- as
    /// `{"rows": [...], "facets": {...}}`, the counts beside the rows
    /// rather than in one.
    static func answer(_ v: Value) -> Answer {
        if let rows = v.array ?? v["rows"]?.array {
            let objects = rows.compactMap(\.object)
            return .rows(columns: objects.first?.keys ?? [], rows: objects, facets: Facets(json: v["facets"]))
        }
        if let n = v["affected"]?.int { return .affected(n) }
        if let c = v["collections"]?.array { return .schemas(c.compactMap(\.object)) }
        if let m = v["message"]?.string ?? v["ok"]?.string { return .ok(m) }
        return .ok(v.json)
    }
}

/// Where an answer from home left the tenant: its change (`Fennec-Seq`) and
/// the history that change is of (`Fennec-History`, `<term>-<id>`; none for a
/// database that never had a replica).
struct Position: Equatable, Sendable {
    let seq: Int
    let history: String?

    /// The history's term, one more at each promotion.
    var term: Int {
        guard let h = history, let t = h.split(separator: "-", maxSplits: 1).first else { return 0 }
        return Int(t) ?? 0
    }

    /// The history's lineage: its id's top half, which every fork of the
    /// tenant keeps and a tenant made again under its name does not.
    var lineage: Substring? {
        guard let h = history, let dash = h.firstIndex(of: "-") else { return nil }
        let id = h[h.index(after: dash)...]
        return id.count >= 8 ? id.prefix(8) : nil
    }

    /// Whether this is of another lineage than `q`: a tenant made again.
    func isOfAnotherLineage(than q: Position) -> Bool {
        guard let a = lineage, let b = q.lineage else { return false }
        return a != b
    }

    /// Whether this is past `q`: of another lineage -- home's word on a
    /// tenant made again, whose terms start again -- or a later term of the
    /// same, or further in one.
    func isPast(_ q: Position?) -> Bool {
        guard let q else { return true }
        return isOfAnotherLineage(than: q) || term > q.term || (term == q.term && seq > q.seq)
    }
}

extension Fennec {
    /// A fennec-server over HTTP, with no local file: every query goes to the
    /// server, through the same builder (`FennecRemote`). With `reads:
    /// .nearest` the builder's reads go to the tenant's read replica near
    /// the app, where a fennec-shard router has one, after the app's own
    /// writes (`FennecRemote.Reads`). `timeout` is seconds to an answer's
    /// head and between the pieces of its body; `retry` how a request goes
    /// again where it cannot have run or may run twice, nil for never
    /// (`FennecRemote.Retry`).
    public static func connect(
        url: String, token: String? = nil, reads: FennecRemote.Reads = .home, timeout: Double = 30,
        retry: FennecRemote.Retry? = FennecRemote.Retry()
    ) -> FennecRemote {
        FennecRemote(url: url, token: token, reads: reads, timeout: timeout, retry: retry)
    }
}

/// When the last piece of a body came, for `FennecRemote.read`'s watch.
private final class Beat: @unchecked Sendable {
    private let lock = NSLock()
    private var last = ProcessInfo.processInfo.systemUptime
    func touch() {
        lock.lock()
        last = ProcessInfo.processInfo.systemUptime
        lock.unlock()
    }
    var idle: Double {
        lock.lock()
        defer { lock.unlock() }
        return ProcessInfo.processInfo.systemUptime - last
    }
}
