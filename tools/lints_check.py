#!/usr/bin/env python3
"""Every crate takes the workspace's lints: `python3 tools/lints_check.py`.

docs/design/conventions.md#unsafe-code. The root Cargo.toml's
`[workspace.lints.clippy]` denies `undocumented_unsafe_blocks`, so every
`unsafe` block and `unsafe impl` says why it is sound in a `// SAFETY:`
comment, in every clippy run CI makes. A crate reaches that table only by
writing `[lints] workspace = true`, which a new crate could leave out, and
then nothing it holds would be asked: this holds every member of the
workspace to it, and the table to the lint. CI's fmt job runs it."""

import pathlib
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent


def main() -> int:
    root = tomllib.loads((ROOT / "Cargo.toml").read_text())
    workspace = root["workspace"]
    problems = []
    level = workspace.get("lints", {}).get("clippy", {}).get("undocumented_unsafe_blocks")
    if level != "deny":
        problems.append(
            "Cargo.toml: [workspace.lints.clippy] undocumented_unsafe_blocks is "
            f"{level!r}, not \"deny\""
        )
    members = workspace.get("members", [])
    if not members:
        problems.append("Cargo.toml: the workspace names no members")
    for member in members:
        manifest = ROOT / member / "Cargo.toml"
        lints = tomllib.loads(manifest.read_text()).get("lints")
        if lints != {"workspace": True}:
            problems.append(
                f"{member}/Cargo.toml: takes {lints!r} for its lints, not the "
                "workspace's: write `[lints]` and `workspace = true`"
            )
    for p in problems:
        print(p, file=sys.stderr)
    if not problems:
        print(f"{len(members)} crates take the workspace's lints")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
