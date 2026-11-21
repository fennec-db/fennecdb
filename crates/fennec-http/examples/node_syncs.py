#!/usr/bin/env python3
"""A `--dir` node's window under `--sync <ms>`, as the power-cut rig reads
it: `make node-sync-bench` (docs/design/durability.md, *A node's syncs*).

Each run starts a `fennec-server --dir` of N tenants under `--sync 250`,
or one `--file` under `--sync 250 --cdc` (`file`), and gives every tenant a
process of its own: WRITERS threads putting a row RATE times a second each
(0: back to back), each noting its answer's time and `Fennec-Seq`, and a
reader long-polling `/_changes`, which hands over only what an fsync
covered, noting when each page's head came and the change it reached
(`Fennec-Next`). A write is handed over by the first page past its change.
Over the steady part of the run -- a second in, to its end -- it reports
the pages' gaps and the writes' handover, median, p99 and longest, and the
writes a second, every tenant's together. `--heavy spill` has the first
tenant put a 17 MB batch a second, a spill under its write lock, and
`--heavy batch` 512 KB batches back to back, its own figures apart from
the others'.

Every build named (`--bin`, this tree's release build, and `--base`, one
built elsewhere -- main's, copied aside) runs each case once a round, the
order turning each round, and a round's runs each start once the 1-minute
load is under QUIET (or after waiting --max-wait, said in the log): a run
beside someone else's work measures that work too. One JSON line a run to
OUT, its progress and the medians of each case and build to OUT's `.log`.
Standard library alone; not run in CI.

    node_syncs.py [--bin B] [--base B] [--tenants 2,8,32,64] [--seconds 20]
                  [--writers 1] [--rate 100] [--heavy none,spill,batch]
                  [--file] [--rounds 2] [--quiet 4] [--out F]
"""
import argparse, http.client, json, multiprocessing as mp, os, re, shutil
import signal, statistics, subprocess, sys, tempfile, threading, time

TOK, ADM, REP = "node-syncs-token", "node-syncs-admin", "node-syncs-repl"


class Cx:
    def __init__(self, host, port, token, timeout=60):
        self.host, self.port, self.token, self.timeout = host, port, token, timeout
        self.cx = None

    def req(self, method, path, body=b"", timed=False):
        if self.cx is None:
            self.cx = http.client.HTTPConnection(self.host, self.port, timeout=self.timeout)
        h = {"Authorization": f"Bearer {self.token}", "Content-Type": "application/json"}
        try:
            self.cx.request(method, path, body=body, headers=h)
            r = self.cx.getresponse()
            at = time.time()
            data = r.read()
            hd = {k.lower(): v for k, v in r.getheaders()}
            return (r.status, hd, data, at) if timed else (r.status, hd, data)
        except BaseException:
            self.cx.close()
            self.cx = None
            raise

    def query(self, prefix, q, params=()):
        return self.req("POST", prefix + "/query", json.dumps({"query": q, "params": list(params)}).encode())


def tenant(host, port, prefix, writers, rate, heavy, start, deadline, out):
    """One tenant's load and reader, in a process of its own: a thread's
    turn in one interpreter would be the reader's lateness, read as the
    node's."""
    acks, pages, lock = [], [], threading.Lock()
    cursor = threading.Event()

    def reader():
        c = Cx(host, port, TOK, timeout=120)
        st, h, b = c.req("GET", prefix + "/_changes?wait=0")
        assert st == 200, (st, b[:200])
        since = int(h["fennec-next"])
        cursor.set()
        while time.time() < deadline + 1.0:
            wait = max(1, min(30_000, int((deadline + 1.0 - time.time()) * 1000)))
            st, h, b, at = c.req("GET", f"{prefix}/_changes?since={since}&limit=100000&wait={wait}", timed=True)
            if st != 200:
                return
            nxt = int(h["fennec-next"])
            if nxt != since:
                with lock:
                    pages.append((at, nxt))
                since = nxt

    def writer(lane):
        c = Cx(host, port, TOK, timeout=120)
        cursor.wait(60)
        while time.time() < start:
            time.sleep(0.005)
        n, due = 0, time.time()
        if heavy:
            docs, size, pause = (512, 1000, 0.0) if heavy == "batch" else (17 * 1024, 1024, 1.0)
            text = "y" * size
        while time.time() < deadline:
            n += 1
            if heavy:
                rows = ",".join(f'{{n: {n * 100_000 + i}, s: "{text}"}}' for i in range(docs))
                st, h, b = c.query(prefix, f"put docs [{rows}]")
            else:
                st, h, b = c.query(prefix, "put docs {n: $1, s: $2}", [lane * 10_000_000 + n, "x" * 200])
            at = time.time()
            if not 200 <= st < 300:
                print(f"refused {st}: {b[:200]!r}", file=sys.stderr)
                return
            with lock:
                acks.append((at, int(h["fennec-seq"])))
            if heavy == "spill":
                time.sleep(pause)
            elif rate:
                due += 1.0 / rate
                wait = due - time.time()
                if wait > 0:
                    time.sleep(wait)

    threads = [threading.Thread(target=reader)]
    threads += [threading.Thread(target=writer, args=(i,)) for i in range(1 if heavy else writers)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    with open(out, "w") as f:
        json.dump({"acks": acks, "pages": pages}, f)


def pct(v, p):
    v = sorted(v)
    return v[min(len(v) - 1, int(round((len(v) - 1) * p)))] if v else float("nan")


def judged(d, start, end):
    """The pages' gaps and each write's handover, ms, over [start, end)."""
    pages = sorted(d["pages"])
    gaps = [(b[0] - a[0]) * 1000 for a, b in zip(pages, pages[1:]) if a[0] >= start and b[0] < end]
    acks = sorted((a for a in d["acks"] if start <= a[0] < end), key=lambda a: a[1])
    hand, j = [], 0
    for at, seq in acks:
        while j < len(pages) and pages[j][1] <= seq:
            j += 1
        if j < len(pages):
            hand.append(max(0.0, (pages[j][0] - at) * 1000))
    return gaps, hand, len(acks)


def run(binary, case, a, log):
    """One server and its load; a dict of what it measured."""
    work = tempfile.mkdtemp(prefix="node-syncs-")
    env = dict(os.environ, FENNEC_HTTP_TOKEN=TOK, FENNEC_ADMIN_TOKEN=ADM, FENNEC_REPLICATION_TOKEN=REP)
    n, heavy = case["tenants"], case["heavy"]
    if n == 0:
        cmd = [binary, "--file", os.path.join(work, "t.fennec"), "--sync", "250", "--cdc"]
    else:
        cmd = [binary, "--dir", os.path.join(work, "n"), "--sync", "250"]
    # Two connections a tenant: past the default 100 a node of 64 refused
    # some of its writers, which then wrote nothing.
    cmd += ["--http", "127.0.0.1:0", "--max-connections", "0"]
    srv = subprocess.Popen(cmd, env=env, stderr=subprocess.PIPE,
                           stdout=subprocess.DEVNULL, text=True)
    procs = []
    try:
        addr = None
        for line in srv.stderr:
            m = re.search(r"listening on: http://([0-9.]+):(\d+)", line)
            if m:
                addr = (m.group(1), int(m.group(2)))
                break
        if not addr:
            raise SystemExit(f"{binary} ended before it listened")
        threading.Thread(target=lambda: [None for _ in srv.stderr], daemon=True).start()
        host, port = addr
        names = [f"t{i}" for i in range(n)] or [""]
        for name in names:
            if name:
                st, _, b = Cx(host, port, ADM).req("PUT", f"/_admin/tenants/{name}")
                assert st in (200, 201), (st, b[:200])
            st, _, b = Cx(host, port, TOK).query(f"/t/{name}" if name else "", "create collection docs (n int, s text)")
            assert 200 <= st < 300, (st, b[:200])
        start = time.time() + 3.0 + 0.5 * len(names)
        deadline = start + a.seconds
        outs = []
        ctx = mp.get_context("spawn")
        for i, name in enumerate(names):
            out = os.path.join(work, f"t{i}.json")
            outs.append(out)
            p = ctx.Process(target=tenant, args=(host, port, f"/t/{name}" if name else "", a.writers, a.rate,
                                                 heavy if i == 0 and heavy != "none" else "", start, deadline, out))
            p.start()
            procs.append(p)
        for p in procs:
            p.join(a.seconds + 120)
        gaps, hand, writes, apart = [], [], 0, None
        for i, out in enumerate(outs):
            with open(out) as f:
                g, h, w = judged(json.load(f), start + 1.0, deadline)
            if i == 0 and heavy != "none":
                apart = {"gap_max": max(g, default=None), "hand_max": max(h, default=None), "writes": w}
                continue
            gaps, hand, writes = gaps + g, hand + h, writes + w
        return {"gap_p50": pct(gaps, .5), "gap_p99": pct(gaps, .99), "gap_max": max(gaps, default=None),
                "hand_p50": pct(hand, .5), "hand_p99": pct(hand, .99), "hand_max": max(hand, default=None),
                "writes_s": writes / (deadline - start - 1.0), "heavy_tenant": apart}
    finally:
        for p in procs:
            if p.is_alive():
                p.kill()
        if srv.poll() is None:
            srv.send_signal(signal.SIGTERM)
            try:
                srv.wait(30)
            except subprocess.TimeoutExpired:
                srv.kill()
        shutil.rmtree(work, ignore_errors=True)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..")
    p.add_argument("--bin", default=os.path.join(root, "target", "release", "fennec-server"))
    p.add_argument("--base", default="", help="another build's fennec-server, run in turns with --bin")
    p.add_argument("--tenants", default="2,8,32,64")
    p.add_argument("--seconds", type=float, default=20)
    p.add_argument("--writers", type=int, default=1)
    p.add_argument("--rate", type=float, default=100, help="writes a second a writer; 0 back to back")
    p.add_argument("--heavy", default="none", help="none, spill or batch, comma-separated")
    p.add_argument("--file", action="store_true", help="also a file's server under --sync 250")
    p.add_argument("--rounds", type=int, default=2)
    p.add_argument("--quiet", type=float, default=4.0)
    p.add_argument("--max-wait", type=float, default=1800)
    p.add_argument("--out", default=os.path.join(root, "target", "node-syncs", "results.jsonl"))
    a = p.parse_args()
    builds = [("this", os.path.abspath(a.bin))] + ([("base", os.path.abspath(a.base))] if a.base else [])
    cases = [{"tenants": int(n), "heavy": h} for h in a.heavy.split(",") for n in a.tenants.split(",")]
    if a.file:
        cases.insert(0, {"tenants": 0, "heavy": "none"})
    os.makedirs(os.path.dirname(os.path.abspath(a.out)), exist_ok=True)
    logf = open(a.out + ".log", "a", buffering=1)

    def log(s):
        print(s, flush=True)
        logf.write(s + "\n")

    done = []
    for r in range(a.rounds):
        for case in cases:
            for name, binary in builds[r % 2:] + builds[:r % 2]:
                t0 = time.time()
                while os.getloadavg()[0] >= a.quiet and time.time() - t0 < a.max_wait:
                    time.sleep(10)
                load = os.getloadavg()[0]
                res = run(binary, case, a, log)
                res.update(build=name, round=r, load_before=load, load_after=os.getloadavg()[0], **case)
                done.append(res)
                with open(a.out, "a") as f:
                    f.write(json.dumps(res) + "\n")
                what = "a file" if case["tenants"] == 0 else f"{case['tenants']} tenants"
                log(f"{name:>4} {what:>11} heavy {case['heavy']:<5} writes/s {res['writes_s']:7.0f}  gaps "
                    f"{res['gap_p50']:4.0f} / {res['gap_p99']:4.0f} / {res['gap_max'] or 0:4.0f}  handed over "
                    f"{res['hand_p50']:4.0f} / {res['hand_p99']:4.0f} / {res['hand_max'] or 0:4.0f} ms  "
                    f"(p50 / p99 / max; load {load:.1f} -> {res['load_after']:.1f})"
                    + (f"  heavy tenant: handed over {res['heavy_tenant']['hand_max'] or 0:.0f} ms at most"
                       if res["heavy_tenant"] else ""))
    log("medians of the runs, by case and build:")
    for case in cases:
        for name, _ in builds:
            rs = [d for d in done if d["build"] == name and d["tenants"] == case["tenants"] and d["heavy"] == case["heavy"]]
            m = lambda k: statistics.median(d[k] for d in rs if d[k] is not None)
            log(f"  {name:>4} tenants {case['tenants']:>2} heavy {case['heavy']:<5} writes/s {m('writes_s'):7.0f}  "
                f"gaps {m('gap_p50'):4.0f} / {m('gap_p99'):4.0f} / {m('gap_max'):4.0f}  handed over "
                f"{m('hand_p50'):4.0f} / {m('hand_p99'):4.0f} / {m('hand_max'):4.0f} ms")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    main()
