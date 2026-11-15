#!/usr/bin/env python3
"""tools/version.py refuses to move the version past a released version
(from v0.2.0 on, its tag in the repository) whose fixture set is missing or
is not the one its release wrote -- every file the bytes its SHA256SUMS
names, none beside them (RELEASING.md, step 6) -- and asks nothing of a
version from before v0.2.0, of the version a release already has, or of
a version never tagged, whose set would be built from its tag. And every
set in this tree is whole. Not of this tree's version: between a
release's tag and its set's pull request main holds a version with no
set, which only the next `make version` refuses."""

import hashlib
import importlib.util
import pathlib
import subprocess
import sys
import tempfile

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


def main():
    released = lambda root, v: v in ("0.2.0", "0.1.12")
    never = lambda root, v: False
    with tempfile.TemporaryDirectory() as d:
        root = pathlib.Path(d)
        why = version.missing_set(root, "0.2.0", "1.0.0", released)
        assert why and "make compat-fixtures V=0.2.0" in why, "a release with no set let through"
        assert version.missing_set(root, "0.2.0", "1.0.0", never) is None, "a set asked of a version never tagged"
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

        # A tag in a repository of its own: `tagged` asks git for it.
        repo = root / "repo"
        repo.mkdir()
        git = lambda *a: subprocess.run(["git", "-C", str(repo), *a], check=True, capture_output=True)
        git("init", "-q")
        git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "a release")
        git("tag", "v0.2.0")
        assert version.tagged(repo, "0.2.0"), "a tag not found"
        assert not version.tagged(repo, "0.2.1"), "a version never tagged taken as released"
    assert version.current_version(HERE.parent), "the workspace's version"
    for s in sorted(p for p in SETS.iterdir() if p.is_dir()):
        problems = version.set_problems(s)
        assert not problems, "\n".join(problems)
    print("version.py: a release after one with no whole fixture set is refused; every set in the tree is whole")


if __name__ == "__main__":
    sys.exit(main())
