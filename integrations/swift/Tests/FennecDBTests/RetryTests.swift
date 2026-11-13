// What a connection sends again, and when it gives a request up: only what
// cannot have run -- an answer naming its wait, a connection refused -- or
// may run twice -- a read the builder made or sent near, a write under a
// key -- never after the connection's own timeout
// (docs/design/js-client.md#retries). Against a scripted server on
// loopback (Stub.swift), no fennec-server needed. macOS alone, as the
// stub's sockets are Darwin's.
#if os(macOS)
    import Testing

    @testable import FennecDB

    private let wrote = #"{"affected":1}"#

    /// What `body` threw, nil where it threw nothing.
    private func failure(_ body: () async throws -> some Any) async -> Error? {
        do {
            _ = try await body()
            return nil
        } catch {
            return error
        }
    }

    @Suite struct Retries {
        /// A 503 naming its wait ran nothing: any request goes again, a
        /// write without a key too, after that wait and never sooner --
        /// `retry_after_ms` before `Retry-After`.
        @Test func a503NamingItsWaitIsSentAgainNoSoonerThanTheWait() async throws {
            let s = try Stub(
                .answer(
                    503, #"{"error":"the tenant is being moved; retry shortly","retry_after":1,"retry_after_ms":300}"#,
                    ["retry-after": "1"]),
                .answer(200, wrote))
            defer { s.close() }
            #expect(try await Fennec.connect(url: s.url).execute("put notes {n: 1}") == 1)
            let seen = s.seen
            #expect(seen.count == 2)
            #expect(seen[1].at - seen[0].at >= 0.3)
        }

        /// A 429 that names its wait only in `Retry-After` is sent again
        /// after that wait.
        @Test func a429NamingItsWaitOnlyInTheHeaderIsSentAgainAfterIt() async throws {
            let s = try Stub(.answer(429, #"{"error":"slow down"}"#, ["retry-after": "1"]), .answer(200, wrote))
            defer { s.close() }
            #expect(try await Fennec.connect(url: s.url).execute("put notes {n: 1}") == 1)
            let seen = s.seen
            #expect(seen.count == 2)
            #expect(seen[1].at - seen[0].at >= 1.0)
        }

        /// Cancelled in the pause before a retry, a request ends there and
        /// nothing goes again.
        @Test func aCancelInThePauseBeforeARetryEndsTheRequest() async throws {
            let s = try Stub(.answer(503, #"{"error":"busy","retry_after_ms":5000}"#), .answer(200, "[]"))
            defer { s.close() }
            let db = Fennec.connect(url: s.url)
            let (paused, signal) = AsyncStream<Void>.makeStream()
            db.pausing = { _ in signal.yield() }
            let task = Task { try await db.from("docs").rows() }
            for await _ in paused { break }
            task.cancel()
            let e = await failure { try await task.value }
            #expect(e is CancellationError)
            #expect(s.seen.count == 1)
        }

        /// A connection refused sent nothing: any request goes again, a
        /// write without a key too.
        @Test func aConnectionRefusedIsTriedAgainWhateverTheRequest() async throws {
            let s = try Stub(.answer(200, wrote), listening: false)
            defer { s.close() }
            let db = Fennec.connect(url: s.url)
            db.pausing = { s.pausing($0) }
            #expect(try await db.execute("put notes {n: 1}") == 1)
            #expect(s.pauses == 1)
            #expect(s.seen.count == 1)
        }

        /// A write without a key may have landed before a reset: it is
        /// never sent again -- by the client, nor by URLSession over the
        /// connection it kept -- and the client makes no key. Under a key
        /// it goes again, answered from the key.
        @Test func aWriteIsSentAgainAfterAResetOnlyUnderAKey() async throws {
            let s = try Stub(.answer(200, wrote), .reset, .answer(200, wrote))
            defer { s.close() }
            let db = Fennec.connect(url: s.url)
            #expect(try await db.execute("put notes {n: 0}") == 1)
            let e = await failure { try await db.execute("put notes {n: 1}") }
            #expect((e as? FennecError)?.code == .io)
            #expect(s.seen.count == 2, "a write that may have landed is never made twice")
            #expect(s.keys == [nil, nil], "the client makes no key of its own")

            let k = try Stub(.reset, .answer(200, wrote, ["idempotent-replayed": "true"]))
            defer { k.close() }
            let keyed = Fennec.connect(url: k.url).withIdempotencyKey("n-1")
            #expect(try await keyed.from("notes").insert(["n": 1] as Value) == 1)
            #expect(k.keys == ["n-1", "n-1"])
        }

        /// A read the builder made goes again after a reset; a text the
        /// caller says reads (`asRead()`), sent home, is a claim nothing
        /// checks, and goes once. Sent near it is marked
        /// (`Fennec-Read: replica`), which every server runs read-only:
        /// again.
        @Test func aBuilderReadIsSentAgainAfterAResetAndATextSentHomeIsNot() async throws {
            let s = try Stub(.reset, .answer(200, #"[{"id":1}]"#), .reset, .answer(200, #"[{"id":1}]"#))
            defer { s.close() }
            let db = Fennec.connect(url: s.url)
            #expect(try await db.from("docs").rows() == [["id": 1]])
            #expect(s.seen.count == 2)
            #expect(await failure { try await db.asRead().query("get docs") } != nil)
            #expect(s.seen.count == 3)

            let near = try Stub(.reset, .answer(200, #"[{"id":2}]"#))
            defer { near.close() }
            let reader = Fennec.connect(url: near.url, reads: .nearest).asRead()
            #expect(try await reader.query("get docs") == [["id": 2]])
            #expect(near.seen.map { $0.head["fennec-read"] } == ["replica", "replica"])
        }

        /// A batch stopped after some of its statements landed (`completed`
        /// above 0) is never sent again, whatever wait it names: sent again
        /// whole, they were made twice.
        @Test func aStopAfterStatementsLandedIsNeverSentAgain() async throws {
            let s = try Stub(
                .answer(503, #"{"error":"moved","completed":1,"at":1,"retry_after_ms":50}"#, ["retry-after": "1"]),
                .answer(200, #"{"results":[]}"#))
            defer { s.close() }
            let e = await failure {
                try await Fennec.connect(url: s.url).batch([("put notes {n: 1}", []), ("compact", [])])
            }
            #expect((e as? FennecError)?.status == 503 && (e as? FennecError)?.completed == 1)
            #expect(s.seen.count == 1)
        }

        /// The timeout bounds the wait for the head; past it nothing goes
        /// again, a read the builder made neither: the server may still be
        /// running it.
        @Test func nothingGoesAgainAfterTheConnectionsOwnTimeout() async throws {
            let s = try Stub(.hold, .answer(200, "[]"))
            defer { s.close() }
            let db = Fennec.connect(url: s.url, timeout: 0.5)
            let started = Stub.now
            let e = await failure { try await db.from("docs").rows() }
            #expect((e as? FennecError)?.message.contains("timeout") == true)
            #expect(Stub.now - started >= 0.5)
            #expect(s.seen.count == 1)
        }

        /// Three tries unless told, the last answer the caller's; `retry:
        /// nil` sends nothing again.
        @Test func theTriesAreBoundedAndRetryNilSendsNothingAgain() async throws {
            let busy = Stub.Act.answer(503, #"{"error":"too many connections","retry_after":1,"retry_after_ms":5}"#)
            let s = try Stub(busy, busy, busy, .answer(200, "[]"))
            defer { s.close() }
            let e = await failure { try await Fennec.connect(url: s.url).from("docs").rows() }
            #expect((e as? FennecError)?.status == 503 && (e as? FennecError)?.retryAfter == 0.005)
            #expect(s.seen.count == 3)

            let four = try Stub(busy, busy, busy, .answer(200, #"[{"id":1}]"#))
            defer { four.close() }
            let more = Fennec.connect(url: four.url, retry: FennecRemote.Retry(attempts: 4))
            #expect(try await more.from("docs").rows() == [["id": 1]])
            #expect(four.seen.count == 4)

            let off = try Stub(busy, .answer(200, "[]"))
            defer { off.close() }
            let once = await failure { try await Fennec.connect(url: off.url, retry: nil).from("docs").rows() }
            #expect((once as? FennecError)?.status == 503)
            #expect(off.seen.count == 1)
        }

        /// The code the server named is the error's `errorCode` -- never
        /// `code`, the library's kind -- and a refusal naming none has
        /// none; neither is sent again.
        @Test func aRefusalNamesTheServersCode() async throws {
            let s = try Stub(
                .answer(401, #"{"error":"the token has expired","code":"token_expired"}"#),
                .answer(401, #"{"error":"invalid or missing token"}"#))
            defer { s.close() }
            let db = Fennec.connect(url: s.url, token: "t")
            let e = await failure { try await db.from("docs").rows() } as? FennecError
            #expect(e?.status == 401 && e?.code == .denied && e?.errorCode == "token_expired")
            let none = await failure { try await db.from("docs").rows() } as? FennecError
            #expect(none?.status == 401 && none?.errorCode == nil)
            #expect(s.seen.count == 2)
        }
    }
#endif
