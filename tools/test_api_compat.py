#!/usr/bin/env python3
"""tools/api_compat.py: a break seeded into each of the five surfaces
(fennec.h, fennec.d.ts, client.d.ts, the CLI, the metrics) fails the
checker, every kind of break it reads is found, what only grows and what
only moves whitespace or comments passes, BREAKS lets one through, a
baseline written again past a break is caught by the base branch's; then
the committed baselines themselves.

    python3 tools/test_api_compat.py
"""

import contextlib
import importlib.util
import sys
import io
import json
import os
import shutil
import subprocess
import re
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("api_compat", os.path.join(HERE, "api_compat.py"))
compat = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compat)

HEADER = """
/* a comment */
#ifndef T_H
#define T_H
#define T_OK 0   /* zero */
#define T_BAD 1
extern "C" {
const char *t_version(void);
int32_t t_open(const uint8_t *path, size_t path_len, uint64_t *handle);
void t_free(char *s);
}
#endif
"""

TYPES = """
// Types.
/** A row. */
export type Row = { id: number };
export interface Opts {
  limit?: number;
  /** names */
  name: string;
  run(n: number): string;
  run(s: string): string;
}
export declare class Db {
  private constructor(x: number);
  private secret: string;
  static open(path: string, opts?: Opts): Promise<Db>;
  get(id: number): Row | null;
  get(id: string): Row | null;
  close(): void;
}
export function from(name: string): Db;
export function from(table: Row): Db;
export function inc(by?: number): number;
declare const hidden: unique symbol;
type Helper<T> = T[];
"""

CLIENT = """
export { Db, from } from './fennec.js';
export type { Row, Opts } from './fennec.js';
"""


# The binaries' surfaces, as their crates' tests dump them.
CLI = {
    "fennec": {
        "commands": {
            "fennec": {"--help": "switch", "--read-only": "switch", "-c": "value"},
            "fennec import": {"--dry-run": "switch", "--table": "value"},
        },
        "exit codes": {"0": "it did what it was asked", "1": "it failed", "2": "a flag was refused"},
    },
    "fennec-server": {
        "commands": {"fennec-server": {"--file": "value", "--studio": "switch"}},
        "exit codes": {"0": "stopped as asked", "1": "a failure", "2": "refused at the start"},
    },
    "fennec-shard": {
        "commands": {"fennec-shard": {"--listen": "value"}},
        "exit codes": {"0": "stopped as asked", "1": "a failure", "2": "refused at the start"},
    },
}
METRICS = {
    "fennec-server": {
        "fennec_documents": {"type": "gauge", "labels": ["collection"]},
        "fennec_statement_duration_seconds": {"type": "histogram", "labels": ["kind"]},
        "fennec_statements_total": {"type": "counter", "labels": ["kind"]},
    },
    "fennec-shard": {
        "fennec_router_nodes": {"type": "gauge", "labels": []},
        "fennec_router_tenants": {"type": "gauge", "labels": ["node", "region"]},
    },
}


def dumps(header=HEADER, types=TYPES, client=CLIENT, cli=None, metrics=None):
    return {
        "fennec.h": compat.dump("fennec.h", header),
        "fennec.d.ts": compat.dump("fennec.d.ts", types),
        "client.d.ts": compat.dump("client.d.ts", client),
        "cli": json.loads(json.dumps(CLI if cli is None else cli)),
        "metrics": json.loads(json.dumps(METRICS if metrics is None else metrics)),
    }


def changed(doc, edit):
    """A deep copy of `doc` that `edit` changed."""
    doc = json.loads(json.dumps(doc))
    edit(doc)
    return doc


def put_dumps(directory, cli=CLI, metrics=METRICS):
    """The binaries' dumps written into `directory` as their tests write them."""
    os.makedirs(directory, exist_ok=True)
    for surface, doc in (("cli", cli), ("metrics", metrics)):
        for binary, name in compat.baseline_files(surface):
            if binary in doc:
                with open(os.path.join(directory, name), "w", encoding="utf-8") as f:
                    f.write(compat.written(doc[binary]))


def entry_for(old, new, surface, release="1.0", reason="on purpose", pick=0):
    """The BREAKS entry the failure prints for the pick-th break old -> new."""
    symbol, change, before, after = compat.breaks(surface, old[surface], new[surface])[pick]
    return {
        "surface": surface,
        "symbol": symbol,
        "change": change,
        "before": compat.digest(before),
        "after": compat.digest(after),
        "release": release,
        "reason": reason,
    }


def failures(old, new, allowed=()):
    """(surface, symbol, change) of each break not allowed, old -> new."""
    out = []
    for s in compat.SURFACES:
        out += [(s, sym, ch) for sym, ch, _, _ in compat.judge(s, old[s], new[s], list(allowed))[1]]
    return out


class Header(unittest.TestCase):
    base = dumps()

    def broken(self, a, b):
        return failures(self.base, dumps(header=HEADER.replace(a, b)))

    def test_what_it_reads(self):
        h = self.base["fennec.h"]
        self.assertEqual(h["macros"], {"T_OK": "0", "T_BAD": "1"})
        self.assertEqual(h["functions"]["t_version"], {"returns": "const char*", "params": []})
        self.assertEqual(
            h["functions"]["t_open"]["params"][0], {"type": "const uint8_t*", "name": "path"}
        )

    def test_whitespace_and_comments_do_not_count(self):
        h = HEADER.replace("const uint8_t *path, size_t path_len", "const uint8_t*   path,\n    /* len */ size_t   path_len")
        self.assertEqual(failures(self.base, dumps(header=h)), [])

    def test_a_function_removed(self):
        self.assertEqual(self.broken("void t_free(char *s);", ""), [("fennec.h", "t_free", "function removed")])

    def test_a_return_type_changed(self):
        self.assertEqual(self.broken("int32_t t_open", "int64_t t_open"), [("fennec.h", "t_open", "return type changed")])

    def test_a_parameter_type_changed(self):
        self.assertEqual(
            self.broken("size_t path_len", "uint32_t path_len"), [("fennec.h", "t_open", "parameter type changed")]
        )

    def test_a_parameter_renamed_or_reordered(self):
        self.assertEqual(
            self.broken("size_t path_len", "size_t n"), [("fennec.h", "t_open", "parameter name changed")]
        )
        swapped = HEADER.replace("const uint8_t *path, size_t path_len", "size_t path_len, const uint8_t *path")
        got = failures(self.base, dumps(header=swapped))
        self.assertEqual({c for _, _, c in got}, {"parameter type changed"})

    def test_a_parameter_added_or_removed(self):
        self.assertEqual(
            self.broken("uint64_t *handle", "uint64_t *handle, int x"), [("fennec.h", "t_open", "parameter count changed")]
        )
        self.assertEqual(self.broken("void t_free(char *s)", "void t_free(void)"), [("fennec.h", "t_free", "parameter count changed")])

    def test_a_macro_removed_or_changed(self):
        self.assertEqual(self.broken("#define T_BAD 1", ""), [("fennec.h", "T_BAD", "macro removed")])
        self.assertEqual(self.broken("#define T_BAD 1", "#define T_BAD 2"), [("fennec.h", "T_BAD", "macro value changed")])

    def test_additions_pass(self):
        h = HEADER.replace("void t_free", "int32_t t_more(uint64_t h);\nvoid t_free").replace(
            "#define T_BAD 1", "#define T_BAD 1\n#define T_NEW 9"
        )
        self.assertEqual(failures(self.base, dumps(header=h)), [])


class Declarations(unittest.TestCase):
    base = dumps()

    def broken(self, a, b):
        self.assertIn(a, TYPES)
        return failures(self.base, dumps(types=TYPES.replace(a, b)))

    def test_what_it_reads(self):
        e = self.base["fennec.d.ts"]["exports"]
        self.assertEqual(sorted(e), ["Db", "Opts", "Row", "from", "inc"])  # not `hidden`, not `Helper`
        self.assertEqual(sorted(e["Db"]["members"]), ["close", "constructor", "get", "open"])  # not `secret`
        self.assertEqual(len(e["Db"]["members"]["get"]), 2)
        self.assertEqual(len(e["from"]["declarations"]), 2)

    def test_whitespace_and_comments_do_not_count(self):
        t = TYPES.replace("close(): void;", "/** shut */\n  close( ):   void ; // end")
        t = t.replace("limit?: number;", "limit?:\n    number;")
        self.assertEqual(failures(self.base, dumps(types=t)), [])

    def test_an_export_removed(self):
        self.assertEqual(self.broken("export function inc(by?: number): number;", ""), [("fennec.d.ts", "inc", "export removed")])

    def test_an_export_of_another_kind(self):
        self.assertEqual(
            self.broken("export function inc(by?: number): number;", "export const inc: (by?: number) => number;"),
            [("fennec.d.ts", "inc", "kind changed")],
        )

    def test_a_parameter_narrowed(self):
        self.assertEqual(
            self.broken("open(path: string, opts?: Opts)", "open(path: 'a' | 'b', opts?: Opts)"),
            [("fennec.d.ts", "Db.open", "member changed")],
        )

    def test_a_return_widened(self):
        self.assertEqual(
            self.broken("close(): void;", "close(): void | null;"), [("fennec.d.ts", "Db.close", "member changed")]
        )

    def test_a_parameter_made_required(self):
        self.assertEqual(
            self.broken("by?: number", "by: number"), [("fennec.d.ts", "inc", "declaration changed")]
        )

    def test_an_optional_member_made_required(self):
        self.assertEqual(
            self.broken("limit?: number;", "limit: number;"), [("fennec.d.ts", "Opts.limit", "member changed")]
        )

    def test_an_overload_dropped(self):
        self.assertEqual(
            self.broken("  get(id: string): Row | null;\n", ""), [("fennec.d.ts", "Db.get", "overload removed")]
        )
        self.assertEqual(
            self.broken("export function from(table: Row): Db;", ""), [("fennec.d.ts", "from", "overload removed")]
        )
        self.assertEqual(
            self.broken("  run(s: string): string;\n", ""), [("fennec.d.ts", "Opts.run", "overload removed")]
        )

    def test_a_member_removed(self):
        self.assertEqual(self.broken("  close(): void;\n", ""), [("fennec.d.ts", "Db.close", "member removed")])

    def test_a_heading_changed(self):
        self.assertEqual(
            self.broken("export declare class Db {", "export declare class Db extends Base {"),
            [("fennec.d.ts", "Db", "head changed")],
        )

    def test_a_type_alias_changed(self):
        self.assertEqual(
            self.broken("{ id: number }", "{ id: string }"), [("fennec.d.ts", "Row", "declaration changed")]
        )

    def test_the_private_constructor_is_api_a_private_field_is_not(self):
        self.assertEqual(
            self.broken("private constructor", "constructor"), [("fennec.d.ts", "Db.constructor", "member changed")]
        )
        self.assertEqual(self.broken("private secret: string;", "private secret: number;"), [])

    def test_additions_pass(self):
        t = TYPES.replace("close(): void;", "close(): void;\n  more(): void;")
        t = t.replace("  limit?: number;", "  limit?: number;\n  extra?: string;")
        t = t.replace("export function from(name: string): Db;", "export function from(name: string): Db;\nexport function from(n: number): Db;")
        t += "\nexport function brandNew(): void;\nexport interface Fresh { a: 1 }\n"
        self.assertEqual(failures(self.base, dumps(types=t)), [])

    def test_a_line_it_does_not_know_is_an_error_not_a_skip(self):
        with self.assertRaises(ValueError):
            compat.dump("fennec.d.ts", "export type A = 1;\nsurprise();\n")


class Client(unittest.TestCase):
    base = dumps()

    def test_a_name_no_longer_exported(self):
        got = failures(self.base, dumps(client=CLIENT.replace("Db, from", "Db")))
        self.assertEqual(got, [("client.d.ts", "from", "export removed")])

    def test_a_value_made_a_type(self):
        c = "export { from } from './fennec.js';\nexport type { Row, Opts, Db } from './fennec.js';\n"
        self.assertEqual(failures(self.base, dumps(client=c)), [("client.d.ts", "Db", "kind changed")])

    def test_additions_and_a_type_made_a_value_pass(self):
        c = "export { Db, from, inc, Row } from './fennec.js';\nexport type { Opts } from './fennec.js';\n"
        self.assertEqual(failures(self.base, dumps(client=c)), [])


class Cli(unittest.TestCase):
    """The flags and exit codes of `fennec`, `fennec-server` and
    `fennec-shard`: a flag removed or reread, a command or a code removed, a
    code's meaning changed each fail; anything added passes."""

    def broken(self, edit):
        return failures(dumps(), dumps(cli=changed(CLI, edit)))

    def test_a_flag_removed(self):
        def edit(d):
            del d["fennec-server"]["commands"]["fennec-server"]["--studio"]

        self.assertEqual(self.broken(edit), [("cli", "fennec-server --studio", "flag removed")])

    def test_a_flag_reread(self):
        for flag, to in (("-c", "switch"), ("--read-only", "value")):
            def edit(d):
                d["fennec"]["commands"]["fennec"][flag] = to

            self.assertEqual(self.broken(edit), [("cli", f"fennec {flag}", "flag reread")], flag)

    def test_a_command_removed(self):
        def edit(d):
            del d["fennec"]["commands"]["fennec import"]

        self.assertEqual(self.broken(edit), [("cli", "fennec import", "command removed")])

    def test_an_exit_code_removed_or_given_another_meaning(self):
        def gone(d):
            del d["fennec"]["exit codes"]["2"]

        def reread(d):
            d["fennec-shard"]["exit codes"]["1"] = "the directory could not be opened"

        self.assertEqual(self.broken(gone), [("cli", "fennec exit 2", "exit code removed")])
        self.assertEqual(self.broken(reread), [("cli", "fennec-shard exit 1", "exit code meaning changed")])

    def test_a_binary_gone(self):
        self.assertEqual(self.broken(lambda d: d.pop("fennec-shard")), [("cli", "fennec-shard", "binary removed")])

    def test_additions_pass(self):
        def grow(d):
            d["fennec"]["commands"]["fennec"]["--quiet"] = "switch"
            d["fennec"]["commands"]["fennec types"] = {"--lang": "value"}
            d["fennec-server"]["exit codes"]["3"] = "a new way to end"

        self.assertEqual(self.broken(grow), [])
        added = compat.additions("cli", CLI, changed(CLI, grow))
        self.assertEqual(sorted(added), ["fennec --quiet", "fennec types --lang", "fennec-server exit 3"])


class Metrics(unittest.TestCase):
    """The metrics the server and the router serve: one removed or renamed,
    its type changed or a label removed fail; a metric or a label added
    passes."""

    def broken(self, edit):
        return failures(dumps(), dumps(metrics=changed(METRICS, edit)))

    def test_a_metric_removed_or_renamed(self):
        def gone(d):
            del d["fennec-server"]["fennec_documents"]

        def renamed(d):
            d["fennec-server"]["fennec_docs"] = d["fennec-server"].pop("fennec_documents")

        for edit in (gone, renamed):
            self.assertEqual(self.broken(edit), [("metrics", "fennec-server fennec_documents", "metric removed")])

    def test_a_type_changed(self):
        def edit(d):
            d["fennec-server"]["fennec_statements_total"]["type"] = "gauge"

        self.assertEqual(self.broken(edit), [("metrics", "fennec-server fennec_statements_total", "metric type changed")])

    def test_a_label_removed_or_renamed(self):
        def gone(d):
            d["fennec-shard"]["fennec_router_tenants"]["labels"] = ["node"]

        def renamed(d):
            d["fennec-shard"]["fennec_router_tenants"]["labels"] = ["node", "zone"]

        for edit in (gone, renamed):
            self.assertEqual(self.broken(edit), [("metrics", "fennec-shard fennec_router_tenants", "label removed")])

    def test_a_binary_gone(self):
        self.assertEqual(self.broken(lambda d: d.pop("fennec-shard")), [("metrics", "fennec-shard", "binary removed")])

    def test_a_metric_or_a_label_added_passes(self):
        def grow(d):
            d["fennec-server"]["fennec_new_total"] = {"type": "counter", "labels": []}
            d["fennec-server"]["fennec_documents"]["labels"] = ["collection", "tenant"]

        self.assertEqual(self.broken(grow), [])
        self.assertEqual(compat.additions("metrics", METRICS, changed(METRICS, grow)), ["fennec-server fennec_new_total"])

    def test_a_dump_of_another_shape_is_refused(self):
        for doc in (
            {"x": {"type": "gauge"}},
            {"x": {"type": "meter", "labels": []}},
            {"bad name": {"type": "gauge", "labels": []}},
        ):
            with self.assertRaises(ValueError):
                compat.shaped("metrics", doc, "here")
        for doc in (
            {"commands": {}, "exit codes": {"0": "ok"}},
            {"commands": {"fennec": {"--x": "maybe"}}, "exit codes": {"0": "ok"}},
            {"commands": {"fennec": {"x": "switch"}}, "exit codes": {"0": "ok"}},
            {"commands": {"fennec": {}}, "exit codes": {"zero": "ok"}},
        ):
            with self.assertRaises(ValueError):
                compat.shaped("cli", doc, "here")


class Built(unittest.TestCase):
    """The binaries' dumps through the whole run: `make api-golden` hands
    them over (`--built`), a stale baseline fails until written again, a
    break is refused unless BREAKS names it, and main's holds them too."""

    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix="api-compat-built-")
        self.addCleanup(shutil.rmtree, self.dir)
        for surface, rel in compat.SOURCES.items():
            os.makedirs(os.path.dirname(os.path.join(self.dir, rel)), exist_ok=True)
            with open(os.path.join(self.dir, rel), "w") as f:
                f.write({"fennec.h": HEADER, "fennec.d.ts": TYPES, "client.d.ts": CLIENT}[surface])
        self.golden = os.path.join(self.dir, compat.BASELINE)
        self.staged = os.path.join(self.dir, "target", "api-golden")
        put_dumps(self.golden)
        self.assertEqual(self.run_main("--write")[0], 0)

    def run_main(self, *args):
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(buf):
            code = compat.main(["--root", self.dir, *args])
        return code, buf.getvalue()

    def stage(self, cli=CLI, metrics=METRICS):
        shutil.rmtree(self.staged, ignore_errors=True)
        put_dumps(self.staged, cli, metrics)

    def test_an_addition_is_stale_until_written_again(self):
        grown_cli = changed(CLI, lambda d: d["fennec"]["commands"]["fennec"].update({"--quiet": "switch"}))
        grown_metrics = changed(METRICS, lambda d: d["fennec-shard"].update({"fennec_router_x": {"type": "gauge", "labels": []}}))
        for cli, metrics, surface in ((grown_cli, METRICS, "cli"), (CLI, grown_metrics, "metrics")):
            with self.subTest(surface):
                self.stage(cli, metrics)
                code, out = self.run_main("--built", self.staged)
                self.assertEqual(code, 1, out)
                self.assertIn(f"{surface}: integrations/api-golden/{surface}-*.json is not what the binaries give: run `make api-golden`", out)
                code, out = self.run_main("--write", "--built", self.staged)
                self.assertEqual(code, 0, out)
                self.assertEqual(self.run_main("--built", self.staged)[0], 0)
                self.assertEqual(self.run_main()[0], 0)
                for _, name in compat.baseline_files(surface):
                    with open(os.path.join(self.golden, name), "rb") as a, open(os.path.join(self.staged, name), "rb") as b:
                        self.assertEqual(a.read(), b.read(), name)  # copied as written
                self.stage()
                put_dumps(self.golden)

    def test_a_seeded_break_of_each_is_refused_unless_breaks_names_it(self):
        def edit_cli(d):
            del d["fennec"]["commands"]["fennec import"]["--dry-run"]

        def edit_metrics(d):
            d["fennec-server"]["fennec_documents"]["type"] = "counter"

        for surface, cli, metrics in (
            ("cli", changed(CLI, edit_cli), METRICS),
            ("metrics", CLI, changed(METRICS, edit_metrics)),
        ):
            with self.subTest(surface):
                self.stage(cli, metrics)
                code, out = self.run_main("--write", "--built", self.staged)
                self.assertEqual(code, 1, out)
                self.assertIn(f"BREAKING: {surface}:", out)
                self.assertEqual(self.run_main()[0], 0)  # left as they were
                code, out = self.run_main("--built", self.staged)
                self.assertEqual(code, 1, out)
                self.assertIn(f"BREAKING against the committed baseline (integrations/api-golden/{surface}-*.json)", out)
                entry = entry_for(dumps(), dumps(cli=cli, metrics=metrics), surface)
                path = os.path.join(self.dir, "breaks.py")
                with open(path, "w") as f:
                    f.write(f"BREAKS = [{entry!r}]\n")
                code, out = self.run_main("--write", "--built", self.staged, "--breaks-from", path)
                self.assertEqual(code, 0, out)
                self.assertEqual(self.run_main("--breaks-from", path)[0], 0)
                put_dumps(self.golden)

    def test_a_dump_smaller_than_mains_fails_the_base_pass(self):
        def git(*a):
            subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *a], cwd=self.dir, check=True, capture_output=True)

        git("init", "-q")
        git("add", ".")
        git("commit", "-q", "-m", "base")
        # Edited by hand, or written by a test made to see less: the
        # committed dumps hold, main's do not.
        put_dumps(self.golden, changed(CLI, lambda d: d["fennec-shard"]["exit codes"].pop("2")), METRICS)
        self.assertEqual(self.run_main()[0], 0)
        code, out = self.run_main("--base-ref", "HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn("cli: fennec-shard exit 2: exit code removed", out)
        self.assertIn("written again past this break", out)

    def test_a_file_missing_or_of_another_shape(self):
        self.stage()
        os.remove(os.path.join(self.staged, "metrics-fennec-shard.json"))
        code, out = self.run_main("--write", "--built", self.staged)
        self.assertEqual(code, 2, out)
        self.assertIn("metrics-fennec-shard.json", out)
        os.remove(os.path.join(self.golden, "cli-fennec.json"))
        code, out = self.run_main()
        self.assertEqual(code, 1, out)
        self.assertIn("cli: integrations/api-golden/cli-*.json is missing: run `make api-golden`", out)
        with open(os.path.join(self.golden, "cli-fennec.json"), "w") as f:
            f.write('{"commands": {"fennec": {"--x": 1}}, "exit codes": {}}\n')
        self.assertEqual(self.run_main()[0], 2)

    def test_write_without_built_leaves_them(self):
        code, out = self.run_main("--write")
        self.assertEqual(code, 0, out)
        self.assertIn("cli: left as it is", out)


class Gate(unittest.TestCase):
    """The whole run: a checkout of its own, a baseline, a source broken."""

    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix="api-compat-test-")
        self.addCleanup(shutil.rmtree, self.dir)
        for surface, rel in compat.SOURCES.items():
            os.makedirs(os.path.dirname(os.path.join(self.dir, rel)), exist_ok=True)
        self.put("fennec.h", HEADER)
        self.put("fennec.d.ts", TYPES)
        self.put("client.d.ts", CLIENT)
        put_dumps(os.path.join(self.dir, compat.BASELINE))
        self.run_main("--write")

    def put(self, surface, text):
        with open(os.path.join(self.dir, compat.SOURCES[surface]), "w") as f:
            f.write(text)

    def run_main(self, *args):
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(buf):
            code = compat.main(["--root", self.dir, *args])
        return code, buf.getvalue()

    def test_an_unchanged_checkout_passes(self):
        self.assertEqual(self.run_main()[0], 0)

    def test_one_seeded_break_per_surface_fails_the_run(self):
        seeds = {
            "fennec.h": HEADER.replace("void t_free(char *s);", ""),
            "fennec.d.ts": TYPES.replace("close(): void;", "close(): number;"),
            "client.d.ts": CLIENT.replace("Db, from", "from"),
        }
        for surface, text in seeds.items():
            with self.subTest(surface):
                self.put("fennec.h", HEADER)
                self.put("fennec.d.ts", TYPES)
                self.put("client.d.ts", CLIENT)
                self.put(surface, text)
                code, out = self.run_main()
                self.assertEqual(code, 1, out)
                self.assertIn(f"BREAKING against the committed baseline (integrations/api-golden/{compat.baseline_name(surface)})", out)
                self.assertIn("BREAKS", out)

    def test_an_addition_fails_until_the_baseline_is_written_again(self):
        self.put("fennec.d.ts", TYPES + "\nexport function brandNew(): void;\n")
        code, out = self.run_main()
        self.assertEqual(code, 1, out)
        self.assertIn("make api-golden", out)
        self.assertEqual(self.run_main("--write")[0], 0)
        self.assertEqual(self.run_main()[0], 0)

    def test_a_baseline_edited_smaller_by_hand_fails_though_the_sources_stand(self):
        path = os.path.join(self.dir, compat.BASELINE, "fennec-d-ts.json")
        with open(path) as f:
            doc = json.load(f)
        del doc["exports"]["inc"]
        with open(path, "w") as f:
            f.write(compat.written(doc))
        code, out = self.run_main()
        self.assertEqual(code, 1, out)
        self.assertIn("make api-golden", out)

    def test_a_dump_smaller_than_mains_fails_the_base_pass(self):
        subprocess.run(["git", "init", "-q"], cwd=self.dir, check=True)
        subprocess.run(["git", "add", "."], cwd=self.dir, check=True)
        subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "b"], cwd=self.dir, check=True)
        small = TYPES.replace("export function inc(by?: number): number;\n", "")
        self.put("fennec.d.ts", small)  # the sources and the dump shrink together
        self.assertEqual(self.run_main("--write")[0], 1)  # --write refuses it
        with open(os.path.join(self.dir, compat.BASELINE, "fennec-d-ts.json"), "w") as f:
            f.write(compat.written(compat.dump("fennec.d.ts", small)))
        self.assertEqual(self.run_main()[0], 0)
        code, out = self.run_main("--base-ref", "HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn("inc: export removed", out)

    def test_an_entry_names_one_change_not_a_kind_of_change(self):
        # A loose entry, as the first version matched them, let this through.
        changed = TYPES.replace("export function inc(by?: number): number;", "export function inc(by: number): number;")
        self.put("fennec.d.ts", changed)
        loose = {"surface": "fennec.d.ts", "symbol": "inc", "change": "declaration changed",
                 "release": "1.0", "reason": "x"}
        path = os.path.join(self.dir, "breaks.py")
        with open(path, "w") as f:
            f.write(f"BREAKS = [{loose!r}]\n")
        self.assertEqual(self.run_main("--write", "--breaks-from", path)[0], 1)
        # One for a different change of the same symbol approves nothing.
        other = TYPES.replace("export function inc(by?: number): number;", "export function inc(by?: string): number;")
        wrong = entry_for(dumps(), dumps(types=other), "fennec.d.ts")
        with open(path, "w") as f:
            f.write(f"BREAKS = [{wrong!r}]\n")
        self.assertEqual(self.run_main("--write", "--breaks-from", path)[0], 1)
        # And the entry of this change, printed by the failure, does.
        right = entry_for(dumps(), dumps(types=changed), "fennec.d.ts")
        with open(path, "w") as f:
            f.write(f"BREAKS = [{wrong!r}, {right!r}]\n")
        self.assertEqual(self.run_main("--write", "--breaks-from", path)[0], 0)

    def test_an_entry_kept_as_history_approves_no_later_change(self):
        first = TYPES.replace("close(): void;", "close(): number;")
        entry = entry_for(dumps(), dumps(types=first), "fennec.d.ts")
        path = os.path.join(self.dir, "breaks.py")
        with open(path, "w") as f:
            f.write(f"BREAKS = [{entry!r}]\n")
        self.put("fennec.d.ts", first)
        self.assertEqual(self.run_main("--write", "--breaks-from", path)[0], 0)
        self.put("fennec.d.ts", TYPES.replace("close(): void;", "close(): string;"))
        self.assertEqual(self.run_main("--write", "--breaks-from", path)[0], 1)

    def test_a_break_BREAKS_names_passes_and_write_refuses_one_it_does_not(self):
        changed = TYPES.replace("close(): void;", "close(): number;")
        self.put("fennec.d.ts", changed)
        entry = entry_for(dumps(), dumps(types=changed), "fennec.d.ts", reason="it answers the count")
        self.assertEqual(self.run_main("--write")[0], 1)
        path = os.path.join(self.dir, "breaks.py")
        with open(path, "w") as f:
            f.write(f"BREAKS = [{entry!r}]\n")
        # The baseline still holds the old dump: it is written again, which
        # BREAKS now lets through, and the check then passes.
        self.assertEqual(self.run_main("--breaks-from", path)[0], 1)
        self.assertEqual(self.run_main("--write", "--breaks-from", path)[0], 0)
        code, out = self.run_main("--breaks-from", path)
        self.assertEqual(code, 0, out)

    def test_a_malformed_entry_fails(self):
        path = os.path.join(self.dir, "breaks.py")
        with open(path, "w") as f:
            f.write("BREAKS = [{'surface': 'nowhere'}]\n")
        self.assertEqual(self.run_main("--breaks-from", path)[0], 1)

    def test_a_baseline_written_again_past_a_break_is_held_by_the_base_branchs(self):
        def git(*a):
            subprocess.run(
                ["git", "-c", "user.name=t", "-c", "user.email=t@t", *a],
                cwd=self.dir, check=True, capture_output=True,
            )

        git("init", "-q")
        git("add", ".")
        git("commit", "-q", "-m", "base")
        self.put("fennec.h", HEADER.replace("void t_free(char *s);", ""))
        # The pull request edits the header and the baseline together.
        with open(os.path.join(self.dir, compat.SOURCES["fennec.h"])) as f:
            dump = compat.dump("fennec.h", f.read())
        with open(os.path.join(self.dir, compat.BASELINE, "fennec-h.json"), "w") as f:
            f.write(compat.written(dump))
        self.assertEqual(self.run_main()[0], 0)  # the committed baseline agrees
        code, out = self.run_main("--base-ref", "HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn("written again past this break", out)
        self.assertEqual(self.run_main("--base-ref", "nonesuch")[0], 2)


class Judged(unittest.TestCase):
    """check() itself: every break is judged on its own, whatever a break
    before it with the same symbol and change was told."""

    H2 = HEADER.replace("void t_free", "int32_t t_changes(uint64_t handle, uint64_t since);\nvoid t_free")

    def run_check(self, old, new, allowed):
        lines = []
        # The baseline is the new dump (as after `--write`); main's is the old.
        n = compat.check(new, new, old, list(allowed), out=lines.append)
        return n, "\n".join(lines)

    def two(self, a, b):
        old = dumps(header=self.H2)
        new = dumps(header=self.H2.replace("uint64_t handle, uint64_t since", f"{a}, {b}"))
        return old, new

    def test_two_parameter_types_changed_need_two_entries(self):
        old, new = self.two("uint32_t handle", "uint32_t since")
        got = compat.breaks("fennec.h", old["fennec.h"], new["fennec.h"])
        self.assertEqual([c for _, c, _, _ in got], ["parameter type changed"] * 2)
        first = entry_for(old, new, "fennec.h", pick=0)
        second = entry_for(old, new, "fennec.h", pick=1)
        n, out = self.run_check(old, new, [first])
        self.assertEqual(n, 1, out)
        self.assertIn("#2 since", out)
        n, out = self.run_check(old, new, [first, second])
        self.assertEqual(n, 0, out)
        n, out = self.run_check(old, new, [second])
        self.assertEqual(n, 1, out)
        self.assertIn("#1 handle", out)

    def test_two_parameter_names_changed_need_two_entries(self):
        old, new = self.two("uint64_t h", "uint64_t s")
        first = entry_for(old, new, "fennec.h", pick=0)
        second = entry_for(old, new, "fennec.h", pick=1)
        n, out = self.run_check(old, new, [first])
        self.assertEqual(n, 1, out)
        self.assertIn("#2: since -> #2: s", out)
        n, out = self.run_check(old, new, [first, second])
        self.assertEqual(n, 0, out)

    def test_two_changes_of_one_symbol_on_the_declarations_side(self):
        old = dumps()
        new = dumps(types=TYPES.replace("run(n: number): string;", "run(n: boolean): string;")
                    .replace("run(s: string): string;", "run(s: boolean): string;"))
        got = compat.breaks("fennec.d.ts", old["fennec.d.ts"], new["fennec.d.ts"])
        entries = [entry_for(old, new, "fennec.d.ts", pick=i) for i in range(len(got))]
        if len(got) > 1:
            n, out = self.run_check(old, new, entries[:-1])
            self.assertEqual(n, 1, out)
        n, out = self.run_check(old, new, entries)
        self.assertEqual(n, 0, out)


class Releases(unittest.TestCase):
    """The release an entry names, and the freeze at 1.0."""

    def entry(self, release):
        e = entry_for(dumps(), dumps(types=TYPES.replace("close(): void;", "close(): number;")), "fennec.d.ts")
        return {**e, "release": release}

    def test_until_the_freeze_the_release_is_1_0(self):
        self.assertEqual(compat.well_formed([self.entry("1.0")]), [])
        self.assertTrue(compat.well_formed([self.entry("2.0")]))
        self.assertTrue(compat.well_formed([self.entry("soon")], "1.0"))

    def test_after_it_an_entry_approves_a_major_above_the_frozen_only(self):
        old, new = dumps(), dumps(types=TYPES.replace("close(): void;", "close(): number;"))
        for release, passes in (("1.0", False), ("2.0", True)):
            e = self.entry(release)
            self.assertEqual(compat.well_formed([e], "1.0"), [])  # 1.0's stays as history
            failed = compat.judge("fennec.d.ts", old["fennec.d.ts"], new["fennec.d.ts"], [e], "1.0")[1]
            self.assertEqual(not failed, passes, release)

    def test_an_entry_naming_no_difference_is_allowed(self):
        e = {**self.entry("1.0"), "symbol": "Db.nothing"}
        self.assertEqual(compat.well_formed([e]), [])

    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix="api-compat-freeze-")
        self.addCleanup(shutil.rmtree, self.dir)
        for surface, rel in compat.SOURCES.items():
            os.makedirs(os.path.dirname(os.path.join(self.dir, rel)), exist_ok=True)
            with open(os.path.join(self.dir, rel), "w") as f:
                f.write({"fennec.h": HEADER, "fennec.d.ts": TYPES, "client.d.ts": CLIENT}[surface])
        shutil.copy(os.path.join(HERE, "api_compat.py"), os.path.join(self.dir, "api_compat.py"))
        put_dumps(os.path.join(self.dir, compat.BASELINE))
        self.run_main("--write")

    def run_main(self, *args):
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(buf):
            code = compat.main(["--root", self.dir, *args])
        return code, buf.getvalue()

    def test_a_freeze_needs_current_dumps_and_no_future_entry(self):
        self.assertEqual(self.run_main("--require-frozen")[0], 1)
        with open(os.path.join(self.dir, compat.SOURCES["fennec.d.ts"]), "a") as f:
            f.write("export function brandNew(): void;\n")
        self.assertEqual(self.run_main("--freeze", "1.0")[0], 1)  # the dump is behind
        self.run_main("--write")
        path = os.path.join(self.dir, "breaks.py")
        with open(path, "w") as f:
            f.write(f"BREAKS = [{ {**Releases.entry(self, '2.0')}!r}]\n")
        self.assertEqual(self.run_main("--freeze", "1.0", "--breaks-from", path)[0], 1)  # 2.0 is not made
        code, out = self.run_main("--freeze", "1.0")
        self.assertEqual(code, 0, out)
        self.assertEqual(self.run_main("--require-frozen")[0], 0)
        self.assertEqual(self.run_main("--freeze", "1.0")[0], 1)  # once

    def test_a_freeze_takes_only_the_next_major(self):
        for v in ("0.9", "1.3", "2.0", "1.1"):
            code, out = self.run_main("--freeze", v)
            self.assertEqual(code, 1, v)
            self.assertIn("1.0", out)
        self.assertEqual(self.run_main("--freeze", "1.0")[0], 0)
        for v in ("1.0", "1.3", "3.0", "2.1"):
            code, out = self.run_main("--freeze", v)
            self.assertEqual(code, 1, v)
            self.assertIn("2.0", out)
        self.assertEqual(self.run_main("--freeze", "2.0")[0], 0)

    def test_make_version_refuses_1_0_while_the_baselines_are_not_frozen(self):
        spec = importlib.util.spec_from_file_location("version_tool", os.path.join(HERE, "version.py"))
        version = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(version)
        root = version.pathlib.Path(self.dir)
        os.makedirs(os.path.join(self.dir, "tools"))
        shutil.move(os.path.join(self.dir, "api_compat.py"), os.path.join(self.dir, "tools", "api_compat.py"))
        version.guard("0.9.9", root)  # 0.x promises nothing
        for v in ("1.0.0", "1.2.3"):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as r:
                version.guard(v, root)
            self.assertIn("freeze", str(r.exception))
        self.assertEqual(self.run_main("--freeze", "1.0")[0], 0)
        version.guard("1.0.0", root)


class Order(unittest.TestCase):
    base = dumps()

    def test_swapped_overloads_are_a_change(self):
        t = TYPES.replace(
            "  get(id: number): Row | null;\n  get(id: string): Row | null;",
            "  get(id: string): Row | null;\n  get(id: number): Row | null;",
        )
        self.assertEqual(failures(self.base, dumps(types=t)), [("fennec.d.ts", "Db.get", "overload order changed")])
        t = TYPES.replace(
            "export function from(name: string): Db;\nexport function from(table: Row): Db;",
            "export function from(table: Row): Db;\nexport function from(name: string): Db;",
        )
        self.assertEqual(failures(self.base, dumps(types=t)), [("fennec.d.ts", "from", "overload order changed")])

    def test_an_overload_added_between_passes_and_the_rest_keep_their_order(self):
        t = TYPES.replace("  get(id: number): Row | null;", "  get(id: number): Row | null;\n  get(id: boolean): Row | null;")
        self.assertEqual(failures(self.base, dumps(types=t)), [])


class Unknown(unittest.TestCase):
    """What the scanner knows is recorded; what it does not it refuses."""

    def test_a_namespace_and_a_const_enum_record_their_bodies(self):
        t = TYPES + (
            "export namespace Nm {\n  function a(x: number): void;\n  interface I { k: string }\n  const c: 1;\n}\n"
            "export const enum Color { Red = 1, Green = 2 }\n"
        )
        e = compat.dump("fennec.d.ts", t)["exports"]
        self.assertEqual(e["Color"]["kind"], "enum")
        self.assertEqual(sorted(e["Color"]["members"]), ["Green", "Red"])
        self.assertEqual(sorted(e["Nm"]["members"]), ["I", "a", "c"])
        base = dumps(types=t)
        for a, b, change in (
            ("Red = 1", "Red = 5", "member changed"),
            ("function a(x: number): void;", "function a(x: string): void;", "member changed"),
            ("const c: 1;", "", "member removed"),
        ):
            got = failures(base, dumps(types=t.replace(a, b)))
            self.assertEqual([c for _, _, c in got], [change], a)
        grown = t.replace("Green = 2", "Green = 2, Blue = 3").replace("const c: 1;", "const c: 1;\n  const d: 2;")
        self.assertEqual(failures(base, dumps(types=grown)), [])

    def test_a_macro_under_an_if_is_recorded_with_its_condition(self):
        h = HEADER.replace("#define T_BAD 1", "#ifdef T_WIN\n#define T_BAD 1\n#endif")
        macros = compat.dump("fennec.h", h)["macros"]
        self.assertEqual(macros["T_BAD"], "1 [if defined(T_WIN)]")
        self.assertEqual(macros["T_OK"], "0")
        self.assertEqual(failures(dumps(), dumps(header=h)), [("fennec.h", "T_BAD", "macro value changed")])

    def test_a_form_it_does_not_know_is_refused(self):
        for text in (
            HEADER.replace("void t_free", "typedef struct { int a; } T;\nvoid t_free"),
            HEADER.replace("#define T_BAD 1", "#define T_BAD(x) x"),
            HEADER.replace("void t_free(char *s);", "#ifdef T_WIN\nvoid t_free(char *s);\n#endif"),
        ):
            with self.assertRaises(ValueError):
                compat.dump("fennec.h", text)
        for text in ("export { a as b };", "export * from './x.js';", "export default class A {}"):
            with self.assertRaises(ValueError):
                compat.dump("fennec.d.ts", text)


class Workflow(unittest.TestCase):
    """The check that must stop a merge is a step of a required job."""

    ci = open(os.path.join(HERE, "..", ".github", "workflows", "ci.yml"), encoding="utf-8").read()
    UBUNTU = "matrix.os == 'ubuntu-latest'"
    API = "make api-check BASE=FETCH_HEAD"
    AGENTS = "python3 tools/agents_md.py --check"

    @staticmethod
    def job_of(ci, name):
        m = re.search(rf"^  {name}:\n(.*?)(?=^  \S+:\n|\Z)", ci, re.M | re.S)
        assert m, name
        return m.group(1)

    @staticmethod
    def steps(job):
        """A job's steps as {key: text}, read from the text with comment
        lines dropped, so a comment satisfies nothing; a block scalar
        (`run: |`) is its lines joined."""
        out, cur, key = [], None, None
        for line in job.split("\n"):
            if line.lstrip().startswith("#"):
                continue
            m = re.match(r"^      - (\w[\w-]*):\s*(.*)$", line)
            k = re.match(r"^        (\w[\w-]*):\s*(.*)$", line)
            if m:
                cur = {}
                out.append(cur)
            elif k and cur is not None:
                m = k
            elif key is not None and cur is not None and re.match(r"^          \S", line):
                cur[key] += "\n" + line.strip()
                continue
            elif re.match(r"^      \S", line) or re.match(r"^    \S", line):
                cur, key = None, None
                continue
            else:
                continue
            key, val = m.group(1), m.group(2)
            cur[key] = "" if val in ("|", ">", "|-", ">-") else val
        return out

    def job(self, name):
        return self.job_of(self.ci, name)

    def verdict(self, ci, command, fetched):
        """What is wrong with the one step of `test` running `command`, or
        None: none or several, `continue-on-error`, an `if:` that is not
        the ubuntu one alone, no fetch of origin main before it."""
        steps = self.steps(self.job_of(ci, "test"))
        found = [s for s in steps if command in s.get("run", "")]
        if len(found) != 1:
            return f"{len(found)} steps run {command!r}"
        s = found[0]
        if "continue-on-error" in s:
            return "continue-on-error"
        if s.get("if") != self.UBUNTU:
            return f"if: {s.get('if')!r}"
        if fetched:
            ahead = "\n".join(b.get("run", "") for b in steps[: steps.index(s)]) + "\n" + s["run"].split(command)[0]
            if not re.search(r"git fetch\b[^\n]*\borigin main\b", ahead):
                return "no fetch of origin main before it"
        return None

    def test_the_api_check_is_a_step_that_stops_the_merge(self):
        self.assertIsNone(self.verdict(self.ci, self.API, True))
        self.assertNotIn("make api-check", self.job("fmt"))
        self.assertIn("tools/test_api_compat.py", self.job("fmt"))

    def test_the_size_check_is_a_step_of_both_jobs(self):
        self.assertIsNone(self.verdict(self.ci, self.AGENTS, False))
        self.assertTrue([s for s in self.steps(self.job("fmt")) if self.AGENTS in s.get("run", "")])

    def test_a_missing_step_a_soft_one_a_wider_if_and_no_fetch_each_fail(self):
        step = re.search(r"      - name: The C header, the JS types, the CLI and the metrics only grow\n.*?make api-check BASE=FETCH_HEAD\n", self.ci, re.S).group(0)
        deleted = self.ci.replace(step, "")
        self.assertIn("`make api-check`", deleted)  # its comment stays
        self.assertIn("0 steps", self.verdict(deleted, self.API, True))
        soft = self.ci.replace(step, step.replace("        if:", "        continue-on-error: true\n        if:"))
        self.assertEqual(self.verdict(soft, self.API, True), "continue-on-error")
        for wide in ("always()", f"{self.UBUNTU} || failure()"):
            got = self.verdict(self.ci.replace(step, step.replace(self.UBUNTU, wide)), self.API, True)
            self.assertTrue(got and got.startswith("if:"), got)
        unfetched = self.ci.replace("          git fetch --no-tags --depth=1 origin main\n", "")  # every one
        self.assertIn("no fetch", self.verdict(unfetched, self.API, True))
        astep = re.search(r"      - name: Each AGENTS.md is its CLAUDE.md, and no file is past its size\n.*?--check\n", self.ci, re.S).group(0)
        self.assertIn("0 steps", self.verdict(self.ci.replace(astep, ""), self.AGENTS, False))
        soft = self.ci.replace(astep, astep.replace("        if:", "        continue-on-error: true\n        if:"))
        self.assertEqual(self.verdict(soft, self.AGENTS, False), "continue-on-error")
        wide = self.ci.replace(astep, astep.replace(f"        if: {self.UBUNTU}\n", ""))
        self.assertTrue(self.verdict(wide, self.AGENTS, False).startswith("if:"))

    def test_the_binaries_dumps_are_held_by_make_test(self):
        """The CLI's and the metrics' dumps are held to the binaries by each
        binary's crate's tests/api_golden.rs, which `make test` runs in
        every leg of the required `test` job (Linux and macOS)."""
        crates = os.path.join(HERE, "..", "crates")
        for crate in ("fennec-server", "fennec-shard"):
            with open(os.path.join(crates, crate, "tests", "all.rs"), encoding="utf-8") as f:
                self.assertRegex(f.read(), r"(?m)^mod api_golden;$", crate)
        self.assertTrue(os.path.exists(os.path.join(crates, "fennec-cli", "tests", "api_golden.rs")))
        with open(os.path.join(crates, "fennec-cli", "Cargo.toml"), encoding="utf-8") as f:
            self.assertNotIn("autotests = false", f.read())
        found = [s for s in self.steps(self.job("test")) if s.get("run") == "make test"]
        self.assertEqual(len(found), 1)
        self.assertFalse({"if", "continue-on-error"} & set(found[0]), found[0])
        self.assertIn("os: [ubuntu-latest, ubuntu-24.04-arm, macos-latest]", self.job("test"))
        with open(os.path.join(HERE, "..", "Makefile"), encoding="utf-8") as f:
            self.assertRegex(f.read(), r"(?m)^test:\n(?:\t.*\n)*?\t.*\$\(CARGO\) test --workspace [^\n]*--tests\n")


class Committed(unittest.TestCase):
    def test_the_sources_hold_to_the_committed_baselines(self):
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(buf):
            code = compat.main([])
        self.assertEqual(code, 0, buf.getvalue())

    def test_breaks_is_well_formed(self):
        self.assertEqual(compat.well_formed(compat.BREAKS), [])


if __name__ == "__main__":
    unittest.main()
