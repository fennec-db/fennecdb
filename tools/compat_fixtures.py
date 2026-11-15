#!/usr/bin/env python3
"""A release's fixture set: the files that release's own binaries wrote, and
the answers it gave over them, which every later build opens and answers
alike -- 1.x opens every file written from v0.2.0 on
(docs/design/compatibility.md). The set lives in
crates/fennec-core/tests/compat/v<version>/, read by `compat::` in
fennec-core's, fennec-http's and fennec's tests and by
web/fennec.compat.test.js.

    python3 tools/compat_fixtures.py build v0.2.0 <bin-dir>
        the release's binaries, built from its tag alone: `git archive` of
        the tag into a scratch directory, `cargo build --release` of
        fennec-server and fennec, the tag's own `make wasm`, and
        tools/compat/writer.rs built as an example of the tag's fennec-core;
        the binaries and the tag's web/ copied into <bin-dir> and the scratch
        directory, its target/ with it, deleted
    python3 tools/compat_fixtures.py write <bin-dir> <set-dir>
        the set, written by those binaries (macOS or Linux)
    make compat-fixtures V=0.2.0
        both, into crates/fennec-core/tests/compat/v0.2.0/

What each file holds, and what of it cannot be the same twice (a history's
id, a seal's nonces, a scheduled slot's time), is in the set's README.md,
which `write` writes. The statements below and tools/compat/*.fennecql are
the inputs: a release that writes something new -- a record kind, a type
tag, an index -- adds the statements that write it here before its set is
written.
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

# Before the compact: what the image holds, every type and index among it.
PHASE_A = [
    "create collection kinds (b bool, i int @hash, f float @sorted, t text @text, by bytes, ts timestamp @sorted, "
    "v vector<4>, vi vector<4> @hnsw(cosine, m=8), vh vector<4, f16>, l [int], sp sparse<8> @inverted, j json, "
    "g geo @geo, tr text collate tr @sorted, un text collate und, email text nocase @unique, tag text @hash @text, "
    "word text @text(chars), acc text @text(accents), exp timestamp @ttl(1d), a int, z int, @sorted(a, z), @unique(a, i))",
    'put kinds {b: true, i: 1, f: 1.5, t: "the quick brown fox", by: [0, 159, 255], ts: "2026-01-01T00:00:00Z", '
    'v: [1, 0, 0, 0], vi: [1, 0, 0, 0], vh: [0.5, 0.25, 0, 1], l: [3, 1, 2], sp: "{1:0.5,6:2}/8", '
    'j: {kind: "a", n: 1, deep: {x: [1, "two", null, true]}}, g: [13.4, 52.5], tr: "ılık", un: "Émile", '
    'email: "Alice@Example.com", tag: "red fox", word: "東京タワー", acc: "café crème", exp: "2099-01-01T00:00:00Z", '
    "a: 1, z: 10}",
    'put kinds {b: false, i: 2, f: -2.25, t: "lazy dogs sleep", by: "text as bytes", ts: 1767225600000, '
    'v: [0, 1, 0, 0], vi: [0, 1, 0, 0], vh: [0, 1, 0, 0], l: [], sp: "{2:1}/8", j: [1, 2.5, "x"], '
    'g: [2.35, 48.86], tr: "Istanbul", un: "zoe", email: "bob@example.com", tag: "blue whale", word: "京都", '
    'acc: "cafe", exp: "2099-06-01T00:00:00Z", a: 1, z: 20}',
    "put kinds {i: 3}",
    # Expired before it was written: every read leaves it out (`@ttl`).
    'put kinds {i: 6, t: "expired fox", exp: "2000-01-01T00:00:00Z", a: 6, z: 6}',
    'put kinds {b: true, i: 9007199254740993, f: 0.1, t: "a fox again", vi: [0, 0, 1, 0], '
    'j: {kind: "b", big: 9007199254740993}, a: 2, z: 1}',
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

# After it: the log's records, each kind a write takes.
PHASE_B = [
    "create collection extra (k text @hash, n int)",
    "create index on kinds (j.kind) @hash",
    "alter collection kinds add field note text",
    "alter collection kinds rename field note to remark",
    "alter collection kinds drop field un",
    "drop collection gone",
    'put kinds {b: false, i: 5, f: 2.5, t: "brown bears", ts: "2026-03-01T12:00:00Z", vi: [0, 0, 0, 1], '
    'remark: "after the alter", a: 3, z: 3}',
    'set kinds {f: 9.5, tag: "green fox"} where i = 2',
    "del kinds where i = 3",
    "create or replace statement order_by_ref($r text) as get orders select ref, status, total where ref = $r "
    "and status != 'void'",
]

# A block across collections (kind 9): /batch, its statements one record.
BATCH = [
    'put extra {k: "x", n: 1}',
    'put orders {ref: "o-3", status: "open", total: 50}',
]

SCHEDULED = "create statement tick every 1m as insert ticks {k: 'tick'} if absent"
HOOKED = (
    "create statement gh_issue($n int from event.issue.number, $a text from event.action) "
    "on hook github as insert issues {n: $n, action: $a}"
)
HOOK_EVENT = b'{"action":"opened","issue":{"number":7,"title":"a fixture"}}'

# The shell's file: what an app's own process writes, each `-c` an open and
# a close.
SHELL = [
    "create collection notes (n int @hash, body text @text, at timestamp @sorted)",
    'put notes [{n: 1, body: "first note", at: "2026-02-01T00:00:00Z"}, {n: 2, body: "second", at: 1769990400000}]',
    'set notes {body: "second, edited"} where n = 2',
    'put notes {n: 3, body: "third"}',
    "del notes where n = 1",
]

GRAPH = [
    "create collection vecs (n int @hash, c vector<8> @hnsw(cosine, m=8), l vector<8> @hnsw(l2, m=8), "
    "d vector<8> @hnsw(dot, m=8), q vector<8> @hnsw(cosine, m=8, quant=int8), "
    "bits vector<8> @hnsw(cosine, m=8, quant=bit), h vector<8, f16> @hnsw(cosine, m=8))",
]


def die(msg):
    sys.exit(f"compat_fixtures: {msg}")


def vec(k, salt):
    return "[" + ", ".join(f"{(((k * 7 + i * 13 + salt) % 17) / 17 - 0.5):.4f}" for i in range(8)) + "]"


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
        c = http.client.HTTPConnection(host, int(port), timeout=60)
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


def write_server(bins, work, out):
    """server.fennec, its sealed archive and its sealed backup."""
    secret = work / "github.secret"
    secret.write_text(HOOK_SECRET)
    hooks = work / "hooks"
    hooks.write_text(f"github github {secret}\n")
    key = out / "key"
    run(bins / "fennec", "key", key)
    s = Server(
        str(bins / "fennec-server"),
        ["--file", str(out / "server.fennec"), "--http-token", TOKEN, "--replication-token", REPLICATION,
         "--no-checkpoint", "--hooks", str(hooks), "--auth-delay", "0"],
        work / "server.log",
    )
    archiver = None
    try:
        for q in PHASE_A:
            s.query(q)
        s.query("compact")
        primary = f"http://{s.addr}"
        archive = out / "archive"
        archiver = subprocess.Popen(
            [bins / "fennec", "archive", primary, archive, "--token", REPLICATION, "--key-file", key],
            stdout=open(work / "archive.log", "w"), stderr=subprocess.STDOUT,
        )
        verify = lambda: subprocess.run(
            [bins / "fennec", "verify", archive, "--key-file", key], capture_output=True, text=True
        )
        wait_for("the archive's base image", lambda: verify().returncode == 0, 60)
        for q in PHASE_B:
            s.query(q)
        s.batch(BATCH)
        s.query(SCHEDULED)
        # The scheduler runs a slot once, at its time: the slot's mark (op 5)
        # and its write land then, and the statement is dropped after (op 2)
        # so no later slot writes more.
        wait_for("the scheduled statement's first slot", lambda: '"tick"' in s.query("get ticks"), 150)
        s.query("drop statement tick")
        s.query(HOOKED)
        sig = "sha256=" + hmac.new(HOOK_SECRET.encode(), HOOK_EVENT, hashlib.sha256).hexdigest()
        status, body = s.ask(
            "POST", "/_hooks/github", HOOK_EVENT,
            {"Content-Type": "application/json", "X-Hub-Signature-256": sig, "X-GitHub-Event": "issues"},
            token=None,
        )
        if status // 100 != 2:
            die(f"the hook's event: {status} {body}")
        s.query("get kinds count")
        last = s.seq

        def caught_up():
            r = verify()
            m = re.search(r"restores changes \d+ to (\d+)", r.stdout)
            return r.returncode == 0 and m and int(m.group(1)) >= last

        wait_for(f"the archive at change {last}", caught_up, 60)
        archiver.send_signal(signal.SIGINT)
        archiver.wait(60)
        archiver = None
        run(bins / "fennec", "backup", primary, out / "backup.sealed", "--token", REPLICATION, "--key-file", key)
    finally:
        if archiver and archiver.poll() is None:
            archiver.kill()
            archiver.wait()
        s.stop()


def write_graph(bins, work, out):
    """graph.fennec: the graphs a server's shutdown writes in its checkpoint."""
    s = Server(str(bins / "fennec-server"), ["--file", str(out / "graph.fennec"), "--http-token", TOKEN], work / "graph.log")
    try:
        for q in GRAPH:
            s.query(q)
        rows = ", ".join(
            f"{{n: {k}, c: {vec(k, 0)}, l: {vec(k, 1)}, d: {vec(k, 2)}, q: {vec(k, 3)}, bits: {vec(k, 4)}, h: {vec(k, 5)}}}"
            for k in range(40)
        )
        s.query(f"put vecs [{rows}]")
        s.query("del vecs where n = 7")
    finally:
        s.stop()
    if "checkpoint written" not in (work / "graph.log").read_text():
        die("graph.fennec: the server wrote no checkpoint")


def write_shell(bins, out):
    """shell.fennec: the shell's own file, an open and a close a statement."""
    for q in SHELL:
        run(bins / "fennec", out / "shell.fennec", "-c", q)


def readme(out, bins, versions):
    names = sorted(p.name for p in out.iterdir())
    text = f"""# The fixture set of {versions['server']}

Written by `tools/compat_fixtures.py write` with the release's own binaries,
built from its tag by `tools/compat_fixtures.py build` ({versions['rustc']},
node {versions['node']}): `{versions['server']}`, `{versions['cli']}`, the tag's
`web/fennec.js` and the module its `make wasm` built, and
`tools/compat/writer.rs` built as an example of the tag's fennec-core for
what the binaries cannot write at a few kilobytes. Every later build opens
each file and answers each statement alike: `compat::` in fennec-core's,
fennec-http's and fennec's tests, and `web/fennec.compat.test.js`.

| File | Written by | Holds |
| --- | --- | --- |
| `server.fennec` | fennec-server (`--replication-token`, `--no-checkpoint`, `--hooks`) | an image (`compact`) and the log after it: record kinds 1 to 9, 12 and 13 -- the history, a block, fields added, renamed and dropped, a collection dropped -- every type tag and every index kind but a quantized graph's, and a statement record of every op: a named statement replaced, a write-bound one, a scheduled one (its slot's mark, then its drop) and a webhook consumer with the event it took |
| `shell.fennec` | the `fennec` shell, a `-c` an open and a close | a file an app's own process keeps |
| `graph.fennec` | fennec-server's shutdown | the checkpoint: an image whose graphs (version 9) are every metric and quantization |
| `kept.fennec` | writer.rs `kept` (`save_graphs`) | a graph kept in the file's tail (version 10), writes after it, and the close's mark that the file was synced (kind 14) |
| `spill.fennec` | writer.rs `spill` (`set_spill`) | a block that spilled (kind 10) and landed (kind 11), and kind 14 |
| `synclog.fennec`, `synclog.fennec.sync` | writer.rs `synclog` (`keep_sync_log`) | a file cut back to its last fsync and the sync log holding the rest: the machine went down. The log is its fixed 260 KB, zeros past its entries, as written |
| `backup.sealed` | `fennec backup --key-file key` | a sealed backup of `server.fennec`'s server |
| `archive/` | `fennec archive --key-file key` | a sealed archive: a base image and the segments after it |
| `key` | `fennec key` | the seal's key, the fixture's own |
| `page-idb.json`, `page-idb-sealed.json` | web client `persist` (`{{cryptoKey}}`: `PAGE_KEY` in `tools/compat/web.mjs`) | IndexedDB's records, an image and its chunks, bytes in base64 |
| `page-opfs.fennec` | web client `openFile` | the OPFS file |

Beside each, `<name>.fennecql` holds the statements and `<name>.answers`
the release's answers, a line each as `json::response_to_string` writes
them (the file opened read-only by writer.rs `answers`; a sealed file's or
an archive's restored first by the release's `fennec restore`), and
`<name>.web` the page's (`JSON.stringify` of `run`), read back by a page
opened again.

Not the same twice, so written once: the history's id (drawn at random
when `--replication-token` forks it, and at each restore), a seal's and
AES-GCM's nonces and a sealed page's generation, the scheduled slot's time
and its mark, the hook's arrival, the archive's file names and times.

Files: {', '.join(names)}.
"""
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
        # The lock files the binaries leave beside what they wrote are no
        # part of a database.
        for lock in stage.glob("*.lock"):
            lock.unlink()
        for what in ("kept", "spill", "synclog"):
            run(bins / "compat_writer", what, stage / f"{what}.fennec")
        run("node", COMPAT / "web.mjs", bins / "web", stage, COMPAT / "page.fennecql")
        # The answers, each file read as it lies -- the sync log beside the
        # file it names, its inode -- a sealed one restored first.
        lists = {"server": "server", "shell": "shell", "graph": "graph", "kept": "kept", "spill": "spill", "synclog": "synclog",
                 "backup": "server", "archive": "server", "page-opfs": "page", "page-idb": "page",
                 "page-idb-sealed": "page"}
        for name, src in lists.items():
            shutil.copyfile(COMPAT / f"{src}.fennecql", stage / f"{name}.fennecql")
        for name in ("server", "shell", "graph", "kept", "spill", "synclog", "page-opfs"):
            run(bins / "compat_writer", "answers", stage / f"{name}.fennec", stage / f"{name}.fennecql",
                stage / f"{name}.answers")
        for name, src in (("backup", stage / "backup.sealed"), ("archive", stage / "archive")):
            restored = work / f"{name}.fennec"
            run(bins / "fennec", "restore", src, restored, "--key-file", stage / "key")
            run(bins / "compat_writer", "answers", restored, stage / f"{name}.fennecql", stage / f"{name}.answers")
        versions = {
            "server": run(bins / "fennec-server", "--version").strip(),
            "cli": run(bins / "fennec", "--version").strip(),
            "rustc": os.environ.get("FENNEC_COMPAT_RUSTC") or (bins / "rustc-version").read_text().strip(),
            "node": run("node", "--version").strip(),
        }
        readme(stage, bins, versions)
        out.mkdir(parents=True, exist_ok=True)
        for p in stage.iterdir():
            shutil.move(str(p), out / p.name)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    print(f"{out}: {', '.join(sorted(p.name for p in out.iterdir()))}")


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
        env = {k: v for k, v in os.environ.items() if k != "CARGO_TARGET_DIR"}
        # The rustc beside the cargo that builds, as the tag's
        # rust-toolchain.toml picks it: Homebrew's has no wasm32.
        rustc = str(Path(CARGO).with_name("rustc")) if os.path.isabs(CARGO) else "rustc"
        rustc = subprocess.run([rustc, "--version"], cwd=src, capture_output=True, text=True, env=env).stdout.strip()
        for cmd in (
            [CARGO, "build", "--release", "-p", "fennec-server", "-p", "fennec-cli"],
            [CARGO, "build", "--release", "-p", "fennec-core", "--example", "compat_writer"],
            ["make", "wasm"],
        ):
            subprocess.run(cmd, cwd=src, env=env, check=True)
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
