#!/usr/bin/env python3
"""Slow clients against a release fennec-server, bounded: `make
slow-client-check` (docs/design/http.md#slow-clients).

The server starts on loopback with --max-connections MAX; twice as many
clients each hold a connection badly, a third of them each way, for SECS:

  * head: a request's line, then a header a byte every TRICKLE seconds;
  * body: a head declaring a mebibyte, then its body a byte every TRICKLE;
  * reader: forty answers of a mebibyte asked at once, none read.

Each is cut at its bound -- `--header-timeout` from its first byte for a
head or a body under 64 KiB, the head's 408 read; two write timeouts and
the time its answers take to fill the sockets for a reader -- and comes
back, a refusal past the ceiling (503) waiting its Retry-After. One client
asks a request every 50 ms on a connection kept alive from the start, as a
pooled client does: it must see no failure and a p99 under a second. Every
connection's life is held to its bound. A fresh connection asked every
5 s is told, not held: with twice the ceiling trickling, a slot a cut
frees goes to whoever asks first, mostly the trickler it was cut from --
these bounds hold a connection to its request's time, not a client to its
share of the slots, which is a cap a peer address in front.

Bounded: connections at most 2 x MAX at once, each coming back no sooner
than its refusal's wait; the server is started with --exit-on-stdin-close
and a pipe on its stdin that this process holds, so it ends with this
process however that ends, and is killed on the way out besides.
"""

import argparse
import json
import socket
import statistics
import subprocess
import sys
import threading
import time

ROW = 1 << 20


def parse():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", required=True, help="the fennec-server binary")
    # 50, twice as many clients: refusals and cuts leave some 1 500
    # connections in TIME_WAIT at a time, well inside the ports there are.
    ap.add_argument("--max", type=int, default=50, help="--max-connections")
    ap.add_argument("--secs", type=int, default=300, help="how long the clients trickle")
    ap.add_argument("--header-timeout", type=int, default=10)
    ap.add_argument("--write-timeout", type=int, default=30)
    ap.add_argument("--trickle", type=float, default=0.5, help="seconds between a trickler's bytes")
    return ap.parse_args()


class Server:
    """fennec-server on loopback, port 0, found by its `listening on:` line."""

    def __init__(self, a):
        args = [
            a.server, "--http", "127.0.0.1:0", "--max-connections", str(a.max),
            "--header-timeout", str(a.header_timeout), "--write-timeout", str(a.write_timeout),
            "--exit-on-stdin-close",
        ]
        self.p = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                                  stderr=subprocess.PIPE, text=True)
        deadline = time.monotonic() + 30
        self.port = None
        while time.monotonic() < deadline:
            line = self.p.stderr.readline()
            if not line:
                break
            if "listening on: http://" in line:
                self.port = int(line.split("listening on: http://", 1)[1].split()[0].rsplit(":", 1)[1])
                break
        if self.port is None:
            self.stop()
            sys.exit("slow_client_check: the server said no `listening on:` line")
        # The rest of its log read off, so a full pipe never holds it up.
        threading.Thread(target=self.p.stderr.read, daemon=True).start()

    def stop(self):
        if self.p.poll() is None:
            self.p.kill()
        self.p.wait()


def connect(port, timeout=60):
    s = socket.create_connection(("127.0.0.1", port), timeout=timeout)
    s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    return s


def request(method, path, body=b"", extra=""):
    return (f"{method} {path} HTTP/1.1\r\nHost: localhost\r\n{extra}"
            f"Content-Length: {len(body)}\r\n\r\n").encode() + body


def answer(f):
    """One answer off a file of the socket: (status, body)."""
    line = f.readline()
    if not line:
        raise ConnectionError("closed")
    status = int(line.split()[1])
    length = 0
    while True:
        h = f.readline()
        if h in (b"\r\n", b""):
            break
        k, _, v = h.decode().partition(":")
        if k.strip().lower() == "content-length":
            length = int(v)
    return status, f.read(length)


def refused_wait(head):
    """A 503's Retry-After, in seconds, from its head; 1 without one."""
    for line in head.split(b"\r\n"):
        k, _, v = line.decode(errors="replace").partition(":")
        if k.strip().lower() == "retry-after":
            try:
                return max(float(v), 0.1)
            except ValueError:
                pass
    return 1.0


class Tally:
    def __init__(self):
        self.lock = threading.Lock()
        self.lives = {"head": [], "body": [], "reader": []}
        self.statuses = {"head": {}, "body": {}, "reader": {}}
        self.refused = 0

    def cut(self, kind, life, status):
        with self.lock:
            self.lives[kind].append(life)
            self.statuses[kind][status] = self.statuses[kind].get(status, 0) + 1

    def refusal(self):
        with self.lock:
            self.refused += 1


def trickler(kind, port, a, until, tally, bound):
    """One client holding a connection badly, again and again until `until`."""
    while time.monotonic() < until:
        try:
            s = connect(port, timeout=a.trickle)
        except OSError:
            time.sleep(1)
            continue
        started = time.monotonic()
        try:
            if kind == "reader":
                s.sendall(request("GET", "/t/1") * 40)
                # Its answer's status peeked, nothing taken: a refusal is
                # told apart from a slot.
                s.settimeout(5)
                try:
                    peeked = s.recv(12, socket.MSG_PEEK)
                except OSError:
                    peeked = b""
                s.settimeout(a.trickle)
                if peeked.startswith(b"HTTP/1.1 503"):
                    tally.refusal()
                    time.sleep(1)
                    continue
                status = peeked[9:12].decode(errors="replace") if len(peeked) == 12 else "none"
            elif kind == "head":
                s.sendall(b"POST /query HTTP/1.1\r\nHost: localhost\r\nX-Trickled: ")
            else:
                s.sendall(b"POST /query HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1048576\r\n\r\n")
            got = b""
            while time.monotonic() < until + bound:
                # A byte a while; the answer, a refusal or the end read as
                # they come. A reader reads nothing: its end is its next
                # write's failure.
                try:
                    if kind == "reader":
                        s.sendall(b"GET /collections HTTP/1.1\r\nHost: localhost\r\n\r\n")
                        time.sleep(a.trickle)
                        continue
                    s.sendall(b"a")
                except TimeoutError:
                    # The server's buffer full: it reads nothing while it
                    # writes, which is no end.
                    continue
                except OSError:
                    break
                try:
                    chunk = s.recv(65536)
                    if not chunk:
                        break
                    got += chunk
                    if got.startswith(b"HTTP/1.1 503"):
                        break
                except TimeoutError:
                    pass
                except OSError:
                    break
            life = time.monotonic() - started
            if got.startswith(b"HTTP/1.1 503"):
                tally.refusal()
                time.sleep(refused_wait(got.split(b"\r\n\r\n", 1)[0]))
                continue
            if kind != "reader":
                # What the server sent before its end, read whole.
                s.settimeout(5)
                try:
                    while True:
                        chunk = s.recv(65536)
                        if not chunk:
                            break
                        got += chunk
                except OSError:
                    pass
            if kind != "reader":
                status = got[9:12].decode(errors="replace") if got else "none"
            tally.cut(kind, life, status)
        finally:
            s.close()


def pooled(port, until, out):
    """The well-behaved client: a request every 50 ms on one kept-alive
    connection, each answer's time and every failure."""
    times, failures = [], []
    s = connect(port)
    f = s.makefile("rb")
    i = 0
    while time.monotonic() < until:
        i += 1
        body = json.dumps({"query": "get t select id where id = $1", "params": [1]}).encode()
        t = time.monotonic()
        try:
            s.sendall(request("POST", "/query", body))
            status, got = answer(f)
            took = time.monotonic() - t
            times.append(took)
            if status != 200:
                failures.append(f"{status}: {got[:200]!r}")
        except OSError as e:
            failures.append(f"request {i}: {e}")
            s.close()
            s = connect(port)
            f = s.makefile("rb")
        time.sleep(0.05)
    s.close()
    out["times"], out["failures"] = times, failures


def fresh(port, until, out):
    """A connection of its own every 5 s, asked once: each one's status,
    "none" where it got no answer. Told, not held."""
    told = {}
    while time.monotonic() + 5 < until:
        time.sleep(5)
        got = b""
        try:
            s = connect(port, timeout=30)
            s.sendall(request("POST", "/query", b'{"query":"collections"}', "Connection: close\r\n"))
            while True:
                chunk = s.recv(65536)
                if not chunk:
                    break
                got += chunk
            s.close()
        except OSError:
            pass
        status = got[9:12].decode(errors="replace") if got else "none"
        told[status] = told.get(status, 0) + 1
    out["told"] = told


def p99(xs):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(len(xs) * 0.99))] if xs else 0.0


def main():
    a = parse()
    server = Server(a)
    try:
        port = server.port
        s = connect(port)
        f = s.makefile("rb")
        for q in ("create collection t (s text)", "put t {id: 1, s: '%s'}" % ("x" * ROW)):
            s.sendall(request("POST", "/query", json.dumps({"query": q}).encode()))
            status, got = answer(f)
            if status != 200:
                sys.exit(f"slow_client_check: {q[:30]}: {status} {got[:200]!r}")
        s.close()

        until = time.monotonic() + a.secs
        tally, good, opened = Tally(), {}, {}
        threads = [threading.Thread(target=pooled, args=(port, until, good))]
        threads[0].start()
        time.sleep(0.2)
        # Past the bound, slack for a loaded machine's scheduling: a
        # trickled head or body at its timeout, a reader at two write
        # timeouts and the time its answers take to fill the sockets.
        bounds = {"head": a.header_timeout + 5, "body": a.header_timeout + 5,
                  "reader": 2 * a.write_timeout + 15}
        kinds = ["head", "body", "reader"]
        for n in range(2 * a.max):
            k = kinds[n % 3]
            threads.append(threading.Thread(
                target=trickler, args=(k, port, a, until, tally, bounds[k]), daemon=True))
            threads[-1].start()
        threads.append(threading.Thread(target=fresh, args=(port, until, opened), daemon=True))
        threads[-1].start()
        print(f"slow_client_check: {2 * a.max} trickling clients against --max-connections "
              f"{a.max} for {a.secs} s", flush=True)
        for t in threads:
            t.join(timeout=max(0, until - time.monotonic()) + max(bounds.values()) + 30)

        problems = []
        times = good.get("times", [])
        print(f"pooled client: {len(times)} requests, p50 {statistics.median(times) * 1000:.1f} ms, "
              f"p99 {p99(times) * 1000:.1f} ms, max {max(times) * 1000:.1f} ms, "
              f"{len(good.get('failures', []))} failed" if times else "pooled client: no request")
        if good.get("failures"):
            problems.append(f"the pooled client failed: {good['failures'][:5]}")
        if not times or p99(times) >= 1.0:
            problems.append(f"the pooled client's p99 is {p99(times):.3f} s, past 1 s")
        for k in kinds:
            lives = tally.lives[k]
            print(f"{k}: {len(lives)} cut, lives min {min(lives):.1f} s, p50 "
                  f"{statistics.median(lives):.1f} s, max {max(lives):.1f} s, "
                  f"statuses {tally.statuses[k]}" if lives else f"{k}: none cut")
            if not lives:
                problems.append(f"no {k} trickler was cut in {a.secs} s")
            elif max(lives) > bounds[k]:
                problems.append(f"a {k} trickler lived {max(lives):.1f} s, past {bounds[k]} s")
            if k != "reader" and lives and set(tally.statuses[k]) - {"408"}:
                problems.append(f"a {k} trickler was answered {tally.statuses[k]}, not 408 alone")
        print(f"refusals (503) the tricklers met: {tally.refused}; a fresh connection "
              f"every 5 s was told {opened.get('told', {})}")
        if server.p.poll() is not None:
            problems.append(f"the server ended (exit {server.p.returncode})")
        if problems:
            sys.exit("slow_client_check: " + "; ".join(problems))
        print("slow_client_check: ok")
    finally:
        server.stop()


if __name__ == "__main__":
    main()
