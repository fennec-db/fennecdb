#!/usr/bin/env python3
"""The flake ledger: ci.yml's failures on main and in the merge queue.

    tools/ci_flakes.py [-n 50] [--repo fennec-db/fennecdb] [--json]

Lists the failed jobs of the last N completed ci.yml runs (push to main and
merge_group, in the order they ran), names each one's failing step, and says
whether a later commit fixed it, so that the 1.0 plan's exit criterion --
"0 failures without a fixing commit in the last 50" -- is a number. Standard
library and `gh` (authenticated) only.

"Fixed by a commit" is decided from files, never from a rerun:
  * a merge_group failure is fixed when the same pull request (the `pr-N` of
    the queue branch) later passed ci.yml with at least one file's blob
    different from the failed run's: its author changed something. The same
    blobs passing later is a rerun that passed, which is a flake, whatever
    the cause (infrastructure included): the ledger counts it unfixed.
  * a push failure is fixed when a later push run passed the same job and a
    commit on main from the failed one to that run touched a file the failed
    commit touched.
  * a failure with no later run to judge by is "open", counted unfixed.
The count that matters is "unfixed". Per-failure heuristics are printed so a
person can overrule them; infrastructure failures (a step like "Set up job",
an upload, a runner lost) are marked.
"""
import argparse
import json
import re
import subprocess
import sys

INFRA = re.compile(r"set up job|upload|download|runner|cache|checkout|complete job|post ", re.I)


def gh(*args):
    r = subprocess.run(["gh", *args], capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"gh {' '.join(args)}: {r.stderr.strip()}")
    return json.loads(r.stdout) if r.stdout.strip() else None


def runs(repo, event, limit):
    return gh("run", "list", "-R", repo, "-w", "ci.yml", "--event", event,
              "-L", str(limit), "--json",
              "databaseId,conclusion,status,headSha,headBranch,createdAt,event,displayTitle") or []


_files = {}


def files_of(repo, sha):
    """{path: blob sha} of a commit's change, or None if the commit is gone."""
    if sha not in _files:
        try:
            out = gh("api", f"repos/{repo}/commits/{sha}", "--paginate", "-q", ".files")
            # --paginate with -q prints one array per page; gh joins them.
            _files[sha] = {f["filename"]: f.get("sha") for f in (out or [])}
        except (RuntimeError, TypeError, json.JSONDecodeError):
            _files[sha] = None
    return _files[sha]


def failed_jobs(repo, run_id):
    d = gh("run", "view", str(run_id), "-R", repo, "--json", "jobs")
    out = []
    for j in d["jobs"]:
        if j["conclusion"] != "failure":
            continue
        steps = [s["name"] for s in j["steps"] if s["conclusion"] == "failure"]
        out.append((j["name"], steps[0] if steps else "(no step: job-level failure)"))
    return out


def passed_job(repo, run_id, name, cache={}):
    if run_id not in cache:
        cache[run_id] = {j["name"]: j["conclusion"] for j in gh("run", "view", str(run_id), "-R", repo, "--json", "jobs")["jobs"]}
    return cache[run_id].get(name) == "success"


def pr_of(branch):
    m = re.search(r"/pr-(\d+)-", branch)
    return m.group(1) if m else None


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("-n", type=int, default=50, help="completed runs to read (default 50)")
    ap.add_argument("--repo", default="fennec-db/fennecdb")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()

    allruns = []
    for ev in ("push", "merge_group"):
        allruns += runs(a.repo, ev, a.n * 3)
    # push runs on branches other than main are not the ledger's.
    allruns = [r for r in allruns if r["status"] == "completed" and r["conclusion"] != "cancelled"
               and (r["event"] == "merge_group" or r["headBranch"] == "main")]
    allruns.sort(key=lambda r: r["createdAt"], reverse=True)
    window = allruns[:a.n]
    ordered = sorted(allruns, key=lambda r: r["createdAt"])  # oldest first, for "later"

    rows = []
    for r in window:
        if r["conclusion"] == "success":
            continue
        later = [x for x in ordered if x["createdAt"] > r["createdAt"]]
        mine = files_of(a.repo, r["headSha"])
        for job, step in failed_jobs(a.repo, r["databaseId"]):
            verdict, by = "open", ""
            if r["event"] == "merge_group":
                pr = pr_of(r["headBranch"])
                for x in later:
                    if x["event"] == "merge_group" and pr and pr_of(x["headBranch"]) == pr and passed_job(a.repo, x["databaseId"], job):
                        theirs = files_of(a.repo, x["headSha"])
                        if mine is None or theirs is None:
                            verdict, by = "unknown", "commit gone"
                        elif mine != theirs:
                            verdict, by = "fixed", f"pr-{pr} changed (run {x['databaseId']})"
                        else:
                            verdict, by = "unfixed", f"pr-{pr} passed unchanged (run {x['databaseId']})"
                        break
            else:
                touched = set(mine or {})
                for x in later:
                    if x["event"] != "push":
                        continue
                    ok = passed_job(a.repo, x["databaseId"], job)
                    theirs = set(files_of(a.repo, x["headSha"]) or {})
                    if touched & theirs:
                        if ok:
                            verdict, by = "fixed", f"{x['headSha'][:8]} touched {sorted(touched & theirs)[0]}"
                            break
                    elif ok:
                        verdict, by = "unfixed", f"passed at {x['headSha'][:8]} with no related change"
                        break
            rows.append({"run": r["databaseId"], "event": r["event"], "title": r["displayTitle"][:60],
                         "sha": r["headSha"][:8], "job": job, "step": step,
                         "infra": bool(INFRA.search(step)), "verdict": verdict, "by": by})

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
