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
        the tag into a scratch directory, `cargo build --locked --offline`
        of fennec-server and fennec as the tag's release.yml builds them
        (`--release` and `--profile cli`, for the host's target; their
        crates from outside the workspace, behind `tls`, from cargo's
        cache: nothing downloaded), the tag's own `make wasm`, and
        tools/compat/writer.rs built as an example of the tag's
        fennec-core, by the pinned compiler and no other; the binaries, the
        tag's web/ and its @fennecdb/cloudflare module copied into
        <bin-dir> with MANIFEST.json naming each by its SHA-256, and the
        scratch directory, its target/ with it, deleted
    python3 tools/compat_fixtures.py write <bin-dir> <set-dir>
        the set, written by those binaries (macOS or Linux) once each is
        the bytes the manifest names and every file of the tag's tree the
        tag's own; the manifest in the set beside the files, and their
        sums last in SHA256SUMS, whose digest the set's `RELEASES` line pins
        and `make version` and the compat tests hold the set to. <set-dir>
        is named by the tag, `v0.2.0`, or `v0.2.0+2` for a further set of
        the same tag, which holds what a landed one was found missing
    make compat-fixtures V=0.2.0
        both, into crates/fennec-core/tests/compat/v0.2.0/

What it needs, checked before anything is built or written: rustup, with
the tag's toolchain and its wasm32-unknown-unknown target -- the version
the tag's rust-toolchain.toml names, or, where it names a channel
(v0.2.0's says `stable`), the version its binaries were built with
(`TOOLCHAINS`); git with the tag; node, for the tag's web client; and
Docker with the image `make pgvector-up` runs (`PG_IMAGE` in the
Makefile) already on the machine -- nothing is pulled -- for `--follow`'s
file, a PostgreSQL server in a container of the writer's own, on a port
the system picks, removed after (`FENNEC_COMPAT_PG` names it).

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

# The toolchain each release's binaries were built with, where its
# rust-toolchain.toml names a channel rather than a version: v0.2.0's says
# `stable`, which was 1.99.1 when it was released and moves since. A tag
# whose file names a version builds with that one.
TOOLCHAINS = {"v0.2.0": "1.99.1"}

TOKEN = "compat-token"
ADMIN = "compat-admin"
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
    # The id counter (kind 7) is the image's alone once the compact drops
    # the highest row: the next id is past every live one, never one the
    # release handed out before.
    "create collection retired (n int @hash, s text)",
    'put retired [{n: 1, s: "kept"}, {n: 2, s: "kept too"}, {n: 3, s: "retired before the compact"}]',
    "del retired where n = 3",
    "create statement order_by_ref($r text) as get orders select ref, status, total where ref = $r",
    "create statement on_order after insert orders as insert audit {ref: new.ref, what: 'placed'}",
    'put orders [{ref: "o-1", status: "open", total: 2000}, {ref: "o-2", status: "open", total: 500}]',
    # Numbers past their type's range, which the release stored where a
    # later build refuses them as they are written: a float of 1e400 and
    # -1e400 (infinities), a vector's component of 1e39 (an f32's
    # infinity), an f16's past 65 504 (its infinity) -- in a field, in a
    # list -- and a named statement whose text holds 1e400.
    "create collection edges (n int @hash, f float @sorted, v vector<4>, vh vector<4, f16>, lv [vector<2>], "
    "lf [float])",
    "put edges {n: 1, f: 1e400, v: [1e39, 0, 0, 0], vh: [70000, 0, 0, 1], lv: [[1e39, 1], [0, 1]], "
    "lf: [1e400, -1e400, 2.5]}",
    "put edges {n: 2, f: -1e400, v: [-1e39, 1, 0, 0], vh: [-70000, 1, 0, 0], lv: [[1, -1e39]], lf: []}",
    "put edges {n: 3, f: 1.5, v: [0, 0, 1, 0], vh: [0.5, 0, 0, 0], lv: [], lf: [0.5]}",
    "create statement past_range() as get edges select n where f >= 1e400",
    # Negative numbers -- an int and a time before 1970 -- in a field, a
    # list and json, and the keys of ordered indexes across zero.
    "create collection signed (n int @sorted, at timestamp @sorted, l [int], ts [timestamp], j json)",
    "create index on signed (j.n) @sorted",
    'put signed [{n: -5, at: "1969-07-20T20:17:40Z", l: [-3, 0, 3], ts: ["1969-12-31T23:59:59.999Z", 0], '
    'j: {n: -42, list: [-1, 2], deep: {m: -7}}}, '
    '{n: 0, at: "1970-01-01T00:00:00Z", l: [], ts: [], j: {n: 0}}, '
    '{n: 7, at: "2026-01-01T00:00:00Z", l: [-9007199254740993, 9007199254740993], ts: [-86400000], j: {n: 42}}, '
    '{n: -300, at: -2208988800000, l: [-1], j: {n: -1}}]',
    # One index of every kind that takes options with each away from its
    # default: `@text`'s k1, b, prefix and prefix_min on each of its kinds
    # and beside a `@hash`, `@hnsw`'s metric, m, ef_construction and
    # ef_search, plain and quantized.
    "create collection tuned (n int @hash, t text @text(k1=1.5, b=0.9, prefix=6, prefix_min=2), "
    "c text @text(chars, k1=1.2, b=0.6, prefix=5, prefix_min=1), "
    "a text @text(accents, k1=2, b=1, prefix=4, prefix_min=2), "
    "h text @hash @text(k1=0.5, b=0.2, prefix=3, prefix_min=1), "
    "v vector<4> @hnsw(l2, m=12, ef_construction=40, ef_search=24), "
    "q vector<4> @hnsw(dot, m=6, ef_construction=50, ef_search=30, quant=int8))",
    'put tuned [{n: 1, t: "fox", c: "東京タワー", a: "café", h: "red fox", v: [1, 0, 0, 0], q: [1, 0, 0, 0]}, '
    '{n: 2, t: "fox fox fox and the quick brown dog jumps over the lazy cat once again", c: "京都", '
    'a: "cafe crème brûlée", h: "a fox and a fox", v: [0, 1, 0, 0], q: [0, 1, 0, 0]}, '
    '{n: 3, t: "the quick fox", c: "東京", a: "naïve café", h: "fox", v: [0, 0, 1, 0], q: [0, 0, 1, 0]}, '
    '{n: 4, t: "a quiet quokka", c: "大阪", a: "Zürich", h: "blue whale", v: [0.5, 0.5, 0, 0], '
    'q: [0.5, 0.5, 0, 0]}]',
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


# A graph past 65 536 nodes, whose record takes links of three bytes, and
# steps of three between a list's links once two of them are more than 65 535
# nodes apart: the smallest one the release writes so -- one dimension, `m`
# 2, the least it takes -- of 70 000 vectors put in an order of their own,
# so a node's nearest are nodes made far apart.
WIDE_NODES = 70_000
WIDE_BATCH = 5_000


def wide_value(n):
    """The `n`th vector's one number: a permutation of 0..WIDE_NODES, from
    `n` alone, each a whole number an f32 holds."""
    return (n * 40_503 + 12_345) % WIDE_NODES


def write_graph_wide(bins, work, out):
    """graph-wide.fennec: the checkpoint a server's shutdown writes of a
    graph of 70 000 nodes, its links and steps three bytes wide."""
    if len({wide_value(n) for n in range(WIDE_NODES)}) != WIDE_NODES:
        die("graph-wide: two vectors alike, which would alias")
    s = Server(str(bins / "fennec-server"), ["--file", str(out / "graph-wide.fennec"), "--http-token", TOKEN,
                                             "--auth-delay", "0"], work / "graph-wide.log")
    try:
        s.query("create collection wide (n int @hash, v vector<1> @hnsw(l2, m=2))")
        for start in range(0, WIDE_NODES, WIDE_BATCH):
            rows = ", ".join("{n: %d, v: [%d]}" % (n, wide_value(n)) for n in range(start, start + WIDE_BATCH))
            s.query(f"put wide [{rows}]")
    finally:
        s.stop()
    if "checkpoint written" not in (work / "graph-wide.log").read_text():
        die("graph-wide.fennec: the server wrote no checkpoint")


# What the release writes that a later build reads otherwise on purpose
# (docs/design/compatibility.md, *What 1.x does not read of 0.2.0's
# files*), each a small file of the shell's: a row of a `[[json]]` field
# nesting 66 levels in all, refused where it is read; a field of 65 nested
# lists, one of vectors past a dimension's range in a list (SQL's array of
# them) and a point under 64 lists, each refused at the open. A later
# build's test holds each to its words.
LISTS = "[" * 65 + "int" + "]" * 65
POINTS = "[" * 64 + "geo" + "]" * 64
NESTED_JSON = "{a: " * 64 + "1" + "}" * 64
ON_PURPOSE = {
    "deep": [
        "create collection plain (n int @hash, s text)",
        'put plain [{n: 1, s: "one"}, {n: 2, s: "two"}]',
        "create collection deep (n int @hash, j [[json]])",
        f"put deep [{{n: 1, j: [[{{shallow: true}}]]}}, {{n: 2, j: [[{NESTED_JSON}]]}}]",
    ],
    "refused-lists": [
        "create collection plain (n int @hash, s text)",
        'put plain {n: 1, s: "beside it"}',
        f"create collection lists (l {LISTS})",
    ],
    "refused-dims": [
        "create table plain (n int, s text)",
        "insert into plain (n, s) values (1, 'beside it')",
        "create table wide (e vector(1000000001)[])",
    ],
    "points-deep": [
        "create collection plain (n int @hash, s text)",
        'put plain {n: 1, s: "beside it"}',
        f"create collection points (g {POINTS})",
        "put points {g: " + "[" * 64 + "[13.4, 52.5]" + "]" * 64 + "}",
    ],
}


def write_on_purpose(bins, out):
    for name, writes in ON_PURPOSE.items():
        for q in writes:
            run(bins / "fennec", out / f"{name}.fennec", "-c", q)


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


def pg_image():
    """The PostgreSQL image `make pgvector-up` runs, by its digest: one
    source for both."""
    m = re.search(r"^PG_IMAGE \?= (\S+)$", (ROOT / "Makefile").read_text(), flags=re.M)
    if not m:
        die("the Makefile names no PG_IMAGE")
    return m.group(1)


# The table `--follow` mirrors: its copy, then a change of each kind after
# it, streamed through the slot.
FOLLOW_TABLE = [
    "create table items (id bigint primary key, name text not null, qty integer, price double precision, "
    "at timestamptz, ok boolean)",
    "insert into items values (1, 'one', 1, 1.5, '2026-01-01T00:00:00Z', true), "
    "(2, 'two', 2, 2.5, '2026-01-02T00:00:00Z', false), (3, 'three', 3, 3.5, '2026-01-03T00:00:00Z', null)",
]
FOLLOW_CHANGES = [
    "insert into items values (4, 'four', 4, 4.5, '2026-01-04T00:00:00Z', true)",
    "update items set qty = 20, name = 'two, updated' where id = 2",
    "delete from items where id = 3",
]


def write_follow(bins, work, out):
    """follow.fennec: a PostgreSQL table mirrored by the release's
    `fennec-server --follow` -- its copy, then an insert, an update and a
    delete streamed through its slot -- and `_follow`, the record of which
    collection follows which slot and whether its copy is whole. The
    server is the image `make pgvector-up` runs, in a container of this
    writer's own on a port the system picks, removed after; nothing is
    pulled."""
    image = pg_image()
    if subprocess.run(["docker", "image", "inspect", image], capture_output=True).returncode != 0:
        die(f"{image} is not on this machine: `docker pull {image}` once (the writer pulls nothing)")
    name = os.environ.get("FENNEC_COMPAT_PG") or f"fennec-compat-pg-{os.getpid()}"
    run("docker", "run", "-d", "--rm", "--pull=never", "--name", name, "-e", "POSTGRES_PASSWORD=fennec",
        "-e", "POSTGRES_DB=compat", "-p", "127.0.0.1::5432", image, "-c", "wal_level=logical")
    try:
        ready = lambda: subprocess.run(["docker", "exec", name, "pg_isready", "-h", "127.0.0.1", "-U", "postgres",
                                        "-d", "compat"], capture_output=True).returncode == 0
        wait_for("PostgreSQL", ready, 120)
        port = run("docker", "port", name, "5432").split()[0].rsplit(":", 1)[1]

        def psql(sql):
            run("docker", "exec", name, "psql", "-v", "ON_ERROR_STOP=1", "-U", "postgres", "-d", "compat", "-c", sql)

        for sql in FOLLOW_TABLE:
            psql(sql)
        url = f"postgres://postgres:fennec@127.0.0.1:{port}/compat?sslmode=disable"
        s = Server(str(bins / "fennec-server"),
                   ["--file", str(out / "follow.fennec"), "--http-token", TOKEN, "--auth-delay", "0",
                    "--follow", url, "--follow-table", "items"], work / "follow.log")
        try:
            def rows():
                status, body = s.ask("POST", "/query", json.dumps({"query": "get items order id"}).encode(),
                                     {"Content-Type": "application/json"})
                return json.loads(body) if status == 200 else None

            wait_for("the copy", lambda: (r := rows()) is not None and len(r) == 3, 60)
            for sql in FOLLOW_CHANGES:
                psql(sql)
            wait_for("the changes streamed",
                     lambda: (r := rows()) is not None and [x["id"] for x in r] == [1, 2, 4]
                     and r[1].get("qty") == 20, 60)
        finally:
            s.stop()
    finally:
        subprocess.run(["docker", "rm", "-f", name], capture_output=True)


def now_ms():
    return time.time_ns() // 1_000_000


def iso(ms):
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(ms // 1000)) + f".{ms % 1000:03d}Z"


NODE_WRITES = {
    "a": ["create collection notes (n int @hash, s text)", 'put notes [{n: 1, s: "a one"}, {n: 2, s: "a two"}]'],
    "b": ["create collection notes (n int @hash, s text)", 'put notes {n: 1, s: "b, its first life"}'],
    "c": ["create collection notes (n int @hash, s text)", 'put notes {n: 1, s: "c, before it moved"}'],
}


def write_node(bins, work, out):
    """node-archive/: every tenant of the release's tenant node archived by
    its `fennec archive --tenants --image-every 1s`, over the node's one
    stream, then pruned by its `fennec prune --tenants --keep 1s`: `a`,
    images of it taken by the archiver and its first let go of, its line's
    `began` written before; `b` deleted -- noted gone in its `presence` --
    then made again under its name, its first line moved aside to
    `b.<history>/` and a new one begun; `c` moved away -- its image taken,
    the tenant deleted, the image installed again -- noted gone and back.
    And what the release's `fennec verify --tenants` and `prune` said of
    it, and its `fennec restore --tenants` to the end and to moments --
    before the tenants, with all three, with `b` gone, with `c` away and
    with `b` back -- each tenant restored answering `node.fennecql`
    (node-archive.json)."""
    tenants = work / "tenants"
    tenants.mkdir()
    s = Server(str(bins / "fennec-server"),
               ["--dir", str(tenants), "--admin-token", ADMIN, "--replication-token", REPLICATION,
                "--http-token", TOKEN, "--auth-delay", "0", "--no-checkpoint"], work / "node.log")
    root = out / "node-archive"
    archiver = None
    moments = {}
    try:
        def admin(method, path, body=b""):
            host, port = s.addr.rsplit(":", 1)
            c = http.client.HTTPConnection(host, int(port), timeout=120)
            c.request(method, f"/_admin/tenants/{path}", body=body, headers={"Authorization": f"Bearer {ADMIN}"})
            r = c.getresponse()
            data = r.read()
            c.close()
            if r.status // 100 != 2:
                die(f"{method} /_admin/tenants/{path}: {r.status} {data[:200]!r}")
            return data

        def query(tenant, q):
            status, body = s.ask("POST", f"/t/{tenant}/query", json.dumps({"query": q}).encode(),
                                 {"Content-Type": "application/json"})
            if status != 200:
                die(f"{tenant}: {q}: {status} {body}")

        archiver = subprocess.Popen(
            [bins / "fennec", "archive", "--tenants", f"http://{s.addr}", root, "--token", REPLICATION,
             "--image-every", "1s"],
            stdout=open(work / "node-archive.log", "w"), stderr=subprocess.STDOUT)

        def lines():
            r = subprocess.run([bins / "fennec", "verify", "--tenants", root], capture_output=True, text=True)
            return r.stdout if r.returncode == 0 else ""

        def last(label):
            """The last change `label`'s line restores, as verify says it."""
            m = re.search(rf"^{re.escape(label)}: changes \d+ to (\d+),", lines(), flags=re.M)
            return int(m.group(1)) if m else -1

        def archived(label, write):
            """`write` run, and waited for in `label`'s line."""
            was = last(label)
            write()
            wait_for(f"{label}'s write archived", lambda: last(label) > was, 60)

        def noted(label, word):
            presence = root / label / "presence"
            return presence.is_file() and word in presence.read_text()

        def moment(name):
            # Moments the node's clock and the archive's times tell apart.
            time.sleep(0.05)
            moments[name] = now_ms()
            time.sleep(0.05)

        moment("before")
        for tenant, writes in NODE_WRITES.items():
            admin("PUT", tenant)
            for q in writes:
                query(tenant, q)
        wait_for("the tenants archived", lambda: all(last(x) >= 2 for x in NODE_WRITES), 60)
        archived("a", lambda: query("a", 'put notes {n: 3, s: "a three"}'))
        moment("all three")
        admin("DELETE", "b")
        wait_for("b noted gone", lambda: noted("b", "gone"), 60)
        moment("b gone")
        image = admin("GET", "c/file")
        admin("DELETE", "c")
        wait_for("c noted gone", lambda: noted("c", "gone"), 60)
        moment("c away")
        admin("PUT", "c/file", image)
        wait_for("c noted back", lambda: noted("c", "back"), 60)
        archived("c", lambda: query("c", 'put notes {n: 2, s: "c, back where it was"}'))
        admin("PUT", "b")
        for q in ["create collection notes (n int @hash, s text)", 'put notes {n: 1, s: "b, made again"}',
                  'put notes {n: 2, s: "b, its second life"}']:
            query("b", q)
        wait_for("b's first line moved aside, its second archived",
                 lambda: any(d.name.startswith("b.") for d in root.iterdir()) and last("b") >= 3, 60)
        archived("a", lambda: query("a", 'put notes {n: 4, s: "a four"}'))
        moment("b back")
        # An image the archiver takes of a's end, after the write: the one a
        # prune keeps, letting go of the first.
        wait_for("a's image of its end",
                 lambda: len(list((root / "a").glob("image-*.fennec"))) >= 2, 60)
        archiver.send_signal(signal.SIGINT)
        archiver.wait(60)
        archiver = None
    finally:
        if archiver is not None and archiver.poll() is None:
            archiver.kill()
            archiver.wait()
        s.stop()
    for lock in root.glob("*.lock"):
        lock.unlink()
    time.sleep(1.2)
    said = {"prune": run(bins / "fennec", "prune", "--tenants", root, "--keep", "1s")}
    if not (root / "a" / "began").is_file():
        die("the prune wrote no `began`: a's first image was not let go of")
    said["verify"] = run(bins / "fennec", "verify", "--tenants", root)
    said = {k: v.replace(str(root), "<root>").splitlines() for k, v in said.items()}
    restores = []
    for label, at in [("end", None), *moments.items()]:
        into = work / f"node-{label.replace(' ', '-')}"
        args = [bins / "fennec", "restore", "--tenants", root, into] + ([] if at is None else ["--to", iso(at)])
        r = subprocess.run(args, capture_output=True, text=True)
        answers = {}
        for f in sorted(into.glob("*.fennec")):
            got = work / f"{f.stem}.answers"
            run(bins / "compat_writer", "answers", f, COMPAT / "node.fennecql", got)
            answers[f.stem] = got.read_text().splitlines()
        restores.append({
            "to": None if at is None else iso(at), "label": label, "status": r.returncode,
            "said": (r.stdout + r.stderr).replace(str(into), "<out>").splitlines(),
            "answers": answers,
        })
    (out / "node-archive.json").write_text(json.dumps({**said, "restores": restores}, indent=1,
                                                      ensure_ascii=False) + "\n")


def write_wide_backup(bins, work, out):
    """backup-wide.sealed: a sealed backup of a database past a mebibyte,
    sealed whole in several frames -- the last flagged in what its tag
    covers -- its rows bytes no compressor shrinks: 1 100 rows of 1 000
    pseudo-random bytes, from a fixed seed."""
    s = Server(str(bins / "fennec-server"),
               ["--file", str(work / "wide.fennec"), "--http-token", TOKEN, "--replication-token", REPLICATION,
                "--auth-delay", "0"], work / "wide.log")
    try:
        s.query("create collection blobs (n int @hash, b bytes)")
        x = 0x2545F491
        for start in range(0, 1100, 50):
            rows = []
            for n in range(start, start + 50):
                bs = []
                for _ in range(1000):
                    x = (x * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFFFFFFFFFF
                    bs.append(str(x >> 56))
                rows.append("{n: %d, b: [%s]}" % (n, ", ".join(bs)))
            s.query(f"put blobs [{', '.join(rows)}]")
        run(bins / "fennec", "backup", f"http://{s.addr}", out / "backup-wide.sealed", "--token", REPLICATION,
            "--key-file", out / "key")
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


def forms(set_name):
    """How many forms `RELEASES`' line for the set `set_name` places in a
    file and names absent (crates/fennec-core/tests/compat/walk.rs): what
    the set's README states, and the compat tests hold it to."""
    walk = (SETS / "walk.rs").read_text()
    m = re.search(r'set: "' + re.escape(set_name) + r'",(.*?)\n    \}', walk, flags=re.S)
    if not m:
        die(f"walk.rs: RELEASES has no line for {set_name}: add it before the set is written")
    line = m.group(1)
    shapes = re.search(r"shapes: (\w+),", line).group(1)
    listed = re.search(r"const " + shapes + r": &\[&str\] = &\[(.*?)\n\];", walk, flags=re.S).group(1)
    held = len(re.findall(r'^\s*"[^"]+",$', listed, flags=re.M))
    absent = len(re.findall(r'\(\s*"[^"]+",', line.split("absent:", 1)[1]))
    numbers = sum(len(re.findall(r"\d+", re.search(k + r": &\[([^\]]*)\]", line).group(1)))
                  for k in ("kinds", "ops", "graphs", "schema_tags", "value_tags", "indexes"))
    return held, absent, numbers


def readme(out, versions):
    names = sorted(p.name for p in out.iterdir())
    text = (COMPAT / "README.template.md").read_text()
    for k, v in versions.items():
        text = text.replace("{" + k + "}", v)
    text = text.replace("{files}", ", ".join(names + ["SHA256SUMS"]))
    (out / "README.md").write_text(text)


def write_cloudflare(bins, out):
    """do-sql.json, do-kv.json: what the release's @fennecdb/cloudflare kept
    of the page's writes in a SQLite-backed Durable Object's storage and a
    key-value backed one's, over the release's own stand-ins for each, and
    the answers its module gave once it restored each."""
    run("node", COMPAT / "cloudflare.mjs", bins / "web", bins / "cloudflare", out, COMPAT / "do.fennecql")


def segment_times(archive):
    """Each write's time in a plain archive's segments, in order: a segment
    is `FENECLG\x01`, then a write's time (8 bytes) and its record, whose
    head -- its kind's byte, its collection's varint and its length's --
    says how long it is."""
    def varint(b, i):
        v = shift = 0
        while True:
            x = b[i]
            i += 1
            v |= (x & 0x7F) << shift
            shift += 7
            if x < 0x80:
                return v, i
    times = []
    for seg in sorted(archive.glob("writes-*.log")):
        b = seg.read_bytes()
        if b[:8] != b"FENECLG\x01":
            die(f"{seg}: no segment signature")
        i = 8
        while i < len(b):
            times.append(int.from_bytes(b[i:i + 8], "little"))
            _, j = varint(b, i + 9)
            length, j = varint(b, j)
            i = j + length
    return times


def archive_moments(archive, times):
    """The moments a restore of `archive` is asked for, each once: before
    its image, its image's time and -- past it, before the first write --
    the image alone, each write's time and the moment before the next."""
    image = min(int(p.stem.rsplit("-", 1)[1]) for p in archive.glob("image-*.fennec"))
    moments = [(image - 1, "before its image"), (image, "its image's time")]
    if times and times[0] - 1 > image:
        moments.append((times[0] - 1, "its image alone, before the first write"))
    for i, at in enumerate(times):
        moments.append((at, "a write's time"))
        if i + 1 < len(times) and times[i + 1] > at + 1:
            moments.append((times[i + 1] - 1, "the moment before the next write"))
    seen, out = set(), []
    for at, label in moments:
        if at not in seen:
            seen.add(at)
            out.append((at, label))
    return out


def write_restores(bins, work, out):
    """`<archive>.restores.json`: the release's own `fennec restore --to` of
    each archive at every moment a later build restores it to -- what it
    said and its exit status -- and what the database it restored to each
    change answered to restore.fennecql. The writes' times are the plain
    archive's, read from its segments: both archives carry each write at
    the time its primary appended it, which `fennec verify` says alike."""
    times = segment_times(out / "archive-plain")
    spans = {name: re.sub(r"from \S+ ", "", (out / f"{name}.verify").read_text().split("restores", 1)[1])
             for name in ("archive", "archive-plain")}
    if spans["archive"] != spans["archive-plain"]:
        die(f"the two archives restore other changes or times: {spans}")
    for name, keyed in (("archive", ["--key-file", out / "key"]), ("archive-plain", [])):
        record = {"moments": [], "answers": {}}
        for at, label in archive_moments(out / name, times):
            into = work / f"{name}-at-{at}.fennec"
            r = subprocess.run([bins / "fennec", "restore", out / name, into, "--to", iso(at), *keyed],
                               capture_output=True, text=True)
            said = (r.stdout + r.stderr).replace(str(into), "<out>").splitlines()
            entry = {"to": iso(at), "at": at, "label": label, "status": r.returncode, "said": said}
            if r.returncode == 0:
                change = re.search(r": change (\d+) -- ", r.stdout).group(1)
                entry["change"] = int(change)
                if change not in record["answers"]:
                    got = work / f"{name}-{change}.answers"
                    run(bins / "compat_writer", "answers", into, COMPAT / "restore.fennecql", got)
                    record["answers"][change] = got.read_text().splitlines()
            record["moments"].append(entry)
        if not any(m["label"].startswith("its image alone") for m in record["moments"]):
            die(f"{name}: no moment between its image and its first write")
        (out / f"{name}.restores.json").write_text(json.dumps(record, indent=1, ensure_ascii=False) + "\n")


def write(bins, out):
    bins = Path(bins).resolve()
    out = Path(out).resolve()
    if out.exists() and any(out.iterdir()):
        die(f"{out} holds files already: a set is written once, by its release")
    # A set is `<tag>`, or `<tag>+<n>` for a further one of the same tag
    # (docs/design/compatibility.md): written by that tag's programs alone,
    # each the bytes its manifest names, asked before any of them runs.
    m = re.fullmatch(r"(v\d+\.\d+\.\d+)(?:\+([2-9]|[1-9]\d+))?", out.name)
    if not m:
        die(f"{out.name}: a set is named by its tag, `v<version>`, or `v<version>+<n>` for a further one")
    tag = m.group(1)
    manifest = checked(bins, tag)
    for b in ("fennec-server", "fennec"):
        said = run(bins / b, "--version").strip()
        if said.split()[-1] != tag[1:]:
            die(f"{bins / b} says `{said}`, not {tag}")
    # What `follow.fennec`, the page and the Durable Object's storage need,
    # asked before anything is written.
    if subprocess.run(["docker", "image", "inspect", pg_image()], capture_output=True).returncode != 0:
        die(f"{pg_image()} is not on this machine: `docker pull {pg_image()}` once (the writer pulls nothing)")
    if shutil.which("node") is None:
        die("node runs the release's web client: install it")
    if subprocess.run(["node", "-e", "require('node:sqlite')"], capture_output=True).returncode != 0:
        die("node has no node:sqlite, which a SQLite-backed Durable Object's storage stands on: Node 22.13 or later")
    work = Path(tempfile.mkdtemp(prefix="fennec-compat-"))
    stage = work / "set"
    stage.mkdir()
    try:
        started = time.monotonic()
        took = {}

        def timed(what, f, *args):
            at = time.monotonic()
            f(*args)
            took[what] = time.monotonic() - at
            print(f"compat_fixtures: {what} written in {took[what]:.0f} s", flush=True)

        timed("server", write_server, bins, work, stage)
        timed("graph", write_graph, bins, work, stage)
        timed("graph-wide", write_graph_wide, bins, work, stage)
        timed("shell", write_shell, bins, stage)
        timed("on purpose", write_on_purpose, bins, stage)
        for what in ("kept", "waiting", "spill", "synclog", "synclog-torn", "synclog-spent", "synclog-unbegun",
                     "torn-wide", "torn-tail", "torn-mark", "schedules", "ids"):
            timed(what, run, bins / "compat_writer", what, stage / f"{what}.fennec")
        timed("page", write_page, bins, work, stage)
        timed("durable object", write_cloudflare, bins, stage)
        timed("follow", write_follow, bins, work, stage)
        timed("node", write_node, bins, work, stage)
        timed("backup-wide", write_wide_backup, bins, work, stage)
        # The clock every answer below is read at (writer.rs's `CLOCK`), which
        # a later build's tests read theirs at.
        (stage / "clock").write_text(run(bins / "compat_writer", "clock").strip() + "\n")
        # The lock files the binaries leave beside what they wrote are no
        # part of a database.
        for lock in stage.glob("*.lock"):
            lock.unlink()
        # The answers, each file read as it lies -- the sync log beside the
        # file it names, its inode -- a sealed one restored first.
        lists = {"server": "server", "replica": "replica", "promoted": "replica", "shell": "shell", "graph": "graph",
                 "graph-wide": "graph-wide", "kept": "kept", "waiting": "waiting", "spill": "spill",
                 "synclog": "synclog", "synclog-torn": "synclog", "synclog-spent": "synclog",
                 "synclog-unbegun": "unbegun", "torn-wide": "wide", "torn-tail": "torn", "torn-mark": "torn",
                 "schedules": "schedules", "ids": "ids", "follow": "follow", "backup": "server",
                 "backup-wide": "wide-backup", "archive": "server", "archive-plain": "server", "page-opfs": "page",
                 "page-opfs-stopped": "page", "page-idb": "page", "page-idb-sealed": "page", "do-sql": "do",
                 "do-kv": "do", "node": "node", "restore": "restore", "deep": "deep",
                 "refused-lists": "refused-lists", "refused-dims": "refused-dims", "points-deep": "points-deep"}
        for name, src in lists.items():
            shutil.copyfile(COMPAT / f"{src}.fennecql", stage / f"{name}.fennecql")
        for name in ("server", "replica", "promoted", "shell", "graph", "graph-wide", "kept", "waiting", "spill",
                     "synclog", "synclog-torn", "synclog-spent", "synclog-unbegun", "torn-wide", "torn-tail",
                     "torn-mark", "schedules", "ids", "follow", "page-opfs", "deep", "refused-lists", "refused-dims",
                     "points-deep"):
            run(bins / "compat_writer", "answers", stage / f"{name}.fennec", stage / f"{name}.fennecql",
                stage / f"{name}.answers")
        for name, src, keyed in (("backup", stage / "backup.sealed", ["--key-file", stage / "key"]),
                                 ("backup-wide", stage / "backup-wide.sealed", ["--key-file", stage / "key"]),
                                 ("archive", stage / "archive", ["--key-file", stage / "key"]),
                                 ("archive-plain", stage / "archive-plain", [])):
            restored = work / f"{name}.fennec"
            run(bins / "fennec", "restore", src, restored, *keyed)
            run(bins / "compat_writer", "answers", restored, stage / f"{name}.fennecql", stage / f"{name}.answers")
            if src.is_dir():
                # What the release's `fennec verify` said of the archive,
                # whole: its images and segments, the changes it restores and
                # the times they span, which a later build says alike.
                said = run(bins / "fennec", "verify", src, *keyed)
                (stage / f"{name}.verify").write_text(said.replace(str(src), f"<{name}>"))
        timed("restores to a moment", write_restores, bins, work, stage)
        # The programs that wrote the set, by their sums (`build`'s
        # manifest), and the inputs that drove them: what a file of the set
        # is checked against.
        manifest["inputs"] = {p.relative_to(ROOT).as_posix(): sha256(p)
                              for p in [Path(__file__).resolve(), *sorted(COMPAT.iterdir())] if p.is_file()}
        (stage / "MANIFEST.json").write_text(json.dumps(manifest, indent=1) + "\n")
        versions = {
            "server": run(bins / "fennec-server", "--version").strip(),
            "cli": run(bins / "fennec", "--version").strip(),
            "rustc": manifest["rustc"],
            "node": run("node", "--version").strip(),
            "tag": tag,
            "set": out.name,
            "commit": manifest["commit"],
            "target": manifest["target"],
        }
        held, absent, numbers = forms(out.name)
        versions["forms"] = (f"Forms: {held} held in a file of this set, {absent} named absent below, of the "
                             f"{held + absent} the walk tells apart; and {numbers} numbers -- record kinds, "
                             "statement ops, graph versions, schema tags, value tags and index kinds -- each "
                             "in a file.")
        versions["took"] = ", ".join(f"{k} {v:.0f} s" for k, v in took.items())
        versions["total"] = f"{(time.monotonic() - started) / 60:.0f}"
        readme(stage, versions)
        sums(stage)
        out.mkdir(parents=True, exist_ok=True)
        for p in stage.iterdir():
            shutil.move(str(p), out / p.name)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    size = sum(p.stat().st_size for p in out.rglob("*") if p.is_file())
    sums_digest = sha256(out / "SHA256SUMS")
    print(f"{out}: {len(list(out.rglob('*')))} files, {size} bytes; its SHA256SUMS {sums_digest}, which its "
          "RELEASES line pins (`sums:`)")


def pinned(tag, toolchain_toml):
    """The Rust release `tag`'s binaries are built with: the version its
    rust-toolchain.toml names, or where it names a channel the one in
    `TOOLCHAINS` -- `stable` today is not the stable its release was built
    with."""
    m = re.search(r'^channel = "([^"]+)"', toolchain_toml, flags=re.M)
    channel = m.group(1) if m else ""
    version = channel if re.fullmatch(r"\d+\.\d+\.\d+", channel) else TOOLCHAINS.get(tag)
    if not version:
        die(f"{tag}'s rust-toolchain.toml names `{channel}`, no version: add the version its release was built "
            "with to TOOLCHAINS")
    return version


def toolchain(tag, src):
    """The toolchain `tag`'s binaries are built with ([`pinned`]), held to
    what runs: cargo is rustup's -- Homebrew's ignores `RUSTUP_TOOLCHAIN`
    and builds with whatever it is -- and the cargo and rustc it runs under
    that toolchain say its version. Refused, naming what to install, where
    rustup lacks it or its wasm32 target, or node is missing. The pinned
    version, the environment that selects it and what `rustc --version`
    said."""
    version = pinned(tag, (src / "rust-toolchain.toml").read_text())
    rustup = str(Path(CARGO).with_name("rustup")) if os.path.isabs(CARGO) else "rustup"
    if not os.path.isabs(CARGO) or not Path(rustup).is_file():
        die(f"cargo is {CARGO}, not rustup's (~/.cargo/bin/cargo): a cargo of its own builds with its own "
            f"compiler whatever the tag pins ({version})")
    r = subprocess.run([rustup, "target", "list", "--installed", "--toolchain", version], capture_output=True,
                       text=True)
    if r.returncode != 0 or "wasm32-unknown-unknown" not in r.stdout.split():
        die(f"{tag} builds with Rust {version} and its wasm32 target: `rustup toolchain install {version} "
            "--target wasm32-unknown-unknown`")
    if shutil.which("node") is None:
        die("node runs the tag's web client: install it")
    env = {k: v for k, v in os.environ.items() if k not in ("CARGO_TARGET_DIR", "RUSTC", "RUSTC_WRAPPER")}
    env["CARGO_NET_OFFLINE"] = "true"
    env["RUSTUP_TOOLCHAIN"] = version
    said = {}
    for tool in ("cargo", "rustc"):
        out = subprocess.run([str(Path(CARGO).with_name(tool)), "--version"], cwd=src, capture_output=True,
                             text=True, env=env).stdout.strip()
        if not out.startswith(f"{tool} {version} "):
            die(f"{tool} under RUSTUP_TOOLCHAIN={version} says `{out}`: not the pinned {version}")
        said[tool] = out
    host = re.search(r"^host: (\S+)$", subprocess.run([str(Path(CARGO).with_name("rustc")), "-vV"], cwd=src,
                                                      capture_output=True, text=True, env=env).stdout, flags=re.M)
    return version, env, said, host.group(1)


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def tag_commit(tag):
    return run("git", "-C", ROOT, "rev-parse", f"{tag}^{{commit}}").strip()


# What `build` keeps of the tag's tree beside the binaries, each the tag's
# bytes, which `write` holds to `git show <tag>:<path>` ([`origin_of`]): the
# web client and the collations its `make wasm` copies beside it, and
# @fennecdb/cloudflare's module with the stand-ins for a Durable Object's
# storage its own tests used.
def tag_files(src):
    out = []
    for p in sorted((src / "web").iterdir()):
        if p.suffix == ".js" and not p.name.endswith(".test.js") or p.name == "package.json":
            out.append(f"web/{p.name}")
    out += [f"web/collate/{p.name}" for p in sorted((src / "web" / "collate").iterdir())]
    return out + ["cloudflare/index.js", "cloudflare/fakes.js"]


def origin_of(name):
    """The path in the tag's tree a file `build` kept is a copy of, or None
    for one it built (the module)."""
    if name == "web/collate/zones.bin":
        return "crates/fennec-core/src/tz/zones.bin"
    if name.startswith("web/collate/"):
        return "crates/fennec-core/src/collate/" + name.rsplit("/", 1)[1]
    if name.startswith("cloudflare/"):
        return "integrations/" + name
    if name.startswith("web/") and name != "web/fennec.wasm":
        return name
    return None


def build(tag, bins):
    """`tag`'s programs into `bins`, and `MANIFEST.json` naming each by its
    SHA-256 beside the tag, its commit and the compiler that built them,
    which `write` holds the directory to before it runs any."""
    bins = Path(bins).resolve()
    bins.mkdir(parents=True, exist_ok=True)
    if any(bins.iterdir()):
        die(f"{bins} holds files already: build into an empty directory")
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
        version, env, said, host = toolchain(tag, src)
        # As the tag's release.yml builds them: `fennec` under the `cli`
        # profile (panic = abort, its network crates built for size),
        # `fennec-server` under `release`, each for the target named. And
        # `--locked`: the tag's Cargo.lock as it was released, never one
        # resolved again today.
        builds = {
            "fennec": [CARGO, "build", "--profile", "cli", "--locked", "--offline", "--target", host, "-p",
                       "fennec-cli"],
            "fennec-server": [CARGO, "build", "--release", "--locked", "--offline", "--target", host, "-p",
                              "fennec-server"],
            "compat_writer": [CARGO, "build", "--release", "--locked", "--offline", "--target", host, "-p",
                              "fennec-core", "--example", "compat_writer"],
            "web/fennec.wasm": ["make", "wasm"],
        }
        for cmd in builds.values():
            r = subprocess.run(cmd, cwd=src, env=env)
            if r.returncode != 0:
                die(f"{' '.join(map(str, cmd))} in {tag}'s tree failed (exit {r.returncode}). Built offline from "
                    "cargo's cache: where it lacks a crate of the tag's Cargo.lock (rustls, ring and webpki-roots, "
                    f"behind the release binaries' `tls` feature), fetch them once, online -- `git worktree add "
                    f"<dir> {tag} && cargo fetch --locked --manifest-path <dir>/Cargo.toml` -- and build again")
        out = src / "target" / host
        made = {
            "fennec": out / "cli" / "fennec",
            "fennec-server": out / "release" / "fennec-server",
            "compat_writer": out / "release" / "examples" / "compat_writer",
            "web/fennec.wasm": src / "web" / "fennec.wasm",
        }
        made.update({k: src / origin_of(k) for k in tag_files(src)})
        for name, path in made.items():
            (bins / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, bins / name)
        manifest = {
            "tag": tag,
            "commit": tag_commit(tag),
            "toolchain": version,
            "rustc": said["rustc"],
            "cargo": said["cargo"],
            "target": host,
            "built": {k: " ".join(map(str, v[1:] if v[0] == CARGO else v)) for k, v in builds.items()},
            "writer": f"tools/compat/writer.rs {sha256(COMPAT / 'writer.rs')}",
            "files": {name: sha256(bins / name) for name in sorted(made)},
        }
        (bins / "MANIFEST.json").write_text(json.dumps(manifest, indent=1) + "\n")
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    print(f"{bins}: {tag}'s fennec-server, fennec, compat_writer, web/ and cloudflare/, built with {said['rustc']}")


def checked(bins, tag):
    """`bins` held to its `MANIFEST.json` before a program of it runs: every
    file it holds named there by its SHA-256 and none other, the tag and
    commit `tag`'s, the compiler the pinned one, and every file taken from
    the tag's tree -- the web client, its collations, the Durable Object
    module -- the tag's own bytes (`git show`). The manifest."""
    path = bins / "MANIFEST.json"
    if not path.is_file():
        die(f"{bins}: no MANIFEST.json (`compat_fixtures.py build` writes it)")
    m = json.loads(path.read_text())
    problems = []
    if m.get("tag") != tag:
        problems.append(f"built from {m.get('tag')}, not {tag}")
    if m.get("commit") != tag_commit(tag):
        problems.append(f"built from commit {m.get('commit')}, not {tag}'s {tag_commit(tag)}")
    version = pinned(tag, run("git", "-C", ROOT, "show", f"{tag}:rust-toolchain.toml"))
    if m.get("toolchain") != version or not str(m.get("rustc", "")).startswith(f"rustc {version} "):
        problems.append(f"built by {m.get('rustc')}, not the pinned rustc {version}")
    files = m.get("files", {})
    held = {p.relative_to(bins).as_posix() for p in bins.rglob("*") if p.is_file()} - {"MANIFEST.json"}
    for name in sorted(held - files.keys()):
        problems.append(f"{name}: not in the manifest")
    for name, digest in sorted(files.items()):
        if not (bins / name).is_file():
            problems.append(f"{name}: missing")
        elif sha256(bins / name) != digest:
            problems.append(f"{name}: not the bytes the manifest names")
    for name in files:
        origin = origin_of(name)
        if origin is None or not (bins / name).is_file():
            continue
        theirs = subprocess.run(["git", "-C", ROOT, "show", f"{tag}:{origin}"], capture_output=True).stdout
        if (bins / name).read_bytes() != theirs:
            problems.append(f"{name}: not {tag}'s {origin}")
    for need in ("fennec-server", "fennec", "compat_writer", "web/fennec.js", "web/fennec.wasm"):
        if need not in files:
            problems.append(f"{need}: not built")
    if problems:
        die(f"{bins} is not what `build {tag}` made:\n  " + "\n  ".join(problems))
    return m


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
