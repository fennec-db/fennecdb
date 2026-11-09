// A scripted server on loopback for the retry tests (RetryTests.swift), in
// a file of its own, since it needs Foundation and a file importing both
// Foundation and Testing needs their overlay, which the Command Line Tools
// ship without. Plain sockets, not a URLProtocol: what is tested is what
// URLSession itself does at a reset or a refusal.
#if os(macOS)
    import Darwin
    import Foundation

    /// A server on loopback answering from a script, one act a request it
    /// reads: an answer over the kept connection; a reset, the connection
    /// closed with a RST once the request is read whole; or a hold, no
    /// answer until the client lets the connection go. Past the script's
    /// end it answers `[]`. Each request read is noted with the time it
    /// came and its head (`seen`). Made with `listening: false` it refuses
    /// every connection until `listen()`.
    final class Stub: @unchecked Sendable {
        enum Act: Sendable {
            case answer(Int, String, [String: String] = [:])
            case reset
            case hold
        }

        /// A request read: when (`systemUptime`), and its head, names in
        /// lower case.
        struct Seen: Sendable {
            let at: Double
            let head: [String: String]
        }

        private let lock = NSLock()
        private var plan: [Act]
        private var heads: [Seen] = []
        private var conns: Set<Int32> = []
        private var listening = false
        private var closed = false
        private var paused = 0
        /// The listening socket, once listening.
        private var fd: Int32 = -1
        /// Where a stub made with `listening: false` holds its port: the
        /// local end of a live connection to a listener of its own. A
        /// connect to the port is refused -- where to a socket bound but not
        /// listening macOS answers nothing, and the connect waits out its
        /// timeout -- until `listen` binds beside it (`SO_REUSEPORT`), the
        /// port never free in between for another to take.
        private var held: [Int32] = []
        private let accepted = DispatchSemaphore(value: 0)
        let port: Int
        var url: String { "http://127.0.0.1:\(port)" }

        init(_ plan: Act..., listening: Bool = true) throws {
            self.plan = plan
            if listening {
                let s = try Stub.bound(0)
                port = Stub.port(of: s)
                fd = s
                listen()
                return
            }
            let l = try Stub.bound(0)
            Darwin.listen(l, 1)
            let h = Stub.reusing()
            var to = Stub.loopback(UInt16(Stub.port(of: l)))
            let joined = withUnsafePointer(to: &to) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    Darwin.connect(h, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
                }
            }
            let peer = Darwin.accept(l, nil, nil)
            guard joined == 0, peer >= 0 else { throw POSIXError(.ECONNREFUSED) }
            held = [h, peer, l]
            port = Stub.port(of: h)
        }

        /// Seconds on a clock that only goes forward, as `seen` notes them.
        static var now: Double { ProcessInfo.processInfo.systemUptime }

        private static func loopback(_ port: UInt16) -> sockaddr_in {
            var a = sockaddr_in()
            a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
            a.sin_family = sa_family_t(AF_INET)
            a.sin_port = port.bigEndian
            a.sin_addr.s_addr = inet_addr("127.0.0.1")
            return a
        }

        /// A socket whose port another such socket may share.
        private static func reusing() -> Int32 {
            let s = socket(AF_INET, SOCK_STREAM, 0)
            var one: Int32 = 1
            setsockopt(s, SOL_SOCKET, SO_REUSEADDR, &one, socklen_t(MemoryLayout<Int32>.size))
            setsockopt(s, SOL_SOCKET, SO_REUSEPORT, &one, socklen_t(MemoryLayout<Int32>.size))
            return s
        }

        /// A socket bound to `port` on loopback, 0 for one the system picks.
        private static func bound(_ port: UInt16) throws -> Int32 {
            let s = reusing()
            var addr = loopback(port)
            let r = withUnsafePointer(to: &addr) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    bind(s, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
                }
            }
            guard s >= 0, r == 0 else { throw POSIXError(.EADDRNOTAVAIL) }
            return s
        }

        private static func port(of s: Int32) -> Int {
            var addr = sockaddr_in()
            var len = socklen_t(MemoryLayout<sockaddr_in>.size)
            _ = withUnsafeMutablePointer(to: &addr) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(s, $0, &len) }
            }
            return Int(UInt16(bigEndian: addr.sin_port))
        }

        /// Every request read so far.
        var seen: [Seen] {
            lock.lock()
            defer { lock.unlock() }
            return heads
        }

        /// The `Idempotency-Key` each request carried, nil for none.
        var keys: [String?] { seen.map { $0.head["idempotency-key"] } }

        /// How often the client's probe said it was about to wait.
        var pauses: Int {
            lock.lock()
            defer { lock.unlock() }
            return paused
        }

        /// The client's probe: about to wait before a request goes again --
        /// a refused connection's, where this starts listening.
        func pausing(_ wait: Double) {
            lock.lock()
            paused += 1
            lock.unlock()
            listen()
        }

        /// Takes connections from here on: until now each was refused.
        func listen() {
            lock.lock()
            if listening { return lock.unlock() }
            listening = true
            if fd < 0 { fd = (try? Stub.bound(UInt16(port))) ?? -1 }
            let s = fd
            lock.unlock()
            Darwin.listen(s, 16)
            Thread.detachNewThread { [self] in accepting(s) }
        }

        private func accepting(_ fd: Int32) {
            while true {
                let c = Darwin.accept(fd, nil, nil)
                lock.lock()
                let done = closed
                if c >= 0, !done { conns.insert(c) }
                lock.unlock()
                if c < 0 || done {
                    if c >= 0 { Darwin.close(c) }
                    accepted.signal()
                    return
                }
                var one: Int32 = 1
                setsockopt(c, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size))
                Thread.detachNewThread { [self] in serve(c) }
            }
        }

        private func next() -> Act {
            lock.lock()
            defer { lock.unlock() }
            return plan.isEmpty ? .answer(200, "[]") : plan.removeFirst()
        }

        private func serve(_ c: Int32) {
            var buf: [UInt8] = []
            defer {
                // Out of the set under the lock before it is closed: `close`
                // shuts down only what is in it, never a number reused.
                lock.lock()
                conns.remove(c)
                lock.unlock()
                Darwin.close(c)
            }
            while let head = Stub.request(c, &buf) {
                lock.lock()
                heads.append(Seen(at: ProcessInfo.processInfo.systemUptime, head: head))
                lock.unlock()
                switch next() {
                case .answer(let status, let body, let extra):
                    let bytes = Array(body.utf8)
                    var text = "HTTP/1.1 \(status) X\r\ncontent-type: application/json\r\ncontent-length: \(bytes.count)\r\n"
                    for (k, v) in extra { text += "\(k): \(v)\r\n" }
                    if !Stub.write(c, Array((text + "\r\n").utf8) + bytes) { return }
                case .reset:
                    var l = linger(l_onoff: 1, l_linger: 0)
                    setsockopt(c, SOL_SOCKET, SO_LINGER, &l, socklen_t(MemoryLayout<linger>.size))
                    return
                case .hold:
                    var chunk = [UInt8](repeating: 0, count: 4096)
                    while recv(c, &chunk, chunk.count, 0) > 0 {}
                    return
                }
            }
        }

        /// One request read whole -- its head and the body its length
        /// names -- its head as names in lower case and the request line
        /// under ""; nil at the connection's end.
        private static func request(_ c: Int32, _ buf: inout [UInt8]) -> [String: String]? {
            let end: [UInt8] = Array("\r\n\r\n".utf8)
            var chunk = [UInt8](repeating: 0, count: 65536)
            func find() -> Int? {
                guard buf.count >= 4 else { return nil }
                for i in 0...(buf.count - 4) where Array(buf[i..<i + 4]) == end { return i }
                return nil
            }
            var at = find()
            while at == nil {
                let n = recv(c, &chunk, chunk.count, 0)
                if n <= 0 { return nil }
                buf += chunk[0..<n]
                at = find()
            }
            let lines = String(decoding: buf[0..<at!], as: UTF8.self).components(separatedBy: "\r\n")
            var head = ["": lines.first ?? ""]
            for line in lines.dropFirst() {
                guard let colon = line.firstIndex(of: ":") else { continue }
                head[line[..<colon].lowercased()] = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
            }
            let length = Int(head["content-length"] ?? "0") ?? 0
            buf.removeFirst(at! + 4)
            while buf.count < length {
                let n = recv(c, &chunk, chunk.count, 0)
                if n <= 0 { return nil }
                buf += chunk[0..<n]
            }
            buf.removeFirst(length)
            return head
        }

        private static func write(_ c: Int32, _ bytes: [UInt8]) -> Bool {
            var off = 0
            while off < bytes.count {
                let n = bytes[off...].withUnsafeBytes { send(c, $0.baseAddress, $0.count, 0) }
                if n <= 0 { return false }
                off += n
            }
            return true
        }

        /// Lets every connection go and stops listening.
        func close() {
            lock.lock()
            closed = true
            let was = listening
            let listener = fd
            let holding = held
            self.held = []
            for c in conns { shutdown(c, SHUT_RDWR) }
            lock.unlock()
            if was {
                // A connection of its own wakes `accept`, which sees it is
                // closed and returns; the socket is closed only after, so its
                // number is never reused under a thread still reading it.
                let s = socket(AF_INET, SOCK_STREAM, 0)
                var addr = Stub.loopback(UInt16(port))
                _ = withUnsafePointer(to: &addr) {
                    $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                        Darwin.connect(s, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
                    }
                }
                accepted.wait()
                Darwin.close(s)
            }
            if listener >= 0 { Darwin.close(listener) }
            holding.forEach { Darwin.close($0) }
        }
    }
#endif
