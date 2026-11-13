#!/usr/bin/env python3
"""The drill's seeded faults: a region of fennecdb in Docker, broken at
random by a schedule each seed draws, its history held to an oracle.

    make chaos                         # builds the chaos image, then runs this
    make chaos SEEDS=5 SEED0=40 CHAOS_MINUTES=10
    python3 deploy/drill/chaos.py      # the image built already (CHAOS_IMAGE)

What runs, each process a container held to --cpus and --memory on one
Docker network, every token in the environment as the drill's are:

    g1                the global router
    r1, r2            the region's router (--replicas, --auto-failover, the
                      lease) and its standby
    n1, n2, n3        nodes in zones us-1, us-2, us-3 (--dir --lease
                      --sync always), fennec-server built with `test-clock`
    driver            this file again (--seed): the clients, the schedule
                      and the checks, on the same network, Docker's socket
                      handed in

Each tenant gets a replica in another zone. Its clients -- threads of the
driver, a connection each -- write registers (`put regs {id: k, v: <a value
no other write writes>, at: now()}`), read them back and append rows of
values of their own (`put log`), each operation recorded with when it was
sent and answered and how: answered, refused (it did not run), or neither
(it may have run). Meanwhile the seed's schedule applies one fault at a
time, holds it, heals it and settles:

    kill        `docker kill` of a node, started again after a while
    pause       `docker pause` of a node for longer than a lease, or not
    router      tc netem between a router and a node: delay, loss, or a
                partition (all of it lost)
    replica     tc netem between two nodes, the one a primary of tenants
                the other holds the replicas of: delay, loss, or a partition
    catch-up    a node killed, started again with its peers' links to it
                slowed, and killed and started again while its copies catch
                up with the primaries that took its tenants over
    move        a tenant moved to the third node while its source is
                killed, paused or cut off from the router, its image coming
                over a slowed link: a failover during the move
    clock       a node's clock jumped by hours or days, ahead or back
                (`FENNEC_TEST_CLOCK`, crates/fennec-http/src/test_clock.rs:
                Docker moves no container's wall clock)

To settle, every node runs again, every tenant is placed with a replica,
the clients pause, and every replica's rows must equal its primary's (a
digest of each). At the end the clients stop, every register is read once
more, and the history is held to deploy/drill/oracle.py: the YCSB register
checker, every append answered found and none refused found, and no tenant
written by two primaries at once. Exit status 0 when every seed passed.

Knobs (environment): SEEDS (3), SEED0 (1), CHAOS_MINUTES of faults a seed
(3), CHAOS_LEASE seconds (2), CHAOS_TENANTS (4), CHAOS_CLIENTS a tenant
(2), CHAOS_RATE operations a second a client (40), CHAOS_IMAGE
(fennec-kit-chaos), CHAOS_OUT a directory for each seed's result and, when
it failed, its history and logs (a temporary one otherwise), CHAOS_KEEP=1
to leave a failed seed's containers for a look.

The standard library alone.
"""

import http.client
import json
import os
import random
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import oracle  # noqa: E402

IMAGE = os.environ.get("CHAOS_IMAGE", "fennec-kit-chaos")
SEEDS = int(os.environ.get("SEEDS", "3"))
SEED0 = int(os.environ.get("SEED0", "1"))
MINUTES = float(os.environ.get("CHAOS_MINUTES", "3"))
LEASE = int(os.environ.get("CHAOS_LEASE", "2"))
TENANTS = int(os.environ.get("CHAOS_TENANTS", "4"))
CLIENTS = int(os.environ.get("CHAOS_CLIENTS", "2"))
RATE = float(os.environ.get("CHAOS_RATE", "40"))
KEEP = os.environ.get("CHAOS_KEEP") == "1"

# What each container may take: the driver's clients are busy loops, the
# nodes fsync every write; all of it is bounded, beside whatever else the
# machine runs.
LIMITS = {
    "node": ["--cpus", "1", "--memory", "384m"],
    "router": ["--cpus", "0.5", "--memory", "192m"],
    "driver": ["--cpus", "1", "--memory", "512m"],
}

NODES = [("n1", "us-1"), ("n2", "us-2"), ("n3", "us-3")]
ROUTERS = ["r1", "r2"]
REGISTERS = 4
# Rows a tenant starts with, so that a move's image and a copy's catch-up
# take a while over a slowed link: 200 rows of 1 KB.
PAD_ROWS = 200
CLOCK_FILE = "/tmp/fennec-clock"
UPSTREAM = 2 * LEASE

T0 = time.monotonic()


def say(msg):
    print(f"[{time.monotonic() - T0:7.2f}s] {msg}", flush=True)


class Failed(Exception):
    pass


def check(ok, msg):
    if not ok:
        raise Failed(msg)


def docker(*args, check_=True, timeout=120, env=None):
    p = subprocess.run(["docker", *args], capture_output=True, text=True, timeout=timeout,
                       env=None if env is None else {**os.environ, **env})
    if check_ and p.returncode != 0:
        raise Failed(f"docker {' '.join(args[:4])} ...: {p.stderr.strip() or p.stdout.strip()}")
    return p.stdout.strip()


# =================================================================== outer


def outer():
    """Each seed in a driver container of its own; whatever a seed made is
    removed however this ends, an interrupt included."""
    out = os.environ.get("CHAOS_OUT") or tempfile.mkdtemp(prefix="fennec-chaos-")
    out = os.path.abspath(out)
    os.makedirs(out, exist_ok=True)
    check(docker("image", "inspect", IMAGE, check_=False) != "", f"no image {IMAGE}: make chaos builds it")
    say(f"{SEEDS} seed(s) from {SEED0}, {MINUTES:g} min of faults each, lease {LEASE} s; results in {out}")
    verdicts = {}
    current = [None]

    def stop(*_):
        raise KeyboardInterrupt()

    for s in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(s, stop)
    try:
        for seed in range(SEED0, SEED0 + SEEDS):
            run = f"fchaos-{seed}-{secrets.token_hex(3)}"
            current[0] = run
            verdicts[seed] = one_seed(seed, run, out)
            sweep(run)
            current[0] = None
    except KeyboardInterrupt:
        say("interrupted")
    finally:
        if current[0]:
            sweep(current[0])
    print()
    print("== chaos: " + ", ".join(f"seed {s} {'passed' if v else 'FAILED'}" for s, v in verdicts.items()) + " ==")
    ok = len(verdicts) == SEEDS and all(verdicts.values())
    if ok and not os.environ.get("CHAOS_OUT"):
        # The temporary directory made above, and only it; a file the
        # driver wrote as another user stays, said.
        shutil.rmtree(out, ignore_errors=True)
        if os.path.exists(out):
            say(f"left {out}: files the driver wrote as root")
    return 0 if ok else 1


def one_seed(seed, run, out):
    seed_out = os.path.join(out, f"seed-{seed}")
    os.makedirs(seed_out, exist_ok=True)
    driver = f"{run}-driver"
    knobs = {"CHAOS_SEED": str(seed), "CHAOS_RUN": run, "CHAOS_SELF": driver, "CHAOS_IMAGE": IMAGE,
             "CHAOS_MINUTES": str(MINUTES), "CHAOS_LEASE": str(LEASE), "CHAOS_TENANTS": str(TENANTS),
             "CHAOS_CLIENTS": str(CLIENTS), "CHAOS_RATE": str(RATE), "CHAOS_KEEP": "1" if KEEP else "0"}
    args = ["run", "--rm", "--name", driver, "--label", f"fennec-chaos={run}", *LIMITS["driver"],
            "-v", "/var/run/docker.sock:/var/run/docker.sock", "-v", f"{HERE}:/drill:ro",
            "-v", f"{seed_out}:/out"]
    for k in knobs:
        args += ["-e", k]
    say(f"seed {seed}: driver {driver}")
    p = subprocess.Popen(["docker", *args, IMAGE, "python3", "/drill/chaos.py", "--inside"],
                         env={**os.environ, **knobs})
    # The run's own bound, past which it is stopped as failed: its minutes
    # of faults, and the world's making, settling and checks besides.
    try:
        code = p.wait(timeout=MINUTES * 60 + 900)
    except subprocess.TimeoutExpired:
        say(f"seed {seed}: the driver ran past its bound, stopped")
        docker("rm", "-f", driver, check_=False)
        p.wait(timeout=60)
        code = 1
    except KeyboardInterrupt:
        docker("rm", "-f", driver, check_=False)
        raise
    passed = code == 0
    say(f"seed {seed}: {'passed' if passed else 'FAILED'}; its result in {seed_out}")
    return passed


def sweep(run):
    """Every container, volume and network a seed's run made, by its label."""
    label = f"label=fennec-chaos={run}"
    ids = docker("ps", "-aq", "--filter", label, check_=False).split()
    if ids:
        docker("rm", "-f", *ids, check_=False)
    vols = docker("volume", "ls", "-q", "--filter", label, check_=False).split()
    if vols:
        docker("volume", "rm", "-f", *vols, check_=False)
    nets = docker("network", "ls", "-q", "--filter", label, check_=False).split()
    for n in nets:
        docker("network", "rm", n, check_=False)


# =================================================================== inner

RUN = os.environ.get("CHAOS_RUN", "fchaos-x")
SEED = int(os.environ.get("CHAOS_SEED", "1"))
LABEL = f"fennec-chaos={RUN}"
NET = RUN
OUT = "/out"

ROOT = secrets.token_hex(16)    # the routers' --token
DIRS = secrets.token_hex(16)    # the routers' directory replication
ENTRY = secrets.token_hex(16)   # the routers' entry token
DATA = secrets.token_hex(16)    # the nodes' --http-token
REPL = secrets.token_hex(16)    # the nodes' --replication-token
ADMIN = {n: secrets.token_hex(16) for n, _ in NODES}
ROUTER_ENV = {"FENNEC_SHARD_TOKEN": ROOT, "FENNEC_SHARD_REPLICATION_TOKEN": DIRS,
              "FENNEC_SHARD_ENTRY_TOKEN": ENTRY}


def cname(name):
    return f"{RUN}-{name}"


def start(name, cmd, env, kind):
    v = f"{RUN}-{name}"
    docker("volume", "create", "--label", LABEL, v)
    args = ["run", "-d", "--name", cname(name), "--label", LABEL, "--network", NET,
            "--network-alias", name, "--hostname", name, "-v", f"{v}:/data", *LIMITS[kind],
            # tc netem from inside, the faults' links.
            "--cap-add", "NET_ADMIN"]
    for k in env:
        args += ["-e", k]
    docker(*args, IMAGE, *cmd, env=env)


def listening(name, since=None, within=60):
    """Until `name` says it listens -- since `since` (time.time()), for a
    container started again, whose log holds its last run's line too."""
    end = time.monotonic() + within
    # Whole seconds: Docker reads what follows a point as nanoseconds.
    logs = ["docker", "logs"] + (["--since", str(int(since))] if since else []) + [cname(name)]
    while time.monotonic() < end:
        p = subprocess.run(logs, capture_output=True, text=True)
        if "listening on: http://" in p.stderr + p.stdout:
            return
        if docker("inspect", "-f", "{{.State.Running}}", cname(name), check_=False) == "false":
            raise Failed(f"{name} ended before it listened:\n{p.stderr[-2000:]}")
        time.sleep(0.1)
    raise Failed(f"{name} did not listen within {within} s")


def state(name):
    """(running, paused) of a container."""
    s = docker("inspect", "-f", "{{.State.Running}} {{.State.Paused}}", cname(name), check_=False)
    running, _, paused = s.partition(" ")
    return running == "true", paused == "true"


def restart(name):
    """`docker start` of a container stopped, until it listens again."""
    since = time.time() - 0.5
    docker("start", cname(name))
    listening(name, since)


def ip(name):
    return docker("inspect", "-f", "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}", cname(name))


def call(host, method, path, body=None, token=None, timeout=10):
    c = http.client.HTTPConnection(host, 8090 if host in ROUTERS or host == "g1" else 8081, timeout=timeout)
    try:
        h = {}
        if token:
            h["Authorization"] = f"Bearer {token}"
        data = None
        if body is not None:
            data = body if isinstance(body, (bytes, str)) else json.dumps(body)
            h["Content-Type"] = "application/json"
        c.request(method, path, body=data, headers=h)
        r = c.getresponse()
        return r.status, r.read().decode("utf-8", "replace")
    finally:
        c.close()


def shard(router, method, path, body=None, timeout=10):
    return call(router, method, path, body, token=ROOT, timeout=timeout)


def query(host, tenant, sql, params=None, timeout=10):
    body = {"query": sql} if params is None else {"query": sql, "params": params}
    return call(host, "POST", f"/t/{tenant}/query", body, token=DATA, timeout=timeout)


def rows_of(text):
    j = json.loads(text)
    return j["rows"] if isinstance(j, dict) else j


def until(ok, what, within=60.0, every=0.2):
    end = time.monotonic() + within
    last = None
    while time.monotonic() < end:
        try:
            got = ok()
        except (OSError, http.client.HTTPException, Failed, ValueError, KeyError) as e:
            got, last = None, e
        if got:
            return got
        time.sleep(every)
    raise Failed(f"waited {within:g} s for {what}" + (f" (last: {last})" if last else ""))


# ------------------------------------------------------------------ world


def node_cmd():
    # As deploy/systemd/fennec-node.service runs it, with fewer connections.
    return ["fennec-server", "--dir", "/data", "--http", "0.0.0.0:8081", "--sync", "always",
            "--lease", "--max-connections", "200"]


def node_env(n):
    return {"FENNEC_HTTP_TOKEN": DATA, "FENNEC_ADMIN_TOKEN": ADMIN[n], "FENNEC_REPLICATION_TOKEN": REPL,
            "FENNEC_TEST_CLOCK": CLOCK_FILE}


def router_cmd(standby_of=None):
    cmd = ["fennec-shard", "--listen", "0.0.0.0:8090", "--directory", "/data/directory.fennec",
           "--region", "us", "--global", "http://g1:8090", "--replicas",
           "--auto-failover", str(LEASE), "--upstream-timeout", str(UPSTREAM)]
    return cmd + (["--replica-of", f"http://{standby_of}:8090"] if standby_of else [])


def build_world():
    docker("network", "create", "--label", LABEL, NET)
    me = os.environ.get("CHAOS_SELF")
    if me:
        docker("network", "connect", NET, me)
    start("g1", ["fennec-shard", "--listen", "0.0.0.0:8090", "--directory", "/data/directory.fennec",
                 "--upstream-timeout", str(UPSTREAM)], ROUTER_ENV, "router")
    listening("g1")
    start("r1", router_cmd(), ROUTER_ENV, "router")
    listening("r1")
    start("r2", router_cmd("r1"), ROUTER_ENV, "router")
    for n, _ in NODES:
        start(n, node_cmd(), node_env(n), "node")
    for name in ["r2"] + [n for n, _ in NODES]:
        listening(name)
    version = docker("exec", cname("n1"), "fennec-server", "--version")
    check(version.endswith("+test-clock"), f"the nodes' fennec-server is no test-clock build: {version}")
    s, b = shard("g1", "PUT", "/_shard/regions/us", {"routers": [f"http://{r}:8090" for r in ROUTERS]})
    check(s == 200, f"region us: {s} {b}")
    for r in ROUTERS:
        until(lambda: '"name":"us"' in shard(r, "GET", "/_shard/regions")[1], f"{r}'s copy of the global directory")
    for n, zone in NODES:
        s, b = shard("g1", "PUT", f"/_shard/nodes/{n}",
                     {"addr": f"{n}:8081", "token": ADMIN[n], "region": "us", "zone": zone})
        check(s in (200, 201), f"node {n}: {s} {b}")
    tenants = []
    for i in range(1, TENANTS + 1):
        t = f"t{i}"
        s, b = shard("g1", "PUT", f"/_shard/tenants/{t}",
                     {"region": "us", "schema": "create collection regs (v int, at timestamp)"})
        check(s == 201, f"tenant {t}: {s} {b}")
        check(json.loads(b).get("replica_on"), f"tenant {t} was given no replica: {b}")
        tenants.append(t)
    for t in tenants:
        until(lambda: query("r1", t, "get regs count")[0] == 200, f"{t} through r1")
        for sql in ["create collection log (k int)", "create collection pad (b text)"]:
            s, b = query("r1", t, sql)
            check(s == 200, f"{t}: {sql}: {s} {b}")
        for lo in range(1, PAD_ROWS + 1, 50):
            docs = [{"id": i, "b": f"{i:04d}" * 256} for i in range(lo, min(lo + 50, PAD_ROWS + 1))]
            s, b = query("r1", t, "put pad $1", [docs], timeout=30)
            check(s == 200, f"{t}: padding: {s} {b[:200]}")
    netem_works()
    say(f"run {RUN}: g1, r1 and its standby r2, nodes {', '.join(n for n, _ in NODES)}; "
        f"{len(tenants)} tenants, each with a replica; lease {LEASE} s; {version}")
    return tenants


def netem_works():
    """tc netem from inside a node, as the faults use it: a kernel without
    sch_netem would make every link fault nothing, and the run a pass."""
    peer = ip("n2")
    netem("n1", [peer], "delay 1ms")
    shown = docker("exec", cname("n1"), "tc", "qdisc", "show", "dev", "eth0")
    clear("n1")
    check("netem" in shown, f"tc netem did not take on n1: {shown}")


def placement(t, router="r1"):
    s, b = shard(router, "GET", f"/_shard/tenants/{t}")
    check(s == 200, f"{t} on {router}: {s} {b}")
    return json.loads(b)


# ----------------------------------------------------------------- clients

COUNTER = [0]
COUNTER_LOCK = threading.Lock()


def unique():
    with COUNTER_LOCK:
        COUNTER[0] += 1
        return COUNTER[0]


class Client(threading.Thread):
    """A tenant's client: one connection, through r1 or, where r1 cannot be
    reached, r2; each operation recorded with its span and outcome."""

    def __init__(self, idx, tenant):
        super().__init__(daemon=True)
        self.idx = idx
        self.tenant = tenant
        self.rng = random.Random(f"{SEED}/client/{idx}")
        self.ops = []
        self.at = 0
        self.conn = None
        self.stop = threading.Event()
        self.pause = threading.Event()
        self.idle = threading.Event()
        self.backoff = 0.05
        # Answers that do not read as the server says it answers.
        self.garbled = []

    def connect(self):
        self.conn = http.client.HTTPConnection(ROUTERS[self.at], 8090, timeout=10)
        self.conn.connect()

    def ask(self, sql, params):
        """(outcome, status, headers, body): `fail` for what cannot have run
        -- a 4xx, a 503, no connection made -- `unknown` for what may have."""
        body = json.dumps({"query": sql, "params": params})
        if self.conn is None:
            try:
                self.connect()
            except OSError as e:
                self.conn = None
                self.at = (self.at + 1) % len(ROUTERS)
                return "fail", None, {}, f"no connection: {type(e).__name__}"
        try:
            self.conn.request("POST", f"/t/{self.tenant}/query", body=body, headers={
                "Authorization": f"Bearer {DATA}", "Content-Type": "application/json"})
        except (OSError, http.client.HTTPException) as e:
            # Some of it may have gone: what the server read of it may run.
            self.drop()
            return "unknown", None, {}, f"sending: {type(e).__name__}"
        try:
            r = self.conn.getresponse()
            text = r.read().decode("utf-8", "replace")
            headers = {k.lower(): v for k, v in r.getheaders()}
            if headers.get("connection", "").lower() == "close":
                self.drop()
        except (OSError, http.client.HTTPException) as e:
            self.drop()
            return "unknown", None, {}, f"no answer: {type(e).__name__}"
        if 200 <= r.status < 300:
            return "ok", r.status, headers, text
        if r.status == 503 or 400 <= r.status < 500:
            return "fail", r.status, headers, text
        return "unknown", r.status, headers, text

    def drop(self):
        if self.conn is not None:
            self.conn.close()
        self.conn = None

    def run(self):
        while not self.stop.is_set():
            if self.pause.is_set():
                self.idle.set()
                while self.pause.is_set() and not self.stop.is_set():
                    time.sleep(0.01)
                self.idle.clear()
                continue
            try:
                self.once()
            except Exception as e:  # noqa: BLE001 -- a client that died would hang every pause
                self.garbled.append(f"client {self.idx}: {type(e).__name__}: {e}")
                self.drop()
                time.sleep(0.5)

    def once(self):
        """One operation, recorded, and the wait before the next."""
        roll = self.rng.random()
        key = self.rng.randint(1, REGISTERS)
        s = time.monotonic()
        if roll < 0.45:
            v = unique()
            outcome, status, h, text = self.ask("put regs {id: $1, v: $2, at: now()}", [key, v])
            op = oracle.Op("w", self.tenant, key, v, s, time.monotonic(), outcome, why=status)
        elif roll < 0.8:
            outcome, status, h, text = self.ask("get regs where id = $1", [key])
            got = None
            if outcome == "ok":
                try:
                    rows = rows_of(text)
                    got = rows[0]["v"] if rows else None
                except (ValueError, KeyError, IndexError, TypeError):
                    # A 2xx that reads as no rows is the server's to
                    # explain: left out of the reads, and failed at the end.
                    self.garbled.append(f"a read of {self.tenant}/{key} answered {status}: {text[:200]}")
                    outcome = "garbled"
            op = oracle.Op("r", self.tenant, key, got, s, time.monotonic(), outcome, why=status)
        else:
            v = unique()
            outcome, status, h, text = self.ask("put log {id: $1, k: $2}", [v, self.idx])
            op = oracle.Op("a", self.tenant, v, v, s, time.monotonic(), outcome, why=status)
        if outcome == "ok" and op.kind != "r":
            try:
                op.seq = int(h["fennec-seq"]) if "fennec-seq" in h else None
                hist = h.get("fennec-history")
                if hist:
                    term, _, hid = hist.partition("-")
                    op.history = (int(term), int(hid, 16))
            except ValueError:
                # Answered, so it ran: kept as written, and the answer
                # failed at the end.
                self.garbled.append(f"{op!r}: Fennec-Seq {h.get('fennec-seq')!r}, "
                                    f"Fennec-History {h.get('fennec-history')!r}")
        if outcome != "ok" and status is None:
            op.why = text
        self.ops.append(op)
        if outcome == "ok":
            self.backoff = 0.05
            gap = self.rng.uniform(0, 2 / RATE)
        else:
            # Bounded: a client refused or cut off dials again no more than
            # twenty times a second, and less as it goes on.
            gap = self.backoff
            self.backoff = min(self.backoff * 2, 1.0)
        time.sleep(gap)


def pause_all(clients):
    for c in clients:
        c.pause.set()
    until(lambda: all(c.idle.is_set() for c in clients), "every client to pause", 60, 0.05)


def resume_all(clients):
    for c in clients:
        c.pause.clear()


# ------------------------------------------------------------------ faults


def netem(src, dst_ips, spec):
    """`spec` (netem's words) on what `src` sends to each of `dst_ips`, and
    nothing else: a fourth band of a prio qdisc no priority maps to, which
    only the filters fill."""
    script = ("tc qdisc del dev eth0 root 2>/dev/null; "
              "tc qdisc add dev eth0 root handle 1: prio bands 4 && "
              f"tc qdisc add dev eth0 parent 1:4 handle 40: netem {spec}")
    for a in dst_ips:
        script += f" && tc filter add dev eth0 parent 1:0 protocol ip prio 1 u32 match ip dst {a}/32 flowid 1:4"
    docker("exec", cname(src), "sh", "-c", script)


def clear(name):
    docker("exec", cname(name), "sh", "-c", "tc qdisc del dev eth0 root 2>/dev/null; true", check_=False)


def link_spec(rng):
    kind = rng.choice(["delay", "loss", "partition", "delay+loss"])
    d = rng.randint(50, 400)
    p = rng.randint(5, 30)
    return kind, {
        "delay": f"delay {d}ms {d // 4}ms distribution normal",
        "loss": f"loss {p}%",
        "partition": "loss 100%",
        "delay+loss": f"delay {d}ms {d // 4}ms distribution normal loss {p}%",
    }[kind]


class Schedule:
    """The faults, drawn from the seed: each a function applying one, holding
    it, and healing it, the run's record of it returned."""

    WEIGHTS = {"kill": 2, "pause": 2, "router": 3, "replica": 3, "catch-up": 1, "move": 1, "clock": 1}

    def __init__(self, tenants):
        self.rng = random.Random(f"{SEED}/schedule")
        self.tenants = tenants
        # Every kind once first, in the seed's order, so that a short run
        # meets each; then by weight.
        self.first = list(self.WEIGHTS)
        self.rng.shuffle(self.first)

    def next(self):
        if self.first:
            kind = self.first.pop()
        else:
            kinds = list(self.WEIGHTS)
            kind = self.rng.choices(kinds, [self.WEIGHTS[k] for k in kinds])[0]
        return kind, getattr(self, "f_" + kind.replace("-", "_"))

    def hold(self, lo, hi):
        return round(self.rng.uniform(lo, hi) * LEASE, 2)

    def node(self):
        return self.rng.choice([n for n, _ in NODES])

    def f_kill(self):
        n, held = self.node(), self.hold(0.5, 4)
        docker("kill", cname(n))
        time.sleep(held)
        restart(n)
        return {"node": n, "down_s": held}

    def f_pause(self):
        n, held = self.node(), self.hold(0.5, 3)
        docker("pause", cname(n))
        time.sleep(held)
        docker("unpause", cname(n))
        return {"node": n, "paused_s": held, "past_lease": held > LEASE}

    def f_router(self):
        r = self.rng.choice(["r1", "r1", "r2"])
        n = self.node()
        kind, spec = link_spec(self.rng)
        held = self.hold(0.5, 3)
        netem(r, [ip(n)], spec)
        netem(n, [ip(r)], spec)
        time.sleep(held)
        clear(r)
        clear(n)
        return {"router": r, "node": n, "link": kind, "netem": spec, "held_s": held}

    def f_replica(self):
        a, b = self.rng.sample([n for n, _ in NODES], 2)
        kind, spec = link_spec(self.rng)
        held = self.hold(0.5, 3)
        netem(a, [ip(b)], spec)
        netem(b, [ip(a)], spec)
        time.sleep(held)
        clear(a)
        clear(b)
        return {"nodes": [a, b], "link": kind, "netem": spec, "held_s": held}

    def f_catch_up(self):
        n = self.node()
        others = [m for m, _ in NODES if m != n]
        down = self.hold(1.5, 3)
        docker("kill", cname(n))
        time.sleep(down)
        since = time.time() - 0.5
        docker("start", cname(n))
        # Its peers send to it slowly, so that its copies take a while to
        # catch up with what was written while it was down.
        rate = self.rng.choice(["256kbit", "512kbit", "1mbit"])
        there = ip(n)
        for m in others:
            netem(m, [there], f"rate {rate} delay 20ms")
        listening(n, since)

        def behind():
            """The tenants whose copy on `n` the router names and that hold
            fewer appends than their primaries, or answer none yet."""
            out = []
            for t in self.tenants:
                p = placement(t)
                if p.get("replica") != n:
                    continue
                s, mine = query(n, t, "get log count")
                s2, theirs = query(p["node"], t, "get log count")
                if s != 200 or (s2 == 200 and rows_of(mine) != rows_of(theirs)):
                    out.append(t)
            return out

        try:
            seen = until(behind, f"a copy on {n} catching up", within=10 + 3 * LEASE, every=0.1)
        except Failed:
            seen = []
        docker("kill", cname(n))
        time.sleep(self.hold(0.1, 1))
        restart(n)
        for m in others:
            clear(m)
        return {"node": n, "down_s": down, "rate": rate, "restarted_while_behind": seen}

    def f_move(self):
        moves = []
        for t in self.tenants:
            p = placement(t)
            if p.get("state") == "active" and p.get("replica"):
                to = [m for m, _ in NODES if m not in (p["node"], p["replica"])]
                if to:
                    moves.append((t, p["node"], to[0]))
        if not moves:
            return {"skipped": "no tenant to move"}
        t, src, to = self.rng.choice(moves)
        how = self.rng.choice(["kill", "pause", "partition"])
        rate = self.rng.choice(["256kbit", "512kbit"])
        netem(src, [ip("r1")], f"rate {rate}")
        answer = {}

        def move():
            try:
                answer["status"], answer["body"] = shard("r1", "POST", f"/_shard/tenants/{t}/move",
                                                         {"to": to}, timeout=120)
            except (OSError, http.client.HTTPException) as e:
                answer["status"], answer["body"] = None, type(e).__name__
        mover = threading.Thread(target=move, daemon=True)
        mover.start()
        moving = True
        try:
            until(lambda: placement(t).get("state") == "moving", f"{t} to be moving", within=10, every=0.05)
        except Failed:
            moving = False
        time.sleep(self.rng.uniform(0, 1.5))
        held = self.hold(1.5, 3)
        if how == "kill":
            docker("kill", cname(src))
        elif how == "pause":
            docker("pause", cname(src))
        else:
            clear(src)
            netem(src, [ip("r1")], "loss 100%")
            netem("r1", [ip(src)], "loss 100%")
        time.sleep(held)
        if how == "kill":
            restart(src)
        elif how == "pause":
            docker("unpause", cname(src))
        else:
            clear("r1")
        clear(src)
        mover.join(timeout=150)
        return {"tenant": t, "from": src, "to": to, "source": how, "held_s": held, "rate": rate,
                "moving_when_hit": moving, "move_answer": answer.get("status"),
                "move_body": str(answer.get("body"))[:300]}

    def f_clock(self):
        n = self.node()
        shift = self.rng.choice([-1, 1]) * self.rng.choice([3_600_000, 6 * 3_600_000, 86_400_000, 30 * 86_400_000])
        held = self.hold(1, 4)
        docker("exec", cname(n), "sh", "-c", f"echo {shift} > {CLOCK_FILE}")
        time.sleep(held)
        docker("exec", cname(n), "sh", "-c", f": > {CLOCK_FILE}")
        return {"node": n, "shift_ms": shift, "held_s": held}


# ---------------------------------------------------------------- settling


def heal_all():
    """Whatever a fault left: every node and router running and unpaused,
    no netem anywhere, every clock the system's."""
    for name in ["g1", *ROUTERS, *[n for n, _ in NODES]]:
        running, paused = state(name)
        if paused:
            docker("unpause", cname(name))
        if not running:
            restart(name)
        if name != "g1":
            clear(name)
    for n, _ in NODES:
        docker("exec", cname(n), "sh", "-c", f": > {CLOCK_FILE}", check_=False)


def copy_rows(node, t):
    rows = {}
    for c in ["regs", "log"]:
        s, b = query(node, t, f"get {c}", timeout=30)
        if s != 200:
            raise Failed(f"{c} of {t} on {node}: {s} {b[:200]}")
        rows[c] = rows_of(b)
    return rows


def repair(record, why):
    """What deploy/RUNBOOK.md has an operator do for a tenant left without
    its replica (*Replicas*, *Files left on nodes*): a repair, and each file
    it lists as left on a node -- a copy the router noted with no identity
    to take it back by, such as a detached replica that stayed on a node
    failed over -- removed where the directory places neither the tenant
    nor its replica, then a repair again. Without it, a region of three
    nodes runs out of nodes to put a tenant's replica on after two
    failures. Each file removed is recorded."""
    s, b = shard("r1", "POST", "/_shard/replicas", timeout=60)
    try:
        left = json.loads(b).get("left", [])
    except ValueError:
        left = []
    removed = []
    for e in left:
        t, node = e.get("tenant"), e.get("node")
        if node not in ADMIN:
            continue
        p = placement(t)
        if node in (p.get("node"), p.get("replica")):
            continue
        st, _ = call(node, "DELETE", f"/_admin/tenants/{t}", token=ADMIN[node], timeout=30)
        removed.append({"tenant": t, "node": node, "status": st, "why": str(e.get("why", ""))[:160]})
    if removed:
        shard("r1", "POST", "/_shard/replicas", timeout=60)
        record["operator"].append({"after": why, "repair": s, "removed": removed})
        say(f"  an operator's repair: {len(removed)} file(s) left on nodes removed")


def settle(tenants, clients, record, why):
    """Every tenant placed, on a node and a replica both running; the
    clients paused, and every replica's rows the same as its primary's.
    A replica that never comes to hold them is a violation, and ends the
    run: the history is kept for it."""
    heal_all()
    began = time.monotonic()
    repaired = [began]
    lacking = {}

    def placed():
        ps = {t: placement(t) for t in tenants}
        lacking.clear()
        lacking.update({t: p for t, p in ps.items() if not p.get("replica") or p.get("state") != "active"})
        # The router repairs on its own as a node answers again; past 20 s
        # without, as an operator would, every 20 s.
        if lacking and time.monotonic() - repaired[0] > 20:
            repair(record, why)
            repaired[0] = time.monotonic()
        return None if lacking else ps

    try:
        until(placed, "every tenant placed with a replica", within=180, every=0.5)
    except Failed as e:
        raise Failed(f"after {why}: {e}; still lacking: {json.dumps(lacking)}") from None
    pause_all(clients)
    try:
        digests = {}
        for t in tenants:
            last = {}

            def same():
                # Placed again each time: a failover meanwhile -- a lease
                # missed under load -- moves the primary and its copy.
                p = placement(t)
                if not p.get("replica"):
                    return False
                a = copy_rows(p["node"], t)
                b = copy_rows(p["replica"], t)
                last.update(p=p, a=a, b=b)
                return oracle.digest(a) == oracle.digest(b)

            try:
                until(same, f"{t}'s replica to hold its primary's rows", within=60, every=0.25)
            except Failed as e:
                p = last.get("p", {})
                diff = oracle.differences(last["a"], last["b"]) if "a" in last else []
                record["violations"].append(
                    f"after {why}: {t}'s replica on {p.get('replica')} does not hold the rows of "
                    f"its primary on {p.get('node')}: {'; '.join(diff) or e}")
                raise
            digests[t] = {"primary": last["p"]["node"], "replica": last["p"]["replica"],
                          "digest": oracle.digest(last["a"]),
                          "rows": {c: len(r) for c, r in last["a"].items()}}
        record["settles"].append({"after": why, "ms": round((time.monotonic() - began) * 1000),
                                  "digests": digests})
    finally:
        resume_all(clients)


# ------------------------------------------------------------------- inner


def inner():
    record = {"seed": SEED, "run": RUN, "minutes": MINUTES, "lease_s": LEASE, "tenants": TENANTS,
              "clients_per_tenant": CLIENTS, "faults": [], "settles": [], "operator": [],
              "violations": []}
    clients = []
    ok = False
    deadline = time.monotonic() + MINUTES * 60 + 600
    try:
        tenants = build_world()
        clients = [Client(i, t) for i, t in enumerate(t for t in tenants for _ in range(CLIENTS))]
        for c in clients:
            c.start()
        time.sleep(2)
        sched = Schedule(tenants)
        ends = time.monotonic() + MINUTES * 60
        while time.monotonic() < ends:
            kind, fault = sched.next()
            at = time.monotonic() - T0
            say(f"fault {len(record['faults']) + 1}: {kind}")
            got = fault()
            got.update({"fault": kind, "at_s": round(at, 2), "healed_at_s": round(time.monotonic() - T0, 2)})
            record["faults"].append(got)
            say(f"  {json.dumps(got)}")
            settle(tenants, clients, record, f"fault {len(record['faults'])} ({kind})")
            check(time.monotonic() < deadline, "the run outlasted its bound")
            time.sleep(sched.rng.uniform(1, 3))
        for c in clients:
            c.stop.set()
        for c in clients:
            c.join(timeout=30)
        settle(tenants, [], record, "the clients stopped")
        finals(tenants, clients, record)
        ok = not record["violations"]
    except Failed as e:
        say(f"FAILED: {e}")
        record["failed"] = str(e)
    except KeyboardInterrupt:
        record["failed"] = "interrupted"
    except Exception as e:  # noqa: BLE001 -- the driver's own failure, said as one
        traceback.print_exc()
        record["failed"] = f"the driver: {type(e).__name__}: {e}"
    finally:
        for c in clients:
            c.stop.set()
        ops = [op for c in clients for op in c.ops]
        summarize(record, ops)
        if not ok:
            # Every operation and the final reads, which oracle.py reads
            # back as they were checked (Op.from_json).
            with open(os.path.join(OUT, "history.json"), "w") as f:
                json.dump([op.to_json() for op in ops + FINAL_READS], f)
            logs()
        with open(os.path.join(OUT, "result.json"), "w") as f:
            json.dump(record, f, indent=2)
        if KEEP and not ok:
            say(f"CHAOS_KEEP=1: left running; docker rm -f $(docker ps -aq --filter label={LABEL})")
        else:
            teardown()
    return 0 if ok else 1


FINAL_READS = []


def finals(tenants, clients, record):
    """The clients stopped: every register read once more through the
    router, and each primary's appends, for the oracle."""
    ops = [op for c in clients for op in c.ops]
    reads = FINAL_READS
    found = {}
    for t in tenants:
        for k in range(1, REGISTERS + 1):
            s = time.monotonic()
            st, b = query("r1", t, "get regs where id = $1", [k], timeout=30)
            check(st == 200, f"the final read of {t}/{k}: {st} {b[:200]}")
            rows = rows_of(b)
            reads.append(oracle.Op("r", t, k, rows[0]["v"] if rows else None, s, time.monotonic(), "ok"))
        st, b = query("r1", t, "get log select id", timeout=60)
        check(st == 200, f"the appends of {t}: {st} {b[:200]}")
        found[t] = {r["id"] for r in rows_of(b)}
        st, b = query("r1", t, "get log count", timeout=60)
        check(st == 200 and rows_of(b)[0]["count"] == len(found[t]),
              f"{t}'s appends read whole: {len(found[t])} of {b[:200]}")
    history = ops + reads
    record["violations"] += [f"an answer that does not read: {g}" for c in clients for g in c.garbled][:20]
    record["violations"] += [f"register: {v}" for v in oracle.check_registers(history)]
    lost, ran = oracle.check_appends(ops, found)
    record["violations"] += [f"append answered and lost: {op!r}" for op in lost[:20]]
    record["violations"] += [f"append refused and found: {op!r}" for op in ran[:20]]
    record["lost_appends"] = len(lost)
    record["violations"] += [f"epoch: {v}" for v in oracle.check_epochs(ops)]


def summarize(record, ops):
    counts = {}
    for op in ops:
        counts.setdefault(op.kind, {}).setdefault(op.outcome, 0)
        counts[op.kind][op.outcome] += 1
    names = {"w": "writes", "r": "reads", "a": "appends"}
    record["operations"] = {names[k]: v for k, v in counts.items()}
    terms = {}
    for op in ops:
        if op.history:
            terms.setdefault(op.tenant, set()).add(op.history[0])
    record["terms"] = {t: sorted(v) for t, v in sorted(terms.items())}
    print()
    print(f"== chaos seed {SEED} ({RUN}): {'PASSED' if not record['violations'] and 'failed' not in record else 'FAILED'} ==")
    by_kind = {}
    for f in record["faults"]:
        by_kind[f["fault"]] = by_kind.get(f["fault"], 0) + 1
    print(f"faults {len(record['faults'])}: " + ", ".join(f"{k} {n}" for k, n in sorted(by_kind.items())))
    for name, c in record["operations"].items():
        print(f"{name}: " + ", ".join(f"{o} {n}" for o, n in sorted(c.items())))
    print(f"terms reached by tenant: {record['terms']}")
    removed = sum(len(o["removed"]) for o in record["operator"])
    if removed:
        print(f"files left on nodes removed as the runbook's operator does: {removed}")
    print(f"settled {len(record['settles'])} times, every replica's digest its primary's each time"
          if record["settles"] and not record["violations"] else f"settled {len(record['settles'])} times")
    for v in record["violations"]:
        print(f"VIOLATION {v}")
    if "failed" in record:
        print(f"failed: {record['failed']}")


def logs():
    for name in ["g1", *ROUTERS, *[n for n, _ in NODES]]:
        p = subprocess.run(["docker", "logs", "--timestamps", "--tail", "400", cname(name)],
                           stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=60)
        with open(os.path.join(OUT, f"{name}.log"), "w") as f:
            f.write(p.stdout)


def teardown():
    ids = docker("ps", "-aq", "--filter", f"label={LABEL}", check_=False).split()
    # Never the driver itself, which carries the label and goes with --rm.
    me = os.environ.get("CHAOS_SELF")
    mine = docker("ps", "-aq", "--filter", f"name=^{me}$", check_=False).split() if me else []
    ids = [i for i in ids if i not in mine]
    if ids:
        docker("rm", "-f", *ids, check_=False)
    vols = docker("volume", "ls", "-q", "--filter", f"label={LABEL}", check_=False).split()
    if vols:
        docker("volume", "rm", "-f", *vols, check_=False)
    if me:
        docker("network", "disconnect", "-f", NET, me, check_=False)
    docker("network", "rm", NET, check_=False)


if __name__ == "__main__":
    if "--inside" in sys.argv:
        signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
        sys.exit(inner())
    sys.exit(outer())
