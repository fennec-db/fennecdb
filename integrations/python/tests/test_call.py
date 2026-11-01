"""`call`: a named statement called by name, `call <name> $1` over `/query`
through the client's own statement path -- against the real server for what
it answers, and against a scripted one for where it is sent (a read near,
a write home). The statements are defined with the server's token through
the client itself, their names made fresh so no two tests meet."""

import asyncio
import json
import socket
import threading
from datetime import datetime, timezone

import pytest

from conftest import TOKEN, URL, fresh
from fennecdb import AsyncClient, Client, FennecError, call


class Bank:
    """Two accounts, a notes collection and the statements over them."""

    def __init__(self, client: Client) -> None:
        self.client = client
        tag = fresh("c")
        self.accounts, self.notes, self.things = f"accounts_{tag}", f"notes_{tag}", f"things_{tag}"
        self.transfer, self.stamp, self.mine, self.balances = (
            f"transfer_{tag}",
            f"stamp_{tag}",
            f"mine_{tag}",
            f"balances_{tag}",
        )
        self.kinds, self.silent = f"kinds_{tag}", f"silent_{tag}"
        a, n = self.accounts, self.notes
        client.query(f"create collection {a} (balance int)")
        client.query(f"create collection {n} (body text, by text)")
        client.query(
            f"create collection {self.things} (s text, i int, f float, b bool, at timestamp, "
            "meta json, tags [text], embed vector<3>)"
        )
        client.query(
            f"create statement {self.transfer}($from int, $to int, $amount int) public as begin "
            f"set {a} {{balance: balance - $amount}} where id = $from and balance >= $amount "
            "require 1 else 'insufficient funds'; "
            f"set {a} {{balance: balance + $amount}} where id = $to require 1 else 'no such account'; "
            f"get {a} select id, balance where id = $from end"
        )
        client.query(f"create statement {self.stamp}() public as insert {n} {{body: 'z'}} returning id")
        client.query(
            f"create statement {self.mine}() claims (sub text) as get {n} select body where by = $jwt.sub"
        )
        client.query(f"create statement {self.balances}() public as get {a} select id, balance order id")
        client.query(
            f"create statement {self.kinds}($s text, $i int, $f float, $b bool, $at timestamp, "
            f"$meta json, $tags [text], $embed vector<3>) public as insert {self.things} "
            "{s: $s, i: $i, f: $f, b: $b, at: $at, meta: $meta, tags: $tags, embed: $embed} "
            "returning s, i, f, b, at, meta, tags"
        )
        client.query(f"create statement {self.silent}($n int) public as set {a} {{balance: $n}} where id = 1")
        client.query(f"put {a} [{{balance: 1000}}, {{balance: 1000}}]")
        client.query(f"put {n} [{{body: 'hello', by: 'alice'}}, {{body: 'hi', by: 'bob'}}]")

    def balance(self, id: int) -> int:
        return self.client.query(f"get {self.accounts} select balance where id = $1", [id])[0]["balance"]

    def drop(self) -> None:
        for name in (self.transfer, self.stamp, self.mine, self.balances, self.kinds, self.silent):
            self.client.query(f"drop statement if exists {name}")
        for name in (self.accounts, self.notes, self.things):
            self.client.query(f"drop collection if exists {name}")


@pytest.fixture()
def bank(client):
    b = Bank(client)
    try:
        yield b
    finally:
        b.drop()


def test_a_call_answers_the_last_statement_of_its_body(client, bank):
    out = client.call(bank.transfer, {"from": 1, "to": 2, "amount": 300})
    assert out == [{"id": 1, "balance": 700}]
    assert (bank.balance(1), bank.balance(2)) == (700, 1300)
    # The text it sent is the builder's, so the same call by hand is the same.
    text, params = call(bank.transfer, {"from": 2, "to": 1, "amount": 50}).to_call()
    assert text == f"call {bank.transfer} $1"
    assert client.query(text, params) == [{"id": 2, "balance": 1250}]
    # No arguments, a body that writes and answers a row; one that answers none
    # is the affected count.
    assert client.call(bank.stamp) == [{"id": 3}]
    assert client.call(bank.silent, {"n": 7}) == {"affected": 1}
    assert client.call(bank.balances) == [{"id": 1, "balance": 7}, {"id": 2, "balance": 1250}]


def test_arguments_of_several_types_reach_the_statement_as_values(client, bank):
    at = datetime(2026, 9, 19, 12, 34, 56, 789000, tzinfo=timezone.utc)
    out = client.call(
        bank.kinds,
        {
            "s": "çağ \"quoted\" '; del x; --",
            "i": -42,
            "f": 2.5,
            "b": True,
            "at": at,
            "meta": {"lang": "tr", "rank": 2, "deep": {"ok": [1, 2]}},
            "tags": ["a", "b"],
            "embed": [0.5, -0.25, 1],
        },
    )
    assert len(out) == 1
    row = out[0]
    assert (row["s"], row["i"], row["f"], row["b"]) == ("çağ \"quoted\" '; del x; --", -42, 2.5, True)
    assert row["meta"] == {"lang": "tr", "rank": 2, "deep": {"ok": [1, 2]}}
    assert row["tags"] == ["a", "b"]
    assert row["at"] == "2026-09-19T12:34:56.789Z"
    # The numbers of a vector argument are what the index holds.
    assert client.query(f"get {bank.things} select embed") == [{"embed": [0.5, -0.25, 1.0]}]


def test_a_refusal_is_the_clients_error_with_its_status_and_reason(client, bank):
    # require not met: 412, the statement's own words.
    with pytest.raises(FennecError) as e:
        client.call(bank.transfer, {"from": 1, "to": 2, "amount": 5000})
    assert (e.value.status, e.value.reason) == (412, "insufficient funds")
    with pytest.raises(FennecError) as e:
        client.call(bank.transfer, {"from": 1, "to": 99, "amount": 10})
    assert (e.value.status, e.value.reason) == (412, "no such account")
    # The block was put back: the first set did not stay.
    assert (bank.balance(1), bank.balance(2)) == (1000, 1000)
    # Not a statement, or not granted: 404.
    with pytest.raises(FennecError) as e:
        client.call("no_such_statement_here", {"a": 1})
    assert e.value.status == 404
    # An argument missing, extra or of another type: 400.
    for args in ({"from": 1, "to": 2}, {"from": 1, "to": 2, "amount": "ten"}, {"from": 1, "to": 2, "amount": 1, "by": 3}):
        with pytest.raises(FennecError) as e:
            client.call(bank.transfer, args)
        assert e.value.status == 400, args
    # A claim the statement reads and the call lacks: 403.
    with pytest.raises(FennecError) as e:
        client.call(bank.mine)
    assert e.value.status == 403
    # A name the builder refuses never leaves the client.
    with pytest.raises(FennecError) as e:
        client.call("stamp; del x")
    assert e.value.status == 0
    assert (bank.balance(1), bank.balance(2)) == (1000, 1000)


def test_claims_are_the_as_part_of_the_server_token_s_call(client, bank):
    assert client.call(bank.mine, claims={"sub": "alice"}) == [{"body": "hello"}]
    assert client.call(bank.mine, None, claims={"sub": "bob"}) == [{"body": "hi"}]
    assert client.call(bank.mine, claims={"sub": "nobody"}) == []
    # A claim of another type than the statement declares is 403 too.
    with pytest.raises(FennecError) as e:
        client.call(bank.mine, claims={"sub": 5})
    assert e.value.status == 403
    # Claims are a parameter, never the text.
    assert client.call(bank.mine, claims={"sub": "x' or 1=1 --"}) == []


def test_an_idempotency_key_makes_a_call_once(client, bank):
    args = {"from": 1, "to": 2, "amount": 10}
    key = f"{bank.transfer}-1"
    first = client.call(bank.transfer, args, idempotency_key=key)
    assert not client.replayed
    again = client.call(bank.transfer, args, idempotency_key=key)
    assert client.replayed
    assert first == again == [{"id": 1, "balance": 990}]
    assert (bank.balance(1), bank.balance(2)) == (990, 1010)
    # The key with another request is refused, and moves nothing.
    with pytest.raises(FennecError) as e:
        client.call(bank.transfer, {"from": 1, "to": 2, "amount": 20}, idempotency_key=key)
    assert e.value.status == 422
    # A copy's key is every write's of it, a call too.
    keyed = client.with_idempotency_key(f"{bank.transfer}-2")
    keyed.call(bank.transfer, args)
    keyed.call(bank.transfer, args)
    assert keyed.replayed
    assert (bank.balance(1), bank.balance(2)) == (980, 1020)


def test_a_call_that_reads_is_sent_read_and_one_that_writes_is_refused_there(bank):
    # `reads="nearest"` sends a marked request, which every server runs
    # read-only on every route, a router near a replica answering it there.
    near = Client(URL, TOKEN, reads="nearest")
    assert near.call(bank.balances, read=True) == [{"id": 1, "balance": 1000}, {"id": 2, "balance": 1000}]
    assert near.call(bank.mine, claims={"sub": "alice"}, read=True) == [{"body": "hello"}]
    before = bank.balance(1), bank.balance(2)
    for name, args in [(bank.transfer, {"from": 1, "to": 2, "amount": 10}), (bank.stamp, None)]:
        with pytest.raises(FennecError) as e:
            near.call(name, args, read=True)
        assert e.value.status == 403, name
        assert "only reads" in str(e.value)
    # Never landed.
    assert (bank.balance(1), bank.balance(2)) == before
    assert near.query(f"get {bank.notes} count", read=True) == [{"count": 2}]
    # Not said, a call goes home and lands; a client reading home lands it too.
    assert near.call(bank.stamp) == [{"id": 3}]
    assert Client(URL, TOKEN).call(bank.stamp, read=True) == [{"id": 4}]


def test_a_call_waits_and_times_out_as_a_statement_does(client, bank):
    # The server holds a call whose body sets or deletes and writes nothing
    # (`Fennec-Wait`); one that inserts is refused, naming the statement.
    with pytest.raises(FennecError) as e:
        client.call(bank.stamp, wait=0.3)
    assert e.value.status == 400
    assert bank.stamp in str(e.value)
    # A held call that writes nothing ends on time, answering its last row.
    gone = f"gone_{bank.transfer}"
    client.query(f"create statement {gone}($id int) public as del {bank.accounts} where id = $id")
    try:
        import time

        began = time.monotonic()
        assert client.call(gone, {"id": 99}, wait=0.3) == {"affected": 0}
        assert 0.3 <= time.monotonic() - began < 5
    finally:
        client.query(f"drop statement if exists {gone}")
    # The client's own timeout applies to a call as to a text.
    tight = Client(URL, TOKEN, timeout=5)
    assert tight.call(bank.balances)


def test_the_async_client_awaits_the_same_call(bank):
    async def go():
        async with AsyncClient(URL, TOKEN) as db:
            assert await db.call(bank.transfer, {"from": 1, "to": 2, "amount": 100}) == [{"id": 1, "balance": 900}]
            assert await db.call(bank.mine, claims={"sub": "alice"}) == [{"body": "hello"}]
            assert await db.call(bank.stamp) == [{"id": 3}]
            with pytest.raises(FennecError) as e:
                await db.call(bank.transfer, {"from": 1, "to": 2, "amount": 5000})
            assert (e.value.status, e.value.reason) == (412, "insufficient funds")
            with pytest.raises(FennecError) as e:
                await db.call("no_such_statement_here")
            assert e.value.status == 404
            with pytest.raises(FennecError) as e:
                await db.call(bank.transfer, [1, 2, 3])
            assert e.value.status == 0
            key = f"{bank.transfer}-async"
            args = {"from": 1, "to": 2, "amount": 10}
            first = await db.call(bank.transfer, args, idempotency_key=key)
            assert await db.call(bank.transfer, args, idempotency_key=key) == first
            assert db.replayed
            assert bank.balance(1) == 890
            # Marked, on a client reading near: a call that writes is refused.
            async with AsyncClient(URL, TOKEN, reads="nearest") as near:
                assert len(await near.call(bank.balances, read=True)) == 2
                with pytest.raises(FennecError) as e:
                    await near.call(bank.stamp, read=True)
                assert e.value.status == 403
            # A held call given up closes its connection, and the next opens another.
            gone = f"agone_{bank.transfer}"
            await db.query(f"create statement {gone}($id int) public as del {bank.accounts} where id = $id")
            try:
                with pytest.raises(asyncio.TimeoutError):
                    await asyncio.wait_for(db.call(gone, {"id": 99}, wait=5), 0.2)
                assert len(await db.call(bank.balances)) == 2
            finally:
                await db.query(f"drop statement if exists {gone}")

    asyncio.run(go())


# ------------------------------------------------------- against a scripted server


def _scripted(answers):
    """A server on loopback answering each request with the next of
    `answers` -- `(status, body, headers)` -- on one connection kept alive.
    Its port, and each request's headers and body, headers lower-cased."""
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen()
    seen = []
    answers = list(answers)

    def read_request(conn):
        data = b""
        while b"\r\n\r\n" not in data:
            chunk = conn.recv(65536)
            if not chunk:
                return None
            data += chunk
        head, _, rest = data.partition(b"\r\n\r\n")
        lines = head.split(b"\r\n")
        headers = {"request-line": lines[0].decode()}
        for line in lines[1:]:
            k, _, v = line.decode().partition(":")
            headers[k.strip().lower()] = v.strip()
        length = int(headers.get("content-length", 0))
        while len(rest) < length:
            rest += conn.recv(65536)
        headers["body"] = json.loads(rest)
        return headers

    def serve():
        while answers:
            conn, _ = srv.accept()
            while answers:
                req = read_request(conn)
                if req is None:
                    break
                seen.append(req)
                status, body, extra = answers.pop(0)
                raw = json.dumps(body).encode()
                head = f"HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n"
                head += f"Content-Length: {len(raw)}\r\nConnection: keep-alive\r\n"
                for k, v in extra.items():
                    head += f"{k}: {v}\r\n"
                conn.sendall(head.encode() + b"\r\n" + raw)
            conn.close()
        srv.close()

    threading.Thread(target=serve, daemon=True).start()
    return srv.getsockname()[1], seen


H1 = "1-00000000000000aa"


def test_a_call_marked_read_goes_near_and_one_not_goes_home():
    script = [
        (200, {"affected": 1}, {"Fennec-Seq": "9", "Fennec-History": H1}),
        (200, [{"n": 1}], {}),
        (200, [{"n": 1}], {}),
        (200, [{"n": 1}], {}),
    ]
    port, seen = _scripted(script)
    db = Client(f"http://127.0.0.1:{port}", reads="nearest")
    db.query("put t {n: 1}")
    assert db.call("history", {"limit": 2}, read=True) == [{"n": 1}]
    assert db.call("history", {"limit": 2}) == [{"n": 1}]
    assert db.call("history", None, claims={"sub": "a"}, idempotency_key="k-1", read=True) == [{"n": 1}]
    put, near, home, keyed = seen
    assert near["request-line"].startswith("POST /query")
    assert near["body"] == {"query": "call history $1", "params": [{"limit": 2}]}
    assert (near["fennec-read"], near["fennec-after"], near["fennec-history"]) == ("replica", "9", H1)
    # Not said to read, it goes home and carries no mark.
    assert home["body"] == near["body"]
    assert "fennec-read" not in home and "fennec-after" not in home
    # Claims, key and read together: the claims after the arguments' place.
    assert keyed["body"] == {"query": "call history as $1", "params": [{"sub": "a"}]}
    assert keyed["idempotency-key"] == "k-1" and keyed["fennec-read"] == "replica"


def test_the_async_call_is_marked_alike():
    port, seen = _scripted([(200, [{"n": 1}], {}), (200, [{"n": 1}], {})])

    async def go():
        async with AsyncClient(f"http://127.0.0.1:{port}", reads="nearest") as db:
            await db.call("history", read=True)
            await db.call("history")

    asyncio.run(go())
    near, home = seen
    assert near["fennec-read"] == "replica" and "fennec-read" not in home
    assert near["body"] == home["body"] == {"query": "call history", "params": []}
