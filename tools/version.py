#!/usr/bin/env python3
"""One version wherever a release reads it: `make version V=0.1.5`.

The workspace's version and fennec-wire's and fennec-tls's where the workspace
names them, the
Python package's, the npm packages' and their lock files, NuGet's FennecDb, and
the image the README pulls -- site/build.py holds the README to the
workspace, and packages.yml refuses a release whose packages say another
number. Cargo.lock follows at the next build."""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent


def sub(path, pattern, repl):
    """Replaces the one match of `pattern` in `path`, or stops: a file whose
    shape moved would otherwise keep its old number without a word."""
    p = ROOT / path
    text, n = re.subn(pattern, repl, p.read_text(encoding="utf-8"), count=1, flags=re.M)
    if n != 1:
        sys.exit(f"{path}: no match for {pattern!r}")
    p.write_text(text, encoding="utf-8")


def main():
    if len(sys.argv) != 2 or not re.fullmatch(r"\d+\.\d+\.\d+", sys.argv[1]):
        sys.exit("usage: version.py X.Y.Z")
    v = sys.argv[1]
    sub("Cargo.toml", r'^version = "[^"]+"', f'version = "{v}"')
    sub("Cargo.toml", r'^(fennec-wire = \{ version = )"[^"]+"', rf'\g<1>"{v}"')
    sub("Cargo.toml", r'^(fennec-tls = \{ version = )"[^"]+"', rf'\g<1>"{v}"')
    sub("integrations/python/pyproject.toml", r'^version = "[^"]+"', f'version = "{v}"')
    for path in ("web/package.json", "integrations/react/package.json", "integrations/cloudflare/package.json", "integrations/langchain/package.json",
                 "integrations/grafana/package.json"):
        sub(path, r'^(  "version": )"[^"]+"', rf'\g<1>"{v}"')
    # A lock file names the package's version twice, at its top and as the
    # root of `packages`; each package's `make ...-test` installs from it
    # with `npm ci`. The bump to 0.1.5 found React's still at 0.1.4, and the
    # one to 0.1.6 the two locks made since at 0.1.5.
    for name in ("react", "cloudflare", "langchain"):
        lock = f"integrations/{name}/package-lock.json"
        sub(lock, r'^(  "version": )"[^"]+"', rf'\g<1>"{v}"')
        sub(lock, rf'^(    "": \{{\n      "name": "@fennecdb/{name}",\n      "version": )"[^"]+"', rf'\g<1>"{v}"')
    # The Grafana data source's, published by no registry: mage writes the
    # number into the backend's build info and webpack into plugin.json,
    # which Grafana's catalog reads.
    lock = "integrations/grafana/package-lock.json"
    sub(lock, r'^(  "version": )"[^"]+"', rf'\g<1>"{v}"')
    sub(lock, r'^(    "": \{\n      "name": "fennecdb-datasource",\n      "version": )"[^"]+"', rf'\g<1>"{v}"')
    sub("README.md", r"(ghcr\.io/fennec-db/fennec-server:)\d+\.\d+\.\d+", rf"\g<1>{v}")
    # NuGet's FennecDb. The Go module has no number of its own to write: it
    # is the tag release.yml pushes beside the release's (RELEASING.md).
    sub("integrations/dotnet/FennecDb/FennecDb.csproj", r"(<Version>)[^<]+(</Version>)", rf"\g<1>{v}\g<2>")
    # The native bindings. Kotlin's library and AAR (Maven Central), and the
    # Flutter plugin's Android project; Dart's package and the Flutter
    # plugin, which names the package's version (pub.dev), and its pods.
    sub("integrations/kotlin/build.gradle.kts", r'^(    version = )"[^"]+"', rf'\g<1>"{v}"')
    sub("integrations/dart/fennecdb_flutter/android/build.gradle", r"^version '[^']+'", f"version '{v}'")
    for path in ("integrations/dart/fennecdb/pubspec.yaml", "integrations/dart/fennecdb_flutter/pubspec.yaml",
                 "integrations/dart/fennecdb_flutter/example/pubspec.yaml"):
        sub(path, r"^version: .+$", f"version: {v}")
    sub("integrations/dart/fennecdb_flutter/pubspec.yaml", r"^(  fennecdb: )\S+$", rf"\g<1>{v}")
    for os in ("ios", "macos"):
        sub(f"integrations/dart/fennecdb_flutter/{os}/fennecdb_flutter.podspec", r"^(  s\.version += )'[^']+'", rf"\g<1>'{v}'")
    # SwiftPM is fetched by the tag: Package.swift's binary target names the
    # release's zip, whose checksum swift-binary.yml writes beside it
    # (integrations/swift/set-binary.sh) before the tag is made.
    sub("Package.swift", r'^let release = "[^"]+"', f'let release = "{v}"')
    # The README the package's users read names the release it was written for.
    sub("integrations/swift/README.md", r'(\.package\(url: "https://github\.com/fennec-db/fennecdb", from: )"[^"]+"', rf'\g<1>"{v}"')
    changelogs(v)
    examples(v)
    print(f"version {v}: Cargo.toml, pyproject.toml, every package.json and lock, FennecDb.csproj, README.md, "
          "the Kotlin and Dart packages and their changelogs, the Flutter plugin, Package.swift, the examples")


def changelogs(v):
    """pub.dev shows a package's CHANGELOG.md and warns at the upload when it
    names no entry for the version: 0.1.8 to 0.1.11 went up with only
    0.1.7's. Each release's entry points at its notes, written once."""
    for package in ("fennecdb", "fennecdb_flutter"):
        p = ROOT / f"integrations/dart/{package}/CHANGELOG.md"
        text = p.read_text(encoding="utf-8")
        if re.search(rf"^## {re.escape(v)}$", text, flags=re.M):
            continue
        entry = f"## {v}\n\nThe release notes: https://github.com/fennec-db/fennecdb/releases/tag/v{v}\n\n"
        text, n = re.subn(r"^(# Changelog\n\n)", lambda m: m.group(1) + entry, text, count=1, flags=re.M)
        if n != 1:
            sys.exit(f"{p}: no '# Changelog' heading")
        p.write_text(text, encoding="utf-8")


def examples(v):
    """The Notes examples (examples/) take the published packages at the
    release's version; CI runs them on this checkout's build instead."""
    for name in ("web-local", "react", "node-server"):
        sub(f"examples/{name}/package.json", r'^(    "@fennecdb/web": )"[^"]+"', rf'\g<1>"^{v}"')
    sub("examples/react/package.json", r'^(    "@fennecdb/react": )"[^"]+"', rf'\g<1>"^{v}"')
    sub("examples/python/requirements.txt", r"^fennecdb==\S+$", f"fennecdb=={v}")
    # The Go module is fetched by the tag release.yml pushes beside the release's.
    # Its path is the repository's name, which v0.1.11 and earlier (published
    # as .../fennec/integrations/go) do not declare: the first release after
    # them is the first this line resolves at.
    sub("examples/go/go.mod", r"^(require github\.com/fennec-db/fennecdb/integrations/go )v\S+$", rf"\g<1>v{v}")
    sub("examples/dotnet/Notes.csproj", r'(<PackageReference Include="FennecDb" Version=")[^"]+(")', rf"\g<1>{v}\g<2>")
    # The crates are not on crates.io: Cargo takes them from the release's tag.
    for crate in ("fennec-core", "fennec-ql", "fennec-abi"):
        sub("examples/rust/Cargo.toml", rf'^({crate} = \{{ git = "[^"]+", tag = )"v[^"]+"', rf'\g<1>"v{v}"')
    sub("examples/swift/Package.swift", r'(\.package\(url: "https://github\.com/fennec-db/fennecdb", from: )"[^"]+"', rf'\g<1>"{v}"')
    sub("examples/kotlin-android/gradle.properties", r"^fennecVersion=.+$", f"fennecVersion={v}")
    sub("examples/flutter/pubspec.yaml", r"^version: .+$", f"version: {v}")
    for package in ("fennecdb", "fennecdb_flutter"):
        sub("examples/flutter/pubspec.yaml", rf"^(  {package}: )\S+$", rf"\g<1>^{v}")


if __name__ == "__main__":
    main()
