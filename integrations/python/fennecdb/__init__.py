"""fennecdb over HTTP.

A client for fennec-server's HTTP endpoint (`fennec-server --http <address>`), and on
top of it vector stores for LangChain (`fennecdb.langchain`) and LlamaIndex
(`fennecdb.llama_index`).

    from fennecdb import Client
    db = Client("http://127.0.0.1:8080", token="...")
    db.query("get articles near embed $1 limit 5", [[0.1, 0.2, 0.3]])

The client is the standard library alone, as the server is: a statement is
one `POST /query`, several are one `POST /batch` under one lock.

`collection` starts the query builder, which writes the statement for you
-- every value a parameter, every name checked -- and makes the same text
the JavaScript, Go and .NET builders make of the same chain:

    from fennecdb import or_
    rows = (db.collection("articles").select("title")
              .where("year", ">=", 2024)
              .where(or_({"lang": "tr"}, {"tags": {"has": "rust"}}))
              .near("embed", [0.1, 0.2, 0.3]).limit(5).rows())

`match` takes `highlight` and `snippet` marks in the select list, and any
read `facet` counts, which come back beside the rows as `rows.facets`:

    rows = (db.collection("products").select("title").highlight("title")
              .match("title", "phone").facet("brand", top=10).limit(20).rows())
    rows.facets["brand"]   # [FacetCount(value="acme", count=12), ...]

A named statement (`create statement transfer(...) as ...`) is called by
name with its arguments, an object, as one parameter -- `call transfer $1`
over `/query`, so the errors, the key and `wait` are a statement's; the
server's token may say the claims it is made with (`as $2`), and `read=True`
marks a statement that only reads, to go near a read replica:

    db.call("transfer", {"from": 1, "to": 2, "amount": 300}, idempotency_key=transfer_id)
    db.call("history", read=True)
    text, params = call("transfer", {"from": 1, "to": 2, "amount": 300}).to_call()

A retry after a timeout cannot tell whether the write ran; under an
`Idempotency-Key` it runs once, and the second answer is the first one
kept (`replayed`). One key per write -- the same key with another request
is refused (422):

    db.batch([debit, credit], idempotency_key=transfer_id)
    db.with_idempotency_key(order_id).collection("orders").insert(order)

A worker's claim that finds no job can wait for one at the server
(`Fennec-Wait`): with `wait` a `set` or `del` that writes nothing is held
until it can -- a job enqueued, a delayed one come due, a lease lapsed --
or for that many seconds, at most 30, rather than sent again every few:

    jobs = db.collection("jobs").where(raw("run_at <= now()")).order("run_at").limit(10)
    claimed = jobs.update(claim, returning=True, wait=30)

`AsyncClient` is the same over asyncio, for an event loop a blocking
request would stall:

    async with AsyncClient("http://127.0.0.1:8080", token="...") as db:
        rows = await db.query("get articles near embed $1 limit 5", [[0.1, 0.2, 0.3]])

A client far from a tenant's home reads near itself where the tenant has a
read replica there (`fennec-shard`'s read replicas): with `reads="nearest"`
the builder's reads, and a statement given `read=True`, are marked
(`Fennec-Read: replica`) for a router near a replica to answer, each sent
after the furthest position an answer from home named to this client
(`Fennec-After` and `Fennec-History`: the change, and the history it is a
change of) -- it reads nothing older than it wrote, nor a write a failover
replaced. Writes go home:

    db = Client("https://us.example.com/t/acme", token="...", reads="nearest")

`Client.follow` reads every write on the server's disk (`GET /_changes`,
`fennec-server --cdc`) for a consumer whose cursor the server keeps, and
commits each batch once the loop comes back for the next:

    for batch in db.follow("search-index"):
        for change in batch:
            index(change)          # an exception: this batch is read again

`python -m fennecdb.relay` hands the same to another program or a webhook.
"""

from __future__ import annotations

import asyncio
import errno
import http.client
import json
import random
import re
import select
import socket
import ssl
import threading
import time
import urllib.parse
from datetime import datetime
from typing import Any, Iterable, Iterator, Mapping, NamedTuple, Sequence

__all__ = [
    "AsyncClient",
    "AsyncQuery",
    "Call",
    "Changes",
    "Client",
    "Computed",
    "Cond",
    "FacetCount",
    "FennecError",
    "HeldSlot",
    "LastRun",
    "NamedStatement",
    "Param",
    "Query",
    "Retry",
    "Rows",
    "SchemaError",
    "and_",
    "bucket",
    "call",
    "collection",
    "count_distinct",
    "distance",
    "expr",
    "first",
    "inc",
    "last",
    "not_",
    "or_",
    "param",
    "placeholders",
    "raw",
]


class FennecError(Exception):
    """A statement the server refused: its message, and the HTTP status --
    0 for one the query builder refused before sending anything. A batch
    that stopped also says which statement stopped it, from 0 (`at`: the
    write whose `require` was not met, 412, or whose id was taken, 409),
    and how many statements stayed applied (`completed`: 0, since a batch
    lands whole, but for one holding a `compact`); both None otherwise.

    A `require n else '<reason>'` that refused says its reason (`reason`,
    the builders' `reason=`): the statement's own words, by which a caller
    tells "insufficient funds" from "no such account" -- where `at` moves
    with every statement added in front of it. None where the statement
    gave none.

    A statement bound to the write (`create statement ... after`) that
    refused it is named (`binding`): a guard's `require` not met, a write
    of its body refused. Told the server's token alone, a scoped one the
    `reason` and no name; None otherwise.

    A tenant's quota names itself (`quota`): "bytes" and "rows" answer 507,
    and room is made by deleting; "streams", "held", "requests", "writes"
    and "write_bytes" answer 429, with the seconds to wait before sending
    again (`retry_after`) -- to the millisecond, as the server says it in
    `retry_after_ms` -- as a tenant being moved or failed over answers its
    writes 503, in whole seconds. None otherwise.

    A 500 whose `outcome` is "unknown" is a write a tenant's node landed and
    answered after its lease from the router lapsed: its tenant may have
    been failed over without it. Send it again with the same idempotency
    key, or read it back a lease and a tenth later; `seq` and `history` say
    where it left the node's database, and a read sent after them once the
    tenant settles is answered by a primary holding the write and refused
    504 by one without it. Not the client's position, which this answer
    does not move. None otherwise.

    The body's `code` is `error_code`: "token_expired" on a 401 for a token
    past its `exp`, which a fresh token from its issuer cures. No other
    refusal names one today, and a 401 naming none may be cured by a fresh
    token too (a key rotation), so it is not by itself a sign-out. A string
    that later releases may add to, never an enum; None where the body
    names none, or a non-string or empty one."""

    def __init__(
        self,
        message: str,
        status: int,
        *,
        at: int | None = None,
        completed: int | None = None,
        quota: str | None = None,
        retry_after: float | None = None,
        outcome: str | None = None,
        seq: int | None = None,
        history: str | None = None,
        reason: str | None = None,
        binding: str | None = None,
        error_code: str | None = None,
    ):
        super().__init__(message)
        self.status = status
        self.at = at
        self.completed = completed
        self.reason = reason
        self.binding = binding
        self.quota = quota
        self.retry_after = retry_after
        self.outcome = outcome
        self.seq = seq
        self.history = history
        self.error_code = error_code


class SchemaError(FennecError):
    """The schema the code declares and the server's differ in what no
    open applies: each difference and how to resolve it, as the engine
    writes them, in `refusals`."""

    def __init__(self, refusals: list, status: int):
        self.refusals = refusals
        lines = [f"  - {r['message']}\n    {r['fix']}" for r in refusals]
        super().__init__("the database's schema differs from the code's:\n" + "\n".join(lines), status)


class Changes(NamedTuple):
    """Writes `GET /_changes` handed over, a dict each, and `next`: the last
    one's number, which is the `since` to read on from. `seq` is the last
    write the database holds (`Fennec-Seq`): a `next` that has reached it has
    every write there is."""

    writes: list
    next: int
    seq: int = 0


class LastRun(NamedTuple):
    """A scheduled statement's last run, landed or pending, as `statements`
    lists it: the slot it ran for (what `slot()` read), `"ran"` or
    `"refused"` -- the statement's own refusal, a `require` not met or a
    broken statement, which marks the slot as run -- its status and the
    `reason` its `require` gave, and the slots passed over before it."""

    slot: datetime
    outcome: str
    status: int | None
    reason: str | None
    skipped: int


class HeldSlot(NamedTuple):
    """A due slot the server's state holds back -- a tenant's quota (507), a
    storage error -- tried again as that changes, and since when."""

    slot: datetime
    status: int
    since: datetime


class NamedStatement(NamedTuple):
    """A named statement as `statements` lists it to the server's token,
    each column None where it says nothing of the statement: a called one
    has no `after` and no clock. `after` is a write-bound statement's
    collection and the writes that run it (`("orders", "set or del")`);
    `schedule` a scheduled one's clause as its text writes it (`every 1h`,
    `at '0 3 * * *' in 'Europe/Berlin'`), `next` the slot due next -- past
    while one is overdue or held -- `last` its last run and `held` a slot
    held back. `hook` is a webhook consumer's hook, whose events are sent
    to `/_hooks/<hook>`, and `armed` whether this server -- on a tenant
    node, the tenant -- holds its secret: `False` answers every event of
    it 401. `version` is the history and change its definition landed
    at; `broken` why the schema broke it, which it runs no more until it
    reads again."""

    name: str
    version: tuple[int, int]
    signature: str | None
    writes: bool | None
    comment: str | None
    broken: str | None
    after: tuple[str, str] | None
    schedule: str | None
    next: datetime | None
    last: LastRun | None
    held: HeldSlot | None
    hook: str | None = None
    armed: bool | None = None


def _instant(text: str) -> datetime:
    """The server's ISO text of a time, read as an aware UTC datetime:
    `fromisoformat` takes a `Z` only from Python 3.11."""
    return datetime.fromisoformat(text.replace("Z", "+00:00"))


def _named(row: Mapping[str, Any]) -> NamedStatement:
    """A row of `statements` as a `NamedStatement`."""
    last, held, after = row.get("last"), row.get("held"), row.get("after")
    return NamedStatement(
        name=row["name"],
        version=tuple(row["version"]),
        signature=row.get("signature"),
        writes=row.get("writes"),
        comment=row.get("comment"),
        broken=row.get("broken"),
        after=tuple(after) if after else None,
        schedule=row.get("schedule"),
        next=_instant(row["next"]) if row.get("next") else None,
        last=(
            LastRun(_instant(last["slot"]), last["outcome"], last.get("status"), last.get("reason"), last.get("skipped", 0))
            if last
            else None
        ),
        held=HeldSlot(_instant(held["slot"]), held["status"], _instant(held["since"])) if held else None,
        hook=row.get("hook"),
        armed=row.get("armed"),
    )


class Retry(NamedTuple):
    """How a client sends a request again: only where nothing ran, or where
    running it again changes nothing.

    Nothing ran: a 503, or a quota's 429, naming its wait -- sent after that
    wait, never sooner, unless its body says statements of a batch were
    made (`completed`), which the server never answers so; a connection
    refused; a request not handed to the system whole. Running it again
    changes nothing: a read the builder made, one sent near (`read=True` on
    a client with `reads="nearest"`: `Fennec-Read: replica`, which every
    server runs read-only), and a write under an `Idempotency-Key`, answered
    from its key while the server keeps it (a day) -- these go again after a
    reset or a dropped connection too. Nothing goes again after the client's
    own `timeout`, past which the server may still run it, nor a refusal
    naming no wait; `read=True` sent home is a claim nothing checks, and
    goes once.

    `attempts` tries at most, their waits -- the server's, or 0.25 s doubling,
    30% jitter on top -- within `max_wait` seconds; past either, the last
    answer or error is the caller's. `Client(retry=False)` sends nothing
    again."""

    attempts: int = 3
    max_wait: float = 10.0


def _retry(retry: Retry | bool | None) -> Retry | None:
    if retry is True:
        return Retry()
    if not retry:
        return None
    if not isinstance(retry, Retry) or retry.attempts < 1 or retry.max_wait < 0:
        raise ValueError(f"bad retry: {retry!r}")
    return retry


def _wait_of(status: int, raw: bytes, header: str | None) -> float | None:
    """The seconds a 503 or a 429 asks a client to wait, where it names any:
    then nothing ran. To the millisecond where the body says it
    (`retry_after_ms`), before `Retry-After` -- whole seconds -- and the
    body's `retry_after`, which every 503 of the server's carries. None
    where statements of a batch were made (`completed`): sent again whole,
    they were made twice."""
    if status not in (429, 503):
        return None
    try:
        body = json.loads(raw)
    except ValueError:
        body = None
    if not isinstance(body, dict):
        body = {}
    done = body.get("completed")
    if isinstance(done, int) and done > 0:
        return None
    ms = body.get("retry_after_ms")
    if isinstance(ms, (int, float)) and not isinstance(ms, bool) and ms > 0:
        return ms / 1000
    for said in (header, body.get("retry_after")):
        try:
            wait = float(said)
        except (TypeError, ValueError):
            continue
        if 0 < wait < float("inf"):
            return wait
    return None


def _pause(retry: Retry | None, tries: int, waited: float, wait: float) -> float | None:
    """How long to wait before try `tries + 1`, after `waited` seconds of
    waits: the server's `wait`, or the backoff where longer, 30% jitter on
    top, within what is left of `max_wait`; None where no try is left."""
    base = max(wait, 0.25 * 2 ** (tries - 1))
    if retry is None or tries >= retry.attempts or waited + base > retry.max_wait:
        return None
    return min(base * (1 + random.random() * 0.3), retry.max_wait - waited)


def _bound(sock: socket.socket, secs: float | None) -> None:
    """Gives each of `sock`'s next waits `secs`, None for no bound, and
    gives the request up where none is left. Within a millisecond either
    way: a `settimeout` is a system call (0.3 us), and the one a quick
    request left is as good as set."""
    if secs is not None and secs <= 0:
        raise TimeoutError("timed out")
    was = sock.gettimeout()
    if (was is None) != (secs is None) or (secs is not None and abs(was - secs) > 0.001):
        sock.settimeout(secs)


# One refusal in an OSError's words, as asyncio joins every address's
# error when a host's addresses all fail ("Multiple exceptions: ...").
_REFUSAL = re.compile(rf"\[(?:Errno {errno.ECONNREFUSED}|WinError 1225)\] [^\[]*")


def _refused(e: BaseException) -> bool:
    """Whether connecting was refused, at every address of the host's where
    it has several: `asyncio.open_connection` tries each and, their words
    differing by the address, raises one `OSError` joining them, the
    refusals in it no `ConnectionRefusedError` any more. A request refused
    so sent nothing, and any goes again."""
    if isinstance(e, ConnectionRefusedError):
        return True
    said = str(e)
    rest = said.removeprefix("Multiple exceptions: ")
    if not isinstance(e, OSError) or rest == said:
        return False
    return all(_REFUSAL.fullmatch(part) for part in re.split(r", (?=\[(?:Errno|WinError) )", rest))


class _Shared:
    """What a client and the copies `with_idempotency_key` makes of it
    share, as Go's and .NET's copies share theirs: the change the last
    answer from home left the database at, whether it was replayed, and
    the furthest position an answer from home named, which a read near
    waits for -- an answer that arrives late is not the last."""

    __slots__ = ("seq", "replayed", "at", "_lock")

    def __init__(self) -> None:
        self.seq: int | None = None
        self.replayed = False
        self.at: tuple | None = None
        # A client's threads share its copies' position.
        self._lock = threading.Lock()

    def answered(self, seq: int, history: str | None) -> None:
        """An answer from home named `seq`, of `history`."""
        p = _position(seq, history)
        with self._lock:
            self.seq = seq
            if _later(p, self.at):
                self.at = p

    def told(self, raw: bytes, sent: tuple) -> None:
        """A read near's 504, the read sent after `sent`: no copy held it,
        home either -- a write lost in a failover. Home's position, lower or
        not, is the client's from now on: kept, every read near would wait
        it out for good. Taken only over `sent` -- a write answered
        meanwhile is further, and set back the next read near would miss
        it -- and never of an earlier term of the same lineage: a router
        whose copy of the directory lags a failover sends the read to the
        old primary, fenced, which names the branch it abandoned. Another
        lineage's is a tenant made again under the name, which starts again
        at term 1."""
        try:
            body = json.loads(raw)
        except ValueError:
            return
        if isinstance(body, dict) and isinstance(body.get("seq"), int):
            history = body.get("history")
            p = _position(body["seq"], history if isinstance(history, str) else None)
            with self._lock:
                if self.at is sent and (p[0] >= sent[0] or _another(p, sent)):
                    self.at = p


def _position(seq: int, history: str | None) -> tuple:
    """`(term, seq, history)`: a change, the history it is of
    (`<term>-<id>`; none for a database that never had a replica) and that
    history's term, one more at each promotion."""
    term = 0
    if history:
        try:
            term = int(history.split("-", 1)[0])
        except ValueError:
            term = 0
    return (term, seq, history)


def _lineage(p: tuple) -> str | None:
    """The lineage of position `p`: its history id's top half, which every
    fork of the tenant keeps and a tenant made again under its name does
    not."""
    history = p[2]
    if not history or "-" not in history:
        return None
    return history.split("-", 1)[1][:8] or None


def _another(p: tuple, q: tuple) -> bool:
    """Whether `p` is of another lineage than `q`: a tenant made again."""
    a, b = _lineage(p), _lineage(q)
    return a is not None and b is not None and a != b


def _later(p: tuple, q: tuple | None) -> bool:
    """Whether position `p` is past `q`: of another lineage -- home's word
    on a tenant made again, whose terms start again -- or a later term of
    the same, or further in one."""
    return q is None or _another(p, q) or p[:2] > q[:2]


def _reads(reads: str) -> str:
    if reads not in ("home", "nearest"):
        raise ValueError(f"reads is 'home' or 'nearest' (got {reads!r})")
    return reads


def _near(headers: dict, at: tuple | None, explicit: int | None) -> None:
    """A read marked for a replica near the client, after `explicit` where
    the caller named a change, and after its furthest position otherwise."""
    headers["Fennec-Read"] = "replica"
    if explicit is not None:
        headers["Fennec-After"] = str(explicit)
    elif at is not None:
        headers["Fennec-After"] = str(at[1])
        if at[2]:
            headers["Fennec-History"] = at[2]


class Client:
    """fennec-server's HTTP endpoint, one statement a request.

    A statement goes over a connection kept alive, one a thread: through
    `urlopen`, which opens one a request, a read by id took 152 us against
    57 over the one connection, the server starting a thread for each.

    `timeout` bounds the seconds to an answer's head -- a `wait` holds it
    that much longer -- and those between the pieces of its body, which is
    read for as long as it keeps coming; None, never. A statement that runs
    long before it answers -- a `compact`, a `create index` -- is given a
    client with a longer `timeout`: past it, it may still run, and is never
    sent again. `retry` sends a request again where nothing ran or running
    it twice is harmless (`Retry`); `False`, never.
    """

    def __init__(
        self,
        url: str = "http://127.0.0.1:8080",
        token: str | None = None,
        timeout: float | None = 30.0,
        reads: str = "home",
        retry: Retry | bool = True,
    ):
        parts = urllib.parse.urlsplit(url)
        if parts.scheme not in ("http", "https"):
            raise ValueError(f"not an http(s) URL: {url!r}")
        self.url = url.rstrip("/")
        self.token = token
        self.timeout = timeout
        self.retry = _retry(retry)
        # "nearest": reads marked for a read replica near the client.
        self.reads = _reads(reads)
        self.idempotency_key: str | None = None
        self._shared = _Shared()
        self._host = parts.hostname or "127.0.0.1"
        self._port = parts.port or (443 if parts.scheme == "https" else 80)
        self._tls = parts.scheme == "https"
        self._base = parts.path.rstrip("/")
        self._local = threading.local()

    @property
    def seq(self) -> int | None:
        """The change the last write through this client, or a copy of it,
        left the database at (`Fennec-Seq`): a read on a replica passed it as
        `after` waits for that write."""
        return self._shared.seq

    @seq.setter
    def seq(self, value: int | None) -> None:
        self._shared.seq = value

    @property
    def replayed(self) -> bool:
        """Whether the last write's answer was the one kept for its
        `Idempotency-Key`: the write had run before, and did not again."""
        return self._shared.replayed

    def with_idempotency_key(self, key: str) -> "Client":
        """A copy of the client whose writes -- `query`, `batch` and the
        builder's -- carry `key` as their `Idempotency-Key`: sent again
        after a timeout, a write runs once. It shares this client's
        connections and `seq`. One key per write; the same key with
        another request is refused (422)."""
        if not isinstance(key, str) or not key:
            raise ValueError("an idempotency key is a text, not empty")
        copy = object.__new__(Client)
        copy.__dict__.update(self.__dict__)
        copy.idempotency_key = key
        return copy

    def collection(self, name: str) -> Query:
        """The query builder over collection `name`: chain `select`, `where`,
        `near`, `order`, `limit` ... and end with `rows()`, `first()`,
        `count()`, or a write -- `insert`, `update`, `delete`."""
        return Query(name, self)

    def call(
        self,
        name: str,
        args: Mapping[str, Any] | None = None,
        *,
        claims: Mapping[str, Any] | None = None,
        idempotency_key: str | None = None,
        wait: float | None = None,
        read: bool = False,
    ) -> Any:
        """Calls the named statement `name` with `args`, a mapping of values
        by name: `call <name> $1` over `/query`, sent and answered as
        `query` does it -- the last statement of its body, rows or
        `{"affected": n}`. A refusal is a `FennecError` as any statement's:
        `status` 404 for no such statement or none granted, 403 for a claim
        the token lacks, 400 for an argument missing, extra or of another
        type, 412 for a `require` not met (its `reason`), 409 for a unique
        value taken or a broken statement. `claims` is `as {...}`, the
        claims the call is made with: the server's token alone may say them,
        a scoped token's are its own. `idempotency_key` and `wait` are
        `query`'s. `read=True` says the statement only reads, and on a
        client with `reads="nearest"` it goes near, after this client's
        writes; one that writes is refused there (403), never landed. Not
        said, a call goes home, as every text not known to read does."""
        text, params = Call(name, args, claims=claims).to_call()
        return self.query(text, params, idempotency_key=idempotency_key, wait=wait, read=read)

    def statements(self) -> list[NamedStatement]:
        """Each named statement the database holds, as the server lists them
        to its own token (`statements`) -- a scoped one is refused (403):
        its signature, whether it writes, its comment, why the schema broke
        it, what a binding is bound to, a scheduled one's clock, and a
        webhook consumer's hook and whether the server holds its secret.
        `call` sends a webhook consumer a test event, the event its
        argument, run as a delivery is and keeping no key."""
        return [_named(r) for r in self.query("statements")]

    def query(
        self,
        fennecql: str,
        params: Sequence[Any] | None = None,
        *,
        after: int | None = None,
        idempotency_key: str | None = None,
        wait: float | None = None,
        read: bool = False,
    ) -> Any:
        """Runs one FennecQL statement. Values go in as `$1`, `$2`, ... and
        never into the text. With `after` -- a primary's `seq` -- a replica
        answers once it holds that write. With `idempotency_key` a write
        runs once however often it is sent (`replayed`). With `wait` a
        `set` or `del` that writes nothing is held at the server until it
        can, or that many seconds -- a claim waiting for a job. `read=True`
        says the statement only reads: on a client with `reads="nearest"`,
        a router near the tenant's read replica answers it there, after
        this client's writes. A read answers its rows, a list; one with a
        `facet` clause a `Rows`, the counts as its `facets`."""
        body = {"query": fennecql, "params": list(params or [])}
        return _faceted(
            self._post(
                "/query",
                json.dumps(body).encode(),
                "application/json",
                after,
                key=idempotency_key,
                wait=wait,
                read=read,
            )
        )

    def batch(
        self,
        statements: Iterable[tuple[str, Sequence[Any]]],
        *,
        idempotency_key: str | None = None,
        wait: float | None = None,
        read: bool = False,
    ) -> Any:
        """Runs statements in order under one write lock, as one block:
        their writes -- a create, a drop or a `create index` among them --
        all land or, at the first error, none of them do: a `FennecError`
        whose `at` is the statement that stopped it. A batch holding a
        `compact` runs each statement on its own, and what ran before an
        error stays (`completed`). Answers `{"ok": n, "results": [...]}`,
        a statement's `{"affected": n}` or `{"rows": [...]}` each, and
        leaves the change it wrote at in `seq`. With `idempotency_key` it
        lands once however often it is sent (`replayed`); a batch of reads
        alone takes no key. With `wait` one whose `set`s and `del`s write
        nothing is held until they can, as `query`'s; `read=True`, a batch
        of reads alone, goes near as `query`'s does."""
        lines = [json.dumps({"query": q, "params": list(p)}) for q, p in statements]
        return self._post(
            "/batch",
            "\n".join(lines).encode(),
            "application/x-ndjson",
            key=idempotency_key,
            wait=wait,
            read=read,
        )

    def schema(
        self, fennecql: str, migrations: Sequence[Any] | None = None, *, migrate: bool = False
    ) -> dict:
        """Checks the server's schema against `fennecql` -- `create
        collection` and `create index` statements, a `schema.fennecql` file
        -- and returns the engine's plan. The server owns its schema: it is
        compared, and nothing applied, unless `migrate` -- with the server's
        token -- runs the migrations it has not recorded, in order, and adds
        what only adds, all one block. Raises `SchemaError` naming every
        difference that would lose data or could mean two things."""
        body = {"format": 1, "fennecql": fennecql, "migrations": list(migrations or [])}
        path = "/_schema/apply" if migrate else "/_schema/plan?mode=follow"
        out = self._post(path, json.dumps(body).encode(), "application/json", accept=409)
        if out["refusals"]:
            raise SchemaError(out["refusals"], 409)
        return out

    def changes(
        self,
        since: int | None = None,
        *,
        consumer: str | None = None,
        limit: int | None = None,
        wait: float | None = None,
    ) -> Changes:
        """The writes on the server's disk after `since` -- or after where
        `consumer` stands, or from now with neither -- at most `limit`, each
        `{"seq", "at", "collection", "op", "id", "doc"}`. With `wait`, an
        answer with nothing in it waits that many seconds for a write."""
        query = {"since": since, "consumer": consumer, "limit": limit}
        if wait is not None:
            query["wait"] = int(wait * 1000)
        q = urllib.parse.urlencode({k: v for k, v in query.items() if v is not None})
        status, got, raw = self._send(
            "GET", f"{self._base}/_changes?{q}", None, self._auth(), _more(self.timeout, wait), True
        )
        if not 200 <= status < 300:
            _answer(status, raw)
        nxt = int(got.get("Fennec-Next", since or 0))
        seq = max(int(got.get("Fennec-Seq", 0)), nxt)
        return Changes([json.loads(line) for line in raw.splitlines() if line], nxt, seq)

    def consumer(self, name: str, since: int | None = None) -> dict:
        """Makes `name` a consumer at the last write on disk, or at `since`,
        and returns where it stands, `{"name", "since"}`. One that is there
        already stays where it stands -- `since` is a new consumer's start
        -- so a program can call this every time it starts and go on from
        where it stopped; a `since` ahead of where it stands is a
        `FennecError` (409), a commit sent the old way. `move_consumer`
        moves one."""
        body = json.dumps({} if since is None else {"since": since}).encode()
        return self._post(f"/_changes/consumers/{_seg(name)}", body, "application/json")

    def move_consumer(self, name: str, since: int) -> dict:
        """Moves `name` to `since`: on, once it has done with every write up
        to it (a commit), or back, to read from there again, and returns
        where it stands after -- where it was, for a commit past nothing but
        consumers' own commits, which reads the same writes. A consumer that
        is not there is a `FennecError` (404): `consumer` makes one."""
        body = json.dumps({"since": since}).encode()
        headers = {**self._auth(), "Content-Type": "application/json"}
        status, _, raw = self._send("PATCH", f"{self._base}/_changes/consumers/{_seg(name)}", body, headers)
        return _answer(status, raw)

    def consumers(self) -> list:
        """Each consumer: its `name`, `since`, and how many writes `behind`."""
        status, _, raw = self._send("GET", f"{self._base}/_changes/consumers", None, self._auth(), twice=True)
        return _answer(status, raw)

    def forget_consumer(self, name: str) -> None:
        status, _, raw = self._send("DELETE", f"{self._base}/_changes/consumers/{_seg(name)}", None, self._auth())
        _answer(status, raw)

    def follow(self, consumer: str, *, limit: int = 1000, wait: float = 10.0) -> Iterator[list]:
        """Each batch of writes for `consumer`, made if it is new, committed
        once the loop asks for the next: a batch the loop did not finish is
        read again, so each write comes at least once. Started again, it
        goes on from the last batch committed."""
        self.consumer(consumer)
        while True:
            got = self.changes(consumer=consumer, limit=limit, wait=wait)
            # Nothing read, nothing committed: a commit is a write, which
            # the stream passes over, and committing past it every time
            # would write once a wait.
            if got.writes:
                yield got.writes
                self.move_consumer(consumer, got.next)

    def _auth(self) -> dict:
        return {"Authorization": f"Bearer {self.token}"} if self.token else {}

    def _post(
        self,
        path: str,
        body: bytes,
        content_type: str,
        after: int | None = None,
        accept: int = 0,
        key: str | None = None,
        wait: float | None = None,
        read: bool = False,
    ) -> Any:
        headers = {"Content-Type": content_type}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        near = read and self.reads == "nearest"
        if near:
            sent = self._shared.at
            _near(headers, sent, after)
        elif after is not None:
            headers["Fennec-After"] = str(after)
        key = key or self.idempotency_key
        if key:
            headers["Idempotency-Key"] = key
        if wait:
            headers["Fennec-Wait"] = _wait_ms(wait)
        timeout = _more(self.timeout, wait)
        # What may run twice, sent again after a reset too: a read the
        # builder made (`_BUILT`), one sent near, which every server runs
        # read-only (`Fennec-Read`), and a write under a key. The caller's
        # `read=True` is a claim nothing checks at home: a call it names
        # may have been made to write since.
        twice = read is _BUILT or bool(key)
        status, got, raw = self._send("POST", self._base + path, body, headers, timeout, twice or near)
        # 504: no copy the router reached holds that write in the wait --
        # the write lost in a failover of home, which says where it stands
        # now: the client's position from here on. Asked home with no mark,
        # the read is answered with what home holds.
        if near and status == 504 and "Fennec-After" in headers:
            # A change the caller named is not the client's position.
            if after is None and sent is not None:
                self._shared.told(raw, sent)
            for h in ("Fennec-Read", "Fennec-After", "Fennec-History"):
                headers.pop(h, None)
            near = False
            status, got, raw = self._send("POST", self._base + path, body, headers, timeout, twice)
        if 200 <= status < 300:
            # A replayed batch's answer carries no `Fennec-Seq`: `seq` stays
            # where the last write that sent one left it. A read near's
            # answer may be a replica's: it moves nothing.
            seq = got.get("Fennec-Seq")
            if seq is not None and not near:
                self._shared.answered(int(seq), got.get("Fennec-History"))
            self._shared.replayed = got.get("Idempotent-Replayed") == "true"
        # An answer that is no error: a schema's refusals (409).
        if status == accept:
            return json.loads(raw)
        return _answer(status, raw)

    def _send(
        self,
        method: str,
        path: str,
        body: bytes | None,
        headers: dict,
        timeout: float | None = None,
        twice: bool = False,
    ) -> tuple:
        """A request, its status, headers and body, sent again by the rule
        the router keeps: only what cannot have run, or may run twice. An
        answer naming its wait -- a 503, a quota's 429 -- ran nothing, unless
        it says statements of a batch were made (`_wait_of`), nor did a
        request whose connection was refused or that was not handed to the
        system whole: any request goes again, after that wait and never
        sooner. `twice` -- a read the builder made or sent near, a write
        under a key -- goes again after a reset or a dropped connection too,
        which a write may have landed before. Nothing goes again after the
        client's own timeout, past which the server may still run it, nor a
        refusal naming no wait; past the `Retry`'s tries or its waits, the
        last answer or error is the caller's."""
        tries, waited = 0, 0.0
        while True:
            tries += 1
            error = None
            try:
                status, got, raw = self._once(method, path, body, headers, timeout)
                # Headers asked of a 429 or a 503 alone: an `HTTPMessage`'s
                # `get` walks every header.
                wait = _wait_of(status, raw, got.get("Retry-After")) if status in (429, 503) else None
                if wait is None:
                    return status, got, raw
            except TimeoutError:
                # The client's own: a `compact`, a long read may still be
                # running, and three tries ran a read three times.
                raise
            except _Unsent as e:
                error, wait = e.__cause__, 0.0
            except (OSError, http.client.HTTPException) as e:
                if not (twice or isinstance(e, ConnectionRefusedError)):
                    raise
                error, wait = e, 0.0
            pause = _pause(self.retry, tries, waited, wait)
            if pause is None:
                if error is not None:
                    raise error
                return status, got, raw
            waited += pause
            time.sleep(pause)

    def _once(
        self, method: str, path: str, body: bytes | None, headers: dict, timeout: float | None
    ) -> tuple:
        """One try over this thread's connection: the connect and the
        answer's head within `timeout` -- each wait given what is left of it
        -- and each piece of its body within `self.timeout` of the last, so
        a long answer is read for as long as it comes and one that stops is
        given up. Bounded whole, a long read failed however steadily it
        came. A kept connection the server closed while it was idle is found
        so before the request goes -- its socket reads as ready, the end of
        the stream -- and a new one is opened in its place: a try on it
        would fail as though the request may have run."""
        limit = self.timeout if timeout is None else timeout
        end = None if limit is None else time.monotonic() + limit

        def left() -> float | None:
            return None if end is None else end - time.monotonic()

        conn = getattr(self._local, "conn", None)
        if conn is not None and _closed(conn):
            conn.close()
            conn = None
        if conn is None:
            conn = self._connect()
        try:
            if conn.sock is None:
                conn.timeout = limit
                conn.connect()
            sock = conn.sock
            _bound(sock, left())
            try:
                conn.request(method, path, body, headers)
            except TimeoutError:
                # A server that stopped reading: sent again, it would wait as
                # long ([`TimeoutError`] in `_send`).
                raise
            except OSError as e:
                # Not handed to the system whole, as the router judges it:
                # the server read no request to run.
                raise _Unsent(e) from e
            # A small request goes at once, the bound before it as good for
            # the head; a large one may take its time going.
            if body is not None and len(body) > 65536:
                _bound(sock, left())
            resp = conn.getresponse()
            # `read` gives each wait for a piece of the body the socket's
            # timeout: the timeout between pieces.
            _bound(sock, self.timeout)
            raw = resp.read()
        except BaseException:
            conn.close()
            self._local.conn = None
            raise
        if resp.will_close:
            conn.close()
            self._local.conn = None
        else:
            self._local.conn = conn
        return resp.status, resp.msg, raw

    def _connect(self) -> http.client.HTTPConnection:
        if self._tls:
            return http.client.HTTPSConnection(
                self._host, self._port, timeout=self.timeout, context=ssl.create_default_context()
            )
        return http.client.HTTPConnection(self._host, self._port, timeout=self.timeout)

    def close(self) -> None:
        """Closes this thread's connection; the next request opens another."""
        conn = getattr(self._local, "conn", None)
        if conn is not None:
            conn.close()
            self._local.conn = None


class _Unsent(Exception):
    """A request not handed to the system whole: its cause, the error that
    stopped it, is what the caller meets once no try is left."""


def _more(timeout: float | None, wait: float | None) -> float | None:
    """A request's timeout, and the `wait` the server may hold it on top."""
    return None if timeout is None else timeout + (wait or 0)


def _wait_ms(wait: float) -> str:
    """`Fennec-Wait` for `wait` seconds: milliseconds, as the header is."""
    if isinstance(wait, bool) or not isinstance(wait, (int, float)) or wait < 0:
        raise ValueError(f"wait is seconds, a number from 0 (got {wait!r})")
    return str(int(wait * 1000))


def _seg(name: str) -> str:
    return urllib.parse.quote(name, safe="")


def _faceted(answer: Any) -> Any:
    """A `/query` answer as it came, but for a query that asked facets:
    the server sends `{"rows": [...], "facets": {...}}` for it rather than
    the bare array, and that becomes `Rows` -- a list of the rows, as every
    other read answers, with the counts as its `facets`. A `/batch` answer
    is left as it came, each read's result `{"rows": ..., "facets": ...}`."""
    if isinstance(answer, dict) and "facets" in answer and isinstance(answer.get("rows"), list):
        return _rows(answer)
    return answer


def _closed(conn: http.client.HTTPConnection) -> bool:
    """Whether a kept connection can no longer take a request: the server
    closed it (its socket reads as ready, at the end of the stream) or sent
    what no request asked for. Between requests nothing is due on it."""
    sock = conn.sock
    if sock is None:
        return True
    try:
        ready, _, _ = select.select([sock], [], [], 0)
    except (OSError, ValueError):
        return True
    return bool(ready)


def _answer(status: int, raw: bytes) -> Any:
    """A response's JSON, or the error it says, as `Client` raises it."""
    if 200 <= status < 300:
        return json.loads(raw) if raw else None
    at = completed = quota = retry_after = outcome = seq = history = reason = binding = None
    error_code = None
    try:
        body = json.loads(raw)
        message = body.get("error", raw.decode(errors="replace"))
        at, completed = body.get("at"), body.get("completed")
        # What a `require n else '<reason>'` that refused said, a batch's
        # stop's beside its `at`.
        reason = body.get("reason")
        # The statement bound to the write that refused it, by name.
        binding = body.get("binding")
        # The body's own `code`, which a client tells "refresh the token"
        # by, never by the message's words; only a non-empty string counts.
        code = body.get("code")
        if isinstance(code, str) and code:
            error_code = code
        # A quota's refusal says which, and the wait its `Retry-After`
        # says -- a quota's 429, a move's 503 -- in the body too: what is
        # read here has no headers. To the millisecond where it says it: a
        # bucket of 600 rows a second refills a row in 1.7 ms.
        quota, retry_after = body.get("quota"), body.get("retry_after")
        ms = body.get("retry_after_ms")
        if isinstance(ms, (int, float)) and ms > 0:
            retry_after = ms / 1000
        # A write a tenant's node answered after its lease lapsed: neither
        # made nor refused, its position for a read once the tenant settles.
        if body.get("outcome") == "unknown":
            outcome, seq, history = "unknown", body.get("seq"), body.get("history")
    except (ValueError, AttributeError):
        message = raw.decode(errors="replace")
    raise FennecError(
        message,
        status,
        at=at,
        completed=completed,
        quota=quota,
        retry_after=retry_after,
        outcome=outcome,
        seq=seq,
        history=history,
        reason=reason,
        binding=binding,
        error_code=error_code,
    )


# The async client writes its request head as joined text, so the check
# `http.client` makes for the sync client is its own to make: a CR, LF or NUL
# in a value would end the line, or the head, and let what follows pose as a
# header or a body. Stricter than `http.client`, which lets a folded line
# through; nothing here needs one.
_ILLEGAL_HEADER_VALUE = re.compile(r"[\r\n\x00]")


def _header_value(name: str, value: Any) -> Any:
    """`value`, or a `ValueError` naming the header (never the value's bytes)
    when it holds a CR, LF or NUL."""
    if isinstance(value, str) and _ILLEGAL_HEADER_VALUE.search(value):
        raise ValueError(f"the {name} header's value holds a CR, LF or NUL")
    return value


class AsyncClient:
    """fennec-server's HTTP endpoint over asyncio: `Client`'s calls, awaited.

    One connection, kept alive and opened again when the server closed it;
    calls made at once on one client go one after another over it, as they
    would over one `Client` -- a client each for requests side by side.
    `https://` for a server behind a TLS terminator. The standard library
    alone: asyncio's streams and a request written out by hand, the
    endpoint's answers being a status, a length and JSON. `timeout` and
    `retry` are `Client`'s.
    """

    def __init__(
        self,
        url: str = "http://127.0.0.1:8080",
        token: str | None = None,
        timeout: float | None = 30.0,
        reads: str = "home",
        retry: Retry | bool = True,
    ):
        parts = urllib.parse.urlsplit(url)
        if parts.scheme not in ("http", "https"):
            raise ValueError(f"not an http(s) URL: {url!r}")
        self.url = url.rstrip("/")
        self.token = token
        self.timeout = timeout
        self.retry = _retry(retry)
        self.reads = _reads(reads)
        self._host = parts.hostname or "127.0.0.1"
        self._port = parts.port or (443 if parts.scheme == "https" else 80)
        self._tls = ssl.create_default_context() if parts.scheme == "https" else None
        self._base = parts.path.rstrip("/")
        self.idempotency_key: str | None = None
        # The connection, its lock, `seq` and `replayed`, shared with the
        # copies `with_idempotency_key` makes: one connection still.
        self._s = _AsyncShared()

    @property
    def seq(self) -> int | None:
        """The change the last write left the database at, as `Client.seq`."""
        return self._s.seq

    @property
    def replayed(self) -> bool:
        """Whether the last write's answer was replayed, as `Client.replayed`."""
        return self._s.replayed

    def with_idempotency_key(self, key: str) -> "AsyncClient":
        """A copy whose writes carry `key`, as `Client.with_idempotency_key`,
        over this client's connection."""
        if not isinstance(key, str) or not key:
            raise ValueError("an idempotency key is a text, not empty")
        _header_value("Idempotency-Key", key)
        copy = object.__new__(AsyncClient)
        copy.__dict__.update(self.__dict__)
        copy.idempotency_key = key
        return copy

    def collection(self, name: str) -> AsyncQuery:
        """The query builder, as `Client.collection`, its endpoints awaited."""
        return AsyncQuery(name, self)

    async def call(
        self,
        name: str,
        args: Mapping[str, Any] | None = None,
        *,
        claims: Mapping[str, Any] | None = None,
        idempotency_key: str | None = None,
        wait: float | None = None,
        read: bool = False,
    ) -> Any:
        """A named statement's call, as `Client.call`, awaited; a held one is
        cancelled as any task is, its connection closed with it."""
        text, params = Call(name, args, claims=claims).to_call()
        return await self.query(text, params, idempotency_key=idempotency_key, wait=wait, read=read)

    async def statements(self) -> list[NamedStatement]:
        """The named statements the database holds, as `Client.statements`."""
        return [_named(r) for r in await self.query("statements")]

    async def query(
        self,
        fennecql: str,
        params: Sequence[Any] | None = None,
        *,
        idempotency_key: str | None = None,
        wait: float | None = None,
        read: bool = False,
    ) -> Any:
        """One FennecQL statement, as `Client.query`; a held one is
        cancelled as any task is, its connection closed with it."""
        body = {"query": fennecql, "params": list(params or [])}
        return _faceted(
            await self._post(
                "/query", json.dumps(body).encode(), "application/json", idempotency_key, wait, read
            )
        )

    async def batch(
        self,
        statements: Iterable[tuple[str, Sequence[Any]]],
        *,
        idempotency_key: str | None = None,
        wait: float | None = None,
        read: bool = False,
    ) -> Any:
        """Statements under one write lock, as one block, as `Client.batch`."""
        lines = [json.dumps({"query": q, "params": list(p)}) for q, p in statements]
        return await self._post(
            "/batch", "\n".join(lines).encode(), "application/x-ndjson", idempotency_key, wait, read
        )

    async def close(self) -> None:
        if self._s.conn:
            _, writer = self._s.conn
            self._s.conn = None
            writer.close()
            try:
                await writer.wait_closed()
            except OSError:
                pass

    async def __aenter__(self) -> "AsyncClient":
        return self

    async def __aexit__(self, *exc: Any) -> None:
        await self.close()

    async def _post(
        self,
        path: str,
        body: bytes,
        content_type: str,
        key: str | None = None,
        wait: float | None = None,
        read: bool = False,
    ) -> Any:
        key = key or self.idempotency_key
        _header_value("Idempotency-Key", key)
        held = _wait_ms(wait) if wait else None
        # As `Client._post`'s: what may run twice -- a read the builder made,
        # one sent near, a write under a key -- goes again after a reset too.
        twice = read is _BUILT or bool(key)
        limit = _more(self.timeout, wait)
        async with self._s.lock:
            # A read near, as `Client`'s: marked, after this client's
            # furthest position, and asked home on a 504, taking home's.
            near: dict = {}
            if read and self.reads == "nearest":
                sent = self._s.at
                _near(near, sent, None)
            tries, waited = 0, 0.0
            while True:
                tries += 1
                error = None
                try:
                    status, headers, raw = await self._try(path, body, content_type, key, held, near, limit)
                    if raw is None:
                        raw = await self._body(headers)
                    if status == 504 and "Fennec-After" in near:
                        self._s.told(raw, sent)
                        near = {}
                        status, headers, raw = await self._try(path, body, content_type, key, held, near, limit)
                        if raw is None:
                            raw = await self._body(headers)
                    wait_s = _wait_of(status, raw, headers.get("retry-after"))
                    if wait_s is None:
                        if 200 <= status < 300:
                            if "fennec-seq" in headers and not near:
                                self._s.answered(int(headers["fennec-seq"]), headers.get("fennec-history"))
                            self._s.replayed = headers.get("idempotent-replayed") == "true"
                        return _answer(status, raw)
                except (TimeoutError, asyncio.TimeoutError):
                    # The client's own, as `Client._send`'s: never again.
                    await self.close()
                    raise
                except (OSError, asyncio.IncompleteReadError) as e:
                    await self.close()
                    # Refused, at every address of the host's: nothing went,
                    # and any request goes again.
                    if not (twice or near or _refused(e)):
                        raise
                    error, wait_s = e, 0.0
                except asyncio.CancelledError:
                    # Cancelled under way -- a held claim given up: its
                    # answer, still to come, must not be the next request's.
                    self._drop()
                    raise
                pause = _pause(self.retry, tries, waited, wait_s)
                if pause is None:
                    if error is not None:
                        raise error
                    return _answer(status, raw)
                waited += pause
                await asyncio.sleep(pause)

    def _try(
        self,
        path: str,
        body: bytes,
        content_type: str,
        key: str | None,
        held: str | None,
        extra: dict,
        limit: float | None,
    ) -> Any:
        """One try, to be awaited: the connect, the answer's head and a body
        in hand with it within `limit` -- a body still to come is left to
        `_body`, None in its place. A kept connection the server closed
        while it was idle is let go of before the request goes, as `Client`
        finds one: a try on it would fail as one that may have run. Not a
        coroutine of its own: one more a request cost 0.4 us."""
        if self._s.conn is not None and _gone(*self._s.conn):
            self._drop()
        return asyncio.wait_for(self._exchange(path, body, content_type, key, held, extra), limit)

    def _drop(self) -> None:
        if self._s.conn:
            _, writer = self._s.conn
            self._s.conn = None
            writer.close()

    async def _exchange(
        self,
        path: str,
        body: bytes,
        content_type: str,
        key: str | None,
        held: str | None,
        extra: dict,
    ) -> tuple[int, dict, bytes | None]:
        """The request sent and the answer's head read (`_head`), and its
        body where the loop holds it whole already -- a head and a body the
        server wrote at once, as it writes every answer: nothing to wait
        for. Any other body is `_body`'s, read past the head's bound a piece
        at a time: one more coroutine and a timer the loop holds cost every
        request 3.5 us, 6%."""
        status, headers = await self._head(path, body, content_type, key, held, extra)
        n = headers.get("content-length")
        reader = self._s.conn[0]
        # CPython's buffer, and uvloop's, which runs asyncio's streams: a
        # reader without one has its body read by `_body`.
        if n is not None and len(getattr(reader, "_buffer", b"")) >= int(n):
            raw = await reader.readexactly(int(n))
            if headers.get("connection", "").lower() == "close":
                await self.close()
            return status, headers, raw
        return status, headers, None

    async def _body(self, headers: dict) -> bytes:
        """The body of the answer whose `headers` were read, each piece
        within the timeout of the last (`_Idle`): a long answer is read for
        as long as it comes, one that stops is given up, its connection
        with it."""
        reader, writer = self._s.conn
        idle = _Idle(writer, self.timeout)
        try:
            if "content-length" in headers:
                raw = await _exactly(reader, int(headers["content-length"]), idle)
            elif headers.get("transfer-encoding", "").lower() == "chunked":
                parts = []
                while True:
                    idle.tick()
                    size = int((await reader.readuntil(b"\r\n")).split(b";")[0], 16)
                    if not size:
                        break
                    parts.append(await _exactly(reader, size + 2, idle))
                idle.tick()
                await reader.readuntil(b"\r\n")
                raw = b"".join(p[:-2] for p in parts)
            else:
                parts = []
                while True:
                    idle.tick()
                    part = await reader.read(1 << 16)
                    if not part:
                        break
                    parts.append(part)
                raw = b"".join(parts)
                self._s.conn = None
        except (OSError, asyncio.IncompleteReadError):
            if idle.late:
                raise asyncio.TimeoutError(f"no piece of the answer within {self.timeout} s") from None
            raise
        finally:
            idle.stop()
        if headers.get("connection", "").lower() == "close" and self._s.conn:
            await self.close()
        return raw

    async def _head(
        self,
        path: str,
        body: bytes,
        content_type: str,
        key: str | None,
        held: str | None,
        extra: dict,
    ) -> tuple[int, dict]:
        """The request sent, over a connection opened where none is kept,
        and the answer's status and headers."""
        # Before a connection is opened or a byte written.
        _header_value("Authorization", self.token)
        _header_value("Idempotency-Key", key)
        _header_value("Fennec-Wait", held)
        for name, value in extra.items():
            _header_value(str(name), value)
        if self._s.conn is None:
            self._s.conn = await asyncio.open_connection(
                self._host, self._port, ssl=self._tls, server_hostname=self._host if self._tls else None
            )
        reader, writer = self._s.conn
        head = [
            f"POST {self._base}{path} HTTP/1.1",
            f"Host: {self._host}:{self._port}",
            f"Content-Type: {content_type}",
            f"Content-Length: {len(body)}",
            "Connection: keep-alive",
        ]
        if self.token:
            head.append(f"Authorization: Bearer {self.token}")
        if key:
            head.append(f"Idempotency-Key: {key}")
        if held:
            head.append(f"Fennec-Wait: {held}")
        for name, value in extra.items():
            head.append(f"{name}: {value}")
        writer.write(("\r\n".join(head) + "\r\n\r\n").encode() + body)
        await writer.drain()

        status_line = await reader.readuntil(b"\r\n")
        status = int(status_line.split(b" ", 2)[1])
        headers: dict[str, str] = {}
        while (line := await reader.readuntil(b"\r\n")) != b"\r\n":
            name, _, value = line.decode("latin-1").partition(":")
            headers[name.strip().lower()] = value.strip()
        return status, headers


class _Idle:
    """The clock of an answer's body: `secs` with nothing read, and its
    connection is let go of (`late`), the read under way meeting its end. A
    timer the loop holds, where `wait_for` made a task a wait before 3.12."""

    __slots__ = ("secs", "writer", "loop", "handle", "late")

    def __init__(self, writer: asyncio.StreamWriter, secs: float | None):
        self.secs, self.writer, self.handle, self.late = secs, writer, None, False
        self.loop = asyncio.get_running_loop()

    def tick(self) -> None:
        """Starts the clock again: a piece came, or one is to be read."""
        if self.handle is not None:
            self.handle.cancel()
        if self.secs is not None:
            self.handle = self.loop.call_later(self.secs, self._late)

    def _late(self) -> None:
        self.late = True
        self.writer.transport.abort()

    def stop(self) -> None:
        if self.handle is not None:
            self.handle.cancel()


async def _exactly(reader: asyncio.StreamReader, n: int, idle: _Idle) -> bytes:
    """`n` bytes of `reader`, each wait for a piece within `idle`'s clock."""
    parts = []
    while n:
        idle.tick()
        part = await reader.read(n)
        if not part:
            raise asyncio.IncompleteReadError(b"".join(parts), None)
        parts.append(part)
        n -= len(part)
    return parts[0] if len(parts) == 1 else b"".join(parts)


def _gone(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> bool:
    """Whether a kept stream can no longer take a request: its end read, or
    an error, as the loop met them. The loop reads a server's close as it
    comes whenever it waits, as an idle one does; asking the socket as
    `_closed` does was 1 us a request more, for a loop blocked past the
    close alone -- whose write without a key then fails, never sent twice."""
    return reader.at_eof() or reader.exception() is not None or writer.is_closing()


class _AsyncShared(_Shared):
    """An `AsyncClient`'s connection and what its copies share with it."""

    __slots__ = ("conn", "lock")

    def __init__(self) -> None:
        super().__init__()
        self.conn: tuple[asyncio.StreamReader, asyncio.StreamWriter] | None = None
        self.lock = asyncio.Lock()


def placeholders(start: int, n: int) -> str:
    """`$start, ..., $(start+n-1)`: a list literal's worth of parameters.
    FennecQL binds a parameter to a value, not to a list, so `in` takes one
    per element -- `in [$2, $3, $4]`."""
    return ", ".join(f"${i}" for i in range(start, start + n))


# Below FennecError, which the builder raises.
from .builder import (  # noqa: E402
    AsyncQuery,
    Call,
    Computed,
    Cond,
    FacetCount,
    Param,
    Query,
    Rows,
    _BUILT,
    _rows,
    and_,
    bucket,
    call,
    collection,
    count_distinct,
    distance,
    expr,
    first,
    inc,
    last,
    not_,
    or_,
    param,
    raw,
)
