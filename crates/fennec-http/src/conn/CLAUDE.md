# CLAUDE.md

fennec-http's connections -- the thread a connection gets or its 503, the HTTPS listener, a request read and an answer written in time, a body's ceiling and chunked bodies -- for code in `crates/fennec-http/src/conn.rs` and `crates/fennec-http/src/conn/`, which binds the accept loops (`accept.rs`), a body's framing (`http.rs`), a connection's loop (`lib.rs`), the router's clients and fennec-tls's reads (`read_by`) too; the rest of fennec-http's rules are in [its CLAUDE.md](../../CLAUDE.md), the rules every change meets in the root's [CLAUDE.md](../../../../CLAUDE.md).

## Invariants

### The HTTP server

**[A body is held to the ceiling its head
earns.](../../../../docs/design/http.md#a-bodys-ceiling)** `max_body`, judged
from the head before a byte of the body is read (`body_ceiling`,
`http::read_head`): a tenant node reads an image's import, `PUT
/_admin/tenants/<t>/file`, to `max_import` where it carries the admin token
alone (`admin::carries`), and refuses one carrying another or none with its
route's 401, waited out as every refusal is, its body unread. A route
reading past `max_body` is judged so there, from its head, never by a
ceiling raised for every request and refused after the read.

**[A chunked body grows as it comes, held to the
ceiling](../../../../docs/design/http.md#chunked-bodies)** (`read_onto`); a framing
read two ways is refused (`chunked_framing`, `end_refused`).

**[A connection no thread can be made for is answered
503.](../../../../docs/design/http.md#connections-without-a-thread)** Both accept loops
spawn through `accept::spawn`, which hands back a refused thread's stream
for `accept::refuse` to answer on the accepting thread, non-blocking,
counted by `reason` and marked `Fennec-Unserved`: unrun to the entry and
relays, as a standby's 409 marked `Fennec-Follows` is. Its writing then
shut, the one lingering thread (`accept::linger`) reads it off till the
client ends it; only no such thread, no room (`most_held`) or an answer not
sent whole closes it at once. A new accept loop calls
`accept::ready()` and `accept::widen` before its first accept. A TLS
listener's is told `fennec_tls::REFUSAL`, an alert (`accept::refuse_tls`):
no handshake runs on the accepting thread. A connection's place under
`--max-connections` is an `accept::Slot`, given back by its drop before
its socket closes, on every way out, a panic's unwinding too -- never by a
line after the connection's work, which a panic skips; so is every count
taken around a connection or a held request (`waits::Answering`, a
`waits::Ticket` leaving its group).

**[A request is read in time, and an answer written in
time.](../../../../docs/design/http.md#slow-clients)** A served connection's
requests, the router's too, are read through `conn::Paced`, told each
(`waiting`): the wait for a request's first byte is the idle timeout's;
from that byte -- over TLS its first record's first part -- the request,
head and body alike, has `--header-timeout` and a second more for every
`conn::MIN_RATE` bytes that came, else 408 and `end_refused`; a record in
parts is held to the same bound, its bytes earning as they come
(`fennec_tls::TlsStream::read_by`). The clock is read and a timeout set by
a read past a request's first alone: a request one read takes whole (the
`BufReader`'s 8 KiB), or in the buffer already, pays nothing; a bound past
what `Instant` can name is none, never a panic. `--write-timeout` is set
once a connection; a stream setting its own ends the connection after,
and a handler that sets the read timeout and serves on puts the idle
timeout back first (`node_stream`): `Paced` takes it to stand. A cut and a
write timed out are counted by their bound (`conn::Timeout`) in the
thread's shard, before a 408 is written. A new reader of a served
connection goes through `Paced`, never a bare `BufReader` over the `Conn`.
Over TLS every timeout and mode is set on the session's own socket
(`fennec_tls::TlsStream::socket`), never a `dup`'s: on Windows a
duplicate's timeouts are its own, and bound none of the session's reads.
Held on every platform by the handles' numbers, which a `dup` shared on
Linux and macOS cannot pass: `a_tls_connections_socket_is_its_sessions_own_handle`
(`Conn::socket` against the session's `get_ref`) and fennec-tls's
`what_is_set_through_the_socket_bounds_the_session` (`socket` against
`get_ref`, a dial's and an accept's).

**[`--tls-cert` and `--tls-key` make a listener HTTPS
alone](../../../../docs/design/http.md#tls)**, `--metrics` too, each handshake on its
connection's thread (`Tls::handshake`, `conn::Conn::open`), a plain request
a 400; without the flags the `&Conn` is the `TcpStream` it was, nothing
added. A failed handshake is counted by reason, logged a minute apart at
most; an end on purpose sends `close_notify` (`conn::end_all`), never after
a failed write or `Conn::cut`. The one shutdown waits for a handshake under
way as for a connection listed -- counted from before its first byte until
it is listed (`open::Shaking`), listed with its read side shut once the
shutdown has begun (`open::note`) -- and lets none begin after it: TLS 1.3
ends the client's handshake before the server's, and a session its client
had begun was cut bare. A timeout set where the peer may have reset, bytes
of its still to read, goes through `conn::unless_gone` (fennec-tls's own in
a handshake): macOS's EINVAL there never fails a handshake done.
