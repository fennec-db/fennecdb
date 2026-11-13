#!/usr/bin/env python3
"""The workflows held to what keeps a token or a secret out of reach of code
that is not ours: `python3 tools/workflow_check.py [file ...]`, every file of
.github/workflows/ by default. CI's fmt job runs it.

docs/design/integrations.md#workflows. Four rules, each finding naming its
file, line and job:

- Every `uses:` names a 40-hex commit, with the tag it was taken from in a
  comment (`uses: actions/checkout@<sha> # v7.0.1`). A tag or a branch is
  moved by whoever controls the action's repository, and what it then
  points at runs with this repository's tokens and secrets. Dependabot's
  github-actions updates (.github/dependabot.yml) move a pin and its comment
  together, and rewrite the comment only where it names the pinned commit's
  most specific tag. A local action (`./...`) is this commit's own; a
  `docker://` image is pinned by its digest.
- Every workflow says `permissions:` at its top, so a job that names none
  gets those and not whatever the repository's default is that day.
- A job holding any write -- its own `permissions:`, or the workflow's
  where it names none; `id-token: write` is one -- runs no build: no cargo,
  gradle, make, npm/pnpm/yarn install or build, docker build. A build runs
  code from outside the repository with the job's token in reach: crates'
  build scripts (ring, cc), Gradle's plugins, npm's install scripts. It
  runs in a job that reads alone, and hands what it made to the job that
  writes as an artifact (release.yml's `image` and `image-push`). What is
  read is the commands a step's `run:` names, a script named for building
  (`build-aar.sh`) and an action that builds or sets Gradle up, not what a
  script runs inside: a script that builds under another name is kept out
  of a job that writes by its caller.
- A job that reads a stored secret (`secrets.<NAME>` in an expression,
  `secrets: inherit`) names an `environment:`, which holds the secret and
  whose rules -- its branches, its reviewers -- say which runs may read it;
  a repository secret is read by any workflow of any branch. A secret read
  at a workflow's top level reaches every job and is refused there.
  `secrets.GITHUB_TOKEN` is the job's own token, bounded by its
  `permissions:`. REPOSITORY_SECRETS lists a job let off, with its reason.

The workflows are read by a reader of the YAML they are written in --
block and flow collections, plain, quoted and block scalars, comments --
since Python has no YAML of its own and the tools here take nothing from
outside the standard library. What it does not read (anchors, aliases,
tags) is a finding, never a pass: GitHub reads anchors, and a `uses:` taken
from one would go unchecked."""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
WORKFLOWS = ROOT / ".github" / "workflows"

# Jobs that may read a stored secret without naming an environment, by
# (workflow file, job id), each with the reason it may. None: every job
# that reads one names its environment -- packages.yml's registries,
# desktop.yml's `sign`, site.yml's `deploy`. A job naming its environment
# reads a repository secret of the same name too, so moving a secret from
# the repository's into the environment changes no workflow.
REPOSITORY_SECRETS = {}


class Unreadable(Exception):
    """A workflow written in YAML this reader does not read: a finding."""

    def __init__(self, line, why):
        super().__init__(f"line {line}: {why}")
        self.line = line
        self.why = why


class Scalar(str):
    """A scalar's text, with the line it starts on and the comment after it
    on that line (the tag a `uses:` pin names)."""

    def __new__(cls, text, line, comment=""):
        s = super().__new__(cls, text)
        s.line = line
        s.comment = comment
        return s


class Mapping(dict):
    def __init__(self, line):
        super().__init__()
        self.line = line
        self.lines = {}


class Sequence(list):
    def __init__(self, line):
        super().__init__()
        self.line = line


def item(text):
    return text == "-" or text.startswith("- ")


def uncomment(text):
    """A plain scalar's text and the comment after it: `#` begins one where
    whitespace is before it, quotes or not -- YAML's rule, and GitHub's."""
    m = re.search(r"(?:^|\s)#", text)
    if not m:
        return text.rstrip(), ""
    return text[:m.start()].rstrip(), text[m.end():].strip()


def fold(lines):
    """A folded block scalar's lines (`>`) as YAML joins them: a break
    between two lines of text is a space, an empty line a break of its own
    (the break before it dropped), a line indented deeper kept on its own."""
    out = ""
    for at, text in enumerate(lines):
        if at == 0:
            out = text
            continue
        if not text:
            out += "\n"
            continue
        last = at - 1
        while last >= 0 and not lines[last]:
            last -= 1
        plain = last >= 0 and text[:1] not in " \t" and lines[last][:1] not in " \t"
        if plain:
            out += text if last < at - 1 else " " + text
        else:
            out += "\n" + text
    return out


class Reader:
    """One YAML document of block structure, as workflows are written."""

    def __init__(self, text):
        self.lines = text.split("\n")
        self.i = 0

    def fail(self, line, why):
        raise Unreadable(line, why)

    def peek(self):
        """The next line holding a node -- (indent, text, line number) -- with
        blank and comment lines passed over, or None at the end."""
        while self.i < len(self.lines):
            raw = self.lines[self.i]
            text = raw.strip()
            if not text or text.startswith("#"):
                self.i += 1
                continue
            lead = raw[:len(raw) - len(raw.lstrip())]
            if "\t" in lead:
                self.fail(self.i + 1, "a tab in the indentation")
            indent = len(raw) - len(raw.lstrip(" "))
            return indent, raw[indent:].rstrip(), self.i + 1
        return None

    def document(self):
        head = self.peek()
        if head is not None and head[1] == "---":
            self.i += 1
            head = self.peek()
        if head is None:
            return Mapping(1)
        node = self.node(head[0], -1)
        rest = self.peek()
        if rest is not None:
            self.fail(rest[2], "text past the document's top level, or a second document")
        return node

    def node(self, indent, parent):
        """The node whose first line is the next, at `indent`, inside a parent
        node at `parent`."""
        _, text, line = self.peek()
        if item(text):
            return self.sequence(indent)
        if self.key(text, line) is not None:
            return self.mapping(indent)
        self.i = line
        return self.value(text, line, parent)

    def key(self, text, line):
        """A mapping entry's key and the text after its `:`, or None where the
        line holds no key."""
        if text[0] in "'\"":
            end = self.closing(text, 0)
            if end is None:
                return None
            m = re.match(r"\s*:(?=\s|$)", text[end + 1:])
            if not m:
                return None
            return self.unquote(text[:end + 1], line), text[end + 1 + m.end():]
        if text[0] in "[]{}#|>&*!%@`,?" or item(text):
            return None
        for at, ch in enumerate(text):
            if ch == "#" and text[at - 1] in " \t":
                return None
            if ch == ":" and (at + 1 == len(text) or text[at + 1] in " \t"):
                key = text[:at].rstrip()
                if key == "<<":
                    self.fail(line, "a merge key: this reader takes no anchors")
                return key, text[at + 1:]
        return None

    def mapping(self, indent):
        m = Mapping(self.peek()[2])
        while True:
            head = self.peek()
            if head is None or head[0] < indent:
                return m
            at, text, line = head
            if at > indent:
                self.fail(line, "indented deeper than the mapping it is in")
            if item(text):
                self.fail(line, "a sequence's entry among a mapping's keys")
            entry = self.key(text, line)
            if entry is None:
                self.fail(line, "not a `key: value` line")
            key, rest = entry
            if key in m:
                self.fail(line, f"{key!r} twice in one mapping")
            m.lines[key] = line
            self.i = line
            rest = rest.strip()
            if rest and not rest.startswith("#"):
                m[key] = self.value(rest, line, indent)
                continue
            head = self.peek()
            if head is not None and head[0] > indent:
                m[key] = self.node(head[0], indent)
            elif head is not None and head[0] == indent and item(head[1]):
                # A sequence may stand at its key's indent (`steps:` then
                # `- uses: ...` under it, not indented).
                m[key] = self.sequence(indent)
            else:
                m[key] = Scalar("", line, uncomment(rest)[1])

    def sequence(self, indent):
        s = Sequence(self.peek()[2])
        while True:
            head = self.peek()
            if head is None or head[0] < indent:
                return s
            at, text, line = head
            if at > indent:
                self.fail(line, "indented deeper than the sequence it is in")
            if not item(text):
                return s
            rest = text[1:]
            content = rest.lstrip(" ")
            if not content or content.startswith("#"):
                self.i = line
                head = self.peek()
                if head is not None and head[0] > indent:
                    s.append(self.node(head[0], indent))
                else:
                    s.append(Scalar("", line))
                continue
            # The entry's node starts where its text does: a mapping's later
            # keys stand at that column on the lines after.
            column = indent + 1 + len(rest) - len(content)
            self.lines[line - 1] = " " * column + content
            s.append(self.node(column, indent))

    def value(self, text, line, parent):
        """The value starting at `text` on `line` (taken already) and running
        on over the lines indented deeper than `parent`."""
        first = text[0]
        if first in "&*!":
            self.fail(line, "an anchor, alias or tag: this reader takes none")
        if first in "%@`":
            self.fail(line, f"a value starting with the reserved {first!r}")
        if first in "|>":
            return self.block_scalar(text, line, parent)
        if first in "[{":
            return self.flow(text, line, parent)
        if first in "'\"":
            return self.quoted(text, line, parent)
        return self.plain(text, line, parent)

    def block_scalar(self, header, line, parent):
        m = re.fullmatch(r"([|>])([+-]?)([1-9]?)([+-]?)\s*(?:#(.*))?", header)
        if not m or (m.group(2) and m.group(4)):
            self.fail(line, f"a block scalar's header {header!r}")
        indent = parent + int(m.group(3)) if m.group(3) else None
        lines = []
        while self.i < len(self.lines):
            raw = self.lines[self.i]
            if not raw.strip():
                lines.append("")
                self.i += 1
                continue
            at = len(raw) - len(raw.lstrip(" "))
            if indent is None:
                if at <= parent:
                    break
                indent = at
            if at < indent:
                # A comment less indented than the text closes it, as does
                # the next node; anything else between is not YAML.
                if at > parent and not raw.strip().startswith("#"):
                    self.fail(self.i + 1, "a block scalar's line indented less than its first")
                break
            lines.append(raw[indent:])
            self.i += 1
        # The final line breaks are dropped whatever the chomping says: the
        # rules read the commands and expressions in the text.
        while lines and not lines[-1]:
            lines.pop()
        text = fold(lines) if m.group(1) == ">" else "\n".join(lines)
        return Scalar(text, line, (m.group(5) or "").strip())

    def plain(self, text, line, parent):
        value, comment = uncomment(text)
        parts = [value]
        while not comment and self.i < len(self.lines):
            # A plain scalar runs on over the lines indented deeper than its
            # parent (a long `if:`), a blank line among them a line break.
            j = self.i
            while j < len(self.lines) and not self.lines[j].strip():
                j += 1
            if j == len(self.lines):
                break
            raw = self.lines[j]
            at = len(raw) - len(raw.lstrip(" "))
            if at <= parent or raw.strip().startswith("#"):
                break
            more, comment = uncomment(raw.strip())
            parts.append("\n" * (j - self.i) if j > self.i else " ")
            parts.append(more)
            self.i = j + 1
        return Scalar("".join(parts), line, comment)

    def closing(self, text, start):
        """Where the quoted scalar opening at `start` closes, or None."""
        quote = text[start]
        at = start + 1
        while at < len(text):
            ch = text[at]
            if quote == "'" and ch == "'":
                if text[at + 1:at + 2] == "'":
                    at += 2
                    continue
                return at
            if quote == '"' and ch == "\\":
                at += 2
                continue
            if quote == '"' and ch == '"':
                return at
            at += 1
        return None

    def unquote(self, text, line):
        body = text[1:-1]
        if text[0] == "'":
            return body.replace("''", "'")
        escapes = {"n": "\n", "t": "\t", "r": "\r", "0": "\0", "\\": "\\",
                   '"': '"', "/": "/", " ": " ", "\n": "", "e": "\x1b"}
        out, at = [], 0
        while at < len(body):
            ch = body[at]
            if ch == "\\" and at + 1 < len(body):
                nxt = body[at + 1]
                if nxt in "xuU":
                    width = {"x": 2, "u": 4, "U": 8}[nxt]
                    code = body[at + 2:at + 2 + width]
                    if not re.fullmatch(r"[0-9a-fA-F]+", code) or len(code) != width:
                        self.fail(line, "a bad escape in a double-quoted scalar")
                    out.append(chr(int(code, 16)))
                    at += 2 + width
                    continue
                out.append(escapes.get(nxt, nxt))
                at += 2
                continue
            out.append(ch)
            at += 1
        return "".join(out)

    def quoted(self, text, line, parent):
        """A quoted scalar, which may run on over the lines after its first,
        each line break folded to a space as YAML does."""
        while self.closing(text, 0) is None:
            if self.i >= len(self.lines):
                self.fail(line, "a quoted scalar that does not close")
            raw = self.lines[self.i].strip()
            self.i += 1
            trailing = len(text) - len(text.rstrip("\\"))
            if text[0] == '"' and trailing % 2:
                text = text[:-1] + raw  # an escaped line break joins with nothing
            elif not raw:
                text += "\n"
            elif text.endswith("\n"):
                text += raw
            else:
                text += " " + raw
        end = self.closing(text, 0)
        rest = text[end + 1:].strip()
        if rest and not rest.startswith("#"):
            self.fail(line, f"text after a quoted scalar: {rest!r}")
        return Scalar(self.unquote(text[:end + 1], line), line, rest[1:].strip())

    def flow(self, text, line, parent):
        """A flow collection (`[main]`, `{ os: ubuntu-24.04, arch: amd64 }`),
        read on over the lines after its first until it closes."""
        def balanced(t):
            depth, at, quote = 0, 0, None
            while at < len(t):
                ch = t[at]
                if quote:
                    if quote == '"' and ch == "\\":
                        at += 2
                        continue
                    if ch == quote:
                        if quote == "'" and t[at + 1:at + 2] == "'":
                            at += 2
                            continue
                        quote = None
                elif ch in "'\"" and (at == 0 or t[at - 1] in " \t\n[{,:"):
                    quote = ch
                elif ch == "#" and (at == 0 or t[at - 1] in " \t\n"):
                    nl = t.find("\n", at)
                    at = len(t) if nl < 0 else nl
                    continue
                elif ch in "[{":
                    depth += 1
                elif ch in "]}":
                    depth -= 1
                    if depth == 0:
                        return at
                at += 1
            return None

        while balanced(text) is None:
            if self.i >= len(self.lines):
                self.fail(line, "a flow collection that does not close")
            text = text + "\n" + self.lines[self.i]
            self.i += 1
        end = balanced(text)
        rest = text[end + 1:].strip()
        if rest and not rest.startswith("#"):
            self.fail(line, f"text after a flow collection: {rest!r}")
        node, at = self.flow_node(text, 0, line)
        return node

    def flow_node(self, t, at, line):
        at = self.flow_space(t, at)
        ch = t[at] if at < len(t) else ""
        if ch == "[":
            s, at = Sequence(line), at + 1
            while True:
                at = self.flow_space(t, at)
                if t[at:at + 1] == "]":
                    return s, at + 1
                node, at = self.flow_node(t, at, line)
                at = self.flow_space(t, at)
                if t[at:at + 1] == ":":
                    # A single pair inside a flow sequence: `[a: b]`.
                    pair = Mapping(line)
                    pair[str(node)], at = self.flow_node(t, at + 1, line)
                    node = pair
                    at = self.flow_space(t, at)
                s.append(node)
                if t[at:at + 1] == ",":
                    at += 1
                elif t[at:at + 1] != "]":
                    self.fail(line, "a flow sequence's entries are not separated by commas")
        if ch == "{":
            m, at = Mapping(line), at + 1
            while True:
                at = self.flow_space(t, at)
                if t[at:at + 1] == "}":
                    return m, at + 1
                key, at = self.flow_node(t, at, line)
                if not isinstance(key, str):
                    self.fail(line, "a flow mapping's key is a collection")
                at = self.flow_space(t, at)
                if t[at:at + 1] == ":":
                    value, at = self.flow_node(t, at + 1, line)
                else:
                    value = Scalar("", line)
                if str(key) in m:
                    self.fail(line, f"{str(key)!r} twice in one mapping")
                m[str(key)] = value
                m.lines[str(key)] = line
                at = self.flow_space(t, at)
                if t[at:at + 1] == ",":
                    at += 1
                elif t[at:at + 1] != "}":
                    self.fail(line, "a flow mapping's entries are not separated by commas")
        if ch in "'\"":
            end = self.closing(t, at)
            return Scalar(self.unquote(t[at:end + 1], line), line), end + 1
        if ch in "&*!":
            self.fail(line, "an anchor, alias or tag: this reader takes none")
        if ch in ",]}":
            return Scalar("", line), at
        start = at
        while at < len(t):
            c = t[at]
            if c in ",[]{}":
                break
            if c == ":" and t[at + 1:at + 2] in ("", " ", "\t", "\n", ",", "]", "}"):
                break
            if c == "#" and t[at - 1] in " \t\n":
                break
            at += 1
        return Scalar(" ".join(t[start:at].split()), line), at

    def flow_space(self, t, at):
        while at < len(t):
            if t[at] in " \t\n":
                at += 1
            elif t[at] == "#" and (at == 0 or t[at - 1] in " \t\n"):
                nl = t.find("\n", at)
                at = len(t) if nl < 0 else nl
            else:
                break
        return at


def read(text):
    """A workflow's document: a Mapping, or Unreadable raised."""
    return Reader(text).document()


# --- the rules ---------------------------------------------------------

COMMIT = re.compile(r"[0-9a-f]{40}")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
# The tag a pin was taken from, as its comment names it: `v7.0.1`, `v2`, a
# pre-release's `v1.0.0-rc.1`, or a tag of an action that has no `v`.
VERSION = re.compile(r"v?\d+(?:\.\d+)*(?:[-+][0-9A-Za-z.-]+)?(?:\s|$)")


def pinned(value):
    """Why a `uses:` value is not pinned, or None where it is."""
    v = str(value).strip()
    if v.startswith("./"):
        return None
    if v.startswith("docker://"):
        _, at, ref = v.rpartition("@")
        if at and DIGEST.fullmatch(ref):
            return None
        return "a docker:// image is pinned by its digest (`@sha256:...`)"
    _, at, ref = v.partition("@")
    if not at or not COMMIT.fullmatch(ref):
        return "pin it to its 40-hex commit, the tag in a comment (`@<sha> # v1.2.3`)"
    if not VERSION.match(value.comment):
        return "the pin names no tag in a comment (`# v1.2.3`): Dependabot updates the comment with the commit"
    return None


# Where a shell command may start: a line's start, after an operator or a
# keyword, `sudo`, `env` and assignments before it, and a path to it.
COMMAND = (r"(?:^|[;&|(`]|\$\(|\b(?:then|do|else|elif|if|while|until|exec|time|"
           r"sudo|env|xargs|nohup|!)\s)\s*(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*(?:\S*/)?")
BUILDS = [
    ("cargo", re.compile(COMMAND + r"cargo(?=\s|$)")),
    ("gradle", re.compile(COMMAND + r"gradlew?(?=\s|$)")),
    ("make", re.compile(COMMAND + r"make(?=\s|$)")),
    ("an npm, pnpm or yarn install or build", re.compile(
        COMMAND + r"(?:npm|pnpm|yarn)\s+(?:ci|install|i|add|build|rebuild|run\s+build|run-script\s+build)(?=\s|$)")),
    ("docker build", re.compile(
        COMMAND + r"docker\s+(?:(?:buildx|image|compose|builder)\s+)?(?:build|bake)(?=\s|$)")),
    # A script named for building, wherever the line names it: release.yml's
    # Android AAR is `integrations/kotlin/build-aar.sh`, Gradle and cargo
    # inside, and `python3 site/build.py` is the site's.
    ("a build script", re.compile(r"(?:^|[\s/;&|(=])[\w.-]*build[\w.-]*\.(?:sh|bash|ps1|py)(?=\s|$|[;&|)])")),
]
# Actions that build, or set up what only a build runs: a job that sets
# Gradle up runs it.
BUILD_ACTIONS = re.compile(r"(?:docker/(?:build-push|bake)-action|gradle/actions/setup-gradle|gradle/gradle-build-action)@")


def uncommented(line):
    """A shell line without its comment: from a `#` that starts a word,
    outside quotes (`${#x}` and `"#1"` are no comments)."""
    quote = None
    for at, ch in enumerate(line):
        if quote:
            if ch == quote:
                quote = None
        elif ch in "'\"":
            quote = ch
        elif ch == "#" and (at == 0 or line[at - 1] in " \t;&|("):
            return line[:at].rstrip()
    return line


def commands(script):
    """A `run:` script's lines as a shell reads them: comments left out, a
    line ending in `\\` joined to the next."""
    out, held = [], ""
    for line in script.split("\n"):
        text = uncommented(line.strip())
        if text.endswith("\\"):
            held += text[:-1] + " "
            continue
        if held or text:
            out.append(held + text)
        held = ""
    if held:
        out.append(held)
    return out


def builds(job):
    """What a job runs that builds: (line, what) for each, once a step."""
    found = []
    for step in steps(job):
        uses = step.get("uses")
        if isinstance(uses, Scalar) and BUILD_ACTIONS.match(uses):
            found.append((uses.line, str(uses)))
        run = step.get("run")
        if isinstance(run, Scalar):
            for text in commands(run):
                for what, pattern in BUILDS:
                    if pattern.search(text) and (run.line, what) not in found:
                        found.append((run.line, what))
    return found


def steps(job):
    s = job.get("steps")
    return [x for x in s if isinstance(x, Mapping)] if isinstance(s, Sequence) else []


def writes(permissions):
    """The scopes a `permissions:` value lets its token write."""
    if isinstance(permissions, Mapping):
        return sorted(k for k, v in permissions.items() if isinstance(v, str) and v.strip() == "write")
    if isinstance(permissions, str) and permissions.strip() == "write-all":
        return ["every scope (write-all)"]
    return []


EXPRESSION = re.compile(r"\$\{\{(.*?)\}\}", re.S)
# The context `secrets`, not a property of that name (`inputs.secrets`):
# `secrets.NAME`, `secrets['NAME']`, or the whole of it (`toJSON(secrets)`).
SECRET = re.compile(r"(?<![.\w])secrets\b(?:\s*\.\s*([A-Za-z_][\w-]*)|\s*\[\s*(['\"])(.*?)\2\s*\])?")


def scalars(node, key=None):
    """Every scalar under a node, with whether it is an `if:`, which GitHub
    reads as an expression whole, `${{ }}` or not."""
    if isinstance(node, Mapping):
        for k, v in node.items():
            yield from scalars(v, k)
    elif isinstance(node, Sequence):
        for v in node:
            yield from scalars(v, None)
    elif isinstance(node, Scalar):
        yield node, key == "if"


def secrets_read(node):
    """Each stored secret a node's expressions read: (line, name). The job's
    own GITHUB_TOKEN is no stored secret."""
    found = []
    for scalar, whole in scalars(node):
        texts = [str(scalar)] if whole else EXPRESSION.findall(scalar)
        for text in texts:
            for m in SECRET.finditer(text):
                name = m.group(1) or m.group(3) or "every secret (`secrets` whole)"
                if name.upper() != "GITHUB_TOKEN":
                    found.append((scalar.line, name))
    return found


def check_text(text, name):
    """The findings for one workflow's text, each `name:line: why`."""
    try:
        doc = read(text)
    except Unreadable as e:
        return [f"{name}:{e.line}: {e.why}: tools/workflow_check.py cannot read this, so it passes nothing"]
    if not isinstance(doc, Mapping):
        return [f"{name}:1: a workflow is a mapping"]
    findings = []

    if "permissions" not in doc:
        findings.append(f"{name}:1: no `permissions:` at the top: a job naming none gets the"
                        " repository's default; say `permissions: contents: read` and widen it"
                        " in the jobs that need more")

    for key, value in doc.items():
        if key in ("jobs", "on"):
            continue
        for line, secret in secrets_read(value):
            findings.append(f"{name}:{line}: the workflow's `{key}:` reads secrets.{secret}, which"
                            " reaches every job: read it in the job that needs it, under its environment")

    jobs = doc.get("jobs")
    if not isinstance(jobs, Mapping):
        return findings + [f"{name}:1: no `jobs:` mapping"]
    top = doc.get("permissions")
    for job_id, job in jobs.items():
        where = f"{name}:{jobs.lines[job_id]}: job `{job_id}`"
        if not isinstance(job, Mapping):
            findings.append(f"{where}: not a mapping")
            continue

        uses = [job["uses"]] if isinstance(job.get("uses"), Scalar) else []
        uses += [s["uses"] for s in steps(job) if isinstance(s.get("uses"), Scalar)]
        for u in uses:
            why = pinned(u)
            if why:
                findings.append(f"{name}:{u.line}: job `{job_id}` uses {u}: {why}")

        held = writes(job["permissions"] if "permissions" in job else top)
        if held:
            for line, what in builds(job):
                findings.append(f"{name}:{line}: job `{job_id}` holds {', '.join(held)} write and runs"
                                f" {what}: build in a job that reads alone, and hand what it made"
                                " to the job that writes as an artifact")

        read_ = secrets_read(job)
        if str(job.get("secrets", "")).strip() == "inherit":
            read_.append((job.lines["secrets"], "every secret (`secrets: inherit`)"))
        environment = job.get("environment")
        named = bool(str(environment.get("name", "")).strip()) if isinstance(environment, Mapping) \
            else bool(str(environment or "").strip())
        if read_ and not named and (pathlib.PurePath(name).name, job_id) not in REPOSITORY_SECRETS:
            for line, secret in read_:
                findings.append(f"{name}:{line}: job `{job_id}` reads secrets.{secret} and names no"
                                " `environment:`: keep the secret in an environment whose rules"
                                " say which runs may read it, and name it here")
    return findings


def main():
    paths = [pathlib.Path(p) for p in sys.argv[1:]] or sorted(
        p for p in WORKFLOWS.iterdir() if p.suffix in (".yml", ".yaml"))
    findings = []
    for path in paths:
        try:
            shown = path.resolve().relative_to(ROOT)
        except ValueError:
            shown = path
        findings += check_text(path.read_text(encoding="utf-8"), str(shown))
    for f in findings:
        print(f, file=sys.stderr)
    if findings:
        sys.exit(f"workflow_check: {len(findings)} finding(s) in {len(paths)} workflow(s):"
                 " docs/design/integrations.md#workflows")
    print(f"workflow_check: {len(paths)} workflows, every action pinned by its commit, every"
          " workflow's permissions said, no job that writes builds, every secret read under an environment")


if __name__ == "__main__":
    main()
