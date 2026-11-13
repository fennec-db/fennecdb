#!/usr/bin/env python3
"""Ask OSV (api.osv.dev) about the runtime dependencies of the Kotlin and Dart
packages packages.yml publishes; any known advisory fails the run. It also
holds the audit's ignore list (tools/audit-ignore.json) and reads the other
audit tools' output against it, so one list covers every ecosystem.

Neither package keeps a lockfile (pubspec.lock is ignored: a library's
consumers resolve their own), so the check reads what the manifests declare:
Maven coordinates are pinned in the Gradle files, and a Dart constraint is
resolved to the newest version it admits, as a consumer's `pub get` would.

    python3 tools/osv_check.py [--self-test]       # Kotlin and Dart, by OSV
    python3 tools/osv_check.py --ignores           # the list alone: an entry past its expiry fails
    python3 tools/osv_check.py --npm-audit FILE    # `npm audit --json` output
    python3 tools/osv_check.py --govulncheck FILE  # `govulncheck -format json` output
    python3 tools/osv_check.py --nuget FILE        # `dotnet list package --vulnerable` text
"""
import datetime
import json
import os
import re
import sys
import time
import urllib.request

GRADLE = ["integrations/kotlin/fennecdb/build.gradle.kts", "integrations/kotlin/android/build.gradle.kts"]
PUBSPECS = ["integrations/dart/fennecdb/pubspec.yaml", "integrations/dart/fennecdb_flutter/pubspec.yaml"]
IGNORES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "audit-ignore.json")
# Every configuration whose dependency may reach a consumer or its build; a
# line of one that is not read into a coordinate fails rather than goes
# unaudited.
DEP_LINE = re.compile(r"^\s*(?:api|implementation|compileOnly|runtimeOnly)\(", re.M)
MAVEN = re.compile(r'^\s*(?:api|implementation|compileOnly|runtimeOnly)\(\s*"([^":]+):([^":]+):([^"]+)"\s*\)', re.M)
ADVISORY = re.compile(r"\b(GHSA-[0-9a-z]{4}-[0-9a-z]{4}-[0-9a-z]{4}|CVE-\d{4}-\d{4,}|GO-\d{4}-\d{4,}|OSV-[\w-]+)\b", re.I)


class AuditError(Exception):
    pass


def maven_deps(text):
    """The coordinates of the dependency lines; a line that is not one, or
    whose version is a variable, raises: it would otherwise go unaudited."""
    found = MAVEN.findall(text)
    lines = len(DEP_LINE.findall(text))
    if lines != len(found):
        raise AuditError(f"{lines - len(found)} Gradle dependency line(s) not read into a coordinate")
    for g, a, v in found:
        if v.startswith("$"):
            raise AuditError(f"{g}:{a} has a version that is a variable ({v}): audit cannot resolve it")
    return [(f"{g}:{a}", v) for g, a, v in found]


def pub_runtime_deps(text):
    """`name: ^1.2.3` lines of the `dependencies:` block, sdk entries left out."""
    out, inside = [], False
    for line in text.splitlines():
        if re.match(r"^\S", line):
            inside = line.startswith("dependencies:")
            continue
        m = re.match(r"^  ([a-z_0-9]+):\s*([\^>=<0-9][^#\s]*)\s*(?:#.*)?$", line)
        if inside and m:
            out.append((m.group(1), m.group(2)))
    return out


def _retry(call, tries=3, pause=2):
    for i in range(tries):
        try:
            return call()
        except OSError:  # URLError, HTTPError and timeouts are all OSErrors
            if i == tries - 1:
                raise
            time.sleep(pause * (i + 1))


def post(url, body):
    req = urllib.request.Request(url, json.dumps(body).encode(), {"Content-Type": "application/json"})

    def go():
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.load(r)
    return _retry(go)


def get(url):
    def go():
        with urllib.request.urlopen(url, timeout=30) as r:
            return json.load(r)
    return _retry(go)


def vkey(v):
    return tuple(int(p) for p in v.split("+")[0].split("."))


def caret_bounds(constraint):
    """pub's `^`: >=v <next, where next bumps the first non-zero part of
    major.minor.patch (^0.3.1 is <0.4.0, ^0.0.3 is <0.0.4, ^1.2 is <2.0.0)."""
    low = tuple((vkey(constraint[1:]) + (0, 0, 0))[:3])
    i = next((i for i, p in enumerate(low) if p), 2)
    return low, low[:i] + (low[i] + 1,) + (0,) * (2 - i)


def pick_pub(versions, constraint):
    """The newest stable version the constraint admits."""
    if constraint.startswith("^"):
        low, high = caret_bounds(constraint)
        versions = [v for v in versions if low <= (vkey(v) + (0, 0, 0))[:3] < high]
    elif re.match(r"^\d", constraint):
        versions = [constraint]
    if not versions:
        raise AuditError(f"no version admitted by {constraint}")
    return max(versions, key=vkey)


def resolve_pub(name, constraint):
    data = get(f"https://pub.dev/api/packages/{name}")
    return pick_pub([v["version"] for v in data["versions"] if "-" not in v["version"]], constraint)


def vulns(ecosystem, name, version):
    answer = post("https://api.osv.dev/v1/query", {"package": {"ecosystem": ecosystem, "name": name}, "version": version})
    return [(v["id"], v.get("aliases", [])) for v in answer.get("vulns", [])]


def load_ignores(path=IGNORES, today=None):
    """{advisory id: entry}; an entry past its expiry, or one missing a field,
    fails: an ignore is a decision with a date, not a permanent silence."""
    today = today or datetime.date.today()
    with open(path) as f:
        entries = json.load(f).get("ignore", [])
    out = {}
    for e in entries:
        for k in ("id", "package", "reason", "expires"):
            if not e.get(k):
                raise AuditError(f"audit-ignore.json entry {e!r} has no {k}")
        if datetime.date.fromisoformat(e["expires"]) < today:
            raise AuditError(f"audit-ignore.json: {e['id']} ({e['package']}) expired {e['expires']}: fix it or renew with a reason")
        out[e["id"].upper()] = e
    return out


def ignored(ids, ignores):
    return any(i.upper() in ignores for i in ids)


def npm_findings(report, ignores):
    """High and critical advisories of an `npm audit --json` report that the
    list does not cover. Transitive entries (string `via`) are the advisories
    of another entry, so only the objects are read."""
    bad = []
    for name, v in report.get("vulnerabilities", {}).items():
        for via in v.get("via", []):
            if isinstance(via, dict) and via.get("severity") in ("high", "critical"):
                ids = ADVISORY.findall(via.get("url", "")) + [str(via.get("source", ""))]
                if not ignored(ids, ignores):
                    bad.append(f"{name}: {via.get('title')} ({via.get('url')})")
    return bad


def govuln_findings(stream, ignores):
    """Called vulnerabilities (a finding with a function in its trace) of
    `govulncheck -format json`, a stream of objects, not covered by the list."""
    dec, i, aliases, bad = json.JSONDecoder(), 0, {}, []
    stream = stream.strip()
    while i < len(stream):
        obj, j = dec.raw_decode(stream, i)
        i = j
        while i < len(stream) and stream[i].isspace():
            i += 1
        if "osv" in obj:
            aliases[obj["osv"]["id"]] = [obj["osv"]["id"]] + obj["osv"].get("aliases", [])
        f = obj.get("finding")
        if f and f.get("trace") and f["trace"][0].get("function"):
            ids = aliases.get(f["osv"], [f["osv"]])
            if not ignored(ids, ignores):
                bad.append(f"{f['osv']} in {f['trace'][0].get('module')}")
    return sorted(set(bad))


def nuget_findings(text, ignores, projects=1):
    """Vulnerable packages of `dotnet list package --vulnerable` not covered by
    the list, and any sign the advisory data was not fetched (NU1900: the
    SDK's warning, exit 0 and a clean-looking text otherwise)."""
    bad = []
    if "NU1900" in text or "Error occurred while getting package vulnerability data" in text:
        bad.append("vulnerability data could not be fetched (NU1900)")
    if text.count("has no vulnerable packages") < projects and "has the following vulnerable packages" not in text:
        bad.append("no project reported 'has no vulnerable packages'")
    if "has the following vulnerable packages" in text:
        for line in text.splitlines():
            if line.strip().startswith(">"):
                ids = ADVISORY.findall(line)
                if not ignored(ids, ignores):
                    bad.append(line.strip())
    return bad


def report(bad, label):
    for b in bad:
        print(f"{label}: {b}")
    return 1 if bad else 0


def self_test():
    ok = 'api("a.b:c:1.8.1")\n    testImplementation("x:y:2")'
    assert maven_deps(ok) == [("a.b:c", "1.8.1")]
    assert maven_deps('runtimeOnly("a:b:1")\ncompileOnly("c:d:2")') == [("a:b", "1"), ("c:d", "2")]
    for bad in ('implementation(project(":x"))', 'api("a:b")', "implementation(libs.foo)", 'api("a:b:$v")', 'api("a:b:${v}")'):
        try:
            maven_deps(bad)
        except AuditError:
            continue
        raise AssertionError(bad)
    assert pub_runtime_deps("dependencies:\n  ffi: ^2.1.0\n  flutter:\n    sdk: flutter\ndev_dependencies:\n  test: ^1\n") == [("ffi", "^2.1.0")]
    vs = ["0.2.9", "0.3.0", "0.3.7", "0.4.0", "1.0.0"]
    assert pick_pub(vs, "^0.3.1") == "0.3.7" and pick_pub(vs, "^0.3") == "0.3.7"
    assert pick_pub(["0.0.3", "0.0.4"], "^0.0.3") == "0.0.3" and pick_pub(vs + ["1.2.0", "2.0.0"], "^1.0.0") == "1.2.0"
    today = datetime.date(2026, 1, 1)
    assert load_ignores(IGNORES, today) is not None
    print("ok")
    return 0


def main():
    args = sys.argv[1:]
    if "--self-test" in args:
        return self_test()
    try:
        ignores = load_ignores()
        if "--ignores" in args:
            print(f"{len(ignores)} ignore entries, none expired")
            return 0
        for flag, fn, label in (("--npm-audit", npm_findings, "npm"), ("--nuget", nuget_findings, "nuget")):
            if flag in args:
                text = open(args[args.index(flag) + 1]).read()
                return report(fn(json.loads(text) if flag == "--npm-audit" else text, ignores), label)
        if "--govulncheck" in args:
            return report(govuln_findings(open(args[args.index("--govulncheck") + 1]).read(), ignores), "govulncheck")
        checked, bad = [], []
        for path in GRADLE:
            for name, version in maven_deps(open(path).read()):
                checked.append(("Maven", name, version))
        for path in PUBSPECS:
            for name, constraint in pub_runtime_deps(open(path).read()):
                if name != "fennecdb":  # the sibling package, in this repository
                    checked.append(("Pub", name, resolve_pub(name, constraint)))
    except AuditError as e:
        print(f"audit: {e}")
        return 1
    if not checked:
        print("no dependency found: the manifests' layout changed")
        return 1
    for eco, name, version in sorted(set(checked)):
        found = vulns(eco, name, version)
        shown = [i for i, al in found if not ignored([i] + al, ignores)]
        print(f"{eco} {name} {version}: {', '.join(shown) or 'no known advisory'}" + (" (ignored: %d)" % (len(found) - len(shown)) if len(found) != len(shown) else ""))
        if shown:
            bad.append(name)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
