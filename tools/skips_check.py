#!/usr/bin/env python3
"""A test that skipped itself is a test that did not run, counted.

    python3 tools/skips_check.py tee LOG -- COMMAND...   # runs COMMAND, copies its output to LOG, exits as it does
    python3 tools/skips_check.py check [--runner NAME] LOG...   # the counts in those logs, held to the allowlist

`make test` runs both. A suite that cannot find web/fennec.wasm or a binary
throws under CI (web/ci-prereq.mjs, the Rust fixtures' asserts on CI), so
what is left to skip is what a runner cannot run at all: node's spec
reporter's `skipped N` and cargo's `N ignored` are summed, and past
tools/skips-allowlist.json's count for the runner a job fails. Every entry
carries its reason; a count under its entry is a note to lower it, so the
list shrinks and never hides a new skip behind an old one. Without CI the
check only reports, a developer's machine having built what it has.
"""

import json
import os
import platform
import re
import subprocess
import sys
from pathlib import Path

ALLOWLIST = Path(__file__).with_name("skips-allowlist.json")
KINDS = ("js_skipped", "rust_ignored")
# node --test --test-reporter=spec: `ℹ skipped 3`; cargo: `... 0 failed; 2 ignored; ...`
JS = re.compile(r"^\s*(?:ℹ|#)\s+skipped\s+(\d+)\s*$", re.M)
RUST_ANY = re.compile(r"^test result: ", re.M)
RUST = re.compile(r"^test result: .*?; (\d+) ignored;", re.M)


def runner_name():
    """The allowlist's key for this machine: GitHub's RUNNER_OS, else the platform."""
    return os.environ.get("RUNNER_OS") or {"Darwin": "macOS", "Windows": "Windows"}.get(
        platform.system(), "Linux"
    )


def in_ci():
    """CI set and non-empty, as the Rust and JS sides read it."""
    return bool(os.environ.get("CI"))


def counts(logs):
    """(sums, problems): a log that parsed nothing is a problem, a summary line gone missing."""
    found = {"js_skipped": 0, "rust_ignored": 0}
    problems = []
    for log in logs:
        text = Path(log).read_text(encoding="utf-8", errors="replace")
        js, rust = JS.findall(text), RUST.findall(text)
        found["js_skipped"] += sum(int(n) for n in js)
        found["rust_ignored"] += sum(int(n) for n in rust)
        is_js = Path(log).name.startswith("js")
        if is_js and not js:
            problems.append(f"{log}: no node summary `skipped N` line")
        elif not is_js and not RUST_ANY.search(text):
            problems.append(f"{log}: no cargo `test result:` line")
    return found, problems


def check(runner, logs):
    entries = json.loads(ALLOWLIST.read_text())
    for name, kinds in entries.items():
        for kind, entry in kinds.items():
            if kind not in KINDS or not entry.get("reason"):
                sys.exit(f"{ALLOWLIST.name}: {name}.{kind} needs a known kind and a reason")
    allowed = entries.get(runner, {})
    found, problems = counts(logs)
    problems = problems if in_ci() else []  # a local run teed nothing
    failed = bool(problems)
    for problem in problems:
        print(f"  {problem}: the run printed no summary, so nothing was counted", file=sys.stderr)
    for kind in KINDS:
        n, ok = found[kind], allowed.get(kind, {}).get("count", 0)
        print(f"{runner}: {kind} {n} (allowed {ok})")
        if n > ok:
            failed = True
            print(
                f"  {n - ok} more than {ALLOWLIST.name} allows: a test skipped itself or was ignored. "
                "Make what it needs exist, or add an entry with its reason.",
                file=sys.stderr,
            )
        elif n < ok:
            print(f"  note: lower {runner}.{kind} to {n} in {ALLOWLIST.name}")
    if failed and in_ci():
        sys.exit(1)
    if failed:
        print("(not under CI: reported only)")


def tee(log, command):
    """Run COMMAND; under CI copy its output to LOG too. A developer's run keeps
    its terminal (cargo's and node's colours and progress), and LOG stays empty."""
    Path(log).parent.mkdir(parents=True, exist_ok=True)
    if not in_ci():
        Path(log).write_text("")
        try:
            return subprocess.call(command)
        except KeyboardInterrupt:
            return 130
    p = None
    try:
        with open(log, "w", encoding="utf-8", errors="replace") as out:
            p = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            for line in iter(p.stdout.readline, b""):
                sys.stdout.buffer.write(line)
                sys.stdout.buffer.flush()
                out.write(line.decode("utf-8", errors="replace"))
            return p.wait()
    except KeyboardInterrupt:
        if p:
            p.wait()
        return 130


def main(argv):
    if len(argv) > 3 and argv[0] == "tee" and argv[2] == "--":
        sys.exit(tee(argv[1], argv[3:]))
    if argv and argv[0] == "check":
        rest, runner = argv[1:], runner_name()
        if rest[:1] == ["--runner"]:
            runner, rest = rest[1], rest[2:]
        if rest:
            return check(runner, rest)
    sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
