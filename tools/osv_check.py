#!/usr/bin/env python3
"""Ask OSV (api.osv.dev) about the runtime dependencies of the Kotlin and Dart
packages packages.yml publishes; any known advisory fails the run.

Neither package keeps a lockfile (pubspec.lock is ignored: a library's
consumers resolve their own), so the check reads what the manifests declare:
Maven coordinates are pinned in the Gradle files, and a Dart constraint is
resolved to the newest version it admits, as a consumer's `pub get` would.
The npm packages, Go and NuGet have their own audit tools (ci.yml's `audit`).

    python3 tools/osv_check.py [--self-test]
"""
import json
import re
import sys
import urllib.request

GRADLE = ["integrations/kotlin/fennecdb/build.gradle.kts", "integrations/kotlin/android/build.gradle.kts"]
PUBSPECS = ["integrations/dart/fennecdb/pubspec.yaml", "integrations/dart/fennecdb_flutter/pubspec.yaml"]
# What ships: api/implementation, never test or dev dependencies.
MAVEN = re.compile(r'^\s*(?:api|implementation)\(\s*"([^":]+):([^":]+):([^"]+)"\s*\)', re.M)


def maven_deps(text):
    return [(f"{g}:{a}", v) for g, a, v in MAVEN.findall(text)]


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


def post(url, body):
    req = urllib.request.Request(url, json.dumps(body).encode(), {"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


def resolve_pub(name, constraint):
    with urllib.request.urlopen(f"https://pub.dev/api/packages/{name}", timeout=30) as r:
        versions = [v["version"] for v in json.load(r)["versions"] if "-" not in v["version"]]
    key = lambda v: tuple(int(p) for p in v.split("+")[0].split("."))
    m = re.match(r"^\^(\d+)\.", constraint)
    if m:
        versions = [v for v in versions if key(v)[0] == int(m.group(1))]
    elif re.match(r"^\d", constraint):
        versions = [constraint]
    return max(versions, key=key)


def vulns(ecosystem, name, version):
    answer = post("https://api.osv.dev/v1/query", {"package": {"ecosystem": ecosystem, "name": name}, "version": version})
    return [v["id"] for v in answer.get("vulns", [])]


def main():
    if "--self-test" in sys.argv:
        assert maven_deps('api("a.b:c:1.8.1")\n    testImplementation("x:y:2")') == [("a.b:c", "1.8.1")]
        assert pub_runtime_deps("dependencies:\n  ffi: ^2.1.0\n  flutter:\n    sdk: flutter\ndev_dependencies:\n  test: ^1\n") == [("ffi", "^2.1.0")]
        print("ok")
        return 0
    checked, bad = [], []
    for path in GRADLE:
        for name, version in maven_deps(open(path).read()):
            checked.append(("Maven", name, version))
    for path in PUBSPECS:
        for name, constraint in pub_runtime_deps(open(path).read()):
            if name != "fennecdb":  # the sibling package, in this repository
                checked.append(("Pub", name, resolve_pub(name, constraint)))
    if not checked:
        print("no dependency found: the manifests' layout changed")
        return 1
    for eco, name, version in sorted(set(checked)):
        found = vulns(eco, name, version)
        print(f"{eco} {name} {version}: {', '.join(found) or 'no known advisory'}")
        if found:
            bad.append(name)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
