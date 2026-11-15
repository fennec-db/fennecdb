#!/usr/bin/env python3
"""tools/fennecql_corpus.py: the texts it harvests from each kind of file
and the hand-written list, how it judges one reading against another --
word by word, a panic failing whatever BASE read -- what lets a change
through -- an ACCEPTED entry naming both readings, and a grammar epoch
raised where a digest moved or a 1.x release's text is refused -- what a
failure prints, the frozen corpora written, read back and run every one,
each tree's build its own, and the committed corpus itself
(docs/design/fennecql.md#the-corpus). One test builds two crates of a few
lines; the rest builds nothing.

    python3 tools/test_fennecql_corpus.py
"""

import importlib.util
import io
import json
import os
import re
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("fennecql_corpus", os.path.join(HERE, "fennecql_corpus.py"))
corpus = importlib.util.module_from_spec(spec)
spec.loader.exec_module(corpus)

C1, C2 = "c" + "1" * 16, "c" + "2" * 16
S1, S2 = "s" + "a" * 16, "s" + "b" * 16
D1, D2, D3 = "d" + "3" * 16, "d" + "4" * 16, "d" + "5" * 16


def harvested(kind, text, where):
    """The texts `kind`'s reader finds in `text`, each once, as `harvest`
    keeps them: a block whole, then each of its lines that begins a
    statement."""
    out = []

    def add(t, w):
        if t.strip() not in out:
            out.append(t.strip())

    if kind == "html":
        corpus.from_html(add, text, where)
    elif kind == "md":
        corpus.from_markdown(add, text, where)
    elif kind == "json":
        corpus.from_json(add, json.loads(text), where)
    else:
        corpus.from_source(add, text, where, corpus.SOURCES[os.path.splitext(where)[1]])
    return out


def harvested_list(text):
    out = []
    corpus.from_list(lambda t, w: out.append(t.strip()), text, corpus.LISTED)
    return out


class Harvest(unittest.TestCase):
    def test_a_statement_begins_with_one_of_its_words(self):
        for text in ("get t", "  SELECT 1", "Create collection t (a int)", "statements"):
            self.assertTrue(corpus.begins_statement(text), text)
        for text in ("getter", "the get", "", "1 + 1", "-- get t"):
            self.assertFalse(corpus.begins_statement(text), text)

    def test_the_docs_blocks_and_inline_code(self):
        page = ('<pre data-lang="fennecql">get t\n<span class="k">put</span> t {a: 1}</pre>'
                "<p><code>del t where a &lt; 2</code> and <code>not one</code></p>")
        self.assertEqual(harvested("html", page, "site/content/docs/x.html"),
                         ["get t\nput t {a: 1}", "get t", "put t {a: 1}", "del t where a < 2"])

    def test_markdown_blocks_and_spans_outside_them(self):
        doc = "Say `get a`, not `got a`.\n\n```fennecql\nset t {a: 1} where id = 2\n```\n\n```sh\nget b\n```\n"
        self.assertEqual(harvested("md", doc, "docs/x.md"), ["set t {a: 1} where id = 2", "get a"])

    def test_a_golden_files_strings(self):
        found = harvested("json", '{"cases": [{"fennecql": "get t limit $1"}, "plain words"]}', "integrations/g.json")
        self.assertEqual(found, ["get t limit $1"])

    def test_rust_strings_raw_and_escaped_and_comments_spans(self):
        src = ('let a = "get t where s = \\"x\\"";\n'
               'let b = r#"put t {a: "b"}"#;\n'
               '// Reads `del t where id = 1` first.\n'
               "let c = 'g'; let d = \"getter\";\n")
        self.assertEqual(harvested("rs", src, "crates/x.rs"),
                         ['get t where s = "x"', 'put t {a: "b"}', "del t where id = 1"])

    def test_python_triple_quotes_and_js_templates(self):
        self.assertEqual(harvested("py", "q = '''get t\nwhere a = 1'''\n", "x.py"),
                         ["get t\nwhere a = 1", "get t"])
        self.assertEqual(harvested("js", "run(`insert t {a: ${n}}`)", "web/x.js"), ["insert t {a: ${n}}"])

    def test_the_hand_written_list_a_text_a_line(self):
        found = harvested_list("# a comment\nget a\n\n  BEGIN\nput t {a: 1}; get t\n")
        self.assertEqual(found, ["get a", "BEGIN", "put t {a: 1}; get t"])

    def test_a_texts_kind(self):
        for text, k in (("get t", "get"), ("SELECT * FROM t", "select"), ("Insert Into t values (1)", "insert into"),
                        ("insert t {a: 1}", "insert"), ("create or replace statement f() as get t", "create or replace statement"),
                        ("CREATE UNIQUE INDEX ON t (a)", "create unique index"), ("create index on t (a) @hash", "create index"),
                        ("explain select 1", "explain select"), ("describe statement f", "describe statement"),
                        ("describe t", "describe"), ("CALL f(1)", "call (sql)"), ("call f {a: 1}", "call"),
                        ("call f", "call"), ("delete from t", "delete from"), ("START TRANSACTION", "start transaction"),
                        ("drop table t", "drop table"), ("collections; statements", "collections")):
            self.assertEqual(corpus.kind(text), k, text)

    def test_the_list_holds_five_of_every_kind(self):
        texts = corpus.harvest(corpus.ROOT, [corpus.LISTED])
        counts = {}
        for t in texts:
            counts[corpus.kind(t["text"])] = counts.get(corpus.kind(t["text"]), 0) + 1
        thin = {k: counts.get(k, 0) for k in corpus.REQUIRED_KINDS if counts.get(k, 0) < 5}
        self.assertEqual(thin, {})
        self.assertTrue(all(t["from"] == corpus.LISTED for t in texts))

    def test_the_list_holds_two_of_every_refusal_by_name(self):
        texts = [t["text"] for t in corpus.harvest(corpus.ROOT, [corpus.LISTED])]
        counts = {name: sum(1 for t in texts if re.match(rx, t, re.I)) for name, rx in corpus.REFUSED_KINDS}
        self.assertEqual({k: n for k, n in counts.items() if n < 2}, {})
        # Each pattern is the path it names, and no other's.
        for text, name in (("create function f() returns int", "create function"),
                           ("CREATE OR REPLACE PROCEDURE p()", "create or replace function"),
                           ("create or replace view v as select 1", "create or replace, no statement"),
                           ("create unlogged table t (a int)", "create temp table"),
                           ("users join orders", "a name, then join"),
                           ("alter collection u modify a text", "alter ... modify"),
                           ("alter collection u alter field a @hash", "alter field @other"),
                           ("alter collection u owner to x", "alter collection, another change"),
                           ("explain put t {a: 1}", "explain, no get")):
            found = [n for n, rx in corpus.REFUSED_KINDS if re.match(rx, text, re.I)]
            self.assertEqual(found, [name], text)
        matched = lambda text: [n for n, rx in corpus.REFUSED_KINDS if re.match(rx, text, re.I)]  # noqa: E731
        self.assertEqual(matched("inner join orders"), ["a leading join"])
        for text in ("create or replace statement f() as get t", "alter collection u add a int",
                     "alter collection u alter field a @ttl(1d)", "explain get t", "get t", "create table t (a int)",
                     "alter collection u rename a to b"):
            self.assertEqual(matched(text), [], text)

    def test_both_trees_texts_are_read(self):
        # A change that edits away the texts showing it is still read: the
        # base tree's texts join this tree's, each once.
        head = [{"text": "get a", "from": "docs/a.md"}]
        base = [{"text": "get a", "from": "docs/old.md"}, {"text": "get gone", "from": "docs/old.md"}]
        self.assertEqual(corpus.merged(head, base), [{"text": "get a", "from": "docs/a.md"},
                                                     {"text": "get gone", "from": "docs/old.md"}])

    def test_a_tree_is_harvested_by_the_paths_named(self):
        with tempfile.TemporaryDirectory() as root:
            os.makedirs(os.path.join(root, "docs"))
            for path, body in (("docs/a.md", "`get a`"), ("README.md", "`get a` and `get b`"),
                               ("target/x.md", "`get c`"), ("docs/skip.md", "`get d`")):
                os.makedirs(os.path.dirname(os.path.join(root, path)), exist_ok=True)
                with open(os.path.join(root, path), "w") as f:
                    f.write(body)
            texts = corpus.harvest(root, ["docs/a.md", "README.md", "target/x.md", "gone.md"])
        self.assertEqual(texts, [{"text": "get a", "from": "docs/a.md"},
                                 {"text": "get b", "from": "README.md"}])


class Judge(unittest.TestCase):
    def test_each_verdict(self):
        self.assertEqual(corpus.judge(f"ok {C1}", f"ok {C1}"), "same")
        self.assertEqual(corpus.judge(f"ok {C1}", f"ok {C2}"), "changed")
        self.assertEqual(corpus.judge(f"ok {C1} {D1}", f"ok {C1}"), "changed")
        self.assertEqual(corpus.judge("err query error: no", f"ok {C1}"), "refused")
        self.assertEqual(corpus.judge(f"ok {C1}", "err query error: no"), "parses")
        self.assertEqual(corpus.judge("err query error: a", "err query error: b"), "reworded")
        self.assertEqual(corpus.judge("-", "-"), "same")

    def test_a_panic_is_a_panic_whatever_base_read(self):
        for base in (f"ok {C1}", "err", "err query error: no", "-", "panic"):
            self.assertEqual(corpus.judge("panic", base), "panic", base)

    def test_a_frozen_refusal_is_every_refusal(self):
        # A frozen corpus keeps no refusal's words, which may change.
        self.assertEqual(corpus.judge("err query error: a", "err"), "same")
        self.assertEqual(corpus.judge("err", "err query error: a"), "same")

    def test_what_moves_a_digest(self):
        moves = corpus.moves_digest
        self.assertTrue(moves((f"ok {C2}", f"ok {C2}", "-"), (f"ok {C1}", f"ok {C1}", "-")))
        self.assertTrue(moves((f"ok {S2}", f"ok {S2}", f"ok {S2}"), (f"ok {S1}", f"ok {S1}", f"ok {S1}")))
        self.assertTrue(moves(("err x", "err x", "err x"), (f"ok {S1}", f"ok {S1}", f"ok {S1}")))
        # A statement no definition holds, read otherwise; a definition
        # refused afresh whose stored reading holds -- the row the example
        # writes where `parse` refuses and `stored_definition` reads.
        self.assertFalse(moves((f"ok {D2}", f"ok {D2}", "-"), (f"ok {D1}", f"ok {D1}", "-")))
        self.assertFalse(moves(("err x", "err x", f"ok {S1}"), (f"ok {S1}", f"ok {S1}", f"ok {S1}")))
        self.assertFalse(moves(("err x", "err x", "-"), (f"ok {C1}", f"ok {C1}", "-")))
        # Refused by every reading, the stored one too: an open marks it
        # broken.
        self.assertTrue(moves(("err x", "err x", "-"), (f"ok {S1}", f"ok {S1}", f"ok {S1}")))

    def test_a_digest_moves_word_by_word(self):
        moves = corpus.moves_digest
        # `get notes count; call titles $1`: the `call` read otherwise, the
        # `get` as it was.
        self.assertFalse(moves((f"ok {C1} {D2}", f"ok {C1} {D2}", "-"), (f"ok {C1} {D1}", f"ok {C1} {D1}", "-")))
        self.assertTrue(moves((f"ok {C2} {D1}", f"ok {C2} {D1}", "-"), (f"ok {C1} {D1}", f"ok {C1} {D1}", "-")))
        self.assertTrue(moves((f"ok {C1} {D1}", f"ok {C1} {D1}", "-"), (f"ok {C1} {C1}", f"ok {C1} {C1}", "-")))
        # Read as another number of statements: one holding a digest moves.
        self.assertTrue(moves((f"ok {C1} {D1}", f"ok {C1} {D1}", "-"), (f"ok {C1}", f"ok {C1}", "-")))
        self.assertFalse(moves((f"ok {D1} {D2}", f"ok {D1} {D2}", "-"), (f"ok {D1}", f"ok {D1}", "-")))


def text(t):
    return {"text": t, "from": "here"}


def entry_of(t, base, head):
    return {"text": t, "base": [corpus.norm(r) for r in base], "head": [corpus.norm(r) for r in head],
            "pr": "#1", "why": "on purpose"}


class Compare(unittest.TestCase):
    def compare(self, head, base, accepted=(), epochs=(4, 4), major=0):
        same, groups = corpus.compare([text("q")], [head], [base], *epochs, accepted, major)
        return {k: len(v) for k, v in groups.items() if v}

    def test_alike_passes(self):
        row = (f"ok {C1}", f"ok {C1}", "-")
        self.assertEqual(self.compare(row, row), {})

    def test_a_change_and_a_refusal_fail_and_a_new_reading_is_told(self):
        base = (f"ok {C1}", f"ok {C1}", "-")
        self.assertEqual(self.compare((f"ok {C2}", f"ok {C2}", "-"), base), {"changed": 1})
        self.assertEqual(self.compare(("err no", "err no", "-"), base), {"refused": 1})
        self.assertEqual(self.compare(base, ("err no", "err no", "-")), {"parses": 1})
        self.assertEqual(self.compare(("err a", "err a", "-"), ("err b", "err b", "-")), {"reworded": 1})

    def test_a_panic_fails(self):
        self.assertEqual(self.compare(("panic", "panic", "panic"), ("err no", "err no", "-")), {"panics": 1})
        self.assertEqual(self.compare(("panic", "err", "-"), ("panic", "err", "-")), {"panics": 1})
        # No entry lets one through.
        entry = [entry_of("q", ("err", "err", "-"), ("panic", "panic", "panic"))]
        self.assertEqual(self.compare(("panic", "panic", "panic"), ("err", "err", "-"), entry),
                         {"panics": 1, "unmatched": 1})

    def test_an_entry_lets_through_what_moves_no_digest(self):
        d_head, d_base = (f"ok {D2}", f"ok {D2}", "-"), (f"ok {D1}", f"ok {D1}", "-")
        self.assertEqual(self.compare(d_head, d_base, [entry_of("q", d_base, d_head)]), {"accepted": 1})
        r_head, r_base = ("err no", "err no", f"ok {S1}"), (f"ok {S1}", f"ok {S1}", f"ok {S1}")
        self.assertEqual(self.compare(r_head, r_base, [entry_of("q", r_base, r_head)]), {"accepted": 1})
        # A refusal's words are free: the entry names `err`.
        self.assertEqual(self.compare(("err other words", "err x", f"ok {S1}"), r_base, [entry_of("q", r_base, r_head)]),
                         {"accepted": 1})

    def test_an_entry_names_one_change(self):
        base = (f"ok {D1}", f"ok {D1}", "-")
        once = [entry_of("q", base, (f"ok {D2}", f"ok {D2}", "-"))]
        # A later change of the same text is another change: it fails, and
        # the entry approves nothing.
        self.assertEqual(self.compare((f"ok {D3}", f"ok {D3}", "-"), base, once), {"changed": 1, "unmatched": 1})
        self.assertEqual(self.compare((f"ok {D2}", f"ok {D2}", "-"), (f"ok {D3}", f"ok {D3}", "-"), once),
                         {"changed": 1, "unmatched": 1})
        # Nothing changed: an entry naming no change here approves nothing.
        self.assertEqual(self.compare(base, base, once), {"unmatched": 1})

    def test_a_digest_moves_only_with_the_epoch_raised(self):
        head, base = (f"ok {S2}", f"ok {S2}", f"ok {S2}"), (f"ok {S1}", f"ok {S1}", f"ok {S1}")
        entry = [dict(entry_of("q", base, head), epoch=5)]
        self.assertEqual(self.compare(head, base, entry), {"changed": 1, "held": 1})
        self.assertEqual(self.compare(head, base, [dict(entry_of("q", base, head), epoch=6)], (5, 4)),
                         {"changed": 1, "held": 1})
        self.assertEqual(self.compare(head, base, entry, (5, 4)), {"accepted": 1})
        # Raised, but no entry: still a change.
        self.assertEqual(self.compare(head, base, (), (5, 4)), {"changed": 1})

    def test_a_refusal_of_what_a_1x_release_read_needs_the_epoch(self):
        # 0.x promised nothing: an entry alone lets a refusal through. A
        # 1.x release's text refused is a major version's break.
        head, base = ("err no", "err no", "-"), (f"ok {C1}", f"ok {C1}", "-")
        entry = [entry_of("q", base, head)]
        self.assertEqual(self.compare(head, base, entry, major=0), {"accepted": 1})
        self.assertEqual(self.compare(head, base, entry, major=1), {"refused": 1, "held": 1})
        raised = [dict(entry_of("q", base, head), epoch=5)]
        self.assertEqual(self.compare(head, base, raised, (5, 4), major=1), {"accepted": 1})
        # Saying the meaning is the same lets no refusal through.
        claimed = [dict(entry_of("q", base, head), same_meaning="refused, as it always meant")]
        self.assertEqual(self.compare(head, base, claimed, major=1), {"refused": 1, "held": 1})

    def test_a_statement_read_otherwise_against_a_1x_base_needs_the_epoch_or_its_meaning_kept(self):
        # A `d` word is a hash of a representation: it moves where the tree
        # changed shape and the meaning did not (a field renamed), and where
        # the meaning changed (`drop collection if exists t` read without
        # its `if exists`). Against 1.x an entry alone tells the two apart
        # no more than the word does.
        d_head, d_base = (f"ok {D2}", f"ok {D2}", "-"), (f"ok {D1}", f"ok {D1}", "-")
        entry = [entry_of("q", d_base, d_head)]
        self.assertEqual(self.compare(d_head, d_base, entry, major=1), {"changed": 1, "held": 1})
        # The epoch raised, as a moved digest's.
        raised = [dict(entry_of("q", d_base, d_head), epoch=5)]
        self.assertEqual(self.compare(d_head, d_base, raised, (5, 4), major=1), {"accepted": 1})
        # Or the meaning said to be the same, and why: a claim the summary
        # prints, for a reviewer to read.
        same = [dict(entry_of("q", d_base, d_head), same_meaning="`if_exists` renamed `exists`, as read")]
        self.assertEqual(self.compare(d_head, d_base, same, major=1), {"accepted": 1, "claimed": 1})
        # An empty claim claims nothing, and is refused before anything runs.
        for empty in ("", "   ", None, 1):
            bad = [dict(entry_of("q", d_base, d_head), same_meaning=empty)]
            self.assertEqual(self.compare(d_head, d_base, bad, major=1), {"changed": 1, "held": 1}, empty)
            self.assertEqual(len(corpus.entry_errors(bad)), 1, empty)
        self.assertEqual(corpus.entry_errors(same + entry + raised), [])
        # A claim lets no digest move.
        head, base = (f"ok {S2}", f"ok {S2}", f"ok {S2}"), (f"ok {S1}", f"ok {S1}", f"ok {S1}")
        claimed = [dict(entry_of("q", base, head), same_meaning="the same, said")]
        self.assertEqual(self.compare(head, base, claimed, major=1), {"changed": 1, "held": 1})
        self.assertEqual(self.compare(head, base, claimed, major=0), {"changed": 1, "held": 1})
        # Against 0.x an entry alone stays enough.
        self.assertEqual(self.compare(d_head, d_base, entry, major=0), {"accepted": 1})

    def test_every_claim_of_the_same_meaning_is_in_the_summary(self):
        texts = [text("drop collection t"), text("collections")]
        base = [(f"ok {D1}", f"ok {D1}", "-"), (f"ok {D2}", f"ok {D2}", "-")]
        head = [(f"ok {D2}", f"ok {D2}", "-"), (f"ok {D3}", f"ok {D3}", "-")]
        claims = [dict(entry_of("drop collection t", base[0], head[0]), same_meaning="a field renamed"),
                  dict(entry_of("collections", base[1], head[1]), same_meaning="a variant moved")]
        same, groups = corpus.compare(texts, head, base, 4, 4, claims, 1)
        out = io.StringIO()
        corpus.report("origin/main", "both", texts, same, groups, out)
        said = out.getvalue()
        self.assertIn("- accepted (ACCEPTED): 2", said)
        self.assertIn("- of them saying the meaning is the same (`same_meaning`): 2", said)
        self.assertIn("same_meaning: a field renamed", said)
        self.assertIn("same_meaning: a variant moved", said)

    def test_the_entry_printed_against_a_1x_base_asks_for_the_meaning(self):
        d_head, d_base = (f"ok {D2}", f"ok {D2}", "-"), (f"ok {D1}", f"ok {D1}", "-")
        self.assertEqual(corpus.entry_for("q", d_head, d_base, 1)["same_meaning"], "")
        self.assertNotIn("same_meaning", corpus.entry_for("q", d_head, d_base, 0))
        head, base = (f"ok {C2}", f"ok {C2}", "-"), (f"ok {C1}", f"ok {C1}", "-")
        self.assertNotIn("same_meaning", corpus.entry_for("q", head, base, 1))

    def test_a_base_is_1x_by_its_version(self):
        self.assertEqual([corpus.major(v) for v in ("v0.2.0", "0.2.0", "1.0.0-rc.1", "v1.4.2", "2.0.0")],
                         [0, 0, 1, 1, 2])
        with tempfile.TemporaryDirectory() as d:
            with open(os.path.join(d, "Cargo.toml"), "w") as f:
                f.write('[workspace]\nmembers = []\n\n[workspace.package]\nversion = "1.0.0-rc.2"\n')
            self.assertEqual(corpus.cargo_version(d), "1.0.0-rc.2")

    def test_the_report_counts_and_names(self):
        texts = [text("get a"), text("get b"), text("get c")]
        base = [(f"ok {C1}", f"ok {C1}", "-")] * 3
        head = [base[0], (f"ok {C2}", f"ok {C2}", "-"), ("err no", "err no", "-")]
        same, groups = corpus.compare(texts, head, base, 4, 4)
        out = io.StringIO()
        corpus.report("origin/main", "both", texts, same, groups, out)
        report = out.getvalue()
        self.assertIn("3 texts, 9 readings", report)
        self.assertIn("5 read alike", report)
        self.assertIn("- read otherwise: 1", report)
        self.assertIn("- refused now, parsed before: 1", report)
        self.assertIn('`here`: "get b"', report)
        self.assertIn("- by kind: `get` 3", report)

    def test_a_failure_says_what_changed(self):
        texts = [text("get t where a >= 1"), text("get u"), text("get t where a in [1]")]
        base = [(f"ok {C1}", f"ok {C1}", "-"), (f"ok {C1}", f"ok {C1}", "-"), (f"ok {C1}", f"ok {C1}", "-")]
        head = [(f"ok {C2}", f"ok {C2}", "-"), ("err query error: no\\tu", "err query error: no", "-"),
                (f"ok {C2}", f"ok {C2}", "-")]
        _, groups = corpus.compare(texts, head, base, 4, 4)
        form = "Select {\n    collection: \"t\",\n    op: %s,\n}"
        head_forms = [("ok\x1e" + (form % "Gt").replace("\n", "\\n"),) * 2 + ("-",),
                      ("err query error: no",) * 2 + ("-",),
                      ("ok\x1eDropCollection {\\n    name: \"t\",\\n}",) * 2 + ("-",)]
        base_forms = [("ok\x1e" + (form % "Ge").replace("\n", "\\n"),) * 2 + ("-",),
                      ("ok\x1eSelect",) * 2 + ("-",),
                      ("ok\x1eDropCollection {\\n    name: \"t\",\\n}",) * 2 + ("-",)]
        fails = corpus.failing(groups)
        self.assertEqual([t["text"] for t, _, _ in fails], ["get t where a >= 1", "get t where a in [1]", "get u"])
        out = io.StringIO()
        corpus.explain(fails, [head_forms[0], head_forms[2], head_forms[1]],
                       [base_forms[0], base_forms[2], base_forms[1]], out=out)
        said = out.getvalue()
        # The entry, ready to paste: both readings, a refusal as `err`.
        line = next(x for x in said.split("\n") if '"get u"' in x and x.strip().startswith("{"))
        entry = json.loads(line.strip().rstrip(","))
        self.assertEqual(entry["base"], [f"ok {C1}", f"ok {C1}", "-"])
        self.assertEqual(entry["head"], ["err", "err", "-"])
        # What changed: the forms' differing lines, a refusal's words, and
        # a digest moved under forms alike.
        self.assertIn("-    op: Ge,", said)
        self.assertIn("+    op: Gt,", said)
        self.assertIn("refused: query error: no\tu", said)
        self.assertIn("the forms are alike", said)

    def test_a_failure_is_bounded(self):
        lines = corpus.differing("\n".join(f"a{i}" for i in range(100)), "\n".join(f"b{i}" for i in range(100)))
        self.assertEqual(len(lines), corpus.SHOWN_LINES + 1)
        self.assertTrue(lines[-1].startswith("... "))
        texts = [text(f"get t{i}") for i in range(30)]
        rows = [(f"ok {C1}", f"ok {C1}", "-")] * 30
        _, groups = corpus.compare(texts, [(f"ok {C2}", f"ok {C2}", "-")] * 30, rows, 4, 4)
        out = io.StringIO()
        forms = [("ok\x1ea", "ok\x1ea", "-")] * corpus.SHOWN_TEXTS
        corpus.explain(corpus.failing(groups), forms, None, "no tag here", out=out)
        said = out.getvalue()
        self.assertEqual(said.count('"pr": "#"'), corpus.SHOWN_TEXTS)
        self.assertIn("and 10 texts more", said)
        self.assertIn("BASE's forms not built: no tag here", said)

    def test_every_failing_texts_entry_is_written_to_a_file(self):
        # A change of precedence failed 56 texts: three rounds of pasting
        # 20. The screen stays short; the file holds every entry.
        texts = [text(f"get t{i}") for i in range(30)]
        rows = [(f"ok {C1}", f"ok {C1}", "-")] * 30
        _, groups = corpus.compare(texts, [(f"ok {C2}", f"ok {C2}", "-")] * 30, rows, 4, 4)
        forms = [("ok\x1ea", "ok\x1ea", "-")] * corpus.SHOWN_TEXTS
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "accepted-origin-main.txt")
            out = io.StringIO()
            corpus.explain(corpus.failing(groups), forms, None, "no tag here", out=out, entries_to=path)
            said = out.getvalue()
            with open(path, encoding="utf-8") as f:
                lines = f.read().splitlines()
        self.assertEqual(said.count('"pr": "#"'), corpus.SHOWN_TEXTS)
        self.assertIn(f"of every one of the 30 failing texts, ready to paste: {path}", said)
        entries = [json.loads(line.rstrip(",")) for line in lines if line.strip().startswith("{")]
        self.assertEqual([e["text"] for e in entries], [f"get t{i}" for i in range(30)])
        self.assertTrue(all(line.endswith(",") for line in lines if line.strip().startswith("{")))

    def test_the_entries_file_is_named_by_its_base(self):
        self.assertEqual(os.path.basename(corpus.entries_path("/t", "origin/main")), "accepted-origin-main.txt")
        self.assertEqual(os.path.basename(corpus.entries_path("/t", "v1.0.0-rc.1")), "accepted-v1.0.0-rc.1.txt")


class Frozen(unittest.TestCase):
    def test_written_and_read_back(self):
        texts = [text("get a"), text("create statement s() as get a select id"), text("nope"),
                 text("get [1.00000001, 2]"), text("explain get a")]
        rows = [(f"ok {C1}", f"ok {C1}", "-"),
                (f"ok {S1}", f"ok {S1}", f"ok {S1}"),
                ("err query error: no", "err query error: no", "-"),
                (f"ok {C1}", f"ok {C2}", "-"),
                ("ok e" + "5" * 16, "err query error: x", "-")]
        lines = corpus.frozen_rows(texts, rows)
        # The refused text is left out; `exact` and `stored` written only
        # where `parse` does not make them; a refusal's words dropped.
        self.assertEqual([line["text"] for line in lines], ["get a", "create statement s() as get a select id",
                                                            "get [1.00000001, 2]", "explain get a"])
        self.assertEqual(lines[0], {"text": "get a", "from": "here", "parse": f"ok {C1}"})
        self.assertNotIn("stored", lines[1])
        self.assertEqual(lines[2]["exact"], f"ok {C2}")
        self.assertEqual(lines[3]["exact"], "err")
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "v9.jsonl")
            with open(path, "w") as f:
                for line in [{"tag": "v9", "commit": "0" * 40, "epoch": 4}] + lines:
                    f.write(json.dumps(line) + "\n")
            head, back, read = corpus.read_frozen(path)
        self.assertEqual(head["epoch"], 4)
        self.assertEqual([t["text"] for t in back], [line["text"] for line in lines])
        kept = [rows[0], rows[1], rows[3], ("ok e" + "5" * 16, "err", "-")]
        self.assertEqual(read, kept)
        # And a tree reading as the tag did passes against it.
        same, groups = corpus.compare(back, [rows[0], rows[1], rows[3], rows[4]], read, 4, 4)
        self.assertFalse(any(groups.values()), groups)

    def test_the_lists_texts_are_kept_refused_too(self):
        texts = [text("BEGIN"), text("nope")]
        rows = [("err query error: no", "err query error: no", "-")] * 2
        self.assertEqual(corpus.frozen_rows(texts, rows, {"BEGIN"}),
                         [{"text": "BEGIN", "from": "here", "parse": "err"}])

    def test_every_frozen_corpus_is_run(self):
        with tempfile.TemporaryDirectory() as root:
            d = os.path.join(root, corpus.FROZEN)
            os.makedirs(d)
            for name in ("v1.0.0.jsonl", "v0.2.0.jsonl", "kinds.txt", "v2.0.0.jsonl.orig"):
                open(os.path.join(d, name), "w").close()
            self.assertEqual(corpus.frozen_names(root), ["v0.2.0", "v1.0.0"])
        self.assertIn("v0.2.0", corpus.frozen_names())

    def test_a_frozen_corpus_is_never_replaced(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "v9.jsonl")
            corpus.may_write(path, False)
            open(path, "w").close()
            with self.assertRaises(SystemExit):
                corpus.may_write(path, False)
            corpus.may_write(path, True)

    def test_a_frozen_corpus_main_holds_is_never_changed_nor_removed(self):
        # `--freeze` refuses to write over one, but a pull request may edit
        # one by hand: the step against main refuses that, naming the file.
        with tempfile.TemporaryDirectory() as repo:
            d = os.path.join(repo, corpus.FROZEN)
            os.makedirs(d)
            for name in ("v1.jsonl", "v2.jsonl", "kinds.txt"):
                with open(os.path.join(d, name), "w") as f:
                    f.write(f'{{"tag": "{name}"}}\n')
            git = ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]
            subprocess.run(["git", "init", "-q", repo], check=True)
            subprocess.run(git + ["add", "-A"], cwd=repo, check=True)
            subprocess.run(git + ["commit", "-q", "-m", "frozen"], cwd=repo, check=True)
            base = corpus.commit_of("HEAD", repo)
            self.assertEqual(corpus.frozen_changed(base, repo), [])
            # One added, and the hand-written list grown: both free.
            with open(os.path.join(d, "v3.jsonl"), "w") as f:
                f.write("{}\n")
            with open(os.path.join(d, "kinds.txt"), "a") as f:
                f.write("get more\n")
            self.assertEqual(corpus.frozen_changed(base, repo), [])
            with open(os.path.join(d, "v1.jsonl"), "a") as f:
                f.write('{"text": "get t"}\n')
            os.remove(os.path.join(d, "v2.jsonl"))
            self.assertEqual(corpus.frozen_changed(base, repo),
                             [(f"{corpus.FROZEN}/v1.jsonl", "changed"), (f"{corpus.FROZEN}/v2.jsonl", "removed")])

    def test_a_freeze_reads_the_earlier_frozen_corpora_texts(self):
        # A text an earlier tag's corpus holds that the tree no longer does
        # (nine of v0.2.0's already) is read by the later tag's code too, so
        # it is held to 1.x's reading, not to 0.x's alone, for good.
        with tempfile.TemporaryDirectory() as root:
            d = os.path.join(root, corpus.FROZEN)
            os.makedirs(d)
            for name in ("v0.2.0", "v0.10.0", "v1.0.0-rc.1", "v1.0.0", "v1.2.0"):
                with open(os.path.join(d, f"{name}.jsonl"), "w") as f:
                    f.write(json.dumps({"tag": name}) + "\n")
                    f.write(json.dumps({"text": f"get only_{name}", "from": "docs/gone.md", "parse": "err"}) + "\n")
                    f.write(json.dumps({"text": "get t", "from": "docs/t.md", "parse": "err"}) + "\n")
            self.assertEqual(corpus.earlier_corpora("v1.0.0", root), ["v0.2.0", "v0.10.0", "v1.0.0-rc.1"])
            self.assertEqual(corpus.earlier_corpora("v0.2.0", root), [])
            tree = [{"text": "get t", "from": "docs/t.md"}]
            listed = [{"text": "BEGIN", "from": corpus.LISTED}]
            texts, earlier = corpus.freeze_texts(tree, listed, ["v0.2.0", "v0.10.0"], root)
        self.assertEqual([t["text"] for t in texts], ["get t", "BEGIN", "get only_v0.2.0", "get only_v0.10.0"])
        self.assertEqual(earlier, {"v0.2.0": 1, "v0.10.0": 1})

    def test_a_definition_alone_is_its_own_stored_reading(self):
        self.assertEqual(corpus.default_stored(f"ok {S1}"), f"ok {S1}")
        self.assertEqual(corpus.default_stored(f"ok {S1}r"), f"ok {S1}r")
        self.assertEqual(corpus.default_stored(f"ok {S1} {C1}"), "-")
        self.assertEqual(corpus.default_stored(f"ok {C1}"), "-")
        self.assertEqual(corpus.default_stored("err"), "-")


class Tree(unittest.TestCase):
    def test_an_archive_holds_the_paths_asked_that_the_ref_holds(self):
        with tempfile.TemporaryDirectory() as d:
            corpus.extract("HEAD", d, ["Cargo.toml", "no-such-path"])
            self.assertEqual(os.listdir(d), ["Cargo.toml"])

    def test_every_tree_builds_with_this_trees_compiler(self):
        self.assertRegex(corpus.toolchain() or "", r"^\d+\.\d+\.\d+$")
        self.assertEqual(corpus.build_env("/t")["RUSTUP_TOOLCHAIN"], corpus.toolchain())

    def test_a_build_reaches_no_network(self):
        env = corpus.build_env("/t")
        self.assertEqual(env["CARGO_NET_OFFLINE"], "true")
        self.assertEqual(env["CARGO_TARGET_DIR"], "/t")

    def test_the_builds_go_where_they_are_told(self):
        old = os.environ.pop("FENNECQL_CORPUS_TARGET", None)
        try:
            self.assertEqual(corpus.targets(), os.path.join(corpus.ROOT, "target", "fennecql-corpus"))
            os.environ["FENNECQL_CORPUS_TARGET"] = "/elsewhere/corpus"
            self.assertEqual(corpus.targets(), "/elsewhere/corpus")
        finally:
            os.environ.pop("FENNECQL_CORPUS_TARGET", None)
            if old is not None:
                os.environ["FENNECQL_CORPUS_TARGET"] = old

    def test_two_base_trees_are_each_built(self):
        """Two commits' trees, unpacked from `git archive` with their old
        commit times, built one after the other as the tool builds a base:
        each answers its own reading. Built into one target, the second
        was taken as built and answered the first's."""
        with tempfile.TemporaryDirectory() as d:
            repo, root = os.path.join(d, "repo"), os.path.join(d, "targets")
            os.makedirs(os.path.join(repo, "crates", "fennec-ql", "src"))
            files = {"Cargo.toml": '[workspace]\nmembers = ["crates/fennec-ql"]\nresolver = "2"\n',
                     "crates/fennec-ql/Cargo.toml": '[package]\nname = "fennec-ql"\nversion = "0.0.0"\n'
                                                   'edition = "2021"\n'}
            example = os.path.join(d, "corpus.rs")
            with open(example, "w") as f:
                f.write("fn main() { println!(\"{}\", fennec_ql::READ); }\n")
            env = dict(os.environ, GIT_AUTHOR_DATE="2001-01-01T00:00:00Z", GIT_COMMITTER_DATE="2001-01-01T00:00:00Z")
            git = ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]
            subprocess.run(["git", "init", "-q", repo], check=True)
            commits = []
            for reading in ("first", "second"):
                files["crates/fennec-ql/src/lib.rs"] = f'pub const READ: &str = "{reading}";\n'
                for path, body in files.items():
                    with open(os.path.join(repo, path), "w") as f:
                        f.write(body)
                subprocess.run(git + ["add", "-A"], cwd=repo, check=True)
                subprocess.run(git + ["commit", "-q", "-m", reading], cwd=repo, check=True, env=env)
                commits.append(corpus.commit_of("HEAD", repo))
            read = []
            for commit in commits:
                tree = tempfile.mkdtemp(dir=d)
                corpus.extract(commit, tree, repo=repo)
                exe = corpus.build(tree, corpus.target_for(root, "base", commit), example)
                read.append(subprocess.run([exe], stdout=subprocess.PIPE, check=True).stdout.decode().strip())
            self.assertEqual(read, ["first", "second"])
            # The first commit's target went with it: one base's at a time.
            self.assertEqual(os.listdir(root), [f"base-{commits[1][:12]}"])

    def test_two_checkouts_sharing_a_target_are_each_built(self):
        """Two checkouts of a crate reading otherwise, the second written
        before the first was built, each built as the tool builds this
        tree's head, into one `$FENNECQL_CORPUS_TARGET`: each answers its
        own reading. Built into one `head` target, the second was taken as
        built -- cargo's freshness is by time and by paths within the
        workspace -- and answered the first's."""
        with tempfile.TemporaryDirectory() as d:
            root = os.path.join(d, "shared")
            example = os.path.join(d, "corpus.rs")
            with open(example, "w") as f:
                f.write("fn main() { println!(\"{}\", fennec_ql::READ); }\n")
            checkouts = []
            for reading in ("first", "second"):
                tree = os.path.join(d, reading)
                os.makedirs(os.path.join(tree, "crates", "fennec-ql", "src"))
                for path, body in (("Cargo.toml", '[workspace]\nmembers = ["crates/fennec-ql"]\nresolver = "2"\n'),
                                   ("crates/fennec-ql/Cargo.toml", '[package]\nname = "fennec-ql"\n'
                                                                   'version = "0.0.0"\nedition = "2021"\n'),
                                   ("crates/fennec-ql/src/lib.rs", f'pub const READ: &str = "{reading}";\n')):
                    with open(os.path.join(tree, path), "w") as f:
                        f.write(body)
                checkouts.append(tree)
            read = []
            for tree in checkouts:
                exe = corpus.build(tree, corpus.head_target(root, tree), example)
                read.append(subprocess.run([exe], stdout=subprocess.PIPE, check=True).stdout.decode().strip())
            self.assertEqual(read, ["first", "second"])
            self.assertNotEqual(corpus.head_target(root, checkouts[0]), corpus.head_target(root, checkouts[1]))
            self.assertEqual(corpus.head_target(root, checkouts[0]), corpus.head_target(root, checkouts[0] + "/"))

    def test_a_tree_is_unpacked_under_the_target_and_a_killed_runs_left_behind(self):
        # A SIGKILL leaves the unpacked tree behind: under the target, by
        # the process' id, cleared at the next start once that process is
        # gone -- never another live run's, which may share the target.
        gone = subprocess.Popen(["true"])
        gone.wait()
        with tempfile.TemporaryDirectory() as root:
            mine = corpus.unpacked(root, "base")
            self.assertEqual(os.path.dirname(mine), os.path.join(root, "trees"))
            self.assertTrue(os.path.basename(mine).startswith("base-"))
            left = os.path.join(root, "trees", f"freeze-{gone.pid}")
            live = os.path.join(root, "trees", f"base-{os.getppid()}")
            for p in (left, live, os.path.join(left, "tree")):
                os.makedirs(p, exist_ok=True)
            corpus.clear_leftovers(root)
            self.assertFalse(os.path.exists(left))
            self.assertTrue(os.path.isdir(live) and os.path.isdir(mine))
            corpus.clear_leftovers(os.path.join(root, "nothing-yet"))


WORD = re.compile(r"^[cdes][0-9a-f]{16}r?$")


class Committed(unittest.TestCase):
    """The corpus frozen at v0.2.0, as `--freeze v0.2.0` wrote it."""

    def test_the_frozen_corpus(self):
        head, texts, rows = corpus.read_frozen(corpus.frozen_path("v0.2.0"))
        self.assertEqual(head["tag"], "v0.2.0")
        self.assertEqual(head["commit"], "e5d5424ec78999bb26a45bea80d13fd476729281")
        self.assertEqual(head["epoch"], 4)
        self.assertEqual(head["kept"], len(texts))
        self.assertGreater(len(texts), 8000)
        listed = {t["text"] for t in corpus.harvest(corpus.ROOT, [corpus.LISTED])}
        self.assertEqual(head["listed"], len(listed))
        self.assertLessEqual(listed, {t["text"] for t in texts})
        self.assertEqual(len({t["text"] for t in texts}), len(texts))
        for t, row in zip(texts, rows):
            # A harvested text one reading of which parsed; the list's
            # whatever it read.
            self.assertTrue(any(r.startswith("ok") for r in row) or t["text"] in listed, t)
            for reading in row:
                if reading.startswith("ok"):
                    for w in reading.split()[1:]:
                        self.assertRegex(w, WORD, t)
                else:
                    self.assertIn(reading, ("err", "-"), t)

    def test_each_entry_names_its_text_both_readings_its_pull_request_and_why(self):
        for entry in corpus.ACCEPTED:
            self.assertTrue(entry["text"] and entry["pr"].startswith("#") and len(entry["pr"]) > 1
                            and entry["why"], entry)
            for side in ("base", "head"):
                self.assertEqual(len(entry[side]), 3, entry)
                self.assertTrue(all(r == corpus.norm(r) for r in entry[side]), entry)
            self.assertIsInstance(entry.get("epoch", 0), int, entry)
        self.assertEqual(corpus.entry_errors(corpus.ACCEPTED), [])


if __name__ == "__main__":
    unittest.main()
