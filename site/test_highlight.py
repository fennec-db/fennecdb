#!/usr/bin/env python3
"""The playground's highlighter (`highlight.js`, as `build.py` writes it)
against the docs' (`build.highlight`), over every FennecQL block the site shows
and a few statements written to reach the edges: the same HTML, byte for byte.

    python3 site/test_highlight.py      # needs node on PATH
"""

import glob
import html
import json
import os
import re
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, ROOT)
import build  # noqa: E402

SAMPLES = [
    'create collection notes (body text @text, n int, embed vector<3> @hnsw(cosine))',
    'put notes {body: "Night at the oasis", n: 1, embed: [0.1, 0.2, 0.3]}',
    'get notes select body\n  match body "oasis" near embed $1 fuse\n  limit 10',
    'set notes {n: n + 1} where id = 7',
    "get notes where body = 'it\\'s' and n >= -1.5e3 -- a comment\n  order n desc",
    'get notes where body ~ "şehir" and note_2 = 12_000 and x1 = 1.25',
    'get notes where body = "unterminated\n  limit 5',
    'get notes where tag = "<b>&amp;</b>" and q = \'"\'',
    'get notes select highlight(body), snippet(body, 12) match body "kuş ağacı"',
    'çok güzel İstanbul 東京 ü2 aéb',
    "create statement nightly at '0 3 * * *' in 'Europe/Berlin' as set plans {used: 0, period: slot()}",
    'create statement tidy every 1h as del sessions where at < now() order at limit 1000',
    "get sales select bucket(at, 1h) as hour where at > '2026-01-01' and format = 'at'",
    "create statement late at\n  '0 3 * * *' as del t where at < now()",
    'create statement nightly at "0 3 * * *" as del t where at < now()',
    "create statement paid($id text from event.data.object.id, $e json from event) on hook stripe\n"
    "  where event.type = 'payment_intent.succeeded' as set orders {status: 'paid'} where intent = $id require 1",
    "create statement pushed($kind text from hook.event) on hook github where hook.event = 'push' as insert pushes {kind: $kind}",
    'create collection deliveries (event text @unique, at timestamp @ttl(1d))',
    'put deliveries {event: "evt_1J8", at: now()} if absent',
    '',
]


def site_blocks():
    """The body of every `data-lang="fennecql"` block, as build.py reads it."""
    out = []
    for path in glob.glob(os.path.join(ROOT, "content", "**", "*.html"), recursive=True):
        text = open(path, encoding="utf-8").read()
        for m in build.PRE_RE.finditer(text):
            if 'data-lang="fennecql"' in m.group("attrs"):
                out.append(html.unescape(m.group("body")).strip("\n"))
    return out


def main():
    samples = SAMPLES + site_blocks()
    with tempfile.TemporaryDirectory() as tmp:
        mod = os.path.join(tmp, "highlight.mjs")
        open(mod, "w", encoding="utf-8").write(build.highlight_js())
        run = subprocess.run(
            ["node", "--input-type=module", "-e",
             f"import {{ highlight }} from {json.dumps('file://' + mod)};"
             "let s = ''; process.stdin.on('data', (d) => s += d);"
             "process.stdin.on('end', () => process.stdout.write(JSON.stringify("
             "JSON.parse(s).map((c) => highlight(c, 'fennecql')))));"],
            input=json.dumps(samples), capture_output=True, text=True)
    if run.returncode != 0:
        raise SystemExit(run.stderr)
    got = json.loads(run.stdout)
    bad = [(s, g, build.highlight(s, "fennecql"))
           for s, g in zip(samples, got) if g != build.highlight(s, "fennecql")]
    for s, g, want in bad[:5]:
        print(f"differs on {s!r}:\n  js: {g}\n  py: {want}")
    if bad:
        raise SystemExit(f"{len(bad)} of {len(samples)} statements highlight differently")
    # The editor colours a line at a time: no FennecQL token may span one.
    for s in samples:
        whole = build.highlight(s, "fennecql")
        by_line = "\n".join(build.highlight(ln, "fennecql") for ln in s.split("\n"))
        if whole != by_line:
            raise SystemExit(f"a token spans a line in {s!r}: the editor would colour it apart")
    # `at` is a keyword before a schedule's text alone, in either quotes; a
    # field elsewhere.
    kw_at = '<span class="t-kw">at</span>'
    for s in ("create statement n at '0 3 * * *' as del t", 'create statement n at "0 3 * * *" as del t'):
        if kw_at not in build.highlight(s, "fennecql"):
            raise SystemExit(f"`at` before a schedule's text is no keyword in {s!r}")
    if kw_at in build.highlight("get sales select bucket(at, 1h) where at > $1", "fennecql"):
        raise SystemExit("`at` is a keyword where it is a field")
    # `hook` is a keyword, and `event` before the dot of a path into the
    # event alone; a field `event` elsewhere.
    kw_event, kw_hook = '<span class="t-kw">event</span>', '<span class="t-kw">hook</span>'
    hooked = build.highlight("create statement p($id text from event.data.id) on hook stripe where event.type = 'x' as del t", "fennecql")
    if hooked.count(kw_event) != 2 or kw_hook not in hooked:
        raise SystemExit(f"a hook's `event.` and `hook` are no keywords in {hooked!r}")
    for s in ('create collection deliveries (event text @unique)', 'put deliveries {event: "evt_1J8"}', "get deliveries where event = 'evt_1J8'"):
        if kw_event in build.highlight(s, "fennecql"):
            raise SystemExit(f"`event` is a keyword where it is a field in {s!r}")
    # The rules are written in, not copied: a keyword the docs know is one here.
    assert re.search(r'"fuse"', build.highlight_js())
    # fennec studio serves the file as the repository holds it: the one
    # build.py writes, or the studio's editor would colour by other rules.
    # The Grafana data source's editor is held to the same file.
    for path in (build.STUDIO_HIGHLIGHT, build.GRAFANA_HIGHLIGHT):
        kept = open(path, encoding="utf-8").read()
        if kept != build.highlight_js():
            raise SystemExit(f"{os.path.relpath(path, build.REPO)} is not what build.py writes: make studio-highlight")
    # Its mirror is made of text nodes: no markup is set from either file.
    for name in ("highlight.js",):
        src = open(os.path.join(ROOT, name), encoding="utf-8").read()
        if "innerHTML" in src:
            raise SystemExit(f"site/{name} sets markup: the studio's policy is text alone")
    print(f"{len(samples)} statements highlight the same in the docs and the editor")


if __name__ == "__main__":
    main()
