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

A lost write the reads cannot see -- written over before any read of its
register, or behind an earlier write whose outcome is unknown (w10 answered,
w11 unknown, w12 answered, a read finding 11) -- is found by the ledger:
each register write lands a row of its own value in `wl` in the same block,
never written over, so every write answered is held to the ledger as every
append is to `log` (`check_kept`), and none refused may be in either.

What a failover may lose is what the design names and nothing else
(deploy/RUNBOOK.md, *A node lost*; docs/design/replication.md): a write
answered while its replica is behind -- its stream past `SENT_WAIT` and not
waited for until it catches up, or no stream, the copy down or made anew --
is lost with its primary. `documented` holds each lost write to that, by
what the drill did: lost in a term a promotion ended, answered while the
fault of its span held the replica behind (its node killed, paused or
caught up again, the link between the two cut, dropped, delayed or
slowed), or in a term a failover of that span began, before its settle saw
the new copy caught up. Such a loss is counted, named, and taken as a write
whose outcome is unknown by the register check; any other loss fails.
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


def check_kept(ops, found, kind):
    """Every operation of `kind` answered is in `found` (the values each
    tenant's primary holds, by tenant: `log`'s ids for an append, the
    ledger `wl`'s for a register write), and none refused is: (lost, ran)."""
    lost, ran = [], []
    for op in ops:
        if op.kind != kind:
            continue
        there = op.value in found.get(op.tenant, ())
        if op.outcome == "ok" and not there:
            lost.append(op)
        elif op.outcome == "fail" and there:
            ran.append(op)
    return lost, ran


def check_appends(ops, found):
    """`check_kept` of the appends."""
    return check_kept(ops, found, "a")


def documented(op, windows, final):
    """Whether `op`, a write answered and then lost, is one the design says
    a failover loses: (True, why) or (False, why).

    `windows` are the drill's spans, one a fault, each from the settle
    before it to its own: `from` and `to` on the operations' clock, `start`
    when the fault began -- a write answered before it had its replica
    keeping up, whatever the fault did after -- the fault's `n` and
    `fault`, `placed` -- each tenant's [primary, replica,
    [term, history id]] as the settle before it found them -- and what the
    fault held behind: `behind_on`, the nodes whose copies it killed,
    paused or made catch up, and `links`, the pairs of nodes whose link it
    cut, dropped, delayed or slowed. `final` is each tenant's [term,
    history id] at the end; [0, 0], as a write with no `Fennec-History`,
    is the root's, which no promotion has forked."""
    # No `Fennec-History`: the root's, which no promotion has forked yet.
    h = tuple(op.history) if op.history else (0, 0)
    if tuple(final.get(op.tenant) or (0, 0)) == h:
        return False, f"its term {h[0]} is the one {op.tenant} ends in: no failover ended it"
    w = next((w for w in windows if w["from"] <= op.e < w["to"]), None)
    if w is None:
        return False, "it was answered outside every fault's span"
    placed = w["placed"].get(op.tenant)
    if not placed:
        return False, f"fault {w['n']} ({w['fault']}) found {op.tenant} placed nowhere"
    primary, replica, h0 = placed
    h0 = tuple(h0) if h0 else (0, 0)
    span = f"fault {w['n']} ({w['fault']})"
    if h == h0:
        links = [set(pair) for pair in w.get("links", [])]
        held = replica in w.get("behind_on", []) or {primary, replica} in links
        if held and op.e < w.get("start", w["from"]):
            return False, (f"answered by {primary} in term {h[0]} before {span} began, its replica "
                           f"on {replica} keeping up")
        if held:
            return True, (f"answered by {primary} in term {h[0]} while {span} held its replica on "
                          f"{replica} behind, and lost as a failover promoted the copy")
        return False, (f"answered by {primary} in term {h[0]}, its replica on {replica} held behind "
                       f"by nothing {span} did")
    if h[0] > h0[0]:
        return True, (f"answered in term {h[0]}, which a failover during {span} began, before the "
                      f"settle after it saw the new primary's copy caught up")
    return False, (f"answered in term {h[0]} (history {h[1]:x}) while {op.tenant}'s was term "
                   f"{h0[0]}: a primary failed over still writing, or two in one term")


def as_unknown(ops, values):
    """`ops` with the writes of `values` -- lost inside the window the
    design names -- taken as writes whose outcome is unknown: one may have
    been read on its primary before the failover took it, and a read after
    may find the value before it."""
    out = []
    for op in ops:
        if op.kind == "w" and op.outcome == "ok" and op.value in values:
            op = Op(op.kind, op.tenant, op.key, op.value, op.s, op.e, "unknown", op.seq, op.history, op.why)
        out.append(op)
    return out


def check_epochs(ops, limit=20):
    """No tenant written by two primaries at once, read off the positions
    its answered writes were given: within one history no change number is
    given two writes, a history's id names one term and a term one history,
    and no write is answered in a term after one sent after a later term's
    write was answered -- a promotion forks the history into the next term,
    so an older term answering after a newer one has is the primary failed
    over still writing. A promotion forks to the term after its copy's under
    a fresh id (fennec-http's `history_of`): two copies promoted from one
    term both answer in the next, under two ids, which no other check here
    tells apart."""
    out = []
    by_tenant = {}
    for op in ops:
        if op.kind in ("w", "a") and op.outcome == "ok" and op.seq is not None:
            by_tenant.setdefault(op.tenant, []).append(op)
    for tenant, ws in sorted(by_tenant.items()):
        at = {}
        terms = {}
        ids = {}
        said = set()
        for op in ws:
            term, hid = op.history or (0, 0)
            other = at.get((hid, op.seq))
            if other is not None and other.value != op.value:
                out.append(f"{tenant}: change {op.seq} of history {hid:x} was given two writes: "
                           f"{other!r} and {op!r}")
            at[(hid, op.seq)] = op
            if terms.setdefault(hid, term) != term and ("h", hid) not in said:
                said.add(("h", hid))
                out.append(f"{tenant}: history {hid:x} is term {terms[hid]} and term {term}")
            first = ids.setdefault(term, (hid, op))
            if first[0] != hid and ("t", term, hid) not in said:
                said.add(("t", term, hid))
                out.append(f"{tenant}: term {term} is two histories, {first[0]:x} and {hid:x} -- two "
                           f"primaries promoted from one term: {first[1]!r} and {op!r}")
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
