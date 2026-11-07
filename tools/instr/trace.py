#!/usr/bin/env python3
"""One call of a function, every instruction it retires counted by the
function it is in: lldb single-steps it from its entry to its return.

    trace.py run [--function F] [--hit N] [--out FILE] BIN [ARGS...]
    trace.py diff A B         two traces' totals and per-function deltas
    trace.py show A           one trace's per-function counts

`run` launches BIN ARGS under lldb (`MallocNanoZone=0`, as the counted runs
are), stops at the N-th call of F (`fennec_http::handle`, the 30th: past a
first request's one-time work), steps it instruction by instruction, into
every call, to its return, and writes each instruction's module and
function, one a line, to FILE (`<BIN>.<F>.trace` beside the working
directory unless given). A clock read (`clock_gettime`, the commpage's) is
run whole and counted as one step: its retry loop never settles under single
steps. The same binary traced twice counts the same to the instruction.

A trace splits a call into the program's own code, the allocator
(`libsystem_malloc`) and the rest of the system (`memmove` and the like):
on macOS 26 the allocator's share follows the binary's identity
(docs/design/benchmarks.md#instruction-counts), the own code's does not, so
`diff` of two builds' traces says what a change costs in its own code to
the instruction, whatever draw of the allocator either build got.

macOS-only (lldb's Python module, which needs the Python it was built for:
`run` starts itself again under /usr/bin/python3 when another one runs it);
`diff` and `show` read files and run anywhere. Single-stepping is some
thousand instructions a second: a call of 20 000 takes a few seconds to a
minute.
"""

import argparse
import collections
import os
import re
import subprocess
import sys

# Run whole: a clock read's seqlock retries forever under single steps.
OVER = ("clock_gettime", "clock_gettime_nsec_np", "gettimeofday", "mach_absolute_time",
        "mach_continuous_time", "__commpage_gettimeofday", "__commpage_gettimeofday_internal")
MALLOC = "libsystem_malloc.dylib"


def clean(name):
    """A symbol as the two builds would both name it: Rust's hash and LLVM's
    copy suffixes cut."""
    name = re.sub(r"::h[0-9a-f]{16}$", "", name or "?")
    return re.sub(r" \(\.(llvm\.)?\d+\)$", "", name)


def lldb_module():
    """lldb's Python module, from where `lldb -P` says it is."""
    try:
        import lldb  # noqa: F401
        return lldb
    except ImportError:
        pass
    try:
        path = subprocess.run(["xcrun", "lldb", "-P"], capture_output=True, text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        path = subprocess.run(["lldb", "-P"], capture_output=True, text=True, check=True).stdout.strip()
    sys.path.insert(0, path)
    import lldb
    return lldb


def reexec_under_system_python():
    """lldb's module is built for the Python Xcode ships, /usr/bin/python3;
    importing it under another version fails or crashes."""
    system = "/usr/bin/python3"
    if os.environ.get("INSTR_TRACE_REEXEC") or not os.path.exists(system):
        return
    here = subprocess.run([system, "-c", "import sys; print(sys.version_info[:2])"],
                          capture_output=True, text=True).stdout.strip()
    if here != str(tuple(sys.version_info[:2])):
        os.environ["INSTR_TRACE_REEXEC"] = "1"
        os.execv(system, [system, os.path.abspath(__file__), *sys.argv[1:]])


def trace(binary, args, function, hit, out, env=(), over=OVER, max_steps=5_000_000, log=None):
    """Writes one call's trace to `out` ("module<TAB>function" an
    instruction) and returns its instruction count."""
    lldb = lldb_module()
    log = log or (lambda m: print(m, file=sys.stderr))
    dbg = lldb.SBDebugger.Create()
    dbg.SetAsync(False)
    target = dbg.CreateTarget(binary)
    if not target.IsValid():
        raise RuntimeError(f"lldb cannot load {binary}")
    own = os.path.basename(binary)
    bp = target.BreakpointCreateByRegex(rf"^{re.escape(function)}(::h[0-9a-f]{{16}})?$")
    if bp.GetNumLocations() == 0:
        target.BreakpointDelete(bp.GetID())
        bp = target.BreakpointCreateByName(function)
    if bp.GetNumLocations() == 0:
        raise RuntimeError(f"no function {function} in {binary} (built with its symbols? `-C strip=none`)")
    bp.SetIgnoreCount(hit - 1)
    info = lldb.SBLaunchInfo(list(args))
    info.SetWorkingDirectory(os.getcwd())
    environment = target.GetEnvironment()
    environment.Set("MallocNanoZone", "0", True)
    for kv in env:
        k, v = kv.split("=", 1)
        environment.Set(k, v, True)
    info.SetEnvironment(environment, False)
    err = lldb.SBError()
    proc = target.Launch(info, err)
    if not err.Success():
        raise RuntimeError(f"launch {binary}: {err}")
    try:
        if proc.GetState() != lldb.eStateStopped:
            raise RuntimeError(f"{binary} ended before the {hit}th call of {function}")
        thread = next((t for t in proc if t.GetStopReason() == lldb.eStopReasonBreakpoint), None)
        if thread is None:
            raise RuntimeError(f"{binary} stopped, but not at {function}")
        proc.SetSelectedThread(thread)
        target.BreakpointDelete(bp.GetID())
        frame = thread.GetFrameAtIndex(0)
        sp0 = frame.GetSP()
        arm = target.GetTriple().split("-")[0] in ("arm64", "arm64e", "aarch64")
        if arm:
            ret = frame.FindRegister("lr").GetValueAsUnsigned()
        else:
            ret = proc.ReadPointerFromMemory(sp0, err)
        log(f"stopped at {frame.GetFunctionName()}, its {hit}th call; stepping it to its return")
        names, cache, steps = [], {}, 0
        while steps < max_steps:
            f0 = thread.GetFrameAtIndex(0)
            pc, sp = f0.GetPC(), f0.GetSP()
            # Returned: back at the caller with the call's frame gone (a
            # recursive call's return comes back lower on the stack).
            if pc == ret and (sp >= sp0 if arm else sp > sp0):
                break
            line = cache.get(pc)
            if line is None:
                addr = target.ResolveLoadAddress(pc)
                sym = addr.GetSymbol()
                module = addr.GetModule().GetFileSpec().GetFilename() or "?"
                name = clean(sym.GetName()) if sym.IsValid() else "?"
                if sym.IsValid() and sym.GetType() == lldb.eSymbolTypeTrampoline:
                    name += " (stub)"
                line = cache[pc] = (f"{module}\t{name}", name in OVER or name in over)
            names.append(line[0])
            steps += 1
            if line[1]:
                thread.StepOut()
            else:
                thread.StepInstruction(False)
            if steps % 20000 == 0:
                log(f"{steps} instructions")
        else:
            raise RuntimeError(f"{function} did not return within {max_steps} instructions")
    finally:
        proc.Kill()
        lldb.SBDebugger.Destroy(dbg)
    with open(out, "w") as f:
        f.write("\n".join(names) + "\n")
    return len(names)


def load(path):
    """A trace's counts by (module, function)."""
    c = collections.Counter()
    with open(path) as f:
        for line in f:
            line = line.rstrip("\n")
            if line:
                module, _, name = line.partition("\t")
                c[(module, name)] += 1
    return c


def kind(module, own):
    if module == MALLOC:
        return "malloc"
    return "own" if module == own else "system"


def own_module(c):
    """The traced program's module: the one its first instruction is in."""
    return next(iter(c))[0] if c else "?"


def totals(c, own):
    t = collections.Counter()
    for (module, _), n in c.items():
        t[kind(module, own)] += n
    t["total"] = sum(c.values())
    return t


def diff(a, b, top=40, everything=False):
    """Lines: the totals of trace `a` and `b`, by kind, and the functions
    whose counts differ, the program's own first, largest first."""
    ca, cb = load(a), load(b)
    oa, ob = own_module(ca), own_module(cb)
    ta, tb = totals(ca, oa), totals(cb, ob)
    out = [f"{'':8} {'A':>9} {'B':>9} {'B - A':>8}"]
    for k in ("total", "own", "malloc", "system"):
        out.append(f"{k:8} {ta[k]:9} {tb[k]:9} {tb[k] - ta[k]:+8}")
    # The two builds' own modules may be named apart (replays-base, replays-head).
    key = lambda m, n: ("own" if m in (oa, ob) else m, n)
    xa, xb = collections.Counter(), collections.Counter()
    for (m, n), v in ca.items():
        xa[key(m, n)] += v
    for (m, n), v in cb.items():
        xb[key(m, n)] += v
    rows = [(xb[k] - xa[k], xa[k], xb[k], k) for k in set(xa) | set(xb)
            if xb[k] != xa[k] and (everything or k[0] == "own")]
    rows.sort(key=lambda r: (-abs(r[0]), r[3]))
    what = "every function" if everything else "the program's own functions"
    out.append(f"\n{what} whose count differs ({len(rows)}), largest first:")
    for d, x, y, (m, n) in rows[:top]:
        where = "" if m == "own" else f" [{m}]"
        out.append(f"{d:+7} {x:7} {y:7}  {n[:140]}{where}")
    if len(rows) > top:
        rest = sum(r[0] for r in rows[top:])
        out.append(f"{rest:+7} in {len(rows) - top} more")
    return out


def show(path, top=40):
    c = load(path)
    own = own_module(c)
    t = totals(c, own)
    out = [f"{t['total']} instructions: own {t['own']}, malloc {t['malloc']}, system {t['system']}"]
    for (m, n), v in c.most_common(top):
        out.append(f"{v:7}  {n[:140]}{'' if m == own else f' [{m}]'}")
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run", help="trace one call")
    r.add_argument("--function", default="fennec_http::handle", help="the function, its path as Rust names it")
    r.add_argument("--hit", type=int, default=30, help="which call (1 the first)")
    r.add_argument("--out", help="the trace file")
    r.add_argument("--env", action="append", default=[], help="K=V for the program, again for more")
    r.add_argument("--over", action="append", default=[], help="a function run whole, counted one step")
    r.add_argument("--max-steps", type=int, default=5_000_000)
    r.add_argument("binary")
    r.add_argument("args", nargs=argparse.REMAINDER)
    d = sub.add_parser("diff", help="two traces' totals and per-function deltas (B - A)")
    d.add_argument("--top", type=int, default=40)
    d.add_argument("--all", action="store_true", help="the system's functions too")
    d.add_argument("a")
    d.add_argument("b")
    s = sub.add_parser("show", help="one trace's per-function counts")
    s.add_argument("--top", type=int, default=40)
    s.add_argument("a")
    a = ap.parse_args()
    if a.cmd == "diff":
        print("\n".join(diff(a.a, a.b, a.top, a.all)))
        return
    if a.cmd == "show":
        print("\n".join(show(a.a, a.top)))
        return
    if sys.platform != "darwin":
        print(f"trace.py run: lldb's stepping here is macOS's; nothing to do on {sys.platform}", file=sys.stderr)
        return
    reexec_under_system_python()
    out = a.out or f"{os.path.basename(a.binary)}.{a.function.replace('::', '.')}.trace"
    try:
        n = trace(a.binary, [x for x in a.args if x != "--"], a.function, a.hit, out, a.env, tuple(a.over),
                  a.max_steps)
    except RuntimeError as e:
        sys.exit(f"trace.py: {e}")
    print(f"{n} instructions in the {a.hit}th call of {a.function}: {out}")
    print("\n".join(show(out, 15)))


if __name__ == "__main__":
    main()
