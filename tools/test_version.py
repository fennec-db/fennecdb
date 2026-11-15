#!/usr/bin/env python3
"""tools/version.py refuses to move the version past a release that has no
fixture set (RELEASING.md, step 6), and asks nothing of a version from
before v0.2.0 or of the version a release already has. Not of this tree:
between a release's tag and its set's pull request main holds a version
with no set, which only the next `make version` refuses."""

import importlib.util
import pathlib
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("version", HERE / "version.py")
version = importlib.util.module_from_spec(spec)
spec.loader.exec_module(version)


def main():
    with tempfile.TemporaryDirectory() as d:
        root = pathlib.Path(d)
        assert version.missing_set(root, "0.2.0", "1.0.0"), "a release with no set let through"
        assert "make compat-fixtures V=0.2.0" in version.missing_set(root, "0.2.0", "1.0.0")
        assert version.missing_set(root, "0.1.12", "0.2.0") is None, "a set asked of a release before v0.2.0"
        assert version.missing_set(root, "0.2.0", "0.2.0") is None, "a set asked to set the same version"
        empty = root / "crates" / "fennec-core" / "tests" / "compat" / "v0.2.0"
        empty.mkdir(parents=True)
        assert version.missing_set(root, "0.2.0", "1.0.0"), "a set with no README let through"
        (empty / "README.md").write_text("# The fixture set\n")
        assert version.missing_set(root, "0.2.0", "1.0.0") is None, "a release with its set refused"
    assert version.current_version(HERE.parent), "the workspace's version"
    print("version.py: a release after one with no fixture set is refused")


if __name__ == "__main__":
    sys.exit(main())
