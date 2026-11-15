#!/usr/bin/env python3
"""A release's fixture set: the files that release's own binaries wrote, and
the answers it gave over them, which every later build opens and answers
alike -- 1.x opens every file written from v0.2.0 on
(docs/design/compatibility.md). The set lives in
crates/fennec-core/tests/compat/v<version>/, read by `compat::` in
fennec-core's, fennec-http's, fennec's and fennec-server's tests and by
web/fennec.compat.test.js.

    python3 tools/compat_fixtures.py build v0.2.0 <bin-dir>
        the release's binaries, built from its tag alone: `git archive` of
        the tag into a scratch directory, `cargo build --release --locked
        --offline` of fennec-server and fennec (their crates from outside
        the workspace, behind `tls`, from cargo's cache: nothing
        downloaded), the tag's own `make wasm`, and tools/compat/writer.rs
        built as an example of the tag's fennec-core; the binaries and the
        tag's web/ copied into <bin-dir> and the scratch directory, its
        target/ with it, deleted
    python3 tools/compat_fixtures.py write <bin-dir> <set-dir>
        the set, written by those binaries (macOS or Linux), its files'
        sums last in SHA256SUMS, which `make version` holds the set to
    make compat-fixtures V=0.2.0
        both, into crates/fennec-core/tests/compat/v0.2.0/

What each file holds, what the writer lowers to make it at a few
kilobytes, what of it cannot be the same twice (a history's id, a seal's
nonces, a scheduled slot's time) and what no file holds, and why, is in
the set's README.md, which `write` writes. The statements below and
tools/compat/*.fennecql are the inputs: a release that writes something
new -- a record kind, a shape inside one, a type tag, an index -- adds the
statements that write it here before its set is written.
"""

import hashlib
import hmac
import http.client
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
COMPAT = ROOT / "tools" / "compat"
SETS = ROOT / "crates" / "fennec-core" / "tests" / "compat"
CARGO = os.path.expanduser("~/.cargo/bin/cargo") if os.path.exists(os.path.expanduser("~/.cargo/bin/cargo")) else "cargo"

TOKEN = "compat-token"
REPLICATION = "compat-replication"
HOOK_SECRET = "the fixture set's webhook secret"
# A kept Idempotency-Key and a taken event's key outlive every build that
# opens the set: kept for the defaults (a day, a week), a later build's
# reads would find them gone, as the release's own would by then. So the
# server keeps them for 95 years (`--idempotency-ttl`, the hook's `keep`),
# and every build reads them as the release did when it wrote them.
IDEMPOTENCY_TTL = "3000000000"
HOOK_LINE = "github github {secret} keep 36500d"
IDEMPOTENCY_KEY = "compat-idempotency-key"
IDEMPOTENT = 'put orders {ref: "o-4", status: "open", total: 70}'
CONSUMER = "compat"

# Before the compact: what the image holds, every type and index among it.
PHASE_A = [
    "create collection kinds (b bool, i int @hash, f float @sorted, t text @text, by bytes, ts timestamp @sorted, "
    "v vector<4>, vi vector<4> @hnsw(cosine, m=8), vh vector<4, f16>, l [int], sp sparse<8> @inverted, j json, "
    "g geo @geo, tr text collate tr @sorted, un text collate und @sorted, email text nocase @unique, "
    "tag text @hash @text, word text @text(chars), acc text @text(accents), exp timestamp @ttl(1d), a int, z int, "
    "old text, @sorted(a, z), @unique(a, i))",
    'put kinds {b: true, i: 1, f: 1.5, t: "the quick brown fox", by: [0, 159, 255], ts: "2026-01-01T00:00:00Z", '
    'v: [1, 0, 0, 0], vi: [1, 0, 0, 0], vh: [0.5, 0.25, 0, 1], l: [3, 1, 2], sp: "{1:0.5,6:2}/8", '
    'j: {kind: "a", n: 1, deep: {x: [1, "two", null, true]}}, g: [13.4, 52.5], tr: "ılık", un: "Émile", '
    'email: "Alice@Example.com", tag: "red fox", word: "東京タワー", acc: "café crème", exp: "2099-01-01T00:00:00Z", '
    'a: 1, z: 10, old: "dropped later"}',
    'put kinds {b: false, i: 2, f: -2.25, t: "lazy dogs sleep", by: "text as bytes", ts: 1767225600000, '
    'v: [0, 1, 0, 0], vi: [0, 1, 0, 0], vh: [0, 1, 0, 0], l: [], sp: "{2:1}/8", j: [1, 2.5, "x"], '
    'g: [2.35, 48.86], tr: "Istanbul", un: "zoe", email: "bob@example.com", tag: "blue whale", word: "京都", '
    'acc: "cafe", exp: "2099-06-01T00:00:00Z", a: 1, z: 20}',
    'put kinds {i: 3, un: "Ångström"}',
    # Expired before it was written: every read leaves it out (`@ttl`).
    'put kinds {i: 6, t: "expired fox", exp: "2000-01-01T00:00:00Z", a: 6, z: 6}',
    'put kinds {b: true, i: 9007199254740993, f: 0.1, t: "a fox again", vi: [0, 0, 1, 0], '
    'j: {kind: "b", big: 9007199254740993}, un: "eclair", a: 2, z: 1}',
    # What a schema writes beside `kinds`' fields: a `required` byte set, a
    # field's tags in every combination -- collated, `nocase` and a `@text`
    # beside its own index -- each text index's options, a list of every
    # type a list takes, a descending part of an index over several fields,
    # and a last place dropped in `PHASE_B`.
    "create collection shapes (req int required, tn text collate tr nocase @unique @text, "
    "cb text collate und @hash @text(chars), nb text nocase @sorted @text(accents), ca text @text(chars, accents), "
    "lb [bool], lf [float], lt [text], lby [bytes], lts [timestamp], lv [vector<2>], lvh [vector<2, f16>], "
    "ll [[int]], lsp [sparse<4>], lg [geo], j json, a int, b int, cn text collate und nocase, "
    "cx text @unique @text(chars, accents), last int, @sorted(a desc, b))",
    'put shapes {req: 1, tn: "Işık", cb: "東京 tower", nb: "Crème Brûlée", ca: "naïve 東京", lb: [true, false], '
    'lf: [1.5, -2.25], lt: ["x", "y"], lby: ["ab", "c"], lts: ["2026-01-01T00:00:00Z", 1767225600000], '
    'lv: [[1, 0], [0, 1]], lvh: [[0.5, 0.25]], ll: [[1, 2], [3]], lsp: ["{1:0.5}/4", "{2:1,4:2}/4"], '
    'lg: [[13.4, 52.5]], j: {k: "one", n: 1}, a: 1, b: 2, cn: "Ärger", cx: "Façade 東京", last: 1}',
    'put shapes {req: 2, tn: "istanbul", cb: "京都", nb: "creme", ca: "cafe", lb: [], lf: [0.5], lt: [], lby: [], '
    'lts: [], lv: [], lvh: [], ll: [[]], lsp: [], lg: [], j: {k: "two", n: 2}, a: 1, b: 3}',
    'put shapes {req: 3, a: 2, b: 1, j: {k: "three", n: 3}}',
    "create collection orders (ref text @unique, status text, total int)",
    "create collection audit (ref text @hash, what text)",
    "create collection ticks (k text @unique)",
    "create collection issues (n int @hash, action text)",
    "create collection gone (n int)",
    "put gone {n: 1}",
    "create statement order_by_ref($r text) as get orders select ref, status, total where ref = $r",
    "create statement on_order after insert orders as insert audit {ref: new.ref, what: 'placed'}",
    'put orders [{ref: "o-1", status: "open", total: 2000}, {ref: "o-2", status: "open", total: 500}]',
]

# After it: the log's records, each kind a write takes, and every change a
# field record names: added, renamed, dropped, its expiry moved.
PHASE_B = [
    "create collection extra (k text @hash, n int)",
    "create index on kinds (j.kind) @hash",
    "alter collection kinds add field note text",
    "alter collection kinds rename field note to remark",
    "alter collection kinds drop field old",
    "alter collection kinds alter field exp @ttl(2d)",
    "drop collection gone",
    'put kinds {b: false, i: 5, f: 2.5, t: "brown bears", ts: "2026-03-01T12:00:00Z", vi: [0, 0, 0, 1], '
    'remark: "after the alter", a: 3, z: 3}',
    'set kinds {f: 9.5, tag: "green fox"} where i = 2',
    "del kinds where i = 3",
    "create or replace statement order_by_ref($r text) as get orders select ref, status, total where ref = $r "
    "and status != 'void'",
    # An index on a path of each kind a path takes.
    "create index on shapes (j.n) @sorted",
    "create index on shapes (j.k) @unique",
    # A binding of every set of events, each writing nothing, then a write
    # of each kind waking them.
    *[f"create statement on_{n} after {e} shapes as del ticks where k = 'never'" for n, e in (
        ("i", "insert"), ("s", "set"), ("d", "del"), ("is", "insert or set"), ("id", "insert or del"),
        ("sd", "set or del"), ("isd", "insert or set or del"))],
    'put shapes {req: 4, tn: "Ilık", a: 3, b: 1, j: {k: "four", n: 4}, cn: "apfel", cx: "facade"}',
    # The last place dropped: a document written after it holds no value
    # for it, one fewer than its schema's places.
    "alter collection shapes drop field last",
    'put shapes {req: 5, a: 4, b: 2, cn: "Zebra", cx: "Zürich"}',
    'set shapes {b: 5} where req = 4',
    "del shapes where req = 3",
]

# A block across collections (kind 9): /batch, its statements one record.
BATCH = [
    'put extra {k: "x", n: 1}',
    'put orders {ref: "o-3", status: "open", total: 50}',
]

# A block holding every schema change a block takes beside writes -- a
# collection made and dropped, an index added, a field added, a statement
# defined -- which a version before 10.4 refused as corrupt.
SCHEMA_BATCH = [
    "create collection blocked (n int, s text)",
    'put blocked {n: 1, s: "in a block"}',
    "create index on blocked (n) @hash",
    "alter collection blocked add field m int",
    'put blocked {n: 2, s: "after its field", m: 2}',
    "create statement blocked_n($n int) as get blocked select n, s, m where n = $n",
    "create collection blocked_gone (n int)",
    "drop collection blocked_gone",
]

SCHEDULED = "create statement tick every 1m as insert ticks {k: 'tick'} if absent"
# A slot refused: `tick`'s row is there, so this insert clashes on `k`'s
# `@unique` (409), and the slot's mark says so.
REFUSED = "create statement tock every 1m as insert ticks {k: 'tick'}"
# Every other schedule a record writes -- months, a cron, a named zone and
# an offset -- defined and dropped in one block, so no slot runs between.
SCHEDULES = [
    "create statement monthly every 1mo as del ticks where k = 'never'",
    "create statement nightly at '0 3 * * *' in 'Europe/Berlin' as del ticks where k = 'never'",
    "create statement offset_daily every 1d in '+03:00' as del ticks where k = 'never'",
    "drop statement monthly",
    "drop statement nightly",
    "drop statement offset_daily",
]
HOOKED = (
    "create statement gh_issue($n int from event.issue.number, $a text from event.action) "
    "on hook github as insert issues {n: $n, action: $a}"
)
HOOK_EVENT = b'{"action":"opened","issue":{"number":7,"title":"a fixture"}}'
# The migration `/_schema/apply` runs once and records in `_migrations`.
MIGRATION = 'put extra {k: "migrated", n: 2}'

# The shell's file: what an app's own process writes, each `-c` an open and
# a close -- among them a field's expiry set, as the shell writes it.
SHELL = [
    "create collection notes (n int @hash, body text @text, at timestamp @sorted)",
    'put notes [{n: 1, body: "first note", at: "2026-02-01T00:00:00Z"}, {n: 2, body: "second", at: 1769990400000}]',
    'set notes {body: "second, edited"} where n = 2',
    'put notes {n: 3, body: "third"}',
    "del notes where n = 1",
    "alter collection notes alter field at @ttl(36500d)",
]

# The checkpoint's graphs: every metric, precision and quantization, a row
# whose id takes 8 bytes, rows sharing a vector (aliases) and tombstones
# carrying their vectors in each arena -- f32, f16, int8, and a bit index's
# codes, which it takes only past 2 048 vectors (`BIT_TRAIN`), so `coded`
# holds 2 100 of 4 dimensions.
GRAPH = [
    "create collection vecs (n int @hash, c vector<8> @hnsw(cosine, m=8), l vector<8> @hnsw(l2, m=8), "
    "d vector<8> @hnsw(dot, m=8), q vector<8> @hnsw(cosine, m=8, quant=int8), "
    "qh vector<8, f16> @hnsw(l2, m=8, quant=int8), bits vector<8> @hnsw(cosine, m=8, quant=bit), "
    "h vector<8, f16> @hnsw(cosine, m=8))",
    "create collection coded (n int @hash, b vector<4> @hnsw(cosine, m=4, quant=bit), "
    "bh vector<4, f16> @hnsw(cosine, m=4, quant=bit))",
]
VEC_FIELDS = ["c", "l", "d", "q", "qh", "bits", "h"]
VECS = 40
ALIASES = 4
BIG_ID = 5_000_000_000
CODED = 2100


def die(msg):
    sys.exit(f"compat_fixtures: {msg}")


def unit(seed, dim):
    """`dim` numbers of 4 decimals in [-0.5, 0.5), from `seed` alone: a
    vector no other seed gives (`check_unique`), and one an f16 holds apart
    from the others."""
    x = (seed * 2654435761 + 12345) & 0xFFFFFFFF
    out = []
    for _ in range(dim):
        x = (x * 1103515245 + 12345) & 0x7FFFFFFF
        out.append(round((x % 10000) / 10000 - 0.5, 4))
    return out


def lit(v):
    return "[" + ", ".join(f"{x:.4f}" for x in v) + "]"


def direction(v):
    n = sum(x * x for x in v) ** 0.5
    return tuple(round(x / n, 3) for x in v)


def check_unique(vectors, what):
    seen = set()
    for v in vectors:
        d = direction(v)
        if d in seen:
            die(f"{what}: two vectors of one direction, which would alias")
        seen.add(d)


class Server:
    """A fennec-server on a port the system picks, found by its
    `listening on:` line, stopped by SIGTERM: the one shutdown."""

    def __init__(self, binary, args, log):
        self.log = open(log, "w")
        self.proc = subprocess.Popen(
            [binary, "--http", "127.0.0.1:0", *args], stdout=self.log, stderr=subprocess.PIPE, text=True
        )
        self.addr = None
        found = threading.Event()

        def read():
            for line in self.proc.stderr:
                self.log.write(line)
                self.log.flush()
                m = re.search(r"listening on: http://(\S+)", line)
                if m and not found.is_set():
                    self.addr = m.group(1)
                    found.set()
            found.set()

        threading.Thread(target=read, daemon=True).start()
        if not found.wait(60) or not self.addr:
            self.stop()
            die(f"{binary} said no address (see {log})")
        self.seq = 0

    def ask(self, method, path, body=b"", headers=None, token=TOKEN):
        host, port = self.addr.rsplit(":", 1)
        c = http.client.HTTPConnection(host, int(port), timeout=120)
        h = dict(headers or {})
        if token:
            h["Authorization"] = f"Bearer {token}"
        c.request(method, path, body=body, headers=h)
        r = c.getresponse()
        data = r.read().decode()
        seq = r.getheader("Fennec-Seq")
        if seq:
            self.seq = int(seq)
        c.close()
        return r.status, data

    def query(self, q):
        status, body = self.ask("POST", "/query", json.dumps({"query": q}).encode(), {"Content-Type": "application/json"})
        if status != 200:
            die(f"{q}: {status} {body}")
        return body

    def batch(self, lines):
        body = "\n".join(json.dumps({"query": q}) for q in lines).encode()
        status, out = self.ask("POST", "/batch", body, {"Content-Type": "application/x-ndjson"})
        if status != 200:
            die(f"batch: {status} {out}")

    def stop(self):
        if self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(120)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
                die("a server did not stop on SIGTERM")
        self.log.close()


def wait_for(what, check, seconds):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        got = check()
        if got:
            return got
        time.sleep(0.5)
    die(f"{what}: not within {seconds} s")


def run(*cmd, **kw):
    r = subprocess.run(cmd, capture_output=True, text=True, **kw)
    if r.returncode != 0:
        die(f"{' '.join(map(str, cmd))}: exit {r.returncode}\n{r.stdout}{r.stderr}")
    return r.stdout


def signed(event):
    return "sha256=" + hmac.new(HOOK_SECRET.encode(), event, hashlib.sha256).hexdigest()


def hook_request():
    return {
        "method": "POST",
        "path": "/_hooks/github",
        "headers": {"Content-Type": "application/json", "X-Hub-Signature-256": signed(HOOK_EVENT), "X-GitHub-Event": "issues"},
        "body": HOOK_EVENT.decode(),
        "token": False,
    }


def idempotent_request():
    return {
        "method": "POST",
        "path": "/query",
        "headers": {"Content-Type": "application/json", "Idempotency-Key": IDEMPOTENCY_KEY},
        "body": json.dumps({"query": IDEMPOTENT}),
        "token": True,
    }


def send(s, req):
    return s.ask(req["method"], req["path"], req["body"].encode(), req["headers"], token=TOKEN if req["token"] else None)


def serve_args(file, hooks):
    return ["--file", str(file), "--http-token", TOKEN, "--replication-token", REPLICATION, "--no-checkpoint",
            "--hooks", str(hooks), "--auth-delay", "0", "--idempotency-ttl", IDEMPOTENCY_TTL]


def write_server(bins, work, out):
    """server.fennec, its sealed archive and backup, a replica's file of it
    and that replica promoted, and the answers its own server gave over
    HTTP once started again on it."""
    secret = work / "github.secret"
    secret.write_text(HOOK_SECRET)
    hooks = work / "hooks"
    hooks.write_text(HOOK_LINE.format(secret=secret) + "\n")
    key = out / "key"
    run(bins / "fennec", "key", key)
    s = Server(str(bins / "fennec-server"), serve_args(out / "server.fennec", hooks), work / "server.log")
    archivers = []
    try:
        for q in PHASE_A:
            s.query(q)
        s.query("compact")
        primary = f"http://{s.addr}"
        archive = out / "archive"
        plain = out / "archive-plain"
        archivers = [
            subprocess.Popen(
                [bins / "fennec", "archive", primary, archive, "--token", REPLICATION, *keyed],
                stdout=open(work / f"{archive.name}.log", "w"), stderr=subprocess.STDOUT,
            )
            for archive, keyed in ((archive, ["--key-file", key]), (plain, []))
        ]
        verify = lambda a, keyed: subprocess.run(
            [bins / "fennec", "verify", a, *keyed], capture_output=True, text=True
        )
        both = ((archive, ["--key-file", key]), (plain, []))
        wait_for("the archives' base images", lambda: all(verify(a, k).returncode == 0 for a, k in both), 60)
        for q in PHASE_B:
            s.query(q)
        s.batch(BATCH)
        s.batch(SCHEMA_BATCH)
        s.query(SCHEDULED)
        # The scheduler runs a slot once, at its time: the slot's mark (op 5)
        # and its write land then, and the statement is dropped after (op 2)
        # so no later slot writes more.
        wait_for("the scheduled statement's first slot", lambda: '"tick"' in s.query("get ticks"), 150)
        s.query("drop statement tick")
        s.query(REFUSED)
        wait_for("the refused slot", lambda: '"refused"' in s.query("statements"), 150)
        # A slot that wrote nothing lands its mark with the next block.
        s.query('put extra {k: "after the refused slot", n: 5}')
        s.query("drop statement tock")
        s.batch(SCHEDULES)
        s.query(HOOKED)
        status, body = send(s, hook_request())
        if status // 100 != 2:
            die(f"the hook's event: {status} {body}")
        # The server's own collections: a write kept by its key
        # (`_idempotency`), a consumer of the changes and where it stands
        # (`_consumers`), and a migration run once (`_migrations`).
        status, body = send(s, idempotent_request())
        if status != 200:
            die(f"the keyed write: {status} {body}")
        status, body = s.ask("POST", f"/_changes/consumers/{CONSUMER}", b"")
        if status // 100 != 2:
            die(f"the consumer: {status} {body}")
        s.query('put extra {k: "after the consumer", n: 3}')
        status, body = s.ask("PATCH", f"/_changes/consumers/{CONSUMER}", json.dumps({"since": s.seq}).encode(),
                             {"Content-Type": "application/json"})
        if status != 200:
            die(f"the consumer's commit: {status} {body}")
        status, described = s.ask("GET", "/_schema")
        if status != 200:
            die(f"/_schema: {status} {described}")
        description = json.loads(described)
        description["migrations"] = [MIGRATION]
        status, body = s.ask("POST", "/_schema/apply", json.dumps(description).encode(), {"Content-Type": "application/json"})
        if status != 200 or '"applied":true' not in body.replace(" ", ""):
            die(f"/_schema/apply: {status} {body}")
        s.query("get kinds count")
        last = s.seq

        def caught_up():
            for a, k in both:
                r = verify(a, k)
                m = re.search(r"restores changes \d+ to (\d+)", r.stdout)
                if not (r.returncode == 0 and m and int(m.group(1)) >= last):
                    return False
            return True

        wait_for(f"the archives at change {last}", caught_up, 60)
        for a in archivers:
            a.send_signal(signal.SIGINT)
            a.wait(60)
        archivers = []
        run(bins / "fennec", "backup", primary, out / "backup.sealed", "--token", REPLICATION, "--key-file", key)
        write_replica(bins, work, out, primary, last)
    finally:
        for a in archivers:
            if a.poll() is None:
                a.kill()
                a.wait()
        s.stop()
    # Started again on its file, as a later build is: what the release's
    # own server answers then is what that build's is held to over HTTP.
    s = Server(str(bins / "fennec-server"), serve_args(out / "server.fennec", hooks), work / "server-again.log")
    try:
        queries = []
        for q in statements(COMPAT / "server.fennecql"):
            queries.append(list(s.ask("POST", "/query", json.dumps({"query": q}).encode(), {"Content-Type": "application/json"})))
        description["migrations"] = [MIGRATION]
        requests = [
            # Sent again with its key: answered as it was, written once.
            idempotent_request(),
            # Sent again: taken once by its key, answered as taken.
            hook_request(),
            {"method": "GET", "path": "/_changes/consumers", "headers": {}, "body": "", "token": True},
            {"method": "GET", "path": "/_schema", "headers": {}, "body": "", "token": True},
            # The migration recorded: none left to run.
            {"method": "POST", "path": "/_schema/plan", "headers": {"Content-Type": "application/json"},
             "body": json.dumps(description), "token": True},
        ]
        for r in requests:
            r["status"], r["answer"] = send(s, r)
    finally:
        s.stop()
    serve = {
        "args": ["--http-token", TOKEN, "--replication-token", REPLICATION, "--no-checkpoint", "--auth-delay", "0",
                 "--idempotency-ttl", IDEMPOTENCY_TTL],
        "hooks": HOOK_LINE,
        "secret": HOOK_SECRET,
        "token": TOKEN,
    }
    (out / "server.http.json").write_text(
        json.dumps({"serve": serve, "queries": queries, "requests": requests}, indent=1, ensure_ascii=False) + "\n"
    )


def write_replica(bins, work, out, primary, last):
    """replica.fennec: a replica's file, following its primary to change
    `last`; promoted.fennec: that file promoted, its history forked twice."""
    r = Server(
        str(bins / "fennec-server"),
        ["--file", str(out / "replica.fennec"), "--replica-of", primary, "--replication-token", REPLICATION,
         "--http-token", TOKEN, "--no-checkpoint", "--auth-delay", "0"],
        work / "replica.log",
    )
    try:
        def at():
            status, body = r.ask("GET", "/_replication/status", token=REPLICATION)
            return status == 200 and json.loads(body).get("seq", 0) >= last

        wait_for(f"the replica at change {last}", at, 60)
    finally:
        r.stop()
    shutil.copyfile(out / "replica.fennec", out / "promoted.fennec")
    p = Server(
        str(bins / "fennec-server"),
        ["--file", str(out / "promoted.fennec"), "--promote", "--http-token", TOKEN, "--no-checkpoint",
         "--auth-delay", "0"],
        work / "promoted.log",
    )
    try:
        p.query('put extra {k: "promoted", n: 4}')
    finally:
        p.stop()


def write_graph(bins, work, out):
    """graph.fennec: the graphs a server's shutdown writes in its checkpoint."""
    rows = []
    for k in range(VECS + ALIASES):
        src = k if k < VECS else k - VECS
        rows.append("{n: %d, %s}" % (k, ", ".join(f"{f}: {lit(unit(src * 16 + i, 8))}" for i, f in enumerate(VEC_FIELDS))))
    for i, f in enumerate(VEC_FIELDS):
        check_unique([unit(k * 16 + i, 8) for k in range(VECS)] + [unit(BIG_ID + i, 8)], f"vecs.{f}")
    big = "{id: %d, n: 99, %s}" % (BIG_ID, ", ".join(f"{f}: {lit(unit(BIG_ID + i, 8))}" for i, f in enumerate(VEC_FIELDS)))
    check_unique([unit(100_000 + k, 4) for k in range(CODED)], "coded.b")
    coded = ", ".join(
        "{n: %d, b: %s, bh: %s}" % (k, lit(unit(100_000 + k, 4)), lit(unit(200_000 + k, 4))) for k in range(CODED)
    )
    s = Server(str(bins / "fennec-server"), ["--file", str(out / "graph.fennec"), "--http-token", TOKEN], work / "graph.log")
    try:
        for q in GRAPH:
            s.query(q)
        s.query(f"put vecs [{', '.join(rows)}]")
        s.query(f"put vecs {big}")
        s.query(f"put coded [{coded}]")
        # Rows of a vector of their own: their nodes stay, tombstones that
        # carry their vectors. One sharing its vector: its node lives on.
        s.query("del vecs where n in [7, 11, 19]")
        s.query(f"del vecs where n = {VECS + 1}")
        s.query("del coded where n in [5, 500, 1500]")
    finally:
        s.stop()
    if "checkpoint written" not in (work / "graph.log").read_text():
        die("graph.fennec: the server wrote no checkpoint")


def write_shell(bins, out):
    """shell.fennec: the shell's own file, an open and a close a statement."""
    for q in SHELL:
        run(bins / "fennec", out / "shell.fennec", "-c", q)


def write_page(bins, work, out):
    """The page's stores, the replica's sync among them: a server of the
    release's own for it to sync with."""
    s = Server(str(bins / "fennec-server"), ["--file", str(work / "sync.fennec"), "--http-token", TOKEN],
               work / "sync.log")
    try:
        s.query("create collection tasks (title text @hash, done bool)")
        s.query('put tasks [{title: "seeded", done: false}, {title: "seeded too", done: true}]')
        run("node", COMPAT / "web.mjs", bins / "web", out, COMPAT / "page.fennecql", f"http://{s.addr}", TOKEN)
    finally:
        s.stop()


def statements(path):
    return [l for l in Path(path).read_text().splitlines() if l.strip() and not l.startswith("--")]


def sums(out):
    """SHA256SUMS: every file of the set by its sum, as `sha256sum -c`
    reads it -- what `make version` holds the set to."""
    lines = []
    for p in sorted(out.rglob("*")):
        if p.is_file() and p.name != "SHA256SUMS":
            lines.append(f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.relative_to(out).as_posix()}")
    (out / "SHA256SUMS").write_text("\n".join(lines) + "\n")


def readme(out, versions):
    names = sorted(p.name for p in out.iterdir())
    text = (COMPAT / "README.template.md").read_text()
    for k, v in versions.items():
        text = text.replace("{" + k + "}", v)
    text = text.replace("{files}", ", ".join(names + ["SHA256SUMS"]))
    (out / "README.md").write_text(text)


def write(bins, out):
    bins = Path(bins).resolve()
    out = Path(out).resolve()
    if out.exists() and any(out.iterdir()):
        die(f"{out} holds files already: a set is written once, by its release")
    work = Path(tempfile.mkdtemp(prefix="fennec-compat-"))
    stage = work / "set"
    stage.mkdir()
    try:
        write_server(bins, work, stage)
        write_graph(bins, work, stage)
        write_shell(bins, stage)
        for what in ("kept", "waiting", "spill", "synclog", "synclog-torn", "torn-wide", "torn-tail", "torn-mark"):
            run(bins / "compat_writer", what, stage / f"{what}.fennec")
        write_page(bins, work, stage)
        # The lock files the binaries leave beside what they wrote are no
        # part of a database.
        for lock in stage.glob("*.lock"):
            lock.unlink()
        # The answers, each file read as it lies -- the sync log beside the
        # file it names, its inode -- a sealed one restored first.
        lists = {"server": "server", "replica": "replica", "promoted": "replica", "shell": "shell", "graph": "graph",
                 "kept": "kept", "waiting": "waiting", "spill": "spill", "synclog": "synclog",
                 "synclog-torn": "synclog", "torn-wide": "wide", "torn-tail": "torn", "torn-mark": "torn",
                 "backup": "server", "archive": "server", "archive-plain": "server", "page-opfs": "page",
                 "page-idb": "page", "page-idb-sealed": "page"}
        for name, src in lists.items():
            shutil.copyfile(COMPAT / f"{src}.fennecql", stage / f"{name}.fennecql")
        for name in ("server", "replica", "promoted", "shell", "graph", "kept", "waiting", "spill", "synclog",
                     "synclog-torn", "torn-wide", "torn-tail", "torn-mark", "page-opfs"):
            run(bins / "compat_writer", "answers", stage / f"{name}.fennec", stage / f"{name}.fennecql",
                stage / f"{name}.answers")
        for name, src, keyed in (("backup", stage / "backup.sealed", ["--key-file", stage / "key"]),
                                 ("archive", stage / "archive", ["--key-file", stage / "key"]),
                                 ("archive-plain", stage / "archive-plain", [])):
            restored = work / f"{name}.fennec"
            run(bins / "fennec", "restore", src, restored, *keyed)
            run(bins / "compat_writer", "answers", restored, stage / f"{name}.fennecql", stage / f"{name}.answers")
        versions = {
            "server": run(bins / "fennec-server", "--version").strip(),
            "cli": run(bins / "fennec", "--version").strip(),
            "rustc": os.environ.get("FENNEC_COMPAT_RUSTC") or (bins / "rustc-version").read_text().strip(),
            "node": run("node", "--version").strip(),
        }
        readme(stage, versions)
        sums(stage)
        out.mkdir(parents=True, exist_ok=True)
        for p in stage.iterdir():
            shutil.move(str(p), out / p.name)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    size = sum(p.stat().st_size for p in out.rglob("*") if p.is_file())
    print(f"{out}: {len(list(out.rglob('*')))} files, {size} bytes")


def build(tag, bins):
    bins = Path(bins).resolve()
    bins.mkdir(parents=True, exist_ok=True)
    scratch = Path(tempfile.mkdtemp(prefix=f"fennec-{tag}-"))
    try:
        src = scratch / "src"
        src.mkdir()
        archive = subprocess.Popen(["git", "-C", ROOT, "archive", tag], stdout=subprocess.PIPE)
        subprocess.run(["tar", "-x", "-C", src], stdin=archive.stdout, check=True)
        if archive.wait() != 0:
            die(f"git archive {tag} failed")
        shutil.copyfile(COMPAT / "writer.rs", src / "crates" / "fennec-core" / "examples" / "compat_writer.rs")
        # Nothing downloaded: cargo resolves the tag's Cargo.lock from its
        # own cache, offline. The release binaries take rustls, ring and
        # webpki-roots behind their default `tls` feature -- the only crates
        # from outside the workspace -- so a cache that lacks them is told
        # so, never fetched behind the build's back.
        env = {k: v for k, v in os.environ.items() if k != "CARGO_TARGET_DIR"}
        env["CARGO_NET_OFFLINE"] = "true"
        # The rustc beside the cargo that builds, as the tag's
        # rust-toolchain.toml picks it: Homebrew's has no wasm32.
        rustc = str(Path(CARGO).with_name("rustc")) if os.path.isabs(CARGO) else "rustc"
        rustc = subprocess.run([rustc, "--version"], cwd=src, capture_output=True, text=True, env=env).stdout.strip()
        # `--locked`: the tag's Cargo.lock as it was released, never one
        # resolved again today.
        for cmd in (
            [CARGO, "build", "--release", "--locked", "--offline", "-p", "fennec-server", "-p", "fennec-cli"],
            [CARGO, "build", "--release", "--locked", "--offline", "-p", "fennec-core", "--example", "compat_writer"],
            ["make", "wasm"],
        ):
            r = subprocess.run(cmd, cwd=src, env=env)
            if r.returncode != 0:
                die(f"{' '.join(map(str, cmd))} in {tag}'s tree failed (exit {r.returncode}). Built offline from "
                    "cargo's cache: where it lacks a crate of the tag's Cargo.lock (rustls, ring and webpki-roots, "
                    f"behind the release binaries' `tls` feature), fetch them once, online -- `git worktree add "
                    f"<dir> {tag} && cargo fetch --locked --manifest-path <dir>/Cargo.toml` -- and build again")
        release = src / "target" / "release"
        for b in ("fennec-server", "fennec"):
            shutil.copy2(release / b, bins / b)
        shutil.copy2(release / "examples" / "compat_writer", bins / "compat_writer")
        web = bins / "web"
        shutil.rmtree(web, ignore_errors=True)
        web.mkdir()
        for p in (src / "web").iterdir():
            if p.suffix == ".js" and not p.name.endswith(".test.js") or p.name in ("fennec.wasm", "package.json"):
                shutil.copy2(p, web / p.name)
        shutil.copytree(src / "web" / "collate", web / "collate")
        (bins / "rustc-version").write_text(rustc + "\n")
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    print(f"{bins}: {tag}'s fennec-server, fennec, compat_writer and web/, built with {rustc}")


def main():
    args = sys.argv[1:]
    if len(args) == 3 and args[0] == "build":
        build(args[1], args[2])
    elif len(args) == 3 and args[0] == "write":
        write(args[1], args[2])
    else:
        die("usage: compat_fixtures.py build <tag> <bin-dir> | write <bin-dir> <set-dir>")


if __name__ == "__main__":
    main()
