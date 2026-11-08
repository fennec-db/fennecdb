#!/usr/bin/env python3
"""The load a power cut lands in, and the check after it.

The host's side of the Windows power-cut test (docs/design/windows.md,
decision 9): the guest's fennec-server, under `--sync always`, reached
through QEMU's port forward. The standard library alone: it runs on the
runner beside QEMU.

    client.py setup --state DIR
    client.py load  --state DIR --cut N --seed S
    client.py check --state DIR --cut N --label plain|lost [--commit]

`load` writes until the server stops answering -- the cut -- with a thread
each: single puts, `/batch`es of 8 with an `Idempotency-Key`'d write between
them, a document written over and over, a `/batch` past 16 MiB (a spill)
written over half a second after the last, and a `compact` a fifth of a
second after the last.
Every write is logged before it is sent and again once a 2xx answers it, a
line each, flushed: DIR/events-N.jsonl, the host's record of what the server
acknowledged, kept outside the guest.

`check` reads the database back and holds it to that log and to what the
checks before it found (DIR/state.json): every acknowledged write there and
whole; a write in flight at the cut there whole or not at all; a batch, and
a spill, all there or none of it; every acknowledged `Idempotency-Key`
answered again as it was, `Idempotent-Replayed`, never made twice; and a new
write taken. `--commit` makes what it found the state the next cut is held
to. The token is read from --token-file, never printed.
"""

import argparse
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


def text(*seed, n):
    """A document's text, made again from what names it: the check needs no
    copy of what was written."""
    h = hashlib.sha256(repr(seed).encode()).hexdigest()
    return (h * (n // len(h) + 1))[:n]


class Server:
    """One keep-alive connection to the server; a thread each."""

    def __init__(self, url, token, timeout=120):
        host, _, port = url.removeprefix("http://").partition(":")
        self.host, self.port = host, int(port or 80)
        self.token = token
        self.timeout = timeout
        self.cx = None

    def request(self, method, path, body=b"", headers=None):
        if self.cx is None:
            self.cx = http.client.HTTPConnection(self.host, self.port, timeout=self.timeout)
        h = {"Authorization": f"Bearer {self.token}", "Content-Type": "application/json"}
        h.update(headers or {})
        try:
            self.cx.request(method, path, body=body, headers=h)
            r = self.cx.getresponse()
            data = r.read()
            return r.status, {k.lower(): v for k, v in r.getheaders()}, data
        except BaseException:
            self.cx.close()
            self.cx = None
            raise

    def query(self, q, params=(), headers=None):
        body = json.dumps({"query": q, "params": list(params)}).encode()
        return self.request("POST", "/query", body, headers)

    def batch(self, lines):
        body = "\n".join(json.dumps({"query": q, "params": list(p)}) for q, p in lines).encode()
        return self.request("POST", "/batch", body)

    def rows(self, q, params=()):
        status, _, data = self.query(q, params)
        if status != 200:
            raise SystemExit(f"`{q}` answered {status}: {data[:300]!r}")
        out = json.loads(data)
        return out["rows"] if isinstance(out, dict) else out


def token(args):
    with open(args.token_file) as f:
        return f.read().strip()


def setup(args):
    s = Server(args.url, token(args))
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
    with open(os.path.join(args.state, "state.json"), "w") as f:
        json.dump({"docs": {}, "churn": {}, "spill": None, "idem": []}, f)
    print("SETUP collections docs, churn, spill, idem, probe")


class Load:
    def __init__(self, args):
        self.args = args
        self.token = token(args)
        self.cut = args.cut
        self.out = open(os.path.join(args.state, f"events-{args.cut}.jsonl"), "a", buffering=1)
        self.lock = threading.Lock()
        self.q = 0
        self.rng = random.Random(f"{args.seed}-{args.cut}")
        with open(os.path.join(args.state, "state.json")) as f:
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

    def run(self, op, send):
        """`send` makes one write and answers (status, headers, body); a 2xx
        is noted as acknowledged, anything else stops the thread: a refusal
        before the cut is a finding, and after it nothing answers."""
        s = Server(self.args.url, self.token)
        while True:
            q = self.next_q()
            try:
                status, headers, body, fields = send(s, q)
            except (OSError, http.client.HTTPException) as e:
                self.note(ev="error", op=op, q=q, what=f"{type(e).__name__}: {e}"[:200])
                return
            if 200 <= status < 300:
                self.note(ev="ack", op=op, q=q, status=status, **fields)
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
                st, h, b = s.request("POST", "/query", json.dumps(body).encode(), {"Idempotency-Key": k})
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

    def start(self):
        workers = [
            threading.Thread(target=self.single, args=(0,)),
            threading.Thread(target=self.single, args=(1,)),
            threading.Thread(target=self.batches),
            threading.Thread(target=self.churn),
            threading.Thread(target=self.spill),
            threading.Thread(target=self.compact),
        ]
        self.note(ev="start", cut=self.cut)
        for w in workers:
            w.start()
        for w in workers:
            w.join()
        self.note(ev="end", cut=self.cut)


def events(state, cut):
    out = []
    with open(os.path.join(state, f"events-{cut}.jsonl")) as f:
        for line in f:
            line = line.strip()
            if line:
                out.append(json.loads(line))
    return out


class Check:
    def __init__(self, args):
        self.args = args
        self.s = Server(args.url, token(args), timeout=600)
        self.bad = []
        with open(os.path.join(args.state, "state.json")) as f:
            self.state = json.load(f)
        self.ev = events(args.state, args.cut)
        self.sent = {e["q"]: e for e in self.ev if e["ev"] == "send"}
        self.acked = {e["q"]: e for e in self.ev if e["ev"] == "ack"}
        # The host's clock just before `kill -9` and just after: a write
        # sent before the second may have reached the guest.
        self.cut_at = self.killed = None
        if args.cut_time:
            self.cut_at, self.killed = (float(t) for t in args.cut_time.split())

    def fail(self, what):
        self.bad.append(what)
        if len(self.bad) <= 40:
            print(f"BAD {what}")

    def sends(self, op):
        return [e for e in self.sent.values() if e["op"] == op]

    def run(self):
        a = self.args
        print(f"CHECK cut {a.cut} ({a.label})")
        for e in self.ev:
            if e["ev"] == "fail":
                self.fail(f"the server refused a write before the cut: {e['op']} {e['status']} {e['body'][:200]}")
            # A connection the server dropped while it ran stops its thread
            # and leaves its write in flight to the end: run 2's cut 1 lost a
            # thread so at its first request, unexplained, and passed.
            elif e["ev"] == "error" and self.cut_at is not None and e["t"] < self.cut_at:
                sent = self.sent.get(e["q"], {})
                self.fail(
                    f"a connection failed {self.cut_at - e['t']:.3f} s before the cut, "
                    f"{e['t'] - sent.get('t', e['t']):.3f} s after its write was sent: {e['op']} q={e['q']} {e['what']}"
                )
        self.describe_load()
        found = {}
        found["docs"] = self.docs()
        found["churn"] = self.churn()
        found["spill"] = self.spill()
        found["idem"] = self.idem()
        self.probe()
        if self.bad:
            print(f"VERDICT cut {a.cut} {a.label} FAIL: {len(self.bad)} violations")
            sys.exit(1)
        print(f"VERDICT cut {a.cut} {a.label} PASS")
        if a.commit:
            with open(os.path.join(a.state, "state.json"), "w") as f:
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
            print(f"LOAD the cut came {self.cut_at - first:.2f} s after the load began")
            flight = [q for q in self.sent if q not in self.acked]
            for q in sorted(flight):
                e = self.sent[q]
                if e["t"] <= self.cut_at:
                    print(f"INFLIGHT {names[e['op']]} q={q}, sent {self.cut_at - e['t']:.3f} s before the cut")
                elif e["t"] <= self.killed:
                    print(f"INFLIGHT {names[e['op']]} q={q}, sent as QEMU was killed")
                else:
                    print(f"AFTER {names[e['op']]} q={q}, tried {e['t'] - self.killed:.3f} s after the cut")

    def docs(self):
        committed = {int(k): v for k, v in self.state["docs"].items()}
        must, maybe = dict(committed), {}
        for e in self.sends("s"):
            (must if e["q"] in self.acked else maybe)[e["id"]] = ["s", self.args.cut, None]
        batches = []
        for e in self.sends("b"):
            for i in e["ids"]:
                (must if e["q"] in self.acked else maybe)[i] = ["b", self.args.cut, e["b"]]
            batches.append(e)
        rows = {r["id"]: r for r in self.s.rows("get docs")}
        for i, (w, c, b) in must.items():
            r = rows.get(i)
            if r is None:
                self.fail(f"docs {i} ({w}, cut {c}) was acknowledged and is gone")
            else:
                self.whole_doc(i, w, c, b, r)
        landed = 0
        for i, (w, c, b) in maybe.items():
            r = rows.get(i)
            if r is not None:
                landed += 1
                self.whole_doc(i, w, c, b, r)
        for i in rows:
            if i not in must and i not in maybe:
                self.fail(f"docs {i} is there and no write made it")
        whole = 0
        for e in batches:
            there = sum(1 for i in e["ids"] if i in rows)
            if there not in (0, len(e["ids"])):
                self.fail(f"batch {e['b']} is half there: {there} of {len(e['ids'])}")
            elif e["q"] in self.acked and there != len(e["ids"]):
                self.fail(f"batch {e['b']} was acknowledged and is gone")
            elif e["q"] not in self.acked:
                print(f"BATCH {e['b']} in flight at the cut: {'all there' if there else 'none of it there'}")
            else:
                whole += 1
        print(
            f"DOCS {len(rows)} there: {len(must)} acknowledged, all there and whole; "
            f"{len(maybe)} in flight, {landed} of them there and whole; "
            f"{whole} acknowledged batches whole"
        )
        return {str(i): v for i, v in must.items()} | {str(i): v for i, v in maybe.items() if i in rows}

    def whole_doc(self, i, w, c, b, r):
        n = SINGLE_LEN if w == "s" else BATCH_LEN
        if r.get("w") != w or r.get("c") != c or r.get("p") != text(w, i, n=n) or (w == "b" and r.get("b") != b):
            self.fail(f"docs {i} is not whole: {json.dumps(r)[:200]}")

    def churn(self):
        """One thread writes the rewrites, one at a time: a document holds
        the last version acknowledged, or the one in flight after it."""
        last = {int(k): v for k, v in self.state["churn"].items()}
        allowed = {i: {v} for i, v in last.items()}
        order = sorted(self.sends("c"), key=lambda e: e["q"])
        for e in order:
            if e["q"] in self.acked:
                allowed[e["id"]] = {e["v"]}
            else:
                allowed.setdefault(e["id"], {None}).add(e["v"])
        rows = {r["id"]: r for r in self.s.rows("get churn")}
        out = {}
        moved = 0
        for i in set(allowed) | set(rows):
            r = rows.get(i)
            v = r.get("v") if r else None
            if v not in allowed.get(i, {None}):
                self.fail(f"churn {i} holds version {v}, and only {sorted(map(str, allowed.get(i, {None})))} may be there")
                continue
            if r is not None:
                if r.get("p") != text("c", i, v, n=CHURN_LEN):
                    self.fail(f"churn {i} version {v} is not whole")
                out[str(i)] = v
                moved += v != last.get(i)
        print(f"CHURN {len(rows)} documents, each at its last acknowledged version or the one in flight; {moved} moved this cut")
        return out

    def spill(self):
        allowed = {self.state["spill"]}
        for e in sorted(self.sends("x"), key=lambda e: e["q"]):
            if e["q"] in self.acked:
                allowed = {e["v"]}
            else:
                allowed.add(e["v"])
        rows = self.s.rows("get spill")
        versions = {r.get("v") for r in rows}
        if rows and len(rows) != SPILL_DOCS:
            self.fail(f"a spill is half there: {len(rows)} of {SPILL_DOCS} documents")
        if len(versions) > 1:
            self.fail(f"a spill is half there: its documents hold versions {sorted(versions)}")
        v = rows[0].get("v") if rows else None
        if v not in allowed:
            self.fail(f"the spill holds version {v}, and only {sorted(map(str, allowed))} may be there")
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
        for k in committed | {e["k"] for e in acked}:
            if counts.get(k, 0) != 1:
                self.fail(f"idem {k} was acknowledged and is there {counts.get(k, 0)} times")
        for e in flight:
            if counts.get(e["k"], 0) > 1:
                self.fail(f"idem {e['k']} was in flight and is there {counts[e['k']]} times")
        known = committed | {e["k"] for e in mine}
        for k in counts:
            if k not in known:
                self.fail(f"idem {k} is there and no write made it")
        # Each sent again with its key: an acknowledged one is answered as it
        # was, replayed; one in flight is either.
        replayed = 0
        for e in mine:
            body = {"query": "put idem {k: $1, c: $2}", "params": [e["k"], self.args.cut]}
            st, h, b = self.s.request("POST", "/query", json.dumps(body).encode(), {"Idempotency-Key": e["k"]})
            if not 200 <= st < 300:
                self.fail(f"idem {e['k']} sent again answered {st}: {b[:200]!r}")
                continue
            if e["q"] in self.acked:
                a = self.acked[e["q"]]
                if h.get("idempotent-replayed") != "true" or st != a["status"] or b.decode() != a["answer"]:
                    self.fail(f"idem {e['k']} was acknowledged and sent again was not replayed: {st} {h.get('idempotent-replayed')} {b[:100]!r}")
                else:
                    replayed += 1
            elif h.get("idempotent-replayed") == "true":
                replayed += 1
        counts = {}
        for r in self.s.rows("get idem"):
            counts[r.get("k")] = counts.get(r.get("k"), 0) + 1
        for e in mine:
            if counts.get(e["k"], 0) != 1:
                self.fail(f"idem {e['k']} is there {counts.get(e['k'], 0)} times after it was sent again")
        print(
            f"IDEM {len(acked)} acknowledged keys there once and replayed; "
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


def main():
    p = argparse.ArgumentParser()
    p.add_argument("cmd", choices=["setup", "load", "check"])
    p.add_argument("--url", default="http://127.0.0.1:8080")
    p.add_argument("--token-file", required=True)
    p.add_argument("--state", required=True)
    p.add_argument("--cut", type=int, default=0)
    p.add_argument("--seed", default="0")
    p.add_argument("--label", default="plain")
    p.add_argument("--cut-time")
    p.add_argument("--commit", action="store_true")
    a = p.parse_args()
    if a.cmd == "setup":
        setup(a)
    elif a.cmd == "load":
        Load(a).start()
    else:
        Check(a).run()


if __name__ == "__main__":
    main()
