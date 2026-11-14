#!/usr/bin/env python3
"""The flake ledger: ci.yml's failures on main and in the merge queue.

    tools/ci_flakes.py [-n 50] [--repo fennec-db/fennecdb] [--json]

Lists the failed jobs of the last N completed ci.yml runs (push to main and
merge_group, in the order they ran), names each one's failing step, and says
whether a later commit fixed it, so that the 1.0 plan's exit criterion --
"0 failures without a fixing commit in the last 50" -- is a number. Standard
library and `gh` (authenticated) only.

"Fixed by a commit" is decided from files, never from a rerun. A file counts
only if it is source: not documentation (*.md, docs/, site/content/), not a
lockfile, and one the failing job's area owns (AREAS below: `fmt` and
`clippy` the crates, `mobile` the bindings, ...), or .github/workflows/ci.yml:
  * a merge_group failure is fixed when the same pull request (the `pr-N` of
    the queue branch) later passed ci.yml with such a file changed: added or
    dropped from the pull request's change, or, when both runs were queued on
    the same base, a different blob. Another base is queue drift, not the
    author's work: the same files over a moved base count unfixed. The same
    blobs passing later is a rerun that passed, which is a flake, whatever
    the cause (infrastructure included).
  * a push failure is fixed when, up to the first later push run that passed
    the job, a commit on main touched such a file.
  * a failure with no later run to judge by is "open", counted unfixed.
Every job conclusion but success, skipped and cancelled is a failure
(timed_out and startup_failure included). The count that matters is
"unfixed". Per-failure heuristics are printed so a person can overrule them;
infrastructure failures (a step named for the runner's own work: "Set up
job", an action's own `Run actions/...` step, "Post ...") are marked.
"""
import argparse
import json
import re
import subprocess
import sys

NOT_FAILURES = ("success", "skipped", "cancelled", "neutral", None, "")

# The runner's and the actions' own steps, never a step of ours: a test step
# named "Both sync runners ran every scenario" is not infrastructure.
INFRA = re.compile(
    r"^(set up job|complete job|initialize containers|stop containers|post .+"
    r"|run actions/[\w.-]+@\S+|\(no step.*)$", re.I)

DOCS = re.compile(r"(\.md$|^docs/|^site/content/|^LICENSE|^README)")
LOCKFILES = re.compile(r"(^|/)(Cargo\.lock|package-lock\.json|yarn\.lock|pnpm-lock\.yaml|go\.sum|Package\.resolved|pubspec\.lock|Gemfile\.lock|poetry\.lock|uv\.lock)$")
CI_FILE = ".github/workflows/ci.yml"
RUST = ("crates/", "Cargo.toml", "rust-toolchain.toml", "Makefile")

# job name prefix -> the paths whose change can fix it. The first match wins;
# the last entry is every other job.
AREAS = [
    (r"^(fmt|clippy|test|build|windows)\b", RUST + ("web/", "tools/")),
    (r"^integrations", RUST + ("web/", "integrations/", "tools/")),
    (r"^examples", RUST + ("web/", "integrations/", "examples/", "tools/")),
    (r"^monitoring", ("integrations/grafana/", "deploy/", "site/highlight.js", "studio/", "tools/")),
    (r"^deploy", RUST + ("deploy/", "tools/")),
    (r"^packaging", RUST + ("tools/", "site/install", "packaging/", "Dockerfile", "deploy/")),
    (r"^mobile", ("crates/fennec-ffi/", "crates/fennec-abi/", "crates/fennec-core/", "integrations/", "tools/", "Makefile")),
    (r"", RUST + ("web/", "integrations/", "tools/", "deploy/", "site/")),
]


def owns(job, path):
    """Whether a change to `path` can fix `job`: source, not docs or a lockfile."""
    if path == CI_FILE:
        return True
    if DOCS.search(path) or LOCKFILES.search(path):
        return False
    for pat, prefixes in AREAS:
        if re.search(pat, job, re.I):
            return path.startswith(prefixes)
    return False


def gh(*args):
    r = subprocess.run(["gh", *args], capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"gh {' '.join(args)}: {r.stderr.strip()}")
    return json.loads(r.stdout) if r.stdout.strip() else None


def gh_lines(*args):
    """gh's output as JSON lines (`-q .files[]` over `--paginate` prints one per element)."""
    r = subprocess.run(["gh", *args], capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"gh {' '.join(args)}: {r.stderr.strip()}")
    return [json.loads(line) for line in r.stdout.splitlines() if line.strip()]


def runs(repo, event, limit):
    return gh("run", "list", "-R", repo, "-w", "ci.yml", "--event", event,
              "-L", str(limit), "--json",
              "databaseId,conclusion,status,headSha,headBranch,createdAt,event,displayTitle") or []


_commits = {}


def commit_of(repo, sha):
    """(first parent, {path: blob sha}) of a commit, or None if it is gone.

    `--paginate` over `.files[]` prints one JSON object a line however many
    pages there are (a `-q .files` prints one array a page, which no single
    json.loads reads past the API's 300 files)."""
    if sha not in _commits:
        try:
            parents = gh_lines("api", f"repos/{repo}/commits/{sha}", "--paginate", "-q", ".parents[0]")
            files = gh_lines("api", f"repos/{repo}/commits/{sha}", "--paginate", "-q", ".files[]")
            _commits[sha] = (parents[0]["sha"] if parents else None,
                             {f["filename"]: f.get("sha") for f in files})
        except (RuntimeError, TypeError, KeyError, json.JSONDecodeError):
            _commits[sha] = None
    return _commits[sha]


def jobs_of(repo, run_id, cache={}):
    key = (repo, run_id)
    if key not in cache:
        cache[key] = gh("run", "view", str(run_id), "-R", repo, "--json", "jobs")["jobs"]
    return cache[key]


def failed_jobs(repo, run_id):
    out = []
    for j in jobs_of(repo, run_id):
        if j["conclusion"] in NOT_FAILURES:
            continue
        steps = [s["name"] for s in j["steps"] if s["conclusion"] not in NOT_FAILURES]
        out.append((j["name"], steps[0] if steps else f"(no step: job-level {j['conclusion']})"))
    return out


def passed_job(repo, run_id, name):
    return {j["name"]: j["conclusion"] for j in jobs_of(repo, run_id)}.get(name) == "success"


def pr_of(branch):
    m = re.search(r"/pr-(\d+)-", branch)
    return m.group(1) if m else None


def merge_group_verdict(repo, r, later, job):
    pr = pr_of(r["headBranch"])
    for x in later:
        if x["event"] == "merge_group" and pr and pr_of(x["headBranch"]) == pr and passed_job(repo, x["databaseId"], job):
            mine, theirs = commit_of(repo, r["headSha"]), commit_of(repo, x["headSha"])
            if mine is None or theirs is None:
                return "unknown", "commit gone"
            (pa, fa), (pb, fb) = mine, theirs
            changed = {p for p in set(fa) ^ set(fb)}
            if pa is not None and pa == pb:
                changed |= {p for p in set(fa) & set(fb) if fa[p] != fb[p]}
            owned = sorted(p for p in changed if owns(job, p))
            if owned:
                return "fixed", f"pr-{pr} changed {owned[0]} (run {x['databaseId']})"
            why = "passed unchanged" if not changed else "passed over a moved base or with docs/lockfiles changed only"
            return "unfixed", f"pr-{pr} {why} (run {x['databaseId']})"
    return "open", ""


def push_verdict(repo, r, later, job):
    owned = []
    for x in later:
        if x["event"] != "push":
            continue
        c = commit_of(repo, x["headSha"])
        owned += [p for p in (c[1] if c else {}) if owns(job, p)]
        if passed_job(repo, x["databaseId"], job):
            if owned:
                return "fixed", f"{x['headSha'][:8]} touched {sorted(owned)[0]}"
            return "unfixed", f"passed at {x['headSha'][:8]} with no change the job's area owns"
    return "open", ""


def ledger(repo, n):
    allruns = []
    for ev in ("push", "merge_group"):
        allruns += runs(repo, ev, n * 3)
    # push runs on branches other than main are not the ledger's.
    allruns = [r for r in allruns if r["status"] == "completed" and r["conclusion"] != "cancelled"
               and (r["event"] == "merge_group" or r["headBranch"] == "main")]
    allruns.sort(key=lambda r: r["createdAt"], reverse=True)
    window = allruns[:n]
    ordered = sorted(allruns, key=lambda r: r["createdAt"])  # oldest first, for "later"

    rows = []
    for r in window:
        if r["conclusion"] == "success":
            continue
        later = [x for x in ordered if x["createdAt"] > r["createdAt"]]
        for job, step in failed_jobs(repo, r["databaseId"]):
            if r["event"] == "merge_group":
                verdict, by = merge_group_verdict(repo, r, later, job)
            else:
                verdict, by = push_verdict(repo, r, later, job)
            rows.append({"run": r["databaseId"], "event": r["event"], "title": r["displayTitle"][:60],
                         "sha": r["headSha"][:8], "job": job, "step": step,
                         "infra": bool(INFRA.match(step)), "verdict": verdict, "by": by})
    return window, rows


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("-n", type=int, default=50, help="completed runs to read (default 50)")
    ap.add_argument("--repo", default="fennec-db/fennecdb")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()
    window, rows = ledger(a.repo, a.n)

    if a.json:
        json.dump(rows, sys.stdout, indent=1)
        print()
        return 0
    print(f"{len(window)} completed ci.yml runs (push to main, merge_group), "
          f"{window[-1]['createdAt'][:10]} to {window[0]['createdAt'][:10]}" if window else "no runs")
    for x in rows:
        print(f"- {x['event']:11} run {x['run']} {x['sha']} {x['job']!r} step {x['step']!r}"
              f"{' [infra]' if x['infra'] else ''}: {x['verdict']} {x['by']}  ({x['title']})")
    unfixed = [x for x in rows if x["verdict"] in ("unfixed", "open", "unknown")]
    print(f"failed runs {len({x['run'] for x in rows})}, failed jobs {len(rows)}, "
          f"fixed by a commit {sum(x['verdict'] == 'fixed' for x in rows)}, "
          f"without a fixing commit {len(unfixed)} (infra steps among them {sum(x['infra'] for x in unfixed)})")
    return 1 if unfixed else 0


if __name__ == "__main__":
    sys.exit(main())
