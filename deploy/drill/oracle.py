"""What `make chaos` holds a run's history to (deploy/drill/chaos.py): the
standard library alone, each check a function of the operations recorded,
held by deploy/drill/test_oracle.py.

An operation is what a client asked and what it was answered, its span on
the driver's monotonic clock:

    kind     "w" a register's write, "r" its read, "a" an append
    tenant   the tenant it went to
    key      the register (an append's own value for "a")
    value    the value a write wrote, unique in the run; for a read, the
             value it found, None where the register held none
    s, e     when it was sent and when it was answered
    outcome  "ok" answered 2xx; "fail" refused, so it did not run -- a 4xx,
             a 503 (every 503 says nothing landed), a connection never made;
             "unknown" sent and not answered, a 500 (`outcome: unknown`
             among them), a 502 or a 504: it may have run, then or later
    seq      a write's Fennec-Seq, the change it left the database at
    history  its Fennec-History, (term, id); (0, 0) for the root's

The register check is fennec-bench's YCSB checker (`check` in
crates/fennec-bench/src/bin/ycsb.rs, `make ycsb --verify`) for one field a
record, with a write whose outcome is unknown taken as one that may land at
any moment after it was sent -- an end at infinity, overlapping every later
read and never the last to have ended before one: a read must find the
value of a write that could have been the last before it -- one that ended
before the read began and was followed by none that also did, or one that
overlapped it -- or nothing where no write had ended before it. The final
reads, made after every fault healed and every client stopped, are reads
like any other: a write acknowledged and then lost is found by the first
read after it that finds an older value, the last one at the latest.
"""

import bisect
import hashlib
import json

INF = float("inf")


class Op:
    __slots__ = ("kind", "tenant", "key", "value", "s", "e", "outcome", "seq", "history", "why")

    def __init__(self, kind, tenant, key, value, s, e, outcome, seq=None, history=None, why=None):
        self.kind = kind
        self.tenant = tenant
        self.key = key
        self.value = value
        self.s = s
        self.e = e
        self.outcome = outcome
        self.seq = seq
        self.history = history
        self.why = why

    def to_json(self):
        return [self.kind, self.tenant, self.key, self.value, round(self.s, 6),
                None if self.e == INF else round(self.e, 6), self.outcome, self.seq,
                list(self.history) if self.history else None, self.why]

    @staticmethod
    def from_json(j):
        kind, tenant, key, value, s, e, outcome, seq, history, why = j
        return Op(kind, tenant, key, value, s, INF if e is None else e, outcome, seq,
                  tuple(history) if history else None, why)

    def __repr__(self):
        span = f"[{self.s:.3f}, {'inf' if self.e == INF else f'{self.e:.3f}'}]"
        return f"{self.kind} {self.tenant}/{self.key}={self.value} {span} {self.outcome}"


class Register:
    """The writes one register was sent, by their end, and the latest start
    among the first i + 1 of them -- ycsb.rs's `Reg`."""

    def __init__(self, writes):
        self.ws = sorted(writes, key=lambda w: (w.e, w.s))
        self.ends = [w.e for w in self.ws]
        self.smax = []
        m = -INF
        for w in self.ws:
            m = max(m, w.s)
            self.smax.append(m)

    def latest_start_before(self, t):
        """The latest start of a write that ended before `t`, if one did."""
        n = bisect.bisect_left(self.ends, t)
        return self.smax[n - 1] if n else None


def registers(ops):
    """Each register's writes that may have run: answered, or unknown --
    whose end is taken as infinity, whatever the client saw."""
    by = {}
    for op in ops:
        if op.kind == "w" and op.outcome in ("ok", "unknown"):
            w = op if op.outcome == "ok" else Op(op.kind, op.tenant, op.key, op.value, op.s, INF, op.outcome)
            by.setdefault((op.tenant, op.key), []).append(w)
    return {r: Register(ws) for r, ws in by.items()}


def check_registers(ops, limit=20):
    """Every read answered against the writes: the violations, each a line,
    at most `limit` of them and then their count."""
    regs = registers(ops)
    by_value = {}
    for r, reg in regs.items():
        for w in reg.ws:
            by_value[w.value] = (r, w)
    out = []
    count = 0
    for op in ops:
        if op.kind != "r" or op.outcome != "ok":
            continue
        r = (op.tenant, op.key)
        reg = regs.get(r)
        before = reg.latest_start_before(op.s) if reg else None
        if op.value is None:
            ok = before is None
            why = "found nothing, though a write to it had been answered before the read was sent"
        else:
            found = by_value.get(op.value)
            if found is None or found[0] != r:
                ok = False
                why = f"found {op.value}, which no write to it wrote"
            else:
                w = found[1]
                overlaps = w.s <= op.e and w.e >= op.s
                last = w.e < op.s and before is not None and before <= w.e
                ok = overlaps or last
                # Said only of a read that fails: one that passes may have
                # had no write end before it (`before` None).
                if ok:
                    why = ""
                elif w.s > op.e:
                    why = f"found {op.value}, whose write was sent at {w.s:.3f}, after the read was answered"
                else:
                    # It ended before the read, so a write did: `before`.
                    why = (f"found {op.value}, written over [{w.s:.3f}, {w.e:.3f}], though a write "
                           f"answered before the read was sent began at {before:.3f}, after it")
        if not ok:
            count += 1
            if len(out) < limit:
                out.append(f"{op!r}: {why}")
    if count > limit:
        out.append(f"... {count - limit} more")
    return out


def check_appends(ops, found):
    """Every append answered is in `found` (the values each tenant's
    primary holds, by tenant), and no append refused is: (lost, ran)."""
    lost, ran = [], []
    for op in ops:
        if op.kind != "a":
            continue
        there = op.value in found.get(op.tenant, ())
        if op.outcome == "ok" and not there:
            lost.append(op)
        elif op.outcome == "fail" and there:
            ran.append(op)
    return lost, ran


def check_epochs(ops, limit=20):
    """No tenant written by two primaries at once, read off the positions
    its answered writes were given: within one history no change number is
    given two writes, a history's id names one term, and no write is
    answered in a term after one sent after a later term's write was
    answered -- a promotion forks the history into the next term, so an
    older term answering after a newer one has is the primary failed over
    still writing."""
    out = []
    by_tenant = {}
    for op in ops:
        if op.kind in ("w", "a") and op.outcome == "ok" and op.seq is not None:
            by_tenant.setdefault(op.tenant, []).append(op)
    for tenant, ws in sorted(by_tenant.items()):
        at = {}
        terms = {}
        for op in ws:
            term, hid = op.history or (0, 0)
            other = at.get((hid, op.seq))
            if other is not None and other.value != op.value:
                out.append(f"{tenant}: change {op.seq} of history {hid:x} was given two writes: "
                           f"{other!r} and {op!r}")
            at[(hid, op.seq)] = op
            if terms.setdefault(hid, term) != term:
                out.append(f"{tenant}: history {hid:x} is term {terms[hid]} and term {term}")
        # Swept by start: the highest term among the writes answered before
        # each one was sent.
        ended = sorted(ws, key=lambda o: o.e)
        i = 0
        best = None
        for op in sorted(ws, key=lambda o: o.s):
            while i < len(ended) and ended[i].e < op.s:
                if best is None or (ended[i].history or (0, 0))[0] > (best.history or (0, 0))[0]:
                    best = ended[i]
                i += 1
            term = (op.history or (0, 0))[0]
            if best is not None and (best.history or (0, 0))[0] > term:
                out.append(f"{tenant}: {op!r} was answered in term {term} after {best!r} "
                           f"was answered in term {best.history[0]}")
        if len(out) > limit:
            break
    return out[:limit]


def digest(rows):
    """A copy's rows as one value: each collection's rows by id, as JSON
    with sorted keys, hashed -- the same rows in any order the same."""
    h = hashlib.sha256()
    for collection in sorted(rows):
        h.update(collection.encode() + b"\0")
        for row in sorted(rows[collection], key=lambda r: json.dumps(r.get("id"), sort_keys=True)):
            h.update(json.dumps(row, sort_keys=True, separators=(",", ":")).encode() + b"\n")
    return h.hexdigest()[:16]


def differences(a, b, limit=5):
    """What two copies' rows differ by, for a failure's message."""
    out = []
    for collection in sorted(set(a) | set(b)):
        x = {json.dumps(r.get("id")): r for r in a.get(collection, [])}
        y = {json.dumps(r.get("id")): r for r in b.get(collection, [])}
        only_x = sorted(set(x) - set(y))
        only_y = sorted(set(y) - set(x))
        changed = sorted(k for k in set(x) & set(y) if x[k] != y[k])
        if only_x or only_y or changed:
            out.append(f"{collection}: {len(only_x)} rows only on the first {only_x[:limit]}, "
                       f"{len(only_y)} only on the second {only_y[:limit]}, "
                       f"{len(changed)} differing {[(x[k], y[k]) for k in changed[:limit]]}")
    return out
