// `call`, a named statement's call (`create statement ...`): the builder
// (`Call.call(...).toCall()`) beyond what builder-golden.json holds -- what
// Swift types, and immutability --, the embedded database's `call` and the
// HTTP client's, against a real fennec-server (macOS alone, as the sync
// tests: the iOS simulator's tests cannot start a process).
import Testing

@testable import FennecDB

struct Transfer: Codable, Equatable, FennecValue {
    var from: Int
    var to: Int
    var amount: Int
}

struct Who: Codable, Equatable, FennecValue {
    var sub: String
}

/// The refusal a call ran into, or nil.
func refusal(_ body: () async throws -> Answer) async -> FennecError? {
    do {
        _ = try await body()
        return nil
    } catch let e as FennecError {
        return e
    } catch {
        return FennecError(code: .panic, message: "\(error)")
    }
}

/// A transfer between two accounts, a `require` for each way it can fail, a
/// statement that reads the caller's claims and one that writes none.
let transferStatement =
    "create statement transfer($from int, $to int, $amount int) public as begin "
    + "set accounts {balance: balance - $amount} where id = $from and balance >= $amount require 1 else 'insufficient funds'; "
    + "set accounts {balance: balance + $amount} where id = $to require 1 else 'no such account'; "
    + "get accounts select id, balance where id = $from end"

/// Every kind of argument a statement takes, written to a row and answered.
let kindStatement =
    "create statement kind($s text, $i int, $f float, $b bool, $at timestamp, $meta json, $v vector<3>) public as "
    + "insert kinds {s: $s, i: $i, f: $f, b: $b, at: $at, meta: $meta, v: $v} returning id, s, i, f, b, meta, v"

let kindArguments: Value = [
    "s": "çağ \"quoted\"", "i": -42, "f": 2.5, "b": true, "at": date("2026-09-19T12:34:56.789Z"),
    "meta": ["lang": "tr", "rank": 2], "v": .floats([0.5, -0.25, 1]),
]

@Suite struct CallBuilder {
    /// What Swift types as arguments: a `Codable` struct in its fields'
    /// order, a dictionary with its keys sorted, a `Row` or `Value` in the
    /// order written, an optional; nil, an absent argument and `null` are
    /// none, and an empty object is still one.
    @Test func argumentsAreAnObjectOfValuesByName() throws {
        let want: Value = ["from": 1, "to": 2, "amount": 300]
        let typed = try Call.call("transfer", Transfer(from: 1, to: 2, amount: 300)).toCall()
        #expect(typed.text == "call transfer $1" && typed.params == [want])
        #expect(try Call.call("transfer", want).toCall().params == [want])
        #expect(try Call.call("transfer", Row([("from", 1), ("to", 2), ("amount", 300)])).toCall().params == [want])
        let dictionary = try Call.call("transfer", ["to": 2, "from": 1, "amount": 300]).toCall()
        #expect(dictionary.params.first?.json == #"{"amount":300,"from":1,"to":2}"#)

        for none in [try Call.call("stamp"), try Call.call("stamp", nil), try Call.call("stamp", Value.null),
                     try Call.call("stamp", Optional<Transfer>.none)] {
            let made = try none.toCall()
            #expect(made.text == "call stamp" && made.params.isEmpty)
        }
        let empty = try Call.call("stamp", Value.object(Row())).toCall()
        #expect(empty.text == "call stamp $1" && empty.params == [.object(Row())])
    }

    @Test func claimsFollowTheArgumentsAndAreAParameterToo() throws {
        let both = try Call.call("mine", ["limit": 10], claims: Who(sub: "alice")).toCall()
        #expect(both.text == "call mine $1 as $2")
        #expect(both.params == [["limit": 10], ["sub": "alice"]])
        let alone = try Call.call("mine", claims: ["sub": "x' or 1=1 --"]).toCall()
        #expect(alone.text == "call mine as $1" && alone.params == [["sub": "x' or 1=1 --"]])
        let none = try Call.call("mine", ["limit": 10], claims: Value.null).toCall()
        #expect(none.text == "call mine $1")
    }

    /// A value is never the text: a `Date` is its ISO text, a vector its
    /// numbers, a nested object a json field's value.
    @Test func valuesAreParametersNormalisedAsADocumentsAre() throws {
        let made = try Call.call("kind", kindArguments).toCall()
        #expect(made.text == "call kind $1")
        #expect(made.params.count == 1)
        #expect(made.params[0]["at"] == "2026-09-19T12:34:56.789Z")
        #expect(made.params[0]["v"]?.floats == [0.5, -0.25, 1])
        #expect(made.params[0]["meta"] == ["lang": "tr", "rank": 2])
        #expect(try Call.call("find", ["title": "\"; del docs; --"]).toCall().text == "call find $1")
    }

    @Test func aNameIsCheckedBecauseItIsTheTextOfTheCall() throws {
        for ok in ["a", "Close_day_2", "x" + String(repeating: "y", count: 58)] {
            #expect(try Call.call(ok).toCall().text == "call \(ok)")
        }
        for bad in ["", "1st", "_a", "çağ", "a b", "a-b", "a.b", "a\n", "stamp $1; del docs", "é", "x" + String(repeating: "y", count: 59)] {
            do {
                _ = try Call.call(bad)
                Issue.record("a name \(bad.debugDescription) was taken")
            } catch let e as FennecError {
                #expect(e.code == .builder && e.message == "invalid statement name: \(Builder.quote(bad))")
            }
        }
    }

    @Test func argumentsAndClaimsThatAreNotAnObjectAreRefused() throws {
        let arguments = "a call's arguments are an object of values by name"
        let claims = "a call's claims are an object of values by name"
        let bads: [any FennecValue] = [[1, 2, 300] as Value, "from 1", 7, true, [1, 2] as [Int], 2.5]
        for bad in bads {
            #expect(throws: FennecError.builder(arguments)) { try Call.call("transfer", bad) }
            #expect(throws: FennecError.builder(claims)) { try Call.call("mine", claims: bad) }
        }
        // The name first, then the arguments, then the claims.
        #expect(throws: FennecError.builder("invalid statement name: \"1\"")) { try Call.call("1", "x", claims: "y") }
        #expect(throws: FennecError.builder(arguments)) { try Call.call("a", "x", claims: "y") }
        // `inc` and `expr` read the row a write changes: a call has none.
        let computed = "this object cannot be used as a fennecdb value (argument)"
        #expect(throws: FennecError.builder(computed)) { try Call.call("a", ["n": .inc(1)] as Value) }
        #expect(throws: FennecError.builder(computed)) { try Call.call("a", ["m": ["n": .expr("now()")]] as Value) }
        #expect(throws: FennecError.builder("this object cannot be used as a fennecdb value (claim)")) {
            try Call.call("a", claims: ["n": .inc(1)] as Value)
        }
    }

    /// A call is a value: made once, it renders the same text each time and
    /// is not changed by another made from the same arguments.
    @Test func aCallIsImmutable() throws {
        let base = try Call.call("mine", ["limit": 10])
        let first = try base.toCall()
        let other = try Call.call("mine", ["limit": 11], claims: ["sub": "bob"])
        #expect(try base.toCall().text == first.text && base.toCall().params == first.params)
        #expect(try other.toCall().text == "call mine $1 as $2")
        #expect(base.name == "mine")
    }
}

@Suite struct EmbeddedCall {
    private static func bank() async throws -> Fennec {
        let db = try Fennec.memory()
        try await db.execute("create collection accounts (owner text @hash, balance int)")
        try await db.execute(#"put accounts [{owner: "a", balance: 1000}, {owner: "b", balance: 5}]"#)
        try await db.execute(transferStatement)
        return db
    }

    private static func balances(_ db: Fennec) async throws -> [Int?] {
        try await db.from("accounts").select("balance").order("id").rows().map { $0["balance"]?.int }
    }

    /// `call <name> $1` through the library, as `run` sends it: the answer
    /// of the body's last statement, and a refusal with its words.
    @Test func aCallRunsAStatementInTheDatabase() async throws {
        let db = try await EmbeddedCall.bank()
        let moved = try await db.call("transfer", ["from": 1, "to": 2, "amount": 300])
        #expect(moved.rows == [["id": 1, "balance": 700]])
        // A struct is the arguments too.
        let again = try await db.call("transfer", Transfer(from: 1, to: 2, amount: 100))
        #expect(again.rows.first?["balance"] == 600)
        #expect(try await EmbeddedCall.balances(db) == [600, 405])

        // A `require` not met says why, and the whole call is put back.
        let broke = await refusal { try await db.call("transfer", Transfer(from: 2, to: 1, amount: 9999)) }
        #expect(broke?.code == .unmet && broke?.reason == "insufficient funds")
        let nobody = await refusal { try await db.call("transfer", Transfer(from: 1, to: 99, amount: 50)) }
        #expect(nobody?.code == .unmet && nobody?.reason == "no such account")
        #expect(try await EmbeddedCall.balances(db) == [600, 405])

        let missing = await refusal { try await db.call("nosuch") }
        #expect(missing?.code == .notFound)
        let lacking = await refusal { try await db.call("transfer", ["from": 1]) }
        #expect(lacking?.code == .query && lacking?.message.contains("`to`") == true)
        let typed = await refusal { try await db.call("transfer", ["from": 1, "to": 2, "amount": "much"] as Value) }
        #expect(typed?.code == .query && typed?.message.contains("`amount`") == true)
        // A builder refusal throws before anything runs.
        let named = await refusal { try await db.call("transfer; del accounts") }
        #expect(named?.code == .builder)
        try await db.close()
    }

    @Test func everyKindOfArgumentIsRead() async throws {
        let db = try Fennec.memory()
        try await db.execute("create collection kinds (s text, i int, f float, b bool, at timestamp, meta json, v vector<3>)")
        try await db.execute(kindStatement)
        let row = try #require(try await db.call("kind", kindArguments).rows.first)
        #expect(row["s"] == "çağ \"quoted\"" && row["i"] == -42 && row["f"] == 2.5 && row["b"] == true)
        #expect(row["meta"] == ["lang": "tr", "rank": 2])
        #expect(row["v"]?.floats == [0.5, -0.25, 1])
        let stored = try #require(try await db.from("kinds").first())
        #expect(stored["at"]?.string == "2026-09-19T12:34:56.789Z" || stored["at"]?.int == 1_789_821_296_789)
        try await db.close()
    }

    /// A statement with no arguments, one that writes and answers nothing,
    /// one that reads, and the claims a statement declares: the embedder's,
    /// as a signed-in user's, held to the declared types.
    @Test func noArgumentsAWriteThatAnswersNothingAndClaims() async throws {
        let db = try Fennec.memory()
        try await db.execute("create collection notes (body text @hash, by text)")
        try await db.execute("create statement stamp() public as insert notes {body: 'z'} returning id")
        try await db.execute("create statement wipe() public as del notes where body = 'z'")
        try await db.execute("create statement mine() claims (sub text) as get notes select body where by = $jwt.sub")
        try await db.execute("create statement says($body text) claims (sub text) as insert notes {body: $body, by: $jwt.sub} returning id")

        #expect(try await db.call("stamp").rows == [["id": 1]])
        #expect(try await db.call("stamp", Value.object(Row())).rows == [["id": 2]])
        #expect(try await db.call("wipe").affected == 2)

        let denied = await refusal { try await db.call("mine") }
        #expect(denied?.code == .denied)
        let wrong = await refusal { try await db.call("mine", claims: ["sub": 7]) }
        #expect(wrong != nil)
        #expect(try await db.call("mine", claims: Who(sub: "alice")).rows.isEmpty)
        #expect(try await db.call("says", ["body": "hi"], claims: Who(sub: "alice")).rows == [["id": 3]])
        #expect(try await db.call("mine", claims: ["sub": "alice"]).rows == [["body": "hi"]])
        #expect(try await db.call("mine", claims: ["sub": "bob"]).rows.isEmpty)
        try await db.close()
    }
}

#if os(macOS)
    @Suite(.serialized) struct RemoteCall {
        private static func bank(_ s: Server) async throws -> FennecRemote {
            let db = Fennec.connect(url: s.url)
            try await db.execute("create collection accounts (owner text @hash, balance int)")
            try await db.execute(#"put accounts [{owner: "a", balance: 1000}, {owner: "b", balance: 5}]"#)
            try await db.execute("create collection notes (body text @hash, by text)")
            try await db.execute(transferStatement)
            try await db.execute("create statement stamp() public as insert notes {body: 'z'} returning id")
            try await db.execute("create statement mine() claims (sub text) as get notes select body where by = $jwt.sub")
            return db
        }

        /// `call <name> $1` over `/query`: the answer is `run`'s, a
        /// refusal the `FennecError` of its status, with its `reason`.
        @Test func aCallRunsAStatementOnTheServer() async throws {
            let s = try await Server("call")
            defer { s.stop() }
            let db = try await RemoteCall.bank(s)
            let moved = try await db.call("transfer", ["from": 1, "to": 2, "amount": 300])
            #expect(moved.rows == [["id": 1, "balance": 700]])
            #expect(try await db.call("transfer", Transfer(from: 1, to: 2, amount: 100)).rows.first?["balance"] == 600)
            #expect(db.seq != nil)

            let broke = await refusal { try await db.call("transfer", Transfer(from: 2, to: 1, amount: 9999)) }
            #expect(broke?.status == 412 && broke?.code == .unmet && broke?.reason == "insufficient funds")
            let nobody = await refusal { try await db.call("transfer", Transfer(from: 1, to: 99, amount: 1)) }
            #expect(nobody?.status == 412 && nobody?.reason == "no such account")
            #expect(try await db.from("accounts").select("balance").order("id").rows().map { $0["balance"]?.int } == [600, 405])

            let missing = await refusal { try await db.call("nosuch") }
            #expect(missing?.status == 404 && missing?.code == .notFound)
            let lacking = await refusal { try await db.call("transfer", ["from": 1]) }
            #expect(lacking?.status == 400 && lacking?.message.contains("`to`") == true)
            let typed = await refusal { try await db.call("transfer", ["from": 1, "to": 2, "amount": "much"] as Value) }
            #expect(typed?.status == 400 && typed?.message.contains("`amount`") == true)
            let extra = await refusal { try await db.call("transfer", ["from": 1, "to": 2, "amount": 1, "tip": 1]) }
            #expect(extra?.status == 400)
            // The builder's refusals come before any request: no statement.
            let named = await refusal { try await db.call("transfer; del accounts") }
            #expect(named?.code == .builder && named?.status == nil)
            let listed = await refusal { try await db.call("transfer", [1, 2, 300] as Value) }
            #expect(listed?.code == .builder)
        }

        @Test func everyKindOfArgumentIsRead() async throws {
            let s = try await Server("callkinds")
            defer { s.stop() }
            let db = Fennec.connect(url: s.url)
            try await db.execute("create collection kinds (s text, i int, f float, b bool, at timestamp, meta json, v vector<3>)")
            try await db.execute(kindStatement)
            let row = try #require(try await db.call("kind", kindArguments).rows.first)
            #expect(row["s"] == "çağ \"quoted\"" && row["i"] == -42 && row["f"] == 2.5 && row["b"] == true)
            #expect(row["meta"] == ["lang": "tr", "rank": 2])
            #expect(row["v"]?.floats == [0.5, -0.25, 1])
        }

        /// A key makes a call once: sent again it is answered as the first
        /// time was, and nothing is written twice; with another call, 422.
        @Test func anIdempotencyKeyMakesACallOnce() async throws {
            let s = try await Server("callkey")
            defer { s.stop() }
            let db = try await RemoteCall.bank(s)
            let first = try await db.call("stamp", idempotencyKey: "stamp-1")
            let again = try await db.call("stamp", idempotencyKey: "stamp-1")
            #expect(first.rows == [["id": 1]] && again == first)
            #expect(try await db.from("notes").count() == 1)
            // A copy's key is the call's; the call's own wins.
            #expect(try await db.withIdempotencyKey("stamp-2").call("stamp").rows == [["id": 2]])
            #expect(try await db.withIdempotencyKey("stamp-2").call("stamp").rows == [["id": 2]])
            #expect(try await db.from("notes").count() == 2)
            let other = await refusal { try await db.call("transfer", Transfer(from: 1, to: 2, amount: 1), idempotencyKey: "stamp-1") }
            #expect(other?.status == 422)
            #expect(try await db.from("notes").count() == 2)
        }

        /// `claims` is the call's `as`, which the server's own token may
        /// say; without them a statement declaring some refuses (403).
        @Test func claimsAreTheCallsAs() async throws {
            let s = try await Server("callclaims")
            defer { s.stop() }
            let db = try await RemoteCall.bank(s)
            try await db.execute("create statement says($body text) claims (sub text) as insert notes {body: $body, by: $jwt.sub} returning id")
            let denied = await refusal { try await db.call("mine") }
            #expect(denied?.status == 403 && denied?.code == .denied)
            #expect(try await db.call("mine", claims: Who(sub: "alice")).rows.isEmpty)
            #expect(try await db.call("says", ["body": "hi"], claims: Who(sub: "alice")).rows == [["id": 1]])
            #expect(try await db.call("says", ["body": "yo"], claims: ["sub": "bob"]).rows == [["id": 2]])
            #expect(try await db.call("mine", claims: ["sub": "alice"]).rows == [["body": "hi"]])
            // A claim of another type than the statement declares is refused.
            let wrong = await refusal { try await db.call("mine", claims: ["sub": 7]) }
            #expect(wrong != nil && wrong?.status != nil)
        }

        /// `read` marks the call as it marks any text the caller knows
        /// reads: on a connection reading near, a read replica answers it;
        /// one that writes is refused (403), never landed. Without it the
        /// call goes home, unmarked, as every text not known to read does.
        @Test func readMarksACallThatOnlyReads() async throws {
            let s = try await Server("callread")
            defer { s.stop() }
            _ = try await RemoteCall.bank(s)
            let sent = Wire.Log(script: nil)
            let db = connection(s.url, reads: .nearest, log: sent)
            #expect(try await db.call("mine", claims: ["sub": "alice"], read: true).rows.isEmpty)
            #expect(sent.last["fennec-read"] == "replica")
            #expect(try await db.call("mine", claims: ["sub": "alice"]).rows.isEmpty)
            #expect(sent.last["fennec-read"] == nil)

            let written = await refusal { try await db.call("stamp", read: true) }
            #expect(written?.status == 403)
            #expect(sent.last["fennec-read"] == "replica")
            #expect(try await db.from("notes").count() == 0)
            #expect(try await db.call("stamp").rows == [["id": 1]])
            // A connection reading home marks nothing, `read` or not.
            let home = connection(s.url, reads: .home, log: sent)
            #expect(try await home.call("mine", claims: ["sub": "alice"], read: true).rows.isEmpty)
            #expect(sent.last["fennec-read"] == nil)
        }

        /// `wait` is `withWait`'s: the header goes, the request is given
        /// that long more, and the server holds no call whose body inserts;
        /// one that sets and writes nothing is held until cancelled.
        @Test func waitAndCancellationApplyAsForAText() async throws {
            let s = try await Server("callwait")
            defer { s.stop() }
            let db = try await RemoteCall.bank(s)
            try await db.execute("create statement bump($id int) public as set accounts {balance: balance + 1} where id = $id")
            let sent = Wire.Log(script: nil)
            let wired = connection(s.url, reads: .home, log: sent)

            #expect(try await wired.call("mine", claims: ["sub": "alice"], wait: 2).rows.isEmpty)
            #expect(sent.last["fennec-wait"] == "2000")
            let inserted = await refusal { try await wired.call("stamp", wait: 1) }
            #expect(inserted?.status == 400 && inserted?.message.contains("Fennec-Wait") == true)
            #expect(try await db.from("notes").count() == 0)

            // Nothing to set: held for the wait, then answered with the
            // count it wrote.
            #expect(try await db.call("bump", ["id": 99], wait: 0.3).affected == 0)
            #expect(try await db.call("bump", ["id": 1], wait: 5).affected == 1)
            let held = Task { try await db.call("bump", ["id": 99], wait: 10) }
            try await Task.sleep(nanoseconds: 200_000_000)
            held.cancel()
            do {
                _ = try await held.value
                Issue.record("a cancelled call was answered")
            } catch {}
        }
    }
#endif
