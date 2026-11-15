"""What a client sends again, and when it gives a request up: only what
cannot have run -- an answer naming its wait, a connection refused -- or
may run twice -- a read the builder made or sent near, a write under a key
-- never after the client's own timeout, which bounds the wait for an
answer's head and between the pieces of its body
(docs/design/js-client.md#retries). Against a scripted server on loopback,
no fennec-server needed."""

import asyncio
import errno
import json
import socket
import threading
import time

import pytest

import fennecdb
from fennecdb import AsyncClient, Client, FennecError, Retry


class Server:
    """A server on loopback answering from `plan`, one item a request it
    reads: `(status, body, headers)`, answered over the kept connection;
    "drop", the connection closed with no answer; "hang", no answer until
    the client lets the connection go; "trickle", a head and then a body a
    byte every 20 ms; "stall", a head and five bytes of its body, then
    nothing until the client lets the connection go. Each request read is
    noted with the time it came and its head (`seen`)."""

    def __init__(self, *plan):
        self.plan = list(plan)
        self.seen = []
        self.srv = socket.socket()
        self.srv.bind(("127.0.0.1", 0))
        self.srv.listen()
        self.url = f"http://127.0.0.1:{self.srv.getsockname()[1]}"
        threading.Thread(target=self._accept, daemon=True).start()

    def _accept(self):
        while True:
            try:
                conn, _ = self.srv.accept()
            except OSError:
                return
            threading.Thread(target=self._serve, args=(conn,), daemon=True).start()

    def _serve(self, conn):
        conn.settimeout(10)
        with conn:
            while True:
                req = _read_request(conn)
                if req is None or not self.plan:
                    return
                self.seen.append((time.monotonic(), req.split(b"\r\n\r\n")[0].decode()))
                what = self.plan.pop(0)
                if what == "drop":
                    return
                if what == "hang":
                    try:
                        while conn.recv(65536):
                            pass
                    except OSError:
                        pass
                    return
                if what == "trickle":
                    body = b'{"affected":1}' + b" " * 36
                    conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: %d\r\n\r\n" % len(body))
                    try:
                        for b in body:
                            time.sleep(0.02)
                            conn.sendall(bytes([b]))
                    except OSError:
                        pass
                    continue
                if what == "stall":
                    conn.sendall(b'HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\n{"aff')
                    try:
                        while conn.recv(65536):
                            pass
                    except OSError:
                        pass
                    return
                status, body, headers = what
                raw = json.dumps(body).encode()
                head = f"HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {len(raw)}\r\n"
                for k, v in headers.items():
                    head += f"{k}: {v}\r\n"
                conn.sendall(head.encode() + b"\r\n" + raw)

    def keys(self):
        """The `Idempotency-Key` each request carried, None for none."""
        out = []
        for _, head in self.seen:
            key = [line.split(":", 1)[1].strip() for line in head.split("\r\n") if line.lower().startswith("idempotency-key:")]
            out.append(key[0] if key else None)
        return out

    def close(self):
        self.srv.close()


def _read_request(conn):
    data = b""
    try:
        while b"\r\n\r\n" not in data:
            chunk = conn.recv(65536)
            if not chunk:
                return None
            data += chunk
        head, _, rest = data.partition(b"\r\n\r\n")
        length = 0
        for line in head.split(b"\r\n"):
            if line.lower().startswith(b"content-length:"):
                length = int(line.split(b":")[1])
        while len(rest) < length:
            rest += conn.recv(65536)
    except OSError:
        return None
    return head + b"\r\n\r\n" + rest


MOVED = (503, {"error": "the tenant is being moved; retry shortly", "retry_after": 1}, {"Retry-After": "1"})
OK = (200, {"affected": 1}, {})


def test_a_503_naming_its_wait_is_sent_again_no_sooner_than_the_wait():
    srv = Server(MOVED, OK)
    # A write without a key: nothing ran, so it goes again all the same.
    assert Client(srv.url).query("put notes {n: 1}") == {"affected": 1}
    assert len(srv.seen) == 2
    assert srv.seen[1][0] - srv.seen[0][0] >= 1.0
    # A quota's 429 says it to the millisecond, before its whole seconds.
    quota = Server((429, {"error": "past", "quota": "writes", "retry_after": 1, "retry_after_ms": 300}, {"Retry-After": "1"}), OK)
    Client(quota.url).query("put notes {n: 2}")
    assert len(quota.seen) == 2
    assert quota.seen[1][0] - quota.seen[0][0] >= 0.3


def test_a_refusal_naming_no_wait_is_never_sent_again():
    for refusal in [
        (429, {"error": "past its quota of 4 held requests", "quota": "held"}, {}),
        (507, {"error": "past its quota of 5 rows", "quota": "rows"}, {}),
        (409, {"error": "a value taken"}, {}),
        (500, {"error": "the write landed", "outcome": "unknown", "seq": 7}, {}),
        (503, {"error": "no wait named"}, {}),
        # Statements of a batch made: sent again whole, they were made twice,
        # whatever the stop says -- the server answers no such stop so.
        (503, {"error": "moved", "completed": 1, "at": 1, "retry_after": 1}, {"Retry-After": "1"}),
        (429, {"error": "too many runs", "completed": 2, "at": 2, "retry_after": 1}, {"Retry-After": "1"}),
    ]:
        srv = Server(refusal, (200, [], {}))
        with pytest.raises(FennecError) as e:
            Client(srv.url).collection("docs").rows()
        assert e.value.status == refusal[0]
        assert len(srv.seen) == 1, refusal


def test_a_write_is_sent_again_after_a_dropped_connection_only_under_a_key():
    srv = Server("drop", OK)
    with pytest.raises(OSError):
        Client(srv.url).query("put notes {n: 1}")
    assert len(srv.seen) == 1, "a write that may have landed is never made twice"
    assert srv.keys() == [None], "the client makes no key of its own"

    keyed = Server("drop", (200, {"affected": 1}, {"Idempotent-Replayed": "true"}))
    c = Client(keyed.url)
    assert c.query("put notes {n: 1}", idempotency_key="n-1") == {"affected": 1}
    assert c.replayed
    assert keyed.keys() == ["n-1", "n-1"]
    copy = Server("drop", OK)
    Client(copy.url).with_idempotency_key("n-2").collection("notes").insert({"n": 2})
    assert copy.keys() == ["n-2", "n-2"]

    # A read the builder made goes again. A text, said to read or not, is
    # the caller's: said home, `read=True` is a claim nothing checks -- a
    # call to `history` stops being a read once the statement is made to
    # log a row -- and goes once.
    reads = Server("drop", (200, [{"id": 1}], {}), "drop", "drop", "drop")
    r = Client(reads.url)
    assert r.collection("docs").rows() == [{"id": 1}]
    with pytest.raises(OSError):
        r.query("get docs")
    with pytest.raises(OSError):
        r.query("get docs", read=True)
    with pytest.raises(OSError):
        r.call("history", read=True)
    assert len(reads.seen) == 5

    # Said to read on a client reading near, it goes marked
    # (`Fennec-Read: replica`), which every server runs read-only: again.
    near = Server("drop", (200, [{"id": 2}], {}))
    assert Client(near.url, reads="nearest").query("get docs", read=True) == [{"id": 2}]
    assert len(near.seen) == 2
    assert "Fennec-Read: replica" in near.seen[1][1]


def test_a_connection_refused_is_tried_again_whatever_the_request():
    srv = Server(OK)
    c = Client(srv.url)
    made = c._connect
    refusals = [ConnectionRefusedError(61, "Connection refused")]

    def connect():
        conn = made()
        if refusals:
            err = refusals.pop()

            def refused():
                raise err

            conn.connect = refused
        return conn

    c._connect = connect
    assert c.query("put notes {n: 1}") == {"affected": 1}
    assert len(srv.seen) == 1


def test_the_tries_are_bounded_attempts_and_their_waits_within_max_wait():
    busy = (503, {"error": "too many connections", "retry_after": 1, "retry_after_ms": 5}, {})
    srv = Server(busy, busy, busy, busy, (200, [], {}))
    with pytest.raises(FennecError) as e:
        Client(srv.url).collection("docs").rows()
    assert len(srv.seen) == 3, "three tries unless told"
    # The last answer, with what the server said.
    assert (e.value.status, e.value.retry_after) == (503, 0.005)

    four = Server(busy, busy, busy, (200, [{"id": 1}], {}))
    assert Client(four.url, retry=Retry(attempts=4)).collection("docs").rows() == [{"id": 1}]
    assert len(four.seen) == 4

    # A wait past what is left of max_wait is not waited: the refusal at once.
    long = Server((429, {"error": "past", "quota": "writes", "retry_after_ms": 5000}, {}), OK)
    with pytest.raises(FennecError) as e:
        Client(long.url, retry=Retry(max_wait=1.0)).query("put notes {n: 1}")
    assert e.value.retry_after == 5
    assert len(long.seen) == 1

    off = Server(busy, (200, [], {}))
    with pytest.raises(FennecError):
        Client(off.url, retry=False).collection("docs").rows()
    assert len(off.seen) == 1
    with pytest.raises(ValueError):
        Client(off.url, retry=Retry(attempts=0))


def test_the_timeout_bounds_the_head_and_each_piece_and_nothing_goes_again_after_it():
    # A head at once, then a byte every 20 ms: each wait well within the
    # timeout, the whole a second past it -- read whole, as a long answer is.
    srv = Server("trickle", OK)
    began = time.monotonic()
    assert Client(srv.url, timeout=0.3).query("put notes {n: 1}") == {"affected": 1}
    assert time.monotonic() - began >= 0.9
    assert len(srv.seen) == 1

    # No head: given up at the timeout, and not sent again, a read the
    # builder made nor a write under a key -- past the client's timeout a
    # `compact` may still be running, and a long read ran three times.
    for send in (
        lambda c: c.collection("docs").rows(),
        lambda c: c.query("put notes {n: 1}", idempotency_key="n-1"),
    ):
        hung = Server("hang", (200, [{"id": 1}], {}))
        began = time.monotonic()
        with pytest.raises(TimeoutError):
            send(Client(hung.url, timeout=0.2))
        assert time.monotonic() - began >= 0.2
        assert len(hung.seen) == 1

    # A body that stops: given up the timeout after its last piece.
    stalled = Server("stall", (200, [{"id": 1}], {}))
    with pytest.raises(TimeoutError):
        Client(stalled.url, timeout=0.2).collection("docs").rows()
    assert len(stalled.seen) == 1

    # The change stream's reads go over the same connection, bounded alike.
    changes = Server("hang", (200, {"name": "c", "since": 0}, {}))
    with pytest.raises(TimeoutError):
        Client(changes.url, timeout=0.2).move_consumer("c", 0)
    assert len(changes.seen) == 1, "a commit is a write"


def run(coro):
    return asyncio.run(coro)


def test_async_sends_again_by_the_same_rule():
    async def go():
        srv = Server(MOVED, OK)
        async with AsyncClient(srv.url) as db:
            assert await db.query("put notes {n: 1}") == {"affected": 1}
        assert len(srv.seen) == 2
        assert srv.seen[1][0] - srv.seen[0][0] >= 1.0

        bare = Server("drop", OK)
        async with AsyncClient(bare.url) as db:
            with pytest.raises((OSError, asyncio.IncompleteReadError)):
                await db.query("put notes {n: 1}")
        assert len(bare.seen) == 1, "a write that may have landed is never made twice"

        keyed = Server("drop", OK)
        async with AsyncClient(keyed.url) as db:
            assert await db.query("put notes {n: 1}", idempotency_key="n-1") == {"affected": 1}
        assert keyed.keys() == ["n-1", "n-1"]

        reads = Server("drop", (200, [{"id": 1}], {}), "drop")
        async with AsyncClient(reads.url) as db:
            assert await db.collection("docs").rows() == [{"id": 1}]
            # The caller's `read=True`, home: a claim, sent once.
            with pytest.raises((OSError, asyncio.IncompleteReadError)):
                await db.call("history", read=True)
        assert len(reads.seen) == 3

        near = Server("drop", (200, [{"id": 2}], {}))
        async with AsyncClient(near.url, reads="nearest") as db:
            assert await db.query("get docs", read=True) == [{"id": 2}]
        assert len(near.seen) == 2

        landed = Server((503, {"error": "moved", "completed": 1, "at": 1, "retry_after": 1}, {"Retry-After": "1"}), OK)
        async with AsyncClient(landed.url) as db:
            with pytest.raises(FennecError):
                await db.batch([("compact notes", []), ("put notes {n: 1}", [])])
        assert len(landed.seen) == 1

        held = Server((429, {"error": "past its quota of 4 held requests", "quota": "held"}, {}), OK)
        async with AsyncClient(held.url) as db:
            with pytest.raises(FennecError):
                await db.query("put notes {n: 1}")
        assert len(held.seen) == 1

    run(go())


def test_async_bounds_the_head_and_each_piece_and_tries_a_refused_connection_again(monkeypatch):
    async def go():
        srv = Server("trickle", OK)
        async with AsyncClient(srv.url, timeout=0.3) as db:
            assert await db.query("put notes {n: 1}") == {"affected": 1}
            # Kept, the connection takes the next request.
            assert await db.query("put notes {n: 2}") == {"affected": 1}
        assert len(srv.seen) == 2

        for plan in ("hang", "stall"):
            hung = Server(plan, (200, [{"id": 1}], {}))
            async with AsyncClient(hung.url, timeout=0.2) as db:
                with pytest.raises(asyncio.TimeoutError):
                    await db.collection("docs").rows()
            assert len(hung.seen) == 1, plan

        refused = Server(OK)
        opened = asyncio.open_connection
        refusals = [ConnectionRefusedError(61, "Connection refused")]

        async def open_connection(*a, **k):
            if refusals:
                raise refusals.pop()
            return await opened(*a, **k)

        monkeypatch.setattr(fennecdb.asyncio, "open_connection", open_connection)
        async with AsyncClient(refused.url) as db:
            assert await db.query("put notes {n: 1}") == {"affected": 1}
        assert len(refused.seen) == 1

        # A host of several addresses, each refusing: asyncio joins their
        # words in one OSError, which is a refusal all the same -- one of
        # another error among them is not.
        joined = Server(OK)
        refusals.append(
            OSError(
                f"Multiple exceptions: [Errno {errno.ECONNREFUSED}] Connect call failed ('::1', 8080, 0, 0), "
                f"[Errno {errno.ECONNREFUSED}] Connect call failed ('127.0.0.1', 8080)"
            )
        )
        async with AsyncClient(joined.url) as db:
            assert await db.query("put notes {n: 1}") == {"affected": 1}
        assert len(joined.seen) == 1
        refusals.append(
            OSError(
                f"Multiple exceptions: [Errno {errno.ECONNREFUSED}] Connect call failed ('::1', 8080, 0, 0), "
                f"[Errno {errno.EHOSTUNREACH}] Connect call failed ('127.0.0.1', 8080)"
            )
        )
        other = Server(OK)
        async with AsyncClient(other.url) as db:
            with pytest.raises(OSError):
                await db.query("put notes {n: 1}")
        assert other.seen == []

    run(go())


def test_a_host_s_joined_refusals_are_a_refusal_and_nothing_else_is():
    from fennecdb import _refused

    r = errno.ECONNREFUSED
    assert _refused(ConnectionRefusedError(r, "Connection refused"))
    assert _refused(OSError(f"Multiple exceptions: [Errno {r}] Connect call failed ('::1', 80, 0, 0), [Errno {r}] Connect call failed ('127.0.0.1', 80)"))
    assert not _refused(OSError(f"Multiple exceptions: [Errno {r}] Connect call failed ('::1', 80, 0, 0), [Errno {errno.ENETUNREACH}] Network is unreachable"))
    assert not _refused(OSError("Multiple exceptions: "))
    assert not _refused(ConnectionResetError(errno.ECONNRESET, "reset"))
    assert not _refused(TimeoutError())


def test_a_bodys_code_is_the_errors_error_code():
    from fennecdb import _answer

    def refused(body):
        with pytest.raises(FennecError) as e:
            _answer(401, json.dumps(body).encode())
        return e.value

    e = refused({"error": "the token has expired", "code": "token_expired"})
    assert (e.status, e.error_code) == (401, "token_expired")
    # None where the body names no string, or an empty one.
    assert refused({"error": "bad token"}).error_code is None
    assert refused({"error": "bad token", "code": 7}).error_code is None
    assert refused({"error": "bad token", "code": ""}).error_code is None
    with pytest.raises(FennecError) as e:
        _answer(401, b"not json")
    assert e.value.error_code is None
