#!/usr/bin/env python3
"""One version wherever a release reads it: `make version V=0.1.5`.

The workspace's version and every crate's where the workspace names it (a
`version = "0.1.3"` is `^0.1.3`, which 0.2.0 does not meet), the
Python package's, the npm packages' and their lock files, NuGet's FennecDb, and
the image the README pulls -- site/build.py holds the README to the
workspace, and packages.yml refuses a release whose packages say another
number. Cargo.lock follows at the next build."""

import hashlib
import pathlib
import re
import subprocess
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


def tagged(root, version):
    """Whether `version` was released: its tag is in this repository."""
    r = subprocess.run(["git", "-C", str(root), "rev-parse", "-q", "--verify", f"refs/tags/v{version}"],
                       capture_output=True, text=True)
    return r.returncode == 0


def released_tags(root):
    """Every release this clone has the tag of from v0.2.0 on, oldest
    first, by its version: `vX.Y.Z` alone -- a release candidate's
    `v1.0.0-rc.1` writes no set, its release does."""
    out = subprocess.run(["git", "-C", str(root), "tag", "-l", "v*"], capture_output=True, text=True).stdout
    versions = []
    for tag in out.split():
        m = re.fullmatch(r"v(\d+)\.(\d+)\.(\d+)", tag)
        if m and tuple(map(int, m.groups())) >= (0, 2, 0):
            versions.append(tuple(map(int, m.groups())))
    return [".".join(map(str, v)) for v in sorted(versions)]


def set_problems(set_dir, pin=None):
    """What keeps `set_dir` from being the set its release wrote: a file
    missing, changed or not listed in its SHA256SUMS, or no SHA256SUMS --
    which `tools/compat_fixtures.py write` writes last, every file by its
    sum -- or no file a release wrote beside them: a README and its sums
    alone are no set; and, given `pin` (its `RELEASES` line's `sums`), a
    SHA256SUMS of other bytes: a set written again whole, its sums with it.
    Empty for a whole set."""
    sums = set_dir / "SHA256SUMS"
    if not sums.is_file():
        return [f"{sums}: missing (tools/compat_fixtures.py write writes it last)"]
    stems = {p.stem for p in set_dir.glob("*.fennec")}
    if not any((set_dir / f"{s}.answers").is_file() for s in stems):
        return [f"{set_dir}: no database file beside the release's answers to it (<name>.fennec and "
                "<name>.answers): not a set its release wrote"]
    listed = {}
    for line in sums.read_text(encoding="utf-8").splitlines():
        digest, _, name = line.partition("  ")
        if not re.fullmatch(r"[0-9a-f]{64}", digest) or not name:
            return [f"{sums}: a line not `<sha256>  <path>`: {line!r}"]
        listed[name] = digest
    problems = []
    held = {p.relative_to(set_dir).as_posix() for p in set_dir.rglob("*") if p.is_file()} - {"SHA256SUMS"}
    for name in sorted(held - listed.keys()):
        problems.append(f"{set_dir / name}: in no line of SHA256SUMS")
    for name, digest in sorted(listed.items()):
        p = set_dir / name
        if not p.is_file():
            problems.append(f"{p}: missing")
        elif hashlib.sha256(p.read_bytes()).hexdigest() != digest:
            problems.append(f"{p}: not the bytes its release wrote")
    if pin is not None and hashlib.sha256(sums.read_bytes()).hexdigest() != pin:
        problems.append(f"{sums}: not the one its RELEASES line pins (sums: {pin}): a landed set is never "
                        "written again -- a form found missing goes into a further set of its tag, `<tag>+2/` "
                        "(docs/design/compatibility.md)")
    return problems


def missing_set(root, current, new, released=tagged):
    """Why the release after `current` cannot be cut yet, or None: from
    v0.2.0 on, a released version's fixture set -- the files its own
    binaries wrote, which every later build's tests open
    (tools/compat_fixtures.py) -- is in the working tree, every file the
    bytes SHA256SUMS names, before the version moves past it. Setting the
    version a release already has asks for nothing. A version whose tag
    this clone lacks is refused too, never taken for one never released:
    `make version` is a release step, run in a full clone, and a clone
    without the tags would let every release through."""
    m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", current)
    if current == new or not m or tuple(map(int, m.groups())) < (0, 2, 0):
        return None
    if not released(root, current):
        return (f"v{current}, the version this tree holds, has no tag in this clone (refs/tags/v{current}): "
                "fetch the tags (`git fetch --tags`) and run it again -- `make version` is a release step, run "
                "in a full clone, and a released version's fixture set is asked for by its tag")
    set_dir = root / "crates" / "fennec-core" / "tests" / "compat" / f"v{current}"
    if not set_dir.is_dir():
        return (f"v{current} has no fixture set (crates/fennec-core/tests/compat/v{current}/): write it from "
                f"its tag with `make compat-fixtures V={current}` and land it before the release after it "
                "(RELEASING.md, step 7)")
    problems = set_problems(set_dir)
    if problems:
        return f"v{current}'s fixture set is not the one its release wrote:\n  " + "\n  ".join(problems)
    pin = pins(root).get(f"v{current}") if (root / WALK).is_file() else None
    if pin is None:
        return (f"v{current}'s fixture set has no line in {WALK}'s RELEASES pinning its SHA256SUMS (`sums`): "
                "land the set with its line (RELEASING.md, step 7)")
    problems = set_problems(set_dir, pin)
    if problems:
        return f"v{current}'s fixture set is not the one its line pins:\n  " + "\n  ".join(problems)
    return None


WALK = pathlib.Path("crates") / "fennec-core" / "tests" / "compat" / "walk.rs"
SET_NAME = r"v\d+\.\d+\.\d+(?:\+(?:[2-9]|[1-9]\d+))?"


def releases(root):
    """Each line of `RELEASES` in the compat tests' walk: the set it names
    -- a release's tag, or `<tag>+<n>` for a further set of it -- the tag,
    and the SHA-256 its `sums` pins of the set's SHA256SUMS (None where the
    line pins none)."""
    text = (root / WALK).read_text(encoding="utf-8")
    out = []
    starts = [m for m in re.finditer(rf'^\s*set: "({SET_NAME})",', text, flags=re.M)]
    for i, m in enumerate(starts):
        body = text[m.end():starts[i + 1].start() if i + 1 < len(starts) else len(text)]
        tag = re.search(r'^\s*tag: "([^"]+)",', body, flags=re.M)
        sums = re.search(r'^\s*sums: "([0-9a-f]{64})",', body, flags=re.M)
        out.append((m.group(1), tag.group(1) if tag else None, sums.group(1) if sums else None))
    return out


def named_sets(root):
    """The sets the tree holds, by `RELEASES`: each line a set its tests
    open."""
    return [s for s, _, _ in releases(root)]


def pins(root):
    """Each named set's pinned digest of its SHA256SUMS, by its name."""
    return {s: pin for s, _, pin in releases(root)}


def named_sets_problems(root):
    """What keeps the tree from holding every set it names: v0.2.0's --
    1.x opens every file written from v0.2.0 on -- and each `RELEASES`
    line's, whole as its release wrote it, its SHA256SUMS the one the line
    pins, a further set's tag its first set's; and no set in the tree that
    no line names. Empty where each is."""
    lines = releases(root)
    named = [s for s, _, _ in lines]
    problems = []
    if "v0.2.0" not in named:
        problems.append(f"{WALK}: RELEASES has no v0.2.0 line, and 1.x opens every file written from v0.2.0 on")
    sets_dir = root / "crates" / "fennec-core" / "tests" / "compat"
    for name, tag, pin in lines:
        set_dir = sets_dir / name
        if tag != name.split("+")[0]:
            problems.append(f"{WALK}: {name}'s line names the tag {tag}")
        if tag not in named:
            problems.append(f"{WALK}: {name} is a further set of {tag}, whose first set has no line")
        if pin is None:
            problems.append(f"{WALK}: {name}'s line pins no SHA256SUMS (`sums`)")
        if not set_dir.is_dir():
            problems.append(f"{set_dir}: missing, and RELEASES names it")
        else:
            problems.extend(set_problems(set_dir, pin))
    if "v0.2.0" not in named and not (sets_dir / "v0.2.0").is_dir():
        problems.append(f"{sets_dir / 'v0.2.0'}: missing")
    if sets_dir.is_dir():
        for d in sorted(p for p in sets_dir.iterdir() if p.is_dir()):
            if re.fullmatch(SET_NAME, d.name) and d.name not in named:
                problems.append(f"{d}: a set no line of RELEASES names")
    return problems


def sets_of_tags_problems(root):
    """What keeps the tree from holding the set of every release this clone
    has the tag of, from v0.2.0 on ([`released_tags`]), whole and pinned by
    its `RELEASES` line: a later release's set deleted with its line passes
    every compat test, and is refused here."""
    problems = []
    pinned = pins(root) if (root / WALK).is_file() else {}
    for version in released_tags(root):
        set_dir = root / "crates" / "fennec-core" / "tests" / "compat" / f"v{version}"
        if not set_dir.is_dir():
            problems.append(f"v{version} was released (its tag) and has no fixture set ({set_dir}): write it "
                            f"from its tag with `make compat-fixtures V={version}` (RELEASING.md, step 7)")
        elif pinned.get(f"v{version}") is None:
            problems.append(f"v{version}'s fixture set has no line in {WALK}'s RELEASES pinning its SHA256SUMS")
        else:
            problems.extend(set_problems(set_dir, pinned[f"v{version}"]))
    return problems


def refuse_without_set(root, new):
    """Stops the move to `new` while the release it leaves has no whole
    fixture set ([`missing_set`]), any release tagged before it has none
    ([`sets_of_tags_problems`]), or a set the tree names is not the one its
    line pins ([`named_sets_problems`])."""
    why = missing_set(root, current_version(root), new)
    if why:
        sys.exit(why)
    if current_version(root) == new:
        return
    problems = sets_of_tags_problems(root) + named_sets_problems(root)
    if problems:
        sys.exit("a released version's fixture set is missing, not whole or not the one pinned:\n  "
                 + "\n  ".join(dict.fromkeys(problems)))


def current_version(root):
    m = re.search(r'^version = "([^"]+)"', (root / "Cargo.toml").read_text(encoding="utf-8"), flags=re.M)
    if not m:
        sys.exit("Cargo.toml: no workspace version")
    return m.group(1)
def guard(v, root=ROOT):
    """A 1.0.0 or later release needs the API baselines frozen
    (integrations/api-golden/frozen.json): until then a break is named for
    "1.0" and the baselines are only today's dumps. The checker says which
    command freezes them (docs/design/conventions.md#api-baselines)."""
    if int(v.split(".")[0]) < 1:
        return
    r = subprocess.run([sys.executable, str(root / "tools" / "api_compat.py"), "--root", str(root), "--require-frozen"])
    if r.returncode != 0:
        sys.exit(f"version {v} refused: freeze the API baselines first")


def main():
    if len(sys.argv) != 2 or not re.fullmatch(r"\d+\.\d+\.\d+", sys.argv[1]):
        sys.exit("usage: version.py X.Y.Z")
    v = sys.argv[1]
    refuse_without_set(ROOT, v)
    guard(v)
    sub("Cargo.toml", r'^version = "[^"]+"', f'version = "{v}"')
    for crate in ("core", "ql", "abi", "http", "wire", "import", "tls"):
        sub("Cargo.toml", rf'^(fennec-{crate} = \{{ version = )"[^"]+"', rf'\g<1>"{v}"')
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
    0.1.7's. Each release's entry points at its notes, written once. What a
    change a user must act on wrote under `## Unreleased` as it landed -- a
    break before 1.0 -- becomes the release's entry, its notes' link first:
    an entry put above it would have left it under the release it came
    before."""
    for package in ("fennecdb", "fennecdb_flutter"):
        p = ROOT / f"integrations/dart/{package}/CHANGELOG.md"
        text = p.read_text(encoding="utf-8")
        if re.search(rf"^## {re.escape(v)}$", text, flags=re.M):
            continue
        entry = f"## {v}\n\nThe release notes: https://github.com/fennec-db/fennecdb/releases/tag/v{v}\n\n"
        text, n = re.subn(r"^## Unreleased\n\n", lambda m: entry, text, count=1, flags=re.M)
        if n == 0:
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
