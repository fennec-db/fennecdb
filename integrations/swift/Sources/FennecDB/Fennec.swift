import FennecFFI
import Foundation

/// What the library or the builder refused, and why. `code` is the kind:
/// the engine's (`notFound`, `duplicate` ...), the boundary's (`misuse`,
/// `locked`, `panic`), or `builder` for a chain the query builder refused
/// before anything ran -- its message the JS builder's, word for word.
public struct FennecError: Error, CustomStringConvertible, Sendable, Equatable {
    public enum Code: Int32, Sendable {
        case type = 1, notFound, exists, duplicate, corrupt, query, io, plugin, readOnly, denied
        case panic, misuse, locked
        /// A write's `require n` not met: it wrote another count, and was put back.
        case unmet = 14
        /// A tenant's quota a server holds it to (`quota` names it).
        case quota = 15
        case builder = 100
    }

    public let code: Code
    public let message: String
    /// The HTTP status a server refused with (`FennecRemote`): 422 is an
    /// idempotency key sent with another request. nil for the library's own.
    public let status: Int?
    /// The statement of a `FennecRemote.batch` that stopped it, from 0: the
    /// write whose `require` was not met, the put whose id was taken.
    public let at: Int?
    /// How many statements of a failed batch stayed applied: 0, since a
    /// batch lands whole, but for one holding a `compact`.
    public let completed: Int?
    /// The tenant's quota a server refused with (`FennecRemote` to a tenant
    /// node): "bytes" and "rows" answer 507, and room is made by deleting;
    /// "streams", "held", "requests", "writes" and "write_bytes" answer 429
    /// with `retryAfter`. nil for any other refusal.
    public let quota: String?
    /// Seconds to wait before sending again, where the server said: a
    /// quota's 429 to the millisecond (`retry_after_ms`), a move's or a
    /// failover's 503 in whole seconds (`Retry-After`).
    public let retryAfter: Double?
    /// What a `require n else '<reason>'` that refused said (`.unmet`):
    /// the statement's own words, by which a caller tells "insufficient
    /// funds" from "no such account" -- where `at` moves with every
    /// statement put in front of it. From the library and a server alike;
    /// nil where the statement gave none.
    public let reason: String?
    /// The statement bound to the write (`create statement ... after`)
    /// that refused it, by name: a guard's `require` not met, a write of
    /// its body refused. From the library and the server's token alike, a
    /// scoped token told the `reason` and no name; nil otherwise.
    public let binding: String?
    /// The code a server named beside its words, its body's `code`
    /// (`FennecRemote`): "token_expired" on a 401 for a token past its
    /// `exp`, which a fresh token from its issuer cures. A later server may
    /// name others; nil for none, and for the library's own. Not `code`,
    /// the library's error kind: every SDK names the server's this way.
    public let errorCode: String?
    /// The parameters the library asked for again as JSON (`exact`).
    let exact: [Int]?

    public init(code: Code, message: String) {
        self.init(code: code, message: message, exact: nil)
    }

    init(code: Code, message: String, exact: [Int]?, reason: String? = nil, binding: String? = nil) {
        self.code = code
        self.message = message
        self.exact = exact
        self.status = nil
        self.at = nil
        self.completed = nil
        self.quota = nil
        self.retryAfter = nil
        self.reason = reason
        self.binding = binding
        self.errorCode = nil
    }

    init(
        code: Code, message: String, status: Int, at: Int?, completed: Int?,
        quota: String? = nil, retryAfter: Double? = nil, reason: String? = nil,
        binding: String? = nil, errorCode: String? = nil
    ) {
        self.code = code
        self.message = message
        self.exact = nil
        self.status = status
        self.at = at
        self.completed = completed
        self.quota = quota
        self.retryAfter = retryAfter
        self.reason = reason
        self.binding = binding
        self.errorCode = errorCode
    }

    public var description: String { message }

    static func builder(_ message: String) -> FennecError { FennecError(code: .builder, message: message) }
}

/// An answer to a statement: rows, a count of what was written, a word
/// that it was done, or the schemas `collections` and `describe` give.
/// Rows carry what `facet` counted beside them -- never in a row, since the
/// counts are over every row the query matched, not the page.
public enum Answer: Sendable, Equatable {
    case rows(columns: [String], rows: [Row], facets: Facets = Facets())
    case affected(Int)
    case ok(String)
    case schemas([Row])

    /// The rows, or none.
    public var rows: [Row] {
        if case .rows(_, let r, _) = self { return r }
        return []
    }

    /// What the query's `facet` clauses counted; empty when it asked none.
    public var facets: Facets {
        if case .rows(_, _, let f) = self { return f }
        return Facets()
    }

    /// How many documents a write wrote, or 0.
    public var affected: Int {
        if case .affected(let n) = self { return n }
        return 0
    }
}

/// One value a `facet` counted and how many matched rows hold it: any value
/// the field holds, `.null` for the rows where it is null.
public struct FacetCount: Sendable, Equatable {
    public let value: Value
    public let count: Int

    public init(value: Value, count: Int) {
        self.value = value
        self.count = count
    }
}

/// Each `facet` field's counts, in the order the query asked them, a
/// field's values most first. Not a dictionary, which would lose that order.
public struct Facets: Sendable, Equatable, Sequence {
    public private(set) var fields: [String] = []
    private var counts: [[FacetCount]] = []

    public init() {}

    public var isEmpty: Bool { fields.isEmpty }

    /// A field's counts, by the name -- or path -- `facet` was given.
    public subscript(field: String) -> [FacetCount]? {
        fields.firstIndex(of: field).map { counts[$0] }
    }

    public func makeIterator() -> AnyIterator<(field: String, counts: [FacetCount])> {
        var i = 0
        return AnyIterator {
            guard i < fields.count else { return nil }
            defer { i += 1 }
            return (fields[i], counts[i])
        }
    }

    /// `{"brand": [{"value": "acme", "count": 12}, ...], ...}`, as the server
    /// and the library both answer them; none where it is absent.
    init(json: Value?) {
        guard let object = json?.object else { return }
        for (field, list) in object {
            fields.append(field)
            counts.append((list.array ?? []).map {
                FacetCount(value: $0["value"] ?? .null, count: $0["count"]?.int ?? 0)
            })
        }
    }
}

/// A fennecdb database in a file on the device -- or in memory -- through
/// the native library (`crates/fennec-ffi`): the engine and the file format
/// of the server and the browser.
///
///     let db = try await Fennec.open(path: docs.appendingPathComponent("app.fennec").path)
///     try await db.execute("create collection todos (title text, done bool @hash)")
///     try await db.from("todos").insert(["title": "milk", "done": false] as Value)
///     let open = try await db.from("todos").where("done", false).rows()
///
/// Every call runs off the caller's thread, on a queue of the library's: a
/// read waits for a write under way, a write for its fsync. One instance is
/// safe to share between tasks and threads; open a file once in a process --
/// a second open of it is refused (`locked`), since two databases over one
/// file corrupt it.
public final class Fennec: @unchecked Sendable {
    /// How `open` keeps the file.
    public struct Options: OptionSet, Sendable {
        public let rawValue: UInt32
        public init(rawValue: UInt32) { self.rawValue = rawValue }
        /// Writes wait in a buffer for `sync()`, `flush()` or `close()`,
        /// rather than an fsync each before the call returns -- a few
        /// milliseconds a write on an iPhone or a Mac.
        public static let noSync = Options(rawValue: UInt32(FENNEC_OPEN_NO_SYNC))
        /// The file read into memory rather than mapped: for a file under
        /// iOS's `complete` protection class, whose pages become unreadable
        /// as the device locks -- a mapped page read then is the app's end.
        public static let inMemory = Options(rawValue: UInt32(FENNEC_OPEN_IN_MEMORY))
        /// No compact on its own. Without it the library compacts the file
        /// beside the calls once half of it is dead records -- versions
        /// updates and deletes left behind -- and at least 64 MB.
        public static let noAutoCompact = Options(rawValue: UInt32(FENNEC_OPEN_NO_AUTO_COMPACT))
    }

    let handle: UInt64
    private let state = NSLock()
    private var closed = false
    /// The close under way or done, which a second `close()` awaits.
    private var closeTask: Task<Void, Error>?
    /// Writes under way, which a live query's run waits out.
    private var inflight = 0
    let lives: Lives
    /// The replica's sync, set once by `Fennec.sync`.
    private var replicaSync: Replica?
    /// By text, the parameters the library asked to have as JSON (`exact`):
    /// a list of numbers it keeps as written -- a json field's, an
    /// `[int]`'s, a `[float]`'s -- goes so at once from the text's second
    /// call on, where sent apart as `f32`s it was refused and sent again,
    /// two of the library's calls each time: 3.38 us a put of an `[int]`
    /// of 8 beside a 128-dim vector against 2.12 (`ffi_bench lists`).
    /// Forgotten past 256 texts, as the JavaScript client's are.
    private var asked: [String: Set<Int>] = [:]

    private init(handle: UInt64) {
        self.handle = handle
        self.lives = Lives()
    }

    // Let go of without `close()`, the database closes as it is freed, on
    // whichever thread let it go: its graphs saved and its file synced, which
    // may take the main thread milliseconds. An app closes it itself.
    deinit {
        if !closed {
            // The session holds its delegate, the replica, until it is let
            // go of: left running, its streams outlived the database.
            replicaSync?.stopNow()
            var out: UnsafeMutablePointer<CChar>?
            _ = fennec_close(handle, &out, nil)
            fennec_free_string(out)
        }
    }

    /// The library's version.
    public static var version: String { String(cString: fennec_version()) }

    /// Opens the file at `path`, made when missing. The open reads the
    /// file's index and links the vectors written since its graphs were last
    /// saved, off the caller's thread.
    public static func open(path: String, options: Options = []) async throws -> Fennec {
        try await background {
            var handle: UInt64 = 0
            try call { out, len in
                var p = path
                return p.withUTF8 { fennec_open($0.baseAddress, $0.count, options.rawValue, &handle, out, len) }
            }
            return Fennec(handle: handle)
        }
    }

    /// Opens the file at `url`.
    public static func open(_ url: URL, options: Options = []) async throws -> Fennec {
        try await open(path: url.path, options: options)
    }

    /// A database in memory alone.
    public static func memory() throws -> Fennec {
        var handle: UInt64 = 0
        try call { out, len in fennec_open_memory(&handle, out, len) }
        return Fennec(handle: handle)
    }

    // -------------------------------------------------------------- running

    /// Runs FennecQL -- one statement or several, which land together -- with
    /// `params` for `$1`, `$2` ... and answers.
    public func run(_ text: String, _ params: any FennecValue...) async throws -> Answer {
        try await run(text, params: try params.map { try $0.fennecValue() })
    }

    /// `run`, its parameters as values.
    public func run(_ text: String, params: [Value]) async throws -> Answer {
        try await run(text, params: params, quiet: false)
    }

    /// The rows a statement answers.
    public func query(_ text: String, _ params: any FennecValue...) async throws -> [Row] {
        try await run(text, params: try params.map { try $0.fennecValue() }).rows
    }

    /// The rows a statement answers, each decoded as `T` by its fields.
    public func query<T: Decodable>(as type: T.Type, _ text: String, _ params: any FennecValue...) async throws -> [T] {
        try decoded(try await run(text, params: try params.map { try $0.fennecValue() }).rows)
    }

    /// A write: how many documents it wrote.
    @discardableResult
    public func execute(_ text: String, _ params: any FennecValue...) async throws -> Int {
        try await run(text, params: try params.map { try $0.fennecValue() }).affected
    }

    /// The query builder over a collection: `db.from("todos").where("done", false).rows()`.
    public func from(_ collection: String) throws -> Query {
        try Query.from(collection).bind { [weak self] text, params in
            guard let self else { throw FennecError(code: .misuse, message: "the database was let go of") }
            return try await self.run(text, params: params)
        }
    }

    func run(_ text: String, params: [Value], quiet: Bool) async throws -> Answer {
        if isClosed { throw FennecError(code: .misuse, message: "the database was closed") }
        if !quiet { begin() }
        defer { if !quiet { end() } }
        let handle = self.handle
        // Looked up only where a list of numbers is among the parameters,
        // so a call with none pays nothing.
        let known = params.contains(where: Fennec.numbers) ? places(text) : []
        return try await Fennec.background { [self] in
            var asJson = known
            while true {
                do {
                    return try Fennec.answer(handle: handle, text: text, params: params, asJson: asJson)
                } catch let e as FennecError where e.exact != nil {
                    // A field keeping a list of numbers as written: those
                    // the library asks for go again inside the JSON, and so
                    // at once the next time. Never a place sent as JSON
                    // already: those it names are new, so this ends.
                    let more = asJson.union(e.exact!)
                    if more == asJson { throw e }
                    asJson = more
                    remember(text, more)
                }
            }
        }
    }

    private func places(_ text: String) -> Set<Int> {
        state.lock()
        defer { state.unlock() }
        return asked[text] ?? []
    }

    private func remember(_ text: String, _ places: Set<Int>) {
        state.lock()
        defer { state.unlock() }
        if asked.count >= 256 && asked[text] == nil { asked.removeAll() }
        asked[text] = places
    }

    /// The places the library asked for as JSON for `text`: internal for
    /// the tests.
    func askedPlaces(_ text: String) -> Set<Int> { places(text) }

    /// A call under way, until `end`: internal for the tests, which hold one
    /// across a burst of writes.
    func begin() {
        state.lock()
        inflight += 1
        state.unlock()
    }

    func end() {
        state.lock()
        inflight -= 1
        state.unlock()
        // After an error too: a text that failed may follow statements that
        // wrote, and the change ring says what landed.
        lives.touch(self)
        // A write to a synced collection left a request for the server due.
        syncing?.poll()
    }

    var syncing: Replica? {
        state.lock()
        defer { state.unlock() }
        return replicaSync
    }

    func attach(_ r: Replica) {
        state.lock()
        replicaSync = r
        state.unlock()
    }

    var isClosed: Bool {
        state.lock()
        defer { state.unlock() }
        return closed
    }

    var writing: Bool {
        state.lock()
        defer { state.unlock() }
        return inflight > 0
    }

    // ------------------------------------------------------------- changes

    /// What changed since `since`: the counter now, and the collections
    /// written -- `nil` when it cannot be told which, everything stale.
    public func changes(since: UInt64) async throws -> Changes {
        let handle = self.handle
        return try await Fennec.background { try Fennec.changes(handle: handle, since: since) }
    }

    static func changes(handle: UInt64, since: UInt64) throws -> Changes {
        let out = try call { out, len in fennec_changes(handle, since, out, len) }
        let v = try Value.parse(bytes: out ?? [])
        return Changes(
            seq: UInt64(v["seq"]?.int ?? 0),
            horizon: UInt64(v["horizon"]?.int ?? 0),
            collections: v["collections"]?.array?.compactMap(\.string)
        )
    }

    // ----------------------------------------------------------- durability

    /// Every write so far on disk: written and fsynced, the fsync with no
    /// lock held. What a database opened `.noSync` calls when its writes
    /// must last.
    public func sync() async throws { try await byHandle(.sync) }

    /// Every write so far handed to the system, no fsync: it outlives the
    /// app being killed, not the device losing power -- microseconds, what
    /// an app does as it goes to the background.
    public func flush() async throws { try await byHandle(.flush) }

    /// The file written anew as an image of the database, graphs and all:
    /// the next open links nothing. Holds the database while it writes.
    public func checkpoint() async throws { try await byHandle(.checkpoint) }

    /// Builds the hash, text, ordered and sparse indexes an open leaves for
    /// their first read, so the first search does not pay for its index (a
    /// `@text` index of 100 000 products: 141 ms): those `only` names --
    /// collections and `collection.field`s -- or every one. Each under the
    /// read lock on its own, so reads go on beside it. Answers how many it
    /// built; call it after the open, as the app starts.
    @discardableResult
    public func warm(_ only: [String] = []) async throws -> Int {
        let handle = self.handle
        let names = only.joined(separator: ",")
        return try await Fennec.background {
            var text = names
            let out = try text.withUTF8 { b in
                try Fennec.call { out, len in fennec_warm(handle, b.baseAddress, b.count, out, len) }
            }
            return try Value.parse(bytes: out ?? [])["built"]?.int ?? 0
        }
    }

    /// Saves the graphs, syncs and lets the file go; the live queries stop.
    /// Waits for the calls under way.
    /// A close called while another runs waits for it: returning at once,
    /// it let its caller open the file again before the first had let it go.
    public func close() async throws {
        try await closing().value
    }

    /// The close, begun by the first call and awaited by every one.
    private func closing() -> Task<Void, Error> {
        state.lock()
        defer { state.unlock() }
        if let t = closeTask { return t }
        closed = true
        let t = Task {
            await self.syncing?.stop()
            await self.lives.clear()
            try await self.byHandle(.close)
        }
        closeTask = t
        return t
    }

    private enum Op { case sync, flush, checkpoint, close }

    private func byHandle(_ op: Op) async throws {
        let handle = self.handle
        try await Fennec.background {
            try Fennec.call { out, len in
                switch op {
                case .sync: return fennec_sync(handle, out, len)
                case .flush: return fennec_flush(handle, out, len)
                case .checkpoint: return fennec_checkpoint(handle, out, len)
                case .close: return fennec_close(handle, out, len)
                }
            }
        }
    }

    // -------------------------------------------------------- the boundary

    /// The queue the library's calls run on: each may block -- for the
    /// lock, an fsync -- which is what a cooperative thread of Swift's own
    /// pool should not do.
    static let queue = DispatchQueue(label: "dev.fennecdb", qos: .userInitiated, attributes: .concurrent)

    @discardableResult
    static func background<T: Sendable>(_ body: @escaping @Sendable () throws -> T) async throws -> T {
        try await withCheckedThrowingContinuation { cont in
            queue.async { cont.resume(with: Result { try body() }) }
        }
    }

    /// One call: its code, and what it wrote through its out pointer -- the
    /// answer's bytes, or the error it is thrown as.
    @discardableResult
    static func call(
        _ f: (UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>, UnsafeMutablePointer<Int>) -> Int32
    ) throws -> [UInt8]? {
        var out: UnsafeMutablePointer<CChar>?
        var len = 0
        let code = f(&out, &len)
        defer { fennec_free_string(out) }
        let bytes = out.map { p in p.withMemoryRebound(to: UInt8.self, capacity: len) { Array(UnsafeBufferPointer(start: $0, count: len)) } }
        guard code != FENNEC_OK else { return bytes }
        let v = (try? Value.parse(bytes: bytes ?? [])) ?? .null
        throw FennecError(
            code: FennecError.Code(rawValue: code) ?? .panic,
            message: v["message"]?.string ?? "error \(code)",
            exact: v["exact"]?.array?.compactMap(\.int),
            reason: v["reason"]?.string,
            binding: v["binding"]?.string
        )
    }

    /// Runs `text`: the vectors among the parameters -- a `[Float]`, or a
    /// list of finite numbers, which the engine reads as a vector either way
    /// -- go over as their bytes, their place `null` in the JSON, unless
    /// `asJson` names them. Written out as text and read back, a vector's
    /// digits were most of a put's time: 4.6 us a 128-dim put as bytes,
    /// 23.3 as JSON (`make ffi-bench`).
    static func answer(handle: UInt64, text: String, params: [Value], asJson: Set<Int>) throws -> Answer {
        var json = params
        var vectors: [UInt8] = []
        for (i, p) in params.enumerated() where !asJson.contains(i) {
            guard let (f, exact) = vector(p) else { continue }
            json[i] = .null
            // Marked exact where each `Float` is the number handed: an
            // `[int]` or a `[float]` takes it as it came, in one call
            // (`fennec_abi::EXACT`).
            append(UInt32(i) | (exact ? 1 << 31 : 0), &vectors)
            append(UInt32(f.count), &vectors)
            for x in f {
                // A -0 goes as 0, as JSON writes it: either way, one vector.
                append((x == 0 ? 0 : x).bitPattern, &vectors)
            }
        }
        var t = text
        var p = Value.array(json).json
        let out = try t.withUTF8 { tb in
            try p.withUTF8 { pb in
                try vectors.withUnsafeBufferPointer { vb in
                    try call { out, len in
                        fennec_query(handle, tb.baseAddress, tb.count, pb.baseAddress, pb.count, vb.baseAddress, vb.count, out, len)
                    }
                }
            }
        }
        return try Fennec.decode(answer: try Value.parse(bytes: out ?? []))
    }

    /// Whether `v` goes over apart as a vector (`vector`), its `f32`s
    /// made of nothing.
    private static func numbers(_ v: Value) -> Bool {
        switch v {
        case .floats(let f): return !f.isEmpty
        case .array(let a):
            return !a.isEmpty && a.allSatisfy {
                switch $0 {
                case .int: return true
                case .double(let d): return d.isFinite
                default: return false
                }
            }
        default: return false
        }
    }

    /// A vector's `Float`s, and whether each is the number it was made of:
    /// a `[Float]`'s are, an int of at most 2^24 in size is, and a `Double`
    /// a `Float` holds is (`0.5`) -- where `0.1` and 16777217 are not.
    private static func vector(_ v: Value) -> ([Float], Bool)? {
        switch v {
        case .floats(let f) where !f.isEmpty: return (f, true)
        case .array(let a) where !a.isEmpty:
            var out: [Float] = []
            out.reserveCapacity(a.count)
            var exact = true
            for x in a {
                switch x {
                case .int(let n):
                    exact = exact && n.magnitude <= 1 << 24
                    out.append(Float(n))
                case .double(let d) where d.isFinite:
                    let f = Float(d)
                    exact = exact && Double(f) == d
                    out.append(f)
                default: return nil
                }
            }
            return (out, exact)
        default: return nil
        }
    }

    private static func append(_ n: UInt32, _ out: inout [UInt8]) {
        withUnsafeBytes(of: n.littleEndian) { out.append(contentsOf: $0) }
    }

    static func decode(answer v: Value) throws -> Answer {
        switch v["kind"]?.string {
        case "rows":
            let r = v["result"]
            return .rows(
                columns: r?["columns"]?.array?.compactMap(\.string) ?? [],
                rows: r?["rows"]?.array?.compactMap(\.object) ?? [],
                facets: Facets(json: r?["facets"])
            )
        case "affected": return .affected(v["count"]?.int ?? 0)
        case "ok": return .ok(v["message"]?.string ?? "")
        case "schemas": return .schemas(v["collections"]?.array?.compactMap(\.object) ?? [])
        default: throw FennecError(code: .query, message: "an answer of no kind the binding knows: \(v.json)")
        }
    }
}

/// What `changes(since:)` answers.
public struct Changes: Sendable, Equatable {
    /// The change counter now: the cursor to ask from next.
    public let seq: UInt64
    /// The oldest cursor the ring still answers for.
    public let horizon: UInt64
    /// The collections written since, or `nil`: everything is stale.
    public let collections: [String]?
}

func decoded<T: Decodable>(_ rows: [Row]) throws -> [T] {
    try JSONDecoder().decode([T].self, from: Data(Value.array(rows.map { .object($0) }).json.utf8))
}
