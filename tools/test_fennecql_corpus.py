#!/usr/bin/env python3
"""tools/fennecql_corpus.py without a build: the texts it harvests from
each kind of file, how it judges one reading against another, what lets a
change through -- an ACCEPTED entry, and a grammar epoch raised where a
digest moved -- the frozen corpus written and read back, and the committed
one itself (docs/design/fennecql.md#the-corpus).

    python3 tools/test_fennecql_corpus.py
"""

import importlib.util
import io
import json
import os
import re
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("fennecql_corpus", os.path.join(HERE, "fennecql_corpus.py"))
corpus = importlib.util.module_from_spec(spec)
spec.loader.exec_module(corpus)

C1, C2 = "c" + "1" * 16, "c" + "2" * 16
S1, S2 = "s" + "a" * 16, "s" + "b" * 16
D1, D2 = "d" + "3" * 16, "d" + "4" * 16


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
        self.assertEqual(corpus.judge("panic", f"ok {C1}"), "refused")
        self.assertEqual(corpus.judge(f"ok {C1}", "err query error: no"), "parses")
        self.assertEqual(corpus.judge("err query error: a", "err query error: b"), "reworded")
        self.assertEqual(corpus.judge("-", "-"), "same")

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
        # refused afresh whose stored reading holds.
        self.assertFalse(moves((f"ok {D2}", f"ok {D2}", "-"), (f"ok {D1}", f"ok {D1}", "-")))
        self.assertFalse(moves(("err x", "err x", f"ok {S1}"), (f"ok {S1}", f"ok {S1}", f"ok {S1}")))
        self.assertFalse(moves(("err x", "err x", "-"), (f"ok {C1}", f"ok {C1}", "-")))


def text(t):
    return {"text": t, "from": "here"}


class Compare(unittest.TestCase):
    def compare(self, head, base, accepted=(), epochs=(4, 4)):
        same, groups = corpus.compare([text("q")], [head], [base], *epochs, accepted)
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

    def test_an_entry_lets_through_what_moves_no_digest(self):
        entry = [{"text": "q", "pr": "#1", "why": "a bug"}]
        self.assertEqual(self.compare((f"ok {D2}", f"ok {D2}", "-"), (f"ok {D1}", f"ok {D1}", "-"), entry),
                         {"accepted": 1})
        self.assertEqual(self.compare(("err no", "err no", f"ok {S1}"), (f"ok {S1}", f"ok {S1}", f"ok {S1}"), entry),
                         {"accepted": 1})

    def test_a_digest_moves_only_with_the_epoch_raised(self):
        head, base = (f"ok {S2}", f"ok {S2}", f"ok {S2}"), (f"ok {S1}", f"ok {S1}", f"ok {S1}")
        entry = [{"text": "q", "pr": "#1", "why": "on purpose", "epoch": 5}]
        self.assertEqual(self.compare(head, base, entry), {"changed": 1, "held": 1})
        self.assertEqual(self.compare(head, base, [{"text": "q", "pr": "#1", "why": "w", "epoch": 6}], (5, 4)),
                         {"changed": 1, "held": 1})
        self.assertEqual(self.compare(head, base, entry, (5, 4)), {"accepted": 1})
        # Raised, but no entry: still a change.
        self.assertEqual(self.compare(head, base, (), (5, 4)), {"changed": 1})

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
        self.assertEqual(len({t["text"] for t in texts}), len(texts))
        for t, row in zip(texts, rows):
            self.assertTrue(any(r.startswith("ok") for r in row), t)
            for reading in row:
                if reading.startswith("ok"):
                    for w in reading.split()[1:]:
                        self.assertRegex(w, WORD, t)
                else:
                    self.assertIn(reading, ("err", "-"), t)

    def test_each_entry_names_its_text_its_pull_request_and_why(self):
        for entry in corpus.ACCEPTED:
            self.assertTrue(entry["text"] and entry["pr"].startswith("#") and entry["why"], entry)
            self.assertIsInstance(entry.get("epoch", 0), int, entry)


if __name__ == "__main__":
    unittest.main()
