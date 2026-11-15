#!/usr/bin/env python3
"""tools/version.py refuses to move the version past a released version
(from v0.2.0 on) whose fixture set is missing from the working tree or is
not the one its release wrote -- every file the bytes its SHA256SUMS
names, none beside them (RELEASING.md, step 6) -- and past one whose tag
this clone lacks, naming `git fetch --tags`; it asks nothing of a version
from before v0.2.0 or of the version a release already has. Its `main`
is run as `make version` runs it, a process in a scratch copy of the
tree, so a merge that drops or moves the gate fails here. And the tree
holds every set it names: v0.2.0's, and each line of `RELEASES` in the
compat tests' walk, whole -- not only the newest, a set deleted beside a
later one's failing too. Not of this tree's version: between a release's
tag and its set's pull request main holds a version with no set, which
only the next `make version` refuses."""

import hashlib
import importlib.util
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("version", HERE / "version.py")
version = importlib.util.module_from_spec(spec)
spec.loader.exec_module(version)

SETS = HERE.parent / "crates" / "fennec-core" / "tests" / "compat"


def write_set(d, files):
    d.mkdir(parents=True, exist_ok=True)
    lines = []
    for name, data in sorted(files.items()):
        (d / name).parent.mkdir(parents=True, exist_ok=True)
        (d / name).write_bytes(data)
        lines.append(f"{hashlib.sha256(data).hexdigest()}  {name}")
    (d / "SHA256SUMS").write_text("\n".join(lines) + "\n")


def missing_set_reasons():
    released = lambda root, v: v in ("0.2.0", "0.1.12")
    never = lambda root, v: False
    with tempfile.TemporaryDirectory() as d:
        root = pathlib.Path(d)
        why = version.missing_set(root, "0.2.0", "1.0.0", released)
        assert why and "make compat-fixtures V=0.2.0" in why, "a release with no set let through"
        why = version.missing_set(root, "0.2.0", "1.0.0", never)
        assert why and "git fetch --tags" in why, "a version with no tag in this clone let through"
        assert version.missing_set(root, "0.1.12", "0.2.0", released) is None, "a set asked of a release before v0.2.0"
        assert version.missing_set(root, "0.2.0", "0.2.0", released) is None, "a set asked to set the same version"
        s = root / "crates" / "fennec-core" / "tests" / "compat" / "v0.2.0"
        s.mkdir(parents=True)
        (s / "README.md").write_text("# The fixture set\n")
        why = version.missing_set(root, "0.2.0", "1.0.0", released)
        assert why and "SHA256SUMS: missing" in why, "a set with no SHA256SUMS let through"
        write_set(s, {"README.md": b"# The fixture set\n", "server.fennec": b"FENECDB\x01", "archive/history": b"h"})
        assert version.missing_set(root, "0.2.0", "1.0.0", released) is None, "a whole set refused"
        (s / "server.fennec").write_bytes(b"FENECDB\x02")
        why = version.missing_set(root, "0.2.0", "1.0.0", released)
        assert why and "server.fennec: not the bytes its release wrote" in why, "a file changed let through"
        (s / "server.fennec").write_bytes(b"FENECDB\x01")
        (s / "extra.fennec").write_bytes(b"x")
        why = version.missing_set(root, "0.2.0", "1.0.0", released)
        assert why and "extra.fennec: in no line of SHA256SUMS" in why, "a file beside the set let through"
        (s / "extra.fennec").unlink()
        (s / "archive" / "history").unlink()
        why = version.missing_set(root, "0.2.0", "1.0.0", released)
        assert why and "archive/history: missing" in why, "a file missing let through"


def git(repo, *args):
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@t", *args],
                   check=True, capture_output=True)


def version_py_refuses_as_a_process():
    """`tools/version.py` run in a scratch copy, a repository of its own:
    refused with no set, refused with no tag, and let through -- its first
    file rewritten -- with a whole set."""
    with tempfile.TemporaryDirectory() as d:
        repo = pathlib.Path(d)
        (repo / "tools").mkdir()
        shutil.copyfile(HERE / "version.py", repo / "tools" / "version.py")
        cargo = '[workspace.package]\nversion = "0.2.0"\n'
        (repo / "Cargo.toml").write_text(cargo)
        git(repo, "init", "-q")
        git(repo, "add", ".")
        git(repo, "commit", "-q", "-m", "a release")
        git(repo, "tag", "v0.2.0")
        env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}

        def run():
            return subprocess.run([sys.executable, str(repo / "tools" / "version.py"), "1.0.0"],
                                  capture_output=True, text=True, env=env)

        r = run()
        assert r.returncode != 0 and "make compat-fixtures V=0.2.0" in r.stderr, f"no set let through: {r.stderr}"
        assert (repo / "Cargo.toml").read_text() == cargo, "Cargo.toml changed by a refused move"
        git(repo, "tag", "-d", "v0.2.0")
        r = run()
        assert r.returncode != 0 and "git fetch --tags" in r.stderr, f"a clone with no tag let through: {r.stderr}"
        assert (repo / "Cargo.toml").read_text() == cargo, "Cargo.toml changed by a refused move"
        git(repo, "tag", "v0.2.0")
        write_set(repo / "crates" / "fennec-core" / "tests" / "compat" / "v0.2.0", {"README.md": b"# set\n"})
        r = run()
        assert 'version = "1.0.0"' in (repo / "Cargo.toml").read_text(), f"a whole set refused: {r.stderr}"


def named_sets_held():
    """A set a `RELEASES` line names, deleted, is found missing -- the
    first one beside a later one's too; and the tree's own are whole."""
    with tempfile.TemporaryDirectory() as d:
        root = pathlib.Path(d)
        walk = root / version.WALK
        walk.parent.mkdir(parents=True)
        walk.write_text('const RELEASES: &[Writes] = &[\n    Writes {\n        set: "v0.2.0",\n    },\n'
                        '    Writes {\n        set: "v1.0.0",\n    },\n];\n')
        sets = walk.parent
        for v in ("v0.2.0", "v1.0.0"):
            write_set(sets / v, {"README.md": v.encode()})
        assert version.named_sets(root) == ["v0.2.0", "v1.0.0"], version.named_sets(root)
        assert version.named_sets_problems(root) == [], version.named_sets_problems(root)
        shutil.rmtree(sets / "v0.2.0")
        problems = version.named_sets_problems(root)
        assert any("v0.2.0: missing" in p for p in problems), f"v0.2.0's set deleted beside v1.0.0's: {problems}"
        write_set(sets / "v0.2.0", {"README.md": b"v0.2.0"})
        walk.write_text('const RELEASES: &[Writes] = &[\n    Writes {\n        set: "v1.0.0",\n    },\n];\n')
        problems = version.named_sets_problems(root)
        assert any("no v0.2.0 line" in p for p in problems), f"v0.2.0's line dropped: {problems}"
    problems = version.named_sets_problems(HERE.parent)
    assert not problems, "\n".join(problems)
    for s in sorted(p for p in SETS.iterdir() if p.is_dir()):
        problems = version.set_problems(s)
        assert not problems, "\n".join(problems)


def tags_read_from_git():
    """A tag in a repository of its own: `tagged` asks git for it."""
    with tempfile.TemporaryDirectory() as d:
        repo = pathlib.Path(d)
        git(repo, "init", "-q")
        git(repo, "commit", "-q", "--allow-empty", "-m", "a release")
        git(repo, "tag", "v0.2.0")
        assert version.tagged(repo, "0.2.0"), "a tag not found"
        assert not version.tagged(repo, "0.2.1"), "a version never tagged taken as released"
    assert version.current_version(HERE.parent), "the workspace's version"


class VersionGate(unittest.TestCase):
    """The same checks under `python3 -m unittest tools/test_version.py`."""

    def test_missing_set_reasons(self):
        missing_set_reasons()

    def test_version_py_refuses_as_a_process(self):
        version_py_refuses_as_a_process()

    def test_named_sets_held(self):
        named_sets_held()

    def test_tags_read_from_git(self):
        tags_read_from_git()


def main():
    missing_set_reasons()
    version_py_refuses_as_a_process()
    named_sets_held()
    tags_read_from_git()
    print("version.py: a release after one with no whole fixture set, or with no tag in this clone, is refused, "
          "its main run as a process; every set the tree names is whole")


if __name__ == "__main__":
    sys.exit(main())
