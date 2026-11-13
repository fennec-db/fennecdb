#!/usr/bin/env python3
"""The workflows held to what keeps a token or a secret out of reach of code
that is not ours: `python3 tools/workflow_check.py [file ...]`, every file of
.github/workflows/ by default. CI's fmt job runs it.

docs/design/integrations.md#workflows. Four rules, each finding naming its
file, line and job:

- Every `uses:` names a 40-hex commit, with the tag it was taken from in a
  comment: the commit's most specific tag, X.Y.Z (`uses:
  actions/checkout@<sha> # v7.0.1`, never `# v7`). A tag or a branch is
  moved by whoever controls the action's repository, and what it then
  points at runs with this repository's tokens and secrets. Dependabot's
  github-actions updates (.github/dependabot.yml) move a pin and its comment
  together, and rewrite the comment only where it names the pinned commit's
  most specific tag. MAJOR_TAGS lists an action that publishes no X.Y.Z
  tag, with its reason. A local action (`./...`) is this commit's own; a
  `docker://` image is pinned by its digest.
- Every workflow says `permissions:` at its top, so a job that names none
  gets those and not whatever the repository's default is that day.
- A job holding any write -- its own `permissions:`, or the workflow's
  where it names none; `id-token: write` is one -- runs no build. A build
  runs code from outside the repository with the job's token in reach:
  crates' build scripts (ring, cc), Gradle's and SwiftPM's plugins, npm's
  install scripts, a package's own commands. It runs in a job that reads
  alone, and hands what it made to the job that writes as an artifact
  (release.yml's `image` and `image-push`). What is read is the commands a
  step's `run:` runs, read as its shell reads them (Shell): cargo, gradle,
  maven, make, xcodebuild, npx; an npm, pnpm or yarn install, script or
  build; docker, swift, dotnet, go, flutter or dart building; pip
  installing; a script named for building or packaging (`build-aar.sh`,
  `package.sh`); then an action that builds or sets Gradle up, and a local
  action (`./...`), whose steps this check does not read. A word inside
  quotes, an echo's text, a comment or a here-document `cat` writes is no
  command; `$(...)`, `sh -c '...'` and a here-document a shell reads are.
  What a script runs inside is not read: a script that builds under another
  name is kept out of a job that writes by its caller. WRITE_AND_BUILD
  lists a job let off, with its reason.
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
import posixpath
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
WORKFLOWS = ROOT / ".github" / "workflows"

# Jobs that may read a stored secret without naming an environment, by
# (workflow file, job id), each with the reason it may. Every other job that
# reads one names its environment -- packages.yml's registries, desktop.yml's
# `sign`. A job naming its environment reads a repository secret of the same
# name too, so moving a secret from the repository's into the environment
# changes no workflow; but naming one that does not exist makes GitHub create
# it, with no rules, at the job's first run.
REPOSITORY_SECRETS = {
    ("site.yml", "deploy"): "until the owner creates a main-only site environment"
    " (RELEASING.md, *GitHub*): naming `site` before it exists would make GitHub"
    " create it unprotected, a setting no pull request should change",
}

# Jobs that may hold a write and run a build, by (workflow file, job id),
# each with the reason it may. None: every job that writes takes what it
# publishes from a job that built it reading alone.
WRITE_AND_BUILD = {}

# Actions whose pin may name a major tag alone (`# v2`), by their repository
# (`owner/repo`), each with the reason: an action that publishes no X.Y.Z
# tag has no more specific one for the comment to name. None.
MAJOR_TAGS = {}


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
# The tag a pin was taken from, as its comment names it: the commit's most
# specific tag, X.Y.Z (`v7.0.1`, a pre-release's `v1.0.0-rc.1`, or a tag of
# an action that has no `v`), or for an action in MAJOR_TAGS its major.
VERSION = re.compile(r"v?\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?(?:\s|$)")
MAJOR = re.compile(r"v?\d+(?:\s|$)")


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
    action, at, ref = v.partition("@")
    if not at or not COMMIT.fullmatch(ref):
        return "pin it to its 40-hex commit, the tag in a comment (`@<sha> # v1.2.3`)"
    if VERSION.match(value.comment):
        return None
    repo = "/".join(action.split("/")[:2])
    if repo in MAJOR_TAGS and MAJOR.match(value.comment):
        return None
    if MAJOR.match(value.comment) or re.match(r"v?\d+\.\d+(?:\s|$)", value.comment):
        return (f"the pin's comment names `{value.comment.split()[0]}`, not the commit's most specific"
                " tag (`# v1.2.3`): Dependabot rewrites only a comment naming that tag; an action"
                " publishing no X.Y.Z tag goes in MAJOR_TAGS with the reason")
    return "the pin names no tag in a comment (`# v1.2.3`): Dependabot updates the comment with the commit"


# --- what a `run:` script runs ------------------------------------------

# A shell's operators, longest first so `&&` is not read as two `&`. A
# separator ends a command; a redirection takes the word after it, which is
# a file, not an argument.
SEPARATORS = ("&&", "||", ";;&", ";;", ";&", "|&", "|", "&", ";", "(", ")", "\n")
REDIRECTIONS = ("&>>", "&>", "<<<", "<<-", "<<", ">>", ">&", "<&", "<>", ">|", "<", ">")
OPERATORS = sorted(SEPARATORS + REDIRECTIONS, key=len, reverse=True)
CASE_ENDS = (";;", ";&", ";;&")
# Words a shell reads as its own where a command could start.
KEYWORDS = {"if", "then", "else", "elif", "fi", "do", "done", "while", "until", "!", "{", "}",
            "time", "esac", "[[", "]]"}
ASSIGNMENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*(?:\[[^]]*\])?\+?=")


class Shell:
    """The simple commands a `run:` script runs, read as a shell reads it:
    each a list of its words, quotes taken off and expansions left as
    written, with the bodies of the here-documents it reads. A word inside
    quotes, an argument such as an echo's text, a comment or a here-document
    is no command; the commands of `$(...)`, backquotes, `<(...)` and an
    unquoted here-document's substitutions run, and are read too
    (`commands`). PowerShell's text (`pwsh`) is read the same way, its
    backquote an escape and its here-strings text. A quote or a
    substitution that does not close runs to the script's end: a script is
    read, never refused for its shell."""

    def __init__(self, text, pwsh=False):
        self.s = text
        self.pwsh = pwsh
        self.i = 0
        self.tokens = []       # ("word", text) or ("op", operator)
        self.inner = []        # scripts that run inside this one's words
        self.bodies = {}       # a here-document operator's token index: its body
        self.waiting = []      # here-documents whose bodies start on the next line
        self.delimiter = None  # a `<<` waiting for its delimiter: (token index, strip tabs)
        self.word = None
        self.quoted = False

    def commands(self):
        """[(words, here-document bodies)] for each simple command, those of
        the scripts inside its words after its own."""
        self.lex()
        out = parse(self.tokens, self.bodies)
        for text in self.inner:
            out += Shell(text, self.pwsh).commands()
        return out

    def add(self, text):
        self.word = (self.word or "") + text

    def end(self):
        if self.word is None:
            return
        if self.delimiter is not None:
            at, strip = self.delimiter
            self.waiting.append((at, self.word, strip, not self.quoted))
            self.delimiter = None
        else:
            self.tokens.append(("word", self.word))
        self.word, self.quoted = None, False

    def lex(self):
        s = self.s
        while self.i < len(s):
            c = s[self.i]
            nxt = s[self.i + 1:self.i + 2]
            escape = "`" if self.pwsh else "\\"
            if c == escape:
                if nxt != "\n":  # an escaped line break joins the lines
                    self.add(nxt)
                    self.quoted = True
                self.i += 2
            elif c in " \t\r":
                self.end()
                self.i += 1
            elif c == "#" and self.word is None:
                self.i = self.line_end()
            elif c == "'":
                close = s.find("'", self.i + 1)
                close = len(s) if close < 0 else close
                self.add(s[self.i + 1:close])
                self.quoted = True
                self.i = close + 1
            elif c == '"':
                self.add(self.double())
                self.quoted = True
            elif c == "$":
                self.add(self.dollar())
            elif c == "`":
                self.add(self.backquote())
            elif c == "@" and self.pwsh and self.word is None and nxt in ("'", '"') \
                    and not s[self.i + 2:self.line_end()].strip():
                self.add(self.here_string(nxt))
                self.quoted = True
            elif c == "<" and nxt == "#" and self.pwsh:
                close = s.find("#>", self.i)  # PowerShell's block comment
                self.i = len(s) if close < 0 else close + 2
            elif c in "<>" and nxt == "(" and self.word is None:
                close = self.closing(self.i + 1)
                self.inner.append(s[self.i + 2:close])  # a process substitution runs
                self.add(s[self.i:close + 1])
                self.i = close + 1
            elif c in "{}" and self.pwsh:
                self.end()  # a script block's braces stand between its commands
                self.tokens.append(("op", ";"))
                self.i += 1
            else:
                op = next((o for o in OPERATORS if s.startswith(o, self.i)), None)
                if op is None:
                    self.add(c)
                    self.i += 1
                    continue
                if op in REDIRECTIONS and self.word is not None and self.word.isdigit():
                    self.word = None  # `2>`: the digits name a file descriptor
                self.end()
                self.tokens.append(("op", op))
                self.i += len(op)
                if op in ("<<", "<<-"):
                    self.delimiter = (len(self.tokens) - 1, op == "<<-")
                if op == "\n":
                    self.read_bodies()
        self.end()
        self.read_bodies()

    def line_end(self):
        """Where the line the reader is on ends."""
        newline = self.s.find("\n", self.i)
        return len(self.s) if newline < 0 else newline

    def read_bodies(self):
        """The bodies of the here-documents the line just ended named, each
        to the line holding its delimiter alone."""
        for at, delimiter, strip, expands in self.waiting:
            lines = []
            while self.i < len(self.s):
                newline = self.s.find("\n", self.i)
                stop = len(self.s) if newline < 0 else newline
                line = self.s[self.i:stop]
                self.i = stop + 1
                if (line.lstrip("\t") if strip else line) == delimiter:
                    break
                lines.append(line)
            body = "\n".join(lines)
            self.bodies[at] = body
            if expands:
                # An unquoted delimiter's body has its substitutions run.
                self.inner += substitutions(body, self.pwsh)
        self.waiting = []

    def closing(self, at):
        """Where the parenthesis opening at `at` closes -- quotes, escapes
        and nested ones passed over -- or the text's end."""
        s, depth, i = self.s, 0, at
        escape = "`" if self.pwsh else "\\"
        while i < len(s):
            c = s[i]
            if c == escape:
                i += 2
                continue
            if c == "'":
                close = s.find("'", i + 1)
                i = len(s) if close < 0 else close + 1
                continue
            if c == '"':
                i = self.past_double(i)
                continue
            if c == "(":
                depth += 1
            elif c == ")":
                depth -= 1
                if depth == 0:
                    return i
            i += 1
        return len(s)

    def past_double(self, at):
        """The index after the double-quoted text opening at `at`."""
        s, i = self.s, at + 1
        escape = "`" if self.pwsh else "\\"
        while i < len(s):
            if s[i] == escape:
                i += 2
            elif s[i] == '"':
                return i + 1
            elif s.startswith("$(", i):
                i = self.closing(i + 1) + 1
            else:
                i += 1
        return len(s)

    def double(self):
        """A double-quoted text's characters; its substitutions run."""
        s, out = self.s, ""
        self.i += 1
        escape = "`" if self.pwsh else "\\"
        while self.i < len(s):
            c = s[self.i]
            if c == '"':
                self.i += 1
                return out
            if c == escape:
                nxt = s[self.i + 1:self.i + 2]
                if self.pwsh or nxt in '$`"\\':
                    out += nxt
                elif nxt != "\n":
                    out += c + nxt
                self.i += 2
            elif c == "$":
                out += self.dollar()
            elif c == "`" and not self.pwsh:
                out += self.backquote()
            else:
                out += c
                self.i += 1
        return out

    def dollar(self):
        """What a `$` begins: a substitution, whose script runs, an
        arithmetic expansion, a parameter's, or ANSI-C quoted text."""
        s, at = self.s, self.i
        if s.startswith("$((", at) and not self.pwsh:
            close = self.closing(at + 1)
            self.i = close + 1
            return s[at:close + 1]
        if s.startswith("$(", at):
            close = self.closing(at + 1)
            self.inner.append(s[at + 2:close])
            self.i = close + 1
            return s[at:close + 1]
        if s.startswith("${", at):
            depth, i = 0, at + 1
            while i < len(s):
                if s[i] == "{":
                    depth += 1
                elif s[i] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                elif s[i] in "'\"":
                    close = s.find(s[i], i + 1)
                    i = len(s) if close < 0 else close
                i += 1
            self.i = min(i + 1, len(s))
            return s[at:self.i]
        if s.startswith("$'", at) and not self.pwsh:
            i, out = at + 2, ""
            while i < len(s) and s[i] != "'":
                out += s[i:i + 2] if s[i] == "\\" else s[i]
                i += 2 if s[i] == "\\" else 1
            self.i = i + 1
            self.quoted = True
            return out
        self.i = at + 1
        return "$"

    def backquote(self):
        """A backquoted substitution, whose script runs."""
        s, i, inner = self.s, self.i + 1, ""
        while i < len(s) and s[i] != "`":
            if s[i] == "\\" and s[i + 1:i + 2] in ("`", "\\", "$"):
                inner += s[i + 1]
                i += 2
                continue
            inner += s[i]
            i += 1
        self.inner.append(inner)
        self.i = i + 1
        return "`" + inner + "`"

    def here_string(self, quote):
        """A PowerShell here-string, `@'` or `@"` to a line opening with `'@`
        or `"@`: text, whose substitutions run in the double-quoted kind."""
        s = self.s
        start = s.find("\n", self.i) + 1
        close = s.find("\n" + quote + "@", start - 1)
        stop = len(s) if close < 0 else close
        body = s[start:stop]
        if quote == '"':
            self.inner += substitutions(body, True)
        self.i = len(s) if close < 0 else close + 3
        return body


def substitutions(text, pwsh):
    """The scripts of the substitutions in text that is otherwise data (a
    here-document's body): `$(...)`, and in a POSIX shell's backquotes."""
    found, at = [], 0
    reader = Shell(text, pwsh)
    while at < len(text):
        if text.startswith("$(", at) and not text.startswith("$((", at):
            close = reader.closing(at + 1)
            found.append(text[at + 2:close])
            at = close + 1
        elif text[at] == "`" and not pwsh:
            close = text.find("`", at + 1)
            close = len(text) if close < 0 else close
            found.append(text[at + 1:close])
            at = close + 1
        else:
            at += 1
    return found


def parse(tokens, bodies):
    """A script's tokens as its simple commands: (words, here-document
    bodies) for each. Keywords and assignments before a command, a
    redirection and the file it names, a `for` loop's list and a `case`'s
    patterns are no words of one."""
    out, words, read = [], [], []
    cases = []  # each open `case`: "head", "pattern" or "body"
    skip = False  # in a `for` or `select` head, to its line's end

    def flush():
        nonlocal words, read
        if words or read:
            out.append((words, read))
        words, read = [], []

    at = 0
    while at < len(tokens):
        kind, text = tokens[at]
        if cases and cases[-1] in ("head", "pattern"):
            # `case WORD in`, then patterns up to their `)`, or its `esac`.
            if cases[-1] == "head" and kind == "word" and text == "in":
                cases[-1] = "pattern"
            elif cases[-1] == "pattern" and kind == "word" and text == "esac":
                cases.pop()
            elif cases[-1] == "pattern" and kind == "op" and text == ")":
                cases[-1] = "body"
            at += 1
            continue
        if kind == "op":
            if text in REDIRECTIONS:
                if at in bodies:
                    read.append(bodies[at])
                elif at + 1 < len(tokens) and tokens[at + 1][0] == "word":
                    at += 1  # the file it names
            else:
                flush()
                skip = False
                if text in CASE_ENDS and cases:
                    cases[-1] = "pattern"
            at += 1
            continue
        if skip:
            at += 1
            continue
        if not words:
            if text == "case":
                cases.append("head")
                at += 1
                continue
            if text == "esac" and cases:
                cases.pop()
                at += 1
                continue
            if text in ("for", "select", "function"):
                skip = text != "function"
                at += 2 if text == "function" else 1
                continue
            if text in KEYWORDS or ASSIGNMENT.match(text):
                at += 1
                continue
        words.append(text)
        at += 1
    flush()
    return out


# Commands that run another before them (`sudo cargo build`), and their
# options that take a value of their own.
WRAPPERS = {
    "sudo": {"-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U", "-T", "--user", "--group"},
    "doas": {"-u", "-C"},
    "env": {"-u", "-C", "-S", "--unset", "--chdir", "--split-string"},
    "nohup": set(), "exec": {"-a"}, "builtin": set(), "time": {"-f", "-o"},
    "nice": {"-n", "--adjustment"}, "stdbuf": {"-i", "-o", "-e"},
    "xargs": {"-a", "-d", "-E", "-I", "-L", "-n", "-P", "-s", "--arg-file", "--delimiter",
              "--max-args", "--max-procs", "--max-chars", "--replace"},
    "timeout": {"-k", "-s", "--kill-after", "--signal"}, "command": set(),
}
SHELLS = {"sh", "bash", "dash", "zsh", "ksh", "ash"}
POWERSHELLS = {"pwsh", "powershell"}
INTERPRETERS = SHELLS | POWERSHELLS | {"python", "python3", "node", "deno", "bun", "ruby", "perl", "source", "."}


def name_of(word):
    """A command's name as its program is called, path and `.exe` taken off."""
    name = posixpath.basename(word.replace("\\", "/")).lower()
    return re.sub(r"\.(?:exe|cmd|bat)$", "", name)


def operands(args, valued=()):
    """A command's words that are no option, an option's value passed over."""
    out, at = [], 0
    while at < len(args):
        word = args[at]
        if word == "--":
            return out + args[at + 1:]
        if word.startswith("-") and len(word) > 1:
            at += 2 if word in valued else 1
            continue
        out.append(word)
        at += 1
    return out


def unwrapped(words):
    """The command a command runs: past `sudo`, `env` and the like, their
    options and assignments; [] for `command -v`, which runs nothing."""
    while words:
        while words and ASSIGNMENT.match(words[0]):
            words = words[1:]
        if not words:
            return []
        name = name_of(words[0])
        if name not in WRAPPERS:
            return words
        if name == "command" and any(w in ("-v", "-V") for w in words[1:2]):
            return []
        at = 1
        while at < len(words) and words[at].startswith("-") and len(words[at]) > 1:
            if words[at] == "--":
                at += 1
                break
            at += 2 if words[at] in WRAPPERS[name] else 1
        if name == "timeout" and at < len(words):
            at += 1  # its duration
        words = words[at:]
    return words


def handed(words, read):
    """The scripts a command hands a shell to run: `sh -c '...'`, `eval
    ...`, `pwsh -Command '...'`, or the here-documents a shell reads as its
    script. Each (text, whether PowerShell's)."""
    name = name_of(words[0])
    args = words[1:]
    if name == "eval":
        return [(" ".join(args), False)]
    if name in SHELLS:
        given = operands(args, {"-o", "+o", "-O", "+O", "--rcfile", "--init-file"})
        if any(re.fullmatch(r"-[A-Za-z]*c[A-Za-z]*", a) for a in args):
            return [(given[0], False)] if given else []
        return [] if given else [(body, False) for body in read]
    if name in POWERSHELLS:
        for at, a in enumerate(args):
            if a.lower() in ("-command", "-c") and at + 1 < len(args):
                return [(" ".join(args[at + 1:]), True)]
    return []


# A script, by its extension, and one named for building or packaging
# (`build-aar.sh`, `package.sh`, `linux-packages.sh`, `pack.ps1`).
SCRIPT = re.compile(r"\.(?:sh|bash|ps1|py|js|mjs|cjs|ts)$", re.I)
BUILD_NAME = re.compile(r"build|packag|(?:^|[-_.])pack(?:[-_.]|$)", re.I)
NPM = {"install", "i", "in", "add", "ci", "clean-install", "ic", "install-clean", "isntall", "rebuild", "rb",
       "update", "up", "upgrade", "dedupe", "link", "ln", "run", "run-script", "rum", "urn", "test", "t", "tst",
       "start", "stop", "restart", "install-test", "it", "install-ci-test", "cit", "exec", "x", "pack", "build"}
# pnpm and yarn run a package's script by its bare name (`yarn lint`), so
# every command of theirs builds but these.
PNPM_YARN_READS = {"publish", "npm", "info", "view", "config", "cache", "version", "why", "list", "ls",
                   "outdated", "audit", "login", "logout", "whoami", "help"}
SUBCOMMANDS = {
    "swift": {"build", "test", "run"},
    "dotnet": {"build", "pack", "publish", "test", "run", "restore", "msbuild"},
    "go": {"build", "install", "run", "test", "generate"},
    "pip": {"install", "wheel", "download"},
    "pip3": {"install", "wheel", "download"},
    "flutter": {"build", "test", "run", "drive"},
    "dart": {"compile", "run", "test"},
}
ANY_USE = {"cargo": "cargo", "gradle": "gradle", "gradlew": "gradle", "mvn": "maven", "mvnw": "maven",
           "make": "make", "gmake": "make", "xcodebuild": "xcodebuild", "npx": "npx", "bunx": "npx"}
DOCKER_VALUED = {"-H", "--host", "-c", "--context", "--config", "-l", "--log-level",
                 "--tlscacert", "--tlscert", "--tlskey"}


def build_of(words):
    """What a command builds, named for a finding, or None."""
    name = name_of(words[0])
    args = words[1:]
    if name in ANY_USE:
        return ANY_USE[name]
    given = operands(args)
    first = given[0] if given else None
    if name == "npm" and first in NPM:
        return "an npm, pnpm or yarn install, script or build"
    if name in ("pnpm", "yarn") and not set(args) & {"--version", "-v", "--help", "-h"} \
            and (first not in PNPM_YARN_READS if first else name == "yarn"):
        return "an npm, pnpm or yarn install, script or build"
    if name in SUBCOMMANDS and first in SUBCOMMANDS[name]:
        return f"{'pip' if name == 'pip3' else name} {first}"
    if name == "docker":
        given = operands(args, DOCKER_VALUED)
        if given[:1] in (["build"], ["bake"]) or given[:2] in (
                ["buildx", "build"], ["buildx", "bake"], ["image", "build"], ["builder", "build"]) \
                or given[:1] == ["compose"] and ("build" in given or "--build" in args):
            return "docker build"
    if name in ("python", "python3"):
        if args[:2] == ["-m", "build"]:
            return "python -m build"
        if args[:1] == ["-m"] and args[1:2] in (["pip"], ["pip3"]) and args[2:3] and args[2] in SUBCOMMANDS["pip"]:
            return f"pip {args[2]}"
    script = None
    if SCRIPT.search(words[0]):
        script = words[0]
    elif name in INTERPRETERS:
        script = next((a for a in operands(args, {"-m", "-c", "-e", "-o", "-Command", "-command"})
                       if SCRIPT.search(a)), None)
    if script and posixpath.basename(script.replace("\\", "/")).lower() == "setup.py":
        return "setup.py"
    if script and BUILD_NAME.search(posixpath.basename(script.replace("\\", "/"))):
        return "a build or package script"
    return None


def run_commands(text, pwsh=False, depth=0):
    """Every command a `run:` script runs, as its words: its own, theirs
    inside its substitutions, and those of each script one of them hands a
    shell."""
    out = []
    for words, read in Shell(text, pwsh).commands():
        words = unwrapped(words)
        if not words:
            continue
        out.append(words)
        if depth < 8:
            for script, ps in handed(words, read):
                out += run_commands(script, ps, depth + 1)
    return out


# Actions that build, or set up what only a build runs: a job that sets
# Gradle up runs it.
BUILD_ACTIONS = re.compile(r"(?:docker/(?:build-push|bake)-action|gradle/actions/setup-gradle|gradle/gradle-build-action)@")


def shell_of(step, job, doc):
    """The shell a step's `run:` is read with: its own `shell:`, its job's
    or its workflow's `defaults`, else what the runner starts (PowerShell on
    Windows)."""
    def default(holder):
        defaults = holder.get("defaults")
        run = defaults.get("run") if isinstance(defaults, Mapping) else None
        return run.get("shell") if isinstance(run, Mapping) else None

    for shell in (step.get("shell"), default(job), default(doc)):
        if isinstance(shell, Scalar) and shell.strip():
            return shell.strip().split()[0].lower()
    return "pwsh" if "windows" in str(job.get("runs-on", "")).lower() else "bash"


def builds(job, doc=None):
    """What a job runs that builds: (line, what) for each, once a step."""
    found = []
    for step in steps(job):
        uses = step.get("uses")
        if isinstance(uses, Scalar) and BUILD_ACTIONS.match(uses):
            found.append((uses.line, str(uses)))
        if isinstance(uses, Scalar) and str(uses).strip().startswith("./"):
            found.append((uses.line, f"the local action {str(uses).strip()}, whose steps this check"
                                     " does not read"))
        run = step.get("run")
        if isinstance(run, Scalar):
            pwsh = shell_of(step, job, doc or Mapping(1)) in ("pwsh", "powershell")
            for words in run_commands(str(run), pwsh):
                what = build_of(words)
                if what and (run.line, what) not in found:
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
    base = pathlib.PurePath(name).name
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
        built = builds(job, doc) if held else []
        if (base, job_id) in WRITE_AND_BUILD:
            if not built:
                findings.append(f"{where}: WRITE_AND_BUILD lets it off, and it runs no build beside a"
                                " write: take it out")
        else:
            for line, what in built:
                findings.append(f"{name}:{line}: job `{job_id}` holds {', '.join(held)} write and runs"
                                f" {what}: build in a job that reads alone, and hand what it made"
                                " to the job that writes as an artifact")

        read_ = secrets_read(job)
        if str(job.get("secrets", "")).strip() == "inherit":
            read_.append((job.lines["secrets"], "every secret (`secrets: inherit`)"))
        environment = job.get("environment")
        named = bool(str(environment.get("name", "")).strip()) if isinstance(environment, Mapping) \
            else bool(str(environment or "").strip())
        unnamed = read_ if not named else []
        if (base, job_id) in REPOSITORY_SECRETS:
            if not unnamed:
                findings.append(f"{where}: REPOSITORY_SECRETS lets it off, and it reads no stored secret"
                                " outside an environment: take it out")
        else:
            for line, secret in unnamed:
                findings.append(f"{name}:{line}: job `{job_id}` reads secrets.{secret} and names no"
                                " `environment:`: keep the secret in an environment whose rules"
                                " say which runs may read it, and name it here")

    # A job let off that is gone lets nothing off, and would let off the
    # next job given its name.
    for listed, allowlist in (("REPOSITORY_SECRETS", REPOSITORY_SECRETS), ("WRITE_AND_BUILD", WRITE_AND_BUILD)):
        for workflow_, job_id in allowlist:
            if workflow_ == base and job_id not in jobs:
                findings.append(f"{name}:1: {listed} lets off job `{job_id}`, which this workflow does not have")
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
    let_off = len(REPOSITORY_SECRETS) + len(WRITE_AND_BUILD) + len(MAJOR_TAGS)
    print(f"workflow_check: {len(paths)} workflows, every action pinned by its commit, every"
          " workflow's permissions said, no job that writes builds, every secret read under an environment"
          + (f" ({let_off} let off by name, each with its reason)" if let_off else ""))


if __name__ == "__main__":
    main()
