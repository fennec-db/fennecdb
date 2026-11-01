import Foundation

/// A call of a named statement (`create statement ...`): `call <name> $1`,
/// the arguments the one parameter -- an object, by name -- and `claims`
/// after them `as $2`, the claims the call is made with (a server reads
/// them from its own token alone; a scoped token's are its token's, and it
/// may not say them. An embedded database takes the embedder's: a
/// signed-in user's, held to the declared types as a token's are). Not
/// bound to a collection, so `Call.call(...)` stands beside `Query.from(...)`:
///
///     try Call.call("transfer", ["from": 1, "to": 2, "amount": 300]).toCall()
///     // ("call transfer $1", [{"from": 1, "to": 2, "amount": 300}])
///
/// `db.call(name, args)` runs one. No arguments is a statement of none --
/// `call stamp` -- and an empty object is still one argument (`call stamp
/// $1`). The values are parameters as a query's are -- a `Date` its ISO
/// text, a `[Float]` its numbers -- and never the text. A struct, a
/// dictionary, a `Row` or a `Value` object is the arguments; anything
/// else (a list, a text, a number) is refused. Immutable, as a `Query` is.
public struct Call: Sendable {
    /// The statement's name.
    public let name: String
    let args: Value?
    let claims: Value?

    private init(name: String, args: Value?, claims: Value?) {
        self.name = name
        self.args = args
        self.claims = claims
    }

    /// A call of `name` with `args` -- nil or `Value.null` for none -- and
    /// `claims`, nil or `Value.null` for none. The name is the one thing of
    /// a call written into its text, so it is checked: ASCII letters,
    /// digits and `_`, a letter first, at most 59 bytes.
    public static func call(_ name: String, _ args: (any FennecValue)? = nil, claims: (any FennecValue)? = nil) throws -> Call {
        guard valid(name) else { throw FennecError.builder("invalid statement name: \(Builder.quote(name))") }
        return Call(
            name: name, args: try given(args, "arguments", "argument"), claims: try given(claims, "claims", "claim"))
    }

    /// The engine's rule (`named::valid_name`), a path segment with nothing
    /// to escape: `^[A-Za-z][A-Za-z0-9_]{0,58}$`.
    static func valid(_ name: String) -> Bool {
        let bytes = Array(name.utf8)
        guard let first = bytes.first, bytes.count <= 59, letter(first) else { return false }
        return bytes.allSatisfy { letter($0) || ($0 >= 0x30 && $0 <= 0x39) || $0 == 0x5F }
    }

    private static func letter(_ b: UInt8) -> Bool { (b >= 0x41 && b <= 0x5A) || (b >= 0x61 && b <= 0x7A) }

    /// The arguments or the claims: an object of values by name, or none.
    private static func given(_ v: (any FennecValue)?, _ what: String, _ one: String) throws -> Value? {
        guard let v else { return nil }
        let value = try v.fennecValue()
        switch value {
        case .null: return nil
        case .object: try refuseComputed(value, one); return value
        default: throw FennecError.builder("a call's \(what) are an object of values by name")
        }
    }

    /// `inc` and `expr` read the row a write changes: a call has none, and
    /// the object they are would go to the server as a member by that name.
    private static func refuseComputed(_ v: Value, _ one: String) throws {
        switch v {
        case .object(let r):
            if r[Value.computedKey] != nil {
                throw FennecError.builder("this object cannot be used as a fennecdb value (\(one))")
            }
            for member in r.values { try refuseComputed(member, one) }
        case .array(let list):
            for member in list { try refuseComputed(member, one) }
        default: break
        }
    }

    /// The call's FennecQL and its parameters: what `FennecRemote.batch`
    /// takes beside a query's `toFennecQL()`.
    public func toCall() throws -> (text: String, params: [Value]) {
        let bind = Binder()
        var text = "call \(name)"
        if let args { text += " " + bind.bind(args) }
        if let claims { text += " as " + bind.bind(claims) }
        return (text, bind.params)
    }
}

extension Fennec {
    /// Calls the named statement `name` in this database -- `call <name> $1`
    /// through the library, as any text is run: its arguments an object of
    /// values by name (a struct, a dictionary, a `Row`), nil for a statement
    /// of none, and `claims` the call's `as`: a signed-in user's claims, held
    /// to the types the statement declares as a token's are. Answers as
    /// `run` does -- the rows of a `get` or a `returning` body
    /// (`.rows`), the count of a write that answers none (`.affected`).
    /// A refusal is the `FennecError` the text would throw: `.notFound` for
    /// no such statement, `.query` for an argument missing, extra or of
    /// another type, `.unmet` with its `reason` for a `require` not met.
    ///
    /// A database that is a replica (`Fennec.sync`) runs no named
    /// statement -- it holds its shapes' rows, not its server's -- and
    /// refuses the call (`.query`): the server's `FennecRemote.call` runs it,
    /// and what it writes comes back through the shapes.
    ///
    ///     let moved = try await db.call("transfer", ["from": 1, "to": 2, "amount": 300])
    @discardableResult
    public func call(_ name: String, _ args: (any FennecValue)? = nil, claims: (any FennecValue)? = nil) async throws -> Answer {
        let (text, params) = try Call.call(name, args, claims: claims).toCall()
        return try await run(text, params: params)
    }
}

extension FennecRemote {
    /// Calls the named statement `name` on the server -- `call <name> $1`
    /// over `/query`, as `run` sends any text, so the transport, the errors
    /// and the options are `run`'s. `args` an object of values by name (a
    /// struct, a dictionary, a `Row`), nil for a statement of none.
    ///
    /// - `claims`: the call's `as`, which the server reads from its own
    ///   token alone -- a scoped token's claims are its token's, and it
    ///   may not say them.
    /// - `idempotencyKey`: a write runs once however often it is sent.
    /// - `wait`: seconds the server may hold a statement that writes
    ///   nothing, at most 30 (`withWait`).
    /// - `read`: the call only reads (`asRead()`): on a connection reading
    ///   near, a read replica answers it; one that writes is refused
    ///   (status 403), never landed. Without it a call goes home, as does
    ///   every text not known to read.
    ///
    /// Answers as `run` does; a refusal is a `FennecError` with its
    /// `status` -- 404 no such statement or not granted, 403 a claim
    /// missing, 400 an argument missing, extra or of another type, 409 a
    /// broken statement or a value taken, 412 (`.unmet`) a `require` not
    /// met, its `reason` the statement's.
    ///
    ///     let moved = try await db.call("transfer", ["from": 1, "to": 2, "amount": 300])
    ///     let mine = try await db.call("mine", claims: ["sub": "alice"], read: true)
    @discardableResult
    public func call(
        _ name: String, _ args: (any FennecValue)? = nil, claims: (any FennecValue)? = nil,
        idempotencyKey: String? = nil, wait: Double? = nil, read: Bool = false
    ) async throws -> Answer {
        let (text, params) = try Call.call(name, args, claims: claims).toCall()
        var db = self
        if let wait { db = db.withWait(wait) }
        if read { db = db.asRead() }
        return try await db.run(text, params: params, idempotencyKey: idempotencyKey)
    }
}
