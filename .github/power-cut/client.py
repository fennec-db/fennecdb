#!/usr/bin/env python3
"""The load a power cut lands in, and the check after it.

The host's side of the power-cut tests, Windows' and Linux's
(docs/design/windows.md#the-power-cut, docs/design/durability.md#a-power-cut):
the guest's fennec-server reached through QEMU's port forward. The standard
library alone: it runs on the runner beside QEMU, and nothing in it is either
system's.

    client.py setup   --state DIR
    client.py load    --state DIR --cut N --seed S
    client.py check   --state DIR --cut N --label plain|lost [--commit]
    client.py replica --state DIR --cut N --replica-url URL

`load` writes until the server stops answering -- the cut -- with a thread
each: single puts, `/batch`es of 8 with an `Idempotency-Key`'d write between
them, a document written over and over, a `/batch` past 16 MiB (a spill)
written over half a second after the last, and a `compact` a fifth of a
second after the last (`--ops`, a letter a thread: `ssbcxk`).
Every write is logged before it is sent and again once a 2xx answers it, a
line each, flushed: DIR/events-N.jsonl, the host's record of what the server
acknowledged, kept outside the guest.

`check` reads the database back and holds it to that log and to what the
checks before it found (DIR/state.json): every acknowledged write there and
whole; a write in flight at the cut there whole or not at all; a batch, and
a spill, all there or none of it; every acknowledged `Idempotency-Key`
answered again as it was, `Idempotent-Replayed`, never made twice; and a new
write taken. A cut that came with no write in flight landed between writes
and held none of that to a write cut short: it fails as the rig's (`RIG`).
`--commit` makes what it found the state the next cut is held to. The token
is read from --token-file, never printed.

`--sync-ms 250` holds a server under `--sync 250`, which answers a write
before it is on disk, to what that promises instead: a thread more reads
`/_changes` (the server under `--cdc`), which hands a consumer only what an
fsync has covered, and notes each page as it comes. Every write a page held
must be there after the cut; an acknowledged write may be gone only if no
page held it, and only if it was answered within 250 ms and two syncs' own
time of the cut, a fixed bound (`SYNC_OWN`); and no page may come later
than `GAP_CEILING` after the one before it, nor the cut after the last
(`Check.window`). Neither number is read from the pages being judged. What
the cut kept is a prefix of the writes, by the change each was answered as.
The load leaves out the spills and the compacts there: its durability is
the syncer's, not a compact's, and a 20 MiB page would hold the reader up
past the next sync.

`--prefix /t/<tenant>` writes to a `--dir` node's tenant, `--name` keeping
its files apart (DIR/state-<name>.json, DIR/events-N-<name>.jsonl). `replica`
reads a replica of it as the cut left it (DIR/replica-N-<name>.json), and
`check --replica-url` holds that to the primary as it came back -- the
replica holds nothing the primary lost, and was near the tip when the
primary went (`STANDBY_LAG`), the acknowledged writes it lacked counted:
what a promotion at the cut would have lost -- then waits for the replica
to reach the primary and finds the two the same.

    python3 test_client.py   # the check's own tests, Check.window's among them
"""

import argparse
import bisect
import hashlib
import http.client
import json
import os
import random
import sys
import threading
import time

SINGLE_LEN = 300
BATCH_LEN = 600
BATCH_SIZE = 8
CHURN_IDS = 256
CHURN_LEN = 2048
# 40 documents of 512 KiB: 20 MiB a block, past SPILL_AT's 16 MiB, so the
# block spills (kind 10) before it lands (kind 11).
SPILL_DOCS = 40
SPILL_LEN = 512 * 1024
# Paced so that most cuts land in a compact and many in a spill: a compact
# took 0.03 to 1.4 s and a spill 0.4 to 0.6 s in run 2, whose pauses of
# 0.5 s and 2 s left a compact in flight at three cuts of five and a spill
# at none.
SPILL_EVERY = 0.5
COMPACT_EVERY = 0.2
IDEM_EVERY = 3  # an Idempotency-Key'd write after every third batch
BASE = 10_000_000  # a cut's ids and versions: N * BASE + its own count
# The threads `load` runs, a letter each: single puts (two lanes), batches
# with keyed writes, the rewrites, spills and compacts. Under `--sync <ms>`
# the spills and compacts are left out (the module's docstring).
OPS = "ssbcxk"
OPS_SYNCED = "ssbc"
# `/_changes` pages: as long a wait as the server allows, and as many writes
# a page as it gives, so that a page is one sync's writes.
CHANGES_WAIT_MS = 30_000
CHANGES_LIMIT = 10_000
# `--sync <ms>`'s window is held to fixed numbers, never to what the pages
# being judged show: a bound read from their gaps grew with any stall, and a
# syncer stopped 3 s before the cut, its acknowledged writes lost from 3.0 s
# before it, passed against a bound of 3 058 ms; one syncing every second,
# its writes lost from 907 ms before the cut, against 1 708 ms (#627's
# review; test_client.py holds both, failed).
#
# A sync's own time: the syncer's wake after its sleep, the writes taken
# under the write lock and the fsync after it (`run_syncer`). Measured from
# the pages of run 43438990710's five cuts, 257 to 259 ms apart at the
# median and 261 to 271 ms at most: 250 ms of sleep and 7 to 9 ms of
# syncing at the median, 21 ms at most. Allowed 50 ms, more than twice the
# longest: a write gone that was answered past the bound, 250 ms and two
# syncs' own time (`Check.window`), is a sync that came late or did not
# cover it. The 29 ms a sync is allowed past the longest seen, 58 ms over
# the two, is the bound's only slack.
SYNC_OWN = 0.05
# The longest a page may come after the one before it, after the first
# acknowledged write, or the cut after the last page: one sync's period is
# 250 ms and its own time, and a page seen during the reader's turn later
# by as much as the turn (34 ms at most in that run, the longest gap 271
# ms). A second is four times the interval and nearly four times the
# longest gap seen, so a gap past it is no runner's slow moment but a sync
# 750 ms late: a syncer stopped, stuck or slowed past it. A slower syncer
# under it is held by the loss bound, whenever a cut takes a write answered
# past that.
GAP_CEILING = 1.0
# How far behind the node's tip the standby may be at a cut: it is held to
# the node only as far as it had followed it, so one far behind would pass
# its check for that alone. A durable write is answered once every stream
# keeping up has put it into its socket; a stream that has not within a
# second is let lag, and not waited for until it catches up (`SENT_WAIT`,
# replication.rs). So a standby keeping up lacks at a cut only what the
# guest's sockets held when it went, and one lacking a write answered a
# second before the cut is one the node had stopped waiting for. Its lag is
# how long before the cut the oldest acknowledged write it lacked was
# answered. Run 43438990710's standby was 0 to 51 changes behind the node
# as it came back, the 40 of a spill in flight at the cut among them.
STANDBY_LAG = 1.0
# How long a replica is waited for to reach the primary after a check: four
# waits of `Fennec-Wait`'s 30 s, the most a read may wait.
REPLICA_WAITS = 4
COLLECTIONS = ("docs", "churn", "spill", "idem", "probe")


def text(*seed, n):
    """A document's text, made again from what names it: the check needs no
    copy of what was written."""
    h = hashlib.sha256(repr(seed).encode()).hexdigest()
    return (h * (n // len(h) + 1))[:n]


class Server:
    """One keep-alive connection to the server; a thread each. `prefix` is a
    tenant's path on a `--dir` node, `/t/<tenant>`, or nothing."""

    def __init__(self, url, token, timeout=120, prefix=""):
        host, _, port = url.removeprefix("http://").partition(":")
        self.host, self.port = host, int(port or 80)
        self.token = token
        self.timeout = timeout
        self.prefix = prefix
        self.cx = None

    def request(self, method, path, body=b"", headers=None, timed=False):
        """(status, headers, body), and with `timed` the time the answer's
        head came, before its body was read."""
        if self.cx is None:
            self.cx = http.client.HTTPConnection(self.host, self.port, timeout=self.timeout)
        h = {"Authorization": f"Bearer {self.token}", "Content-Type": "application/json"}
        h.update(headers or {})
        try:
            self.cx.request(method, path, body=body, headers=h)
            r = self.cx.getresponse()
            at = time.time()
            data = r.read()
            out = (r.status, {k.lower(): v for k, v in r.getheaders()}, data)
            return out + (at,) if timed else out
        except BaseException:
            self.cx.close()
            self.cx = None
            raise

    def query(self, q, params=(), headers=None):
        body = json.dumps({"query": q, "params": list(params)}).encode()
        return self.request("POST", self.prefix + "/query", body, headers)

    def batch(self, lines):
        body = "\n".join(json.dumps({"query": q, "params": list(p)}) for q, p in lines).encode()
        return self.request("POST", self.prefix + "/batch", body)

    def rows(self, q, params=()):
        status, _, data = self.query(q, params)
        if status != 200:
            raise SystemExit(f"`{q}` answered {status}: {data[:300]!r}")
        out = json.loads(data)
        return out["rows"] if isinstance(out, dict) else out

    def seq(self):
        """The last change the database holds, on disk or not yet, and its
        history, as `/_changes` answers them: a read's answer carries
        neither. Needs a feed: `--cdc`, or a tenant of a node with
        `--replication-token`."""
        status, h, data = self.request("GET", self.prefix + "/_changes?wait=0&limit=1")
        if status != 200:
            raise SystemExit(f"/_changes answered {status}: {data[:300]!r}")
        return int(h["fennec-seq"]), h.get("fennec-history")


def token(args):
    with open(args.token_file) as f:
        return f.read().strip()


def named(args, kind, cut=None):
    """A file of the state directory: the state, a cut's events or a cut's
    replica, a tenant's apart by `--name`."""
    name = f"-{args.name}" if args.name else ""
    if kind == "state":
        return os.path.join(args.state, f"state{name}.json")
    if kind == "events":
        return os.path.join(args.state, f"events-{cut}{name}.jsonl")
    return os.path.join(args.state, f"replica-{cut}{name}.json")


def setup(args):
    s = Server(args.url, token(args), prefix=args.prefix)
    for q in (
        "create collection docs (w text, c int, b int, p text)",
        "create collection churn (v int, p text)",
        "create collection spill (v int, p text)",
        "create collection idem (k text, c int)",
        "create collection probe (c int, label text)",
    ):
        status, _, data = s.query(q)
        if status != 200:
            raise SystemExit(f"`{q}` answered {status}: {data[:300]!r}")
    with open(named(args, "state"), "w") as f:
        json.dump({"docs": {}, "churn": {}, "spill": None, "idem": []}, f)
    print(f"SETUP{' ' + args.prefix if args.prefix else ''} collections docs, churn, spill, idem, probe")


class Load:
    def __init__(self, args):
        self.args = args
        self.token = token(args)
        self.cut = args.cut
        self.out = open(named(args, "events", args.cut), "a", buffering=1)
        self.lock = threading.Lock()
        self.q = 0
        self.rng = random.Random(f"{args.seed}-{args.cut}")
        # Under `--sync <ms>` the writers wait for the reader to know where
        # the writes on disk stand, so that it is handed every one of theirs.
        self.ready = threading.Event()
        if not args.sync_ms:
            self.ready.set()
        with open(named(args, "state")) as f:
            self.state = json.load(f)

    def note(self, **kv):
        kv["t"] = round(time.time(), 4)
        line = json.dumps(kv)
        with self.lock:
            self.out.write(line + "\n")
            self.out.flush()

    def next_q(self):
        with self.lock:
            self.q += 1
            return self.q

    def server(self, timeout=120):
        return Server(self.args.url, self.token, timeout=timeout, prefix=self.args.prefix)

    def warm(self, s, op):
        """Opens the thread's connection with a `/_health` before its first
        write. Twice a thread lost its connection at its first request, 2 ms
        after sending it (run 2's cut 1, run 42667944461's cut 5), with the
        threads opening theirs together: QEMU's user-mode network forwards
        the port by accepting on the host first and dialing the guest after,
        so a refusal on the guest's side reaches the client only once it has
        written -- most likely that, though which side reset it was never
        seen. A read cannot have written anything, so a reset here is tried
        again and noted (the check counts them); a connection lost after
        its first answer is still a finding."""
        for attempt in range(1, 6):
            try:
                status, _, _ = s.request("GET", "/_health")
            except (OSError, http.client.HTTPException) as e:
                self.note(ev="warm-retry", op=op, attempt=attempt, what=f"{type(e).__name__}: {e}"[:200])
                time.sleep(0.2 * attempt)
                continue
            if status == 200:
                return True
            self.note(ev="warm-retry", op=op, attempt=attempt, what=f"/_health answered {status}")
            time.sleep(0.2 * attempt)
        self.note(ev="error", op=op, q=0, what="no connection answered /_health in 5 attempts")
        return False

    def run(self, op, send):
        """`send` makes one write and answers (status, headers, body); a 2xx
        is noted as acknowledged, with the change it landed as
        (`Fennec-Seq`), and anything else stops the thread: a refusal before
        the cut is a finding, and after it nothing answers."""
        s = self.server()
        if not self.warm(s, op):
            return
        if not self.ready.wait(120):
            self.note(ev="error", op=op, q=0, what="the /_changes reader had no cursor in 120 s")
            return
        while True:
            q = self.next_q()
            try:
                status, headers, body, fields = send(s, q)
            except (OSError, http.client.HTTPException) as e:
                self.note(ev="error", op=op, q=q, what=f"{type(e).__name__}: {e}"[:200])
                return
            if 200 <= status < 300:
                seq = headers.get("fennec-seq")
                self.note(ev="ack", op=op, q=q, status=status, seq=int(seq) if seq else None, **fields)
            else:
                self.note(ev="fail", op=op, q=q, status=status, body=body[:500].decode("utf-8", "replace"))
                return

    def single(self, lane):
        n = 0

        def send(s, q):
            nonlocal n
            n += 1
            i = self.cut * BASE + lane * 1_000_000 + n
            self.note(ev="send", op="s", q=q, id=i)
            st, h, b = s.query("put docs {id: $1, w: \"s\", c: $2, p: $3}", (i, self.cut, text("s", i, n=SINGLE_LEN)))
            return st, h, b, {}

        self.run("s", send)

    def batches(self):
        n = 0

        def send(s, q):
            nonlocal n
            n += 1
            if n % (IDEM_EVERY + 1) == 0:
                k = f"c{self.cut}-{n}"
                body = {"query": "put idem {k: $1, c: $2}", "params": [k, self.cut]}
                self.note(ev="send", op="i", q=q, k=k)
                st, h, b = s.request("POST", s.prefix + "/query", json.dumps(body).encode(), {"Idempotency-Key": k})
                return st, h, b, {"k": k, "answer": b.decode("utf-8", "replace")}
            bn = self.cut * BASE + n
            ids = [self.cut * BASE + 5_000_000 + n * BATCH_SIZE + j for j in range(BATCH_SIZE)]
            self.note(ev="send", op="b", q=q, b=bn, ids=ids)
            st, h, b = s.batch(
                [("put docs {id: $1, w: \"b\", c: $2, b: $3, p: $4}", (i, self.cut, bn, text("b", i, n=BATCH_LEN))) for i in ids]
            )
            return st, h, b, {}

        self.run("b", send)

    def churn(self):
        n = 0

        def send(s, q):
            nonlocal n
            n += 1
            i = self.rng.randint(1, CHURN_IDS)
            v = self.cut * BASE + n
            self.note(ev="send", op="c", q=q, id=i, v=v)
            st, h, b = s.query("put churn {id: $1, v: $2, p: $3}", (i, v, text("c", i, v, n=CHURN_LEN)))
            return st, h, b, {}

        self.run("c", send)

    def spill(self):
        n = 0

        def send(s, q):
            nonlocal n
            n += 1
            time.sleep(SPILL_EVERY if n > 1 else 1.0)
            v = self.cut * BASE + n
            lines = [("put spill {id: $1, v: $2, p: $3}", (i, v, text("x", i, v, n=SPILL_LEN))) for i in range(1, SPILL_DOCS + 1)]
            self.note(ev="send", op="x", q=q, v=v)
            st, h, b = s.batch(lines)
            return st, h, b, {}

        self.run("x", send)

    def compact(self):
        n = 0

        def send(s, q):
            nonlocal n
            n += 1
            time.sleep(COMPACT_EVERY)
            self.note(ev="send", op="k", q=q, n=n)
            st, h, b = s.query("compact")
            return st, h, b, {}

        self.run("k", send)

    def changes(self):
        """What the server hands a consumer as on disk: `/_changes`, a page
        at a time, each noted with the time its head came (`at`), the change
        it reaches (`next`, its `Fennec-Next`) and the writes in it -- a
        document's id, a rewrite's or a spill's version, a key. A page comes
        once an fsync covered its writes, so the pages are the syncs as the
        server made them. The first request, with no cursor, answers where
        the writes on disk stand before any of the load's."""
        s = self.server(timeout=CHANGES_WAIT_MS / 1000 + 60)
        if not self.warm(s, "d"):
            return
        try:
            status, h, body = s.request("GET", s.prefix + "/_changes?wait=0")
        except (OSError, http.client.HTTPException) as e:
            self.note(ev="error", op="d", q=0, what=f"{type(e).__name__}: {e}"[:200])
            return
        if status != 200:
            self.note(ev="fail", op="d", q=0, status=status, body=body[:500].decode("utf-8", "replace"))
            return
        since = int(h["fennec-next"])
        self.note(ev="cursor", next=since)
        self.ready.set()
        while True:
            sent = time.time()
            path = f"{s.prefix}/_changes?since={since}&limit={CHANGES_LIMIT}&wait={CHANGES_WAIT_MS}"
            try:
                status, h, body, at = s.request("GET", path, timed=True)
            except (OSError, http.client.HTTPException) as e:
                self.note(ev="error", op="d", q=0, what=f"{type(e).__name__}: {e}"[:200])
                return
            if status != 200:
                self.note(ev="fail", op="d", q=0, status=status, body=body[:500].decode("utf-8", "replace"))
                return
            nxt = int(h["fennec-next"])
            if nxt == since:
                continue
            writes = []
            for line in body.split(b"\n"):
                if not line.strip():
                    continue
                c = json.loads(line)
                if c.get("op") != "put":
                    continue
                d = c.get("doc") or {}
                col = c.get("collection")
                if col in ("docs", "probe"):
                    writes.append([c["seq"], col, c.get("id")])
                elif col in ("churn", "spill"):
                    writes.append([c["seq"], col, c.get("id"), d.get("v")])
                elif col == "idem":
                    writes.append([c["seq"], col, d.get("k")])
            self.note(ev="durable", sent=round(sent, 4), at=round(at, 4), next=nxt, seq=int(h.get("fennec-seq", nxt)), w=writes)
            since = nxt

    def start(self):
        workers = []
        lane = 0
        for o in self.args.ops or (OPS_SYNCED if self.args.sync_ms else OPS):
            if o == "s":
                workers.append(threading.Thread(target=self.single, args=(lane,)))
                lane += 1
            else:
                target = {"b": self.batches, "c": self.churn, "x": self.spill, "k": self.compact}.get(o)
                if target is None:
                    raise SystemExit(f"--ops: no thread `{o}`")
                workers.append(threading.Thread(target=target))
        if self.args.sync_ms:
            workers.insert(0, threading.Thread(target=self.changes))
        self.note(ev="start", cut=self.cut)
        for w in workers:
            w.start()
        for w in workers:
            w.join()
        self.note(ev="end", cut=self.cut)


def events(args, cut):
    out = []
    with open(named(args, "events", cut)) as f:
        for line in f:
            line = line.strip()
            if line:
                out.append(json.loads(line))
    return out


def by_id(rows):
    return {r["id"]: r for r in rows}


def digest(row):
    """A row as a short digest of its JSON: a replica's snapshot keeps these,
    not its rows, which ran to hundreds of MB over five cuts."""
    return hashlib.sha256(json.dumps(row, sort_keys=True, separators=(",", ":")).encode()).hexdigest()[:16]


def snapshot(s):
    """What the database at `s` holds, collection by collection: a row's
    digest by its id, with its version where it has one; the keys."""
    out = {}
    for c in COLLECTIONS:
        rows = s.rows(f"get {c}")
        if c == "idem":
            out[c] = sorted(r.get("k") for r in rows)
        elif c in ("churn", "spill"):
            out[c] = {str(r["id"]): [r.get("v"), digest(r)] for r in rows}
        else:
            out[c] = {str(r["id"]): digest(r) for r in rows}
    return out


class Check:
    def __init__(self, args):
        self.args = args
        self.tok = token(args)
        self.s = Server(args.url, self.tok, timeout=600, prefix=args.prefix)
        self.bad = []
        with open(named(args, "state")) as f:
            self.state = json.load(f)
        self.ev = events(args, args.cut)
        self.sent = {e["q"]: e for e in self.ev if e["ev"] == "send"}
        self.acked = {e["q"]: e for e in self.ev if e["ev"] == "ack"}
        # The host's clock just before `kill -9` and just after: a write
        # sent before the second may have reached the guest.
        self.cut_at = self.killed = None
        if args.cut_time:
            self.cut_at, self.killed = (float(t) for t in args.cut_time.split())
        # Under `--sync <ms>`: the pages `/_changes` handed over, and what
        # their writes were -- each there after the cut, whatever the
        # answers said.
        self.sync = args.sync_ms / 1000 if args.sync_ms else 0
        self.pages = sorted((e for e in self.ev if e["ev"] == "durable"), key=lambda e: e["at"])
        self.on_disk = {"docs": set(), "churn": {}, "spill": None, "idem": set()}
        for p in self.pages:
            for w in p["w"]:
                if w[1] == "docs":
                    self.on_disk["docs"].add(w[2])
                elif w[1] == "churn":
                    self.on_disk["churn"][w[2]] = max(w[3], self.on_disk["churn"].get(w[2], w[3]))
                elif w[1] == "spill":
                    self.on_disk["spill"] = max(w[3], self.on_disk["spill"] or w[3])
                elif w[1] == "idem":
                    self.on_disk["idem"].add(w[2])
        # Acknowledged writes the cut took, which only `--sync <ms>` allows:
        # (when its answer came, the change it landed as, what it was).
        self.lost = []
        # Acknowledged writes the cut kept: (the change it landed as, what).
        self.kept = []

    def fail(self, what):
        self.bad.append(what)
        if len(self.bad) <= 40:
            print(f"BAD {what}")

    def rig(self, what):
        """A cut that did not test what it was for -- the rig's failure,
        not the server's, and the cut's all the same: it held nothing."""
        self.bad.append(what)
        print(f"RIG {what}")

    def sends(self, op):
        return [e for e in self.sent.values() if e["op"] == op]

    def run(self):
        a = self.args
        print(f"CHECK cut {a.cut} ({a.label}){' ' + a.prefix if a.prefix else ''}")
        for e in self.ev:
            if e["ev"] == "fail":
                self.fail(f"the server refused a write before the cut: {e['op']} {e['status']} {e['body'][:200]}")
            # A connection the server dropped while it ran stops its thread
            # and leaves its write in flight to the end; one lost before its
            # first write is `Load.warm`'s, tried again and counted below.
            elif e["ev"] == "error" and self.cut_at is not None and e["t"] < self.cut_at:
                sent = self.sent.get(e["q"], {})
                self.fail(
                    f"a connection failed {self.cut_at - e['t']:.3f} s before the cut, "
                    f"{e['t'] - sent.get('t', e['t']):.3f} s after its write was sent: {e['op']} q={e['q']} {e['what']}"
                )
        warm = [e for e in self.ev if e["ev"] == "warm-retry"]
        if warm:
            print(f"WARM {len(warm)} first request(s) tried again before any write: " + "; ".join(f"{e['op']} {e['what']}" for e in warm[:6]))
        self.describe_load()
        found = {}
        # A read the server would not answer -- a tenant it could not open
        # among them -- ends the check as a finding, with its verdict.
        try:
            if a.replica_url:
                self.replica_before()
            found["docs"] = self.docs()
            found["churn"] = self.churn()
            found["spill"] = self.spill()
            found["idem"] = self.idem()
            self.probe()
            if self.sync:
                self.window()
            if a.replica_url:
                self.replica_after()
        except SystemExit as e:
            self.fail(f"the check could not go on: {e}")
        if self.bad:
            print(f"VERDICT cut {a.cut} {a.label}{' ' + a.prefix if a.prefix else ''} FAIL: {len(self.bad)} violations")
            sys.exit(1)
        print(f"VERDICT cut {a.cut} {a.label}{' ' + a.prefix if a.prefix else ''} PASS")
        if a.commit:
            with open(named(a, "state"), "w") as f:
                json.dump(found, f)

    def describe_load(self):
        """What the cut landed in: what was acknowledged, what was in flight,
        and the compacts and spills around it."""
        acks = {}
        for q in self.acked:
            op = self.sent[q]["op"]
            acks[op] = acks.get(op, 0) + 1
        names = {"s": "single puts", "b": "batches", "i": "keyed writes", "c": "rewrites", "x": "spills", "k": "compacts"}
        print("LOAD acknowledged: " + ", ".join(f"{acks.get(o, 0)} {names[o]}" for o in names))
        # Writes acknowledged while a compact was in flight: sent after it was
        # sent, answered before it was.
        spans = []
        for q, e in self.sent.items():
            if e["op"] == "k" and q in self.acked:
                spans.append((e["t"], self.acked[q]["t"]))
        during = 0
        for q, e in self.acked.items():
            if self.sent[q]["op"] in "sbicx":
                t0 = self.sent[q]["t"]
                if any(c0 <= t0 and e["t"] <= c1 for c0, c1 in spans):
                    during += 1
        print(f"LOAD {during} writes sent and acknowledged within a compact")
        for op, what in (("x", f"spills of {SPILL_DOCS * SPILL_LEN >> 20} MiB"), ("k", "compacts")):
            # From its send line, after the pause before it, to its answer.
            ms = [(e["t"] - self.sent[q]["t"]) * 1000 for q, e in self.acked.items() if self.sent[q]["op"] == op]
            if ms:
                print(f"LOAD {what} took {min(ms):.0f} to {max(ms):.0f} ms")
        if self.cut_at is not None:
            first = min(e["t"] for e in self.ev)
            flight = self.in_flight()
            print(f"LOAD the cut came {self.cut_at - first:.2f} s after the load began, {len(flight)} writes in flight")
            for q in sorted(self.sent):
                e = self.sent[q]
                if q in self.acked:
                    continue
                if e["t"] <= self.cut_at:
                    print(f"INFLIGHT {names[e['op']]} q={q}, sent {self.cut_at - e['t']:.3f} s before the cut")
                elif e["t"] <= self.killed:
                    print(f"INFLIGHT {names[e['op']]} q={q}, sent as QEMU was killed")
                else:
                    print(f"AFTER {names[e['op']]} q={q}, tried {e['t'] - self.killed:.3f} s after the cut")
            if not flight:
                self.rig(
                    f"cut {self.args.cut} landed between writes: none was in flight when QEMU was killed, "
                    "so it held nothing to a write cut short"
                )

    def in_flight(self):
        """The writes in flight at the cut: sent no later than the host's
        clock after `kill -9`, and never answered -- the INFLIGHT lines. A
        write sent after it (AFTER) found QEMU gone. One sent between the
        clock before the kill and the one after it counts: under `--sync
        250` every write in flight at run 43438990710's cuts was sent
        then, the writes before it all answered."""
        return [q for q, e in self.sent.items() if q not in self.acked and e["t"] <= self.killed]

    def gone(self, e, what, disk):
        """A write of this cut that is not there: a finding, unless the
        server answered it without saying it was on disk (`--sync <ms>`), and
        no `/_changes` page held it -- then a write the cut took, held to the
        window. One never answered may be gone either way, unless a page held
        it."""
        acked = e["q"] in self.acked
        if acked and self.sync:
            a = self.acked[e["q"]]
            self.lost.append((a["t"], a.get("seq"), what))
        if disk:
            self.fail(f"{what} was handed over by /_changes as on disk and is gone")
        elif acked and not self.sync:
            self.fail(f"{what} was acknowledged and is gone")

    def kept_ack(self, e, what):
        if e["q"] in self.acked and self.acked[e["q"]].get("seq") is not None:
            self.kept.append((self.acked[e["q"]]["seq"], what))

    def docs(self):
        committed = {int(k): v for k, v in self.state["docs"].items()}
        rows = by_id(self.s.rows("get docs"))
        known = dict(committed)
        for i, (w, c, b) in committed.items():
            r = rows.get(i)
            if r is None:
                self.fail(f"docs {i} ({w}, cut {c}) was there after the cut before and is gone")
            else:
                self.whole_doc(i, w, c, b, r)
        acked = flight = landed = whole = lost = 0
        writes = [(e, [e["id"]], "s", None, f"docs {e['id']}") for e in self.sends("s")]
        writes += [(e, e["ids"], "b", e["b"], f"batch {e['b']}") for e in self.sends("b")]
        for e, ids, w, b, what in writes:
            for i in ids:
                known[i] = [w, self.args.cut, b]
            there = [i for i in ids if i in rows]
            disk = any(i in self.on_disk["docs"] for i in ids)
            if e["q"] in self.acked:
                acked += 1
            else:
                flight += 1
            for i in there:
                self.whole_doc(i, w, self.args.cut, b, rows[i])
            if len(there) not in (0, len(ids)):
                self.fail(f"{what} is half there: {len(there)} of {len(ids)}")
            elif not there:
                n = len(self.lost)
                self.gone(e, what, disk)
                lost += len(self.lost) - n
                if w == "b" and e["q"] not in self.acked:
                    print(f"BATCH {b} in flight at the cut: none of it there")
            else:
                self.kept_ack(e, what)
                if e["q"] not in self.acked:
                    landed += 1
                    if w == "b":
                        print(f"BATCH {b} in flight at the cut: all there")
                elif w == "b":
                    whole += 1
        for i in rows:
            if i not in known:
                self.fail(f"docs {i} is there and no write made it")
        print(
            f"DOCS {len(rows)} there: {len(committed)} from the cuts before, all there and whole; "
            f"{acked} acknowledged writes of this cut, {acked - lost} there and whole"
            + (f", {lost} gone with the cut" if self.sync else "")
            + f"; {flight} in flight, {landed} of them there and whole; {whole} acknowledged batches whole"
        )
        return {str(i): known[i] for i in rows if i in known}

    def whole_doc(self, i, w, c, b, r):
        n = SINGLE_LEN if w == "s" else BATCH_LEN
        if r.get("w") != w or r.get("c") != c or r.get("p") != text(w, i, n=n) or (w == "b" and r.get("b") != b):
            self.fail(f"docs {i} is not whole: {json.dumps(r)[:200]}")

    def churn(self):
        """One thread writes the rewrites, one at a time, each version after
        the last: a document holds the last version that must be there -- the
        last acknowledged, or under `--sync <ms>` the last a page held -- or
        one sent after it."""
        last = {int(k): v for k, v in self.state["churn"].items()}
        sent = {}
        for e in sorted(self.sends("c"), key=lambda e: e["q"]):
            sent.setdefault(e["id"], []).append(e)
        rows = by_id(self.s.rows("get churn"))
        out = {}
        moved = 0
        for i in set(last) | set(sent) | set(rows):
            mine = sent.get(i, [])
            versions = [last.get(i)] + [e["v"] for e in mine]
            floor = 0
            for k, e in enumerate(mine, 1):
                if (e["q"] in self.acked and not self.sync) or e["v"] <= self.on_disk["churn"].get(i, -1):
                    floor = k
            r = rows.get(i)
            v = r.get("v") if r else None
            allowed = versions[floor:]
            if v not in allowed:
                self.fail(f"churn {i} holds version {v}, and only {sorted(map(str, allowed))} may be there")
                continue
            at = versions.index(v, floor)
            for e in mine[at:]:
                self.gone(e, f"churn {i} version {e['v']}", False)
            if at > 0:
                self.kept_ack(mine[at - 1], f"churn {i} version {v}")
            if r is not None:
                if r.get("p") != text("c", i, v, n=CHURN_LEN):
                    self.fail(f"churn {i} version {v} is not whole")
                out[str(i)] = v
                moved += v != last.get(i)
        print(f"CHURN {len(rows)} documents, each at a version that may be there; {moved} moved this cut")
        return out

    def spill(self):
        """A spill's 40 documents at one version, all there or none: the last
        that must be there, or one sent after it."""
        mine = sorted(self.sends("x"), key=lambda e: e["q"])
        versions = [self.state["spill"]] + [e["v"] for e in mine]
        floor = 0
        for k, e in enumerate(mine, 1):
            if (e["q"] in self.acked and not self.sync) or e["v"] <= (self.on_disk["spill"] or -1):
                floor = k
        allowed = versions[floor:]
        rows = self.s.rows("get spill")
        held = {r.get("v") for r in rows}
        if rows and len(rows) != SPILL_DOCS:
            self.fail(f"a spill is half there: {len(rows)} of {SPILL_DOCS} documents")
        if len(held) > 1:
            self.fail(f"a spill is half there: its documents hold versions {sorted(held)}")
        v = rows[0].get("v") if rows else None
        if v not in allowed:
            self.fail(f"the spill holds version {v}, and only {sorted(map(str, allowed))} may be there")
        else:
            at = versions.index(v, floor)
            for e in mine[at:]:
                self.gone(e, f"spill version {e['v']}", False)
        for r in rows:
            if r.get("p") != text("x", r["id"], r.get("v"), n=SPILL_LEN):
                self.fail(f"spill {r['id']} version {r.get('v')} is not whole")
        print(f"SPILL version {v}, all {len(rows)} documents of {SPILL_LEN >> 10} KiB whole; may be {sorted(map(str, allowed))}")
        return v

    def idem(self):
        committed = set(self.state["idem"])
        mine = self.sends("i")
        acked = [e for e in mine if e["q"] in self.acked]
        flight = [e for e in mine if e["q"] not in self.acked]
        counts = {}
        for r in self.s.rows("get idem"):
            counts[r.get("k")] = counts.get(r.get("k"), 0) + 1
        for k in committed:
            if counts.get(k, 0) != 1:
                self.fail(f"idem {k} was there after the cut before and is there {counts.get(k, 0)} times")
        there = set()
        for e in mine:
            n = counts.get(e["k"], 0)
            if n > 1:
                self.fail(f"idem {e['k']} is there {n} times")
            elif n == 0:
                self.gone(e, f"keyed write {e['k']}", e["k"] in self.on_disk["idem"])
            else:
                there.add(e["k"])
                self.kept_ack(e, f"keyed write {e['k']}")
        known = committed | {e["k"] for e in mine}
        for k in counts:
            if k not in known:
                self.fail(f"idem {k} is there and no write made it")
        # Each sent again with its key: one there is answered as it was,
        # replayed -- its key's record is in the block its write landed in --
        # and one not there is written now, never replayed.
        replayed = 0
        for e in mine:
            body = {"query": "put idem {k: $1, c: $2}", "params": [e["k"], self.args.cut]}
            st, h, b = self.s.request("POST", self.s.prefix + "/query", json.dumps(body).encode(), {"Idempotency-Key": e["k"]})
            if not 200 <= st < 300:
                self.fail(f"idem {e['k']} sent again answered {st}: {b[:200]!r}")
                continue
            again = h.get("idempotent-replayed") == "true"
            if e["k"] not in there:
                if again:
                    self.fail(f"idem {e['k']} is not there, and sent again it was answered as replayed")
            elif e["q"] in self.acked:
                a = self.acked[e["q"]]
                if not again or st != a["status"] or b.decode() != a["answer"]:
                    self.fail(f"idem {e['k']} was acknowledged and sent again was not replayed: {st} {h.get('idempotent-replayed')} {b[:100]!r}")
                else:
                    replayed += 1
            elif again:
                replayed += 1
        counts = {}
        for r in self.s.rows("get idem"):
            counts[r.get("k")] = counts.get(r.get("k"), 0) + 1
        for e in mine:
            if counts.get(e["k"], 0) != 1:
                self.fail(f"idem {e['k']} is there {counts.get(e['k'], 0)} times after it was sent again")
        print(
            f"IDEM {len(acked)} acknowledged keys, {len([e for e in acked if e['k'] in there])} there once and replayed; "
            f"{len(flight)} in flight; {replayed} answered Idempotent-Replayed"
        )
        return sorted(committed | {e["k"] for e in mine})

    def probe(self):
        i = self.args.cut * 10 + (1 if self.args.label == "plain" else 2)
        st, _, b = self.s.query("put probe {id: $1, c: $2, label: $3}", (i, self.args.cut, self.args.label))
        if st != 200:
            self.fail(f"a new write after the cut was refused: {st} {b[:200]!r}")
            return
        rows = self.s.rows("get probe where id = $1", (i,))
        if len(rows) != 1:
            self.fail(f"a new write after the cut was not read back: {rows}")
        else:
            print("PROBE a new write taken and read back")

    def window(self):
        """`--sync <ms>`'s promise, held to fixed numbers: a write the cut
        took was answered within the sync's window of it, and the syncs
        came. The syncer sleeps `ms` after each sync, then takes what was
        written under the write lock and fsyncs it once the lock is let go
        (`run_syncer`); a write landing just after a sync took its writes
        waits out the rest of that sync, the sleep and the whole of the
        next. So an acknowledged write the cut took was answered no longer
        before it than `ms` and two syncs' own time, each allowed `SYNC_OWN`
        -- the host's clock read before the kill, earlier than the guest's
        end, and at the write's answer, later than its landing, only shorten
        the span measured. And each page came once a sync was done, `ms` and
        its own time after the one before: no gap between pages, after the
        first acknowledged write or before the cut is past `GAP_CEILING`,
        or a sync came late. Neither bound is read from the pages: a bound
        read from them grew with the stall it was to find. What the cut
        kept is a prefix of the writes: no write it took landed before one
        it kept."""
        a = self.args
        if self.cut_at is None:
            return
        cut = self.cut_at
        ats = [min(p["at"], cut) for p in self.pages]
        if len(ats) < 2:
            self.fail(f"the /_changes reader was handed {len(ats)} page(s): nothing to hold the window to")
            return
        gaps = [y - x for x, y in zip(ats, ats[1:])]
        spans = [(g, f"between pages {i} and {i + 1}") for i, g in enumerate(gaps, 1)]
        writes = [e["t"] for q, e in self.acked.items() if self.sent[q]["op"] != "k"]
        if writes:
            spans.insert(0, (ats[0] - min(writes), "from the first acknowledged write to the first page"))
        spans.append((cut - ats[-1], "from the last page to the cut"))
        longest = max(spans)
        late = [s for s in spans if s[0] > GAP_CEILING]
        if late:
            self.fail(
                f"a late sync: {len(late)} gap(s) past the ceiling of {GAP_CEILING * 1000:.0f} ms, "
                f"the longest {longest[0] * 1000:.0f} ms {longest[1]}"
            )
        bound = self.sync + 2 * SYNC_OWN
        med = sorted(gaps)[len(gaps) // 2]
        # How long an acknowledged write took to reach the disk: from its
        # answer to the first page reaching the change it landed as.
        lags = []
        unsynced = 0
        nexts = [p["next"] for p in self.pages]
        for q, e in self.acked.items():
            seq = e.get("seq")
            if seq is None or self.sent[q]["op"] == "k":
                continue
            k = bisect.bisect_left(nexts, seq)
            if k == len(nexts):
                unsynced += 1
            else:
                lags.append(max(0.0, self.pages[k]["at"] - e["t"]))
        lags.sort()
        # The reader's turn: from a page's head to its next request, what a
        # sync done meanwhile waited to be seen.
        turns = [y["sent"] - x["at"] for x, y in zip(self.pages, self.pages[1:])]
        if lags:
            print(
                f"SYNCS {len(self.pages)} pages handed over, one every {med * 1000:.0f} ms (median), "
                f"{max(gaps) * 1000:.0f} ms at most, the reader's turn {max(turns) * 1000:.0f} ms at most; "
                f"an acknowledged write handed over {lags[len(lags) // 2] * 1000:.0f} ms after its answer (median), "
                f"{lags[min(len(lags) - 1, len(lags) * 99 // 100)] * 1000:.0f} ms at p99, {lags[-1] * 1000:.0f} ms at most; "
                f"{unsynced} acknowledged writes not handed over before the cut"
            )
        lost = sorted(self.lost, key=lambda x: x[0])
        span = cut - lost[0][0] if lost else 0.0
        when = f"{span * 1000:.0f} ms before it" if span >= 0 else f"{-span * 1000:.0f} ms after it was noted"
        print(
            f"WINDOW cut {a.cut} {a.label}{' ' + a.prefix if a.prefix else ''}: "
            + (
                f"{len(lost)} acknowledged writes gone with the cut, the first answered {when} ({lost[0][2]})"
                if lost
                else "no acknowledged write gone with the cut"
            )
            + f"; the bound {self.sync * 1000:.0f} + 2 x {SYNC_OWN * 1000:.0f} (two syncs' own time) = {bound * 1000:.0f} ms"
            + f"; the longest gap {longest[0] * 1000:.0f} ms, {longest[1]}, against a ceiling of {GAP_CEILING * 1000:.0f} ms"
        )
        if span > bound:
            self.fail(f"an acknowledged write answered {span * 1000:.0f} ms before the cut is gone: past the window's {bound * 1000:.0f} ms ({lost[0][2]})")
        gone = [x for x in lost if x[1] is not None]
        if gone and self.kept:
            first = min(gone, key=lambda x: x[1])
            last = max(self.kept, key=lambda x: x[0])
            if last[0] > first[1]:
                self.fail(
                    f"the cut kept {last[1]} (change {last[0]}) and took {first[2]} (change {first[1]}), which landed before it: "
                    "not a prefix of the writes"
                )
            else:
                print(f"LOST a prefix kept: every acknowledged write gone landed after change {last[0]}, the last kept")

    def replica_before(self):
        """The replica as the cut left it (`replica`), held to the primary as
        it came back: a primary sends a write once an fsync covered it, so it
        holds every write any replica was sent -- the replica's change no
        later than the primary's, each of its documents the primary's or one
        the primary wrote over since."""
        with open(named(self.args, "replica", self.args.cut)) as f:
            snap = json.load(f)
        seq, history = self.s.seq()
        n = len(self.bad)
        if snap["seq"] > seq:
            self.fail(f"the replica held change {snap['seq']}, and the primary came back at {seq}: it lost what it had sent")
        if snap["history"] and history and snap["history"] != history:
            self.fail(f"the replica's history {snap['history']} is not the primary's {history}")
        mine = snapshot(self.s)
        for c in ("docs", "probe"):
            for i, d in snap[c].items():
                if mine[c].get(i) != d:
                    self.fail(f"the replica held {c} {i}, which the primary has not as it was: {mine[c].get(i)} against {d}")
        for c in ("churn", "spill"):
            for i, (v, d) in snap[c].items():
                p = mine[c].get(i)
                if p is None or p[0] < v or (p[0] == v and p[1] != d):
                    self.fail(f"the replica held {c} {i} at version {v}, the primary {p and p[0]}")
        keys = set(mine["idem"])
        for k in snap["idem"]:
            if k not in keys:
                self.fail(f"the replica held idem {k}, which the primary has not")
        if len(self.bad) == n:
            print(
                f"REPLICA as the cut left it: change {snap['seq']}, {len(snap['docs'])} documents; "
                f"the primary came back at change {seq} holding every one"
            )
        self.standby_near(snap["seq"], seq)

    def standby_near(self, held, back):
        """The standby against the node's tip at the cut: the acknowledged
        writes above the change it held -- what a promotion at the cut
        would have lost, every one answered as durable -- counted, and how
        long before the cut the oldest of them was answered held to
        `STANDBY_LAG`. The cuts before this one's left it at the node's
        change (`replica_after`), so only this cut's writes can be above
        it."""
        a = self.args
        acked = [(e["t"], e["seq"]) for q, e in self.acked.items() if e.get("seq") is not None and self.sent[q]["op"] != "k"]
        above = sorted(x for x in acked if x[1] > held)
        tip = max((s for _, s in acked), default=held)
        lag = self.cut_at - above[0][0] if above and self.cut_at is not None else 0.0
        print(
            f"STANDBY cut {a.cut} {a.label}{' ' + a.prefix if a.prefix else ''}: held change {held}, "
            f"{max(0, tip - held)} behind the last acknowledged ({tip}) and {max(0, back - held)} behind the node as it came back ({back}); "
            + (
                f"{len(above)} acknowledged writes above it, the oldest answered {lag * 1000:.0f} ms before the cut: "
                "what a promotion at the cut would have lost"
                if above
                else "no acknowledged write above it: a promotion at the cut would have lost none"
            )
        )
        if lag > STANDBY_LAG:
            self.rig(
                f"the standby lacked a write the node acknowledged {lag * 1000:.0f} ms before cut {a.cut}, "
                f"past {STANDBY_LAG * 1000:.0f} ms: so far behind, it held nothing of the node's last {lag:.1f} s to it"
            )

    def replica_after(self):
        """The replica once it has followed the primary to the change the
        check left it at: the same rows, collection by collection."""
        seq, _ = self.s.seq()
        r = Server(self.args.replica_url, self.tok, timeout=60, prefix=self.args.prefix)
        t0 = time.time()
        for _ in range(REPLICA_WAITS):
            st, _, b = r.query("get probe limit 1", headers={"Fennec-After": str(seq), "Fennec-Wait": "30000"})
            if st == 200:
                break
            if st != 504:
                self.fail(f"the replica answered {st} waiting for change {seq}: {b[:200]!r}")
                return
        else:
            self.fail(f"the replica did not reach change {seq} in {time.time() - t0:.0f} s: {b[:200]!r}")
            return
        took = time.time() - t0
        mine, theirs = snapshot(self.s), snapshot(r)
        n = len(self.bad)
        for c in COLLECTIONS:
            if mine[c] != theirs[c]:
                a, b = mine[c], theirs[c]
                self.fail(f"the replica's {c} is not the primary's: {len(b)} rows against {len(a)}, {len(set(a) ^ set(b))} in one alone")
        if len(self.bad) == n:
            print(
                f"REPLICA reached the primary's change {seq} {took:.1f} s after the check asked, "
                f"and holds the same rows: {len(mine['docs'])} documents"
            )


def replica(args):
    """A replica of the tenant as the cut left it, read before the primary
    comes back: its change, its history and its rows' digests."""
    s = Server(args.replica_url, token(args), timeout=600, prefix=args.prefix)
    seq, history = s.seq()
    snap = snapshot(s)
    with open(named(args, "replica", args.cut), "w") as f:
        json.dump({"seq": seq, "history": history} | snap, f)
    print(f"REPLICA {args.prefix} held change {seq} when the primary went: {len(snap['docs'])} documents")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("cmd", choices=["setup", "load", "check", "replica"])
    p.add_argument("--url", default="http://127.0.0.1:8080")
    p.add_argument("--token-file", required=True)
    p.add_argument("--state", required=True)
    p.add_argument("--cut", type=int, default=0)
    p.add_argument("--seed", default="0")
    p.add_argument("--label", default="plain")
    p.add_argument("--cut-time")
    p.add_argument("--commit", action="store_true")
    p.add_argument("--prefix", default="", help="a --dir node's tenant: /t/<tenant>")
    p.add_argument("--name", default="", help="keeps a tenant's state and events apart")
    p.add_argument("--ops", default="", help="the load's threads, a letter each (ssbcxk)")
    p.add_argument("--sync-ms", type=int, default=0, help="the server's --sync <ms>; 0 for always")
    p.add_argument("--replica-url", default="", help="a replica of the tenant, held to it")
    a = p.parse_args()
    if a.cmd == "setup":
        setup(a)
    elif a.cmd == "load":
        Load(a).start()
    elif a.cmd == "replica":
        replica(a)
    else:
        Check(a).run()


if __name__ == "__main__":
    main()
