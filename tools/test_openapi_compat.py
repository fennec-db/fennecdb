#!/usr/bin/env python3
"""tools/openapi_compat.py: each kind of break it finds, each addition it
lets through, and the gate around them -- a break BREAKS names, the
version it moves, a baseline left behind or written again past a break --
on small documents of its own; then the committed baseline itself.

    python3 tools/test_openapi_compat.py
"""

import contextlib
import copy
import importlib.util
import io
import json
import os
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("openapi_compat", os.path.join(HERE, "openapi_compat.py"))
compat = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compat)


def document():
    """A document as the server writes one, cut down: a read with query
    parameters and a header, a write with a body, a stream, an operation
    asking no token, the shared components behind `$ref`s."""
    return {
        "openapi": "3.1.0",
        "info": {"title": "t", "version": "1"},
        "security": [{"token": []}, {"jwt": []}],
        "paths": {
            "/{collection}": {
                "parameters": [{"$ref": "#/components/parameters/collection"}],
                "get": {
                    "operationId": "readRows",
                    "parameters": [
                        {"$ref": "#/components/parameters/Limit"},
                        {"$ref": "#/components/parameters/Order"},
                        {"name": "Fennec-Read", "in": "header", "schema": {"type": "string", "enum": ["replica"]}},
                    ],
                    "responses": {
                        "200": {
                            "description": "The rows",
                            "headers": {"X-Request-Id": {"schema": {"type": "string"}}},
                            "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Read"}}},
                        },
                        "401": {"description": "No token"},
                        "default": {"description": "A refusal"},
                    },
                },
                "post": {
                    "operationId": "insertRows",
                    "requestBody": {
                        "required": True,
                        "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Near"}}},
                    },
                    "responses": {"201": {"description": "Inserted"}},
                },
            },
            "/_health": {
                "get": {
                    "operationId": "health",
                    "security": [],
                    "responses": {"200": {"description": "Up"}},
                },
            },
            "/_metrics": {
                "get": {
                    "operationId": "metrics",
                    "security": [{"token": []}, {"admin": []}, {}],
                    "responses": {"200": {"description": "Counts"}},
                },
            },
        },
        "components": {
            "parameters": {
                "collection": {"name": "collection", "in": "path", "required": True, "schema": {"type": "string"}},
                "Limit": {"name": "limit", "in": "query", "schema": {"type": "integer", "minimum": 0}},
                "Order": {"name": "order", "in": "query", "schema": {"type": "string"}},
            },
            "schemas": {
                "Document": {"type": "object", "additionalProperties": True},
                "Rows": {"type": "array", "items": {"$ref": "#/components/schemas/Document"}},
                "Faceted": {
                    "type": "object",
                    "properties": {
                        "rows": {"$ref": "#/components/schemas/Rows"},
                        "count": {"type": "integer"},
                        "facets": {"type": "object", "properties": {"value": {}, "n": {"type": "integer"}}},
                    },
                },
                "Read": {"oneOf": [{"$ref": "#/components/schemas/Rows"}, {"$ref": "#/components/schemas/Faceted"}]},
                "Near": {
                    "type": "object",
                    "required": ["vector"],
                    "properties": {
                        "vector": {
                            "oneOf": [{"type": "array", "items": {"type": "number"}, "minItems": 1}, {"type": "string"}]
                        },
                        "limit": {"type": "integer", "minimum": 0, "maximum": 10000},
                        "fuzzy": {"oneOf": [{"type": "boolean"}, {"type": "integer", "maximum": 2}]},
                        "mode": {"type": "string", "enum": ["fast", "exact"]},
                        "where": {"type": "string"},
                    },
                },
            },
        },
    }


def changes(edit):
    """The breaks `edit` makes to the document, as (operation, change)."""
    old = document()
    new = copy.deepcopy(old)
    edit(new)
    return [(op, change) for op, change, _ in compat.breaks(old, new)]


def path(doc, p):
    return doc["paths"][p]


def schemas(doc):
    return doc["components"]["schemas"]


class Breaking(unittest.TestCase):
    """Each kind of break is found, named by its operation and its change."""

    def test_an_unchanged_document_breaks_nothing(self):
        self.assertEqual(changes(lambda d: None), [])

    def test_an_operation_removed(self):
        def edit(d):
            del path(d, "/_health")["get"]

        self.assertEqual(changes(edit), [("GET /_health", "operation removed")])

    def test_an_operations_method_changed(self):
        def edit(d):
            path(d, "/_health")["post"] = path(d, "/_health").pop("get")

        old = document()
        new = copy.deepcopy(old)
        edit(new)
        [(op, change, detail)] = compat.breaks(old, new)
        self.assertEqual((op, change), ("GET /_health", "operation removed"))
        self.assertIn("moved to POST /_health", detail)
        self.assertIn("never deprecated", detail)

    def test_an_operations_path_changed(self):
        def edit(d):
            d["paths"]["/_healthz"] = d["paths"].pop("/_health")

        self.assertEqual(changes(edit), [("GET /_health", "operation removed")])

    def test_a_deprecated_operation_removed_says_its_sunset(self):
        old = document()
        path(old, "/_health")["get"].update({"deprecated": True, "x-fennec-sunset": "2027-05-08T00:00:00Z"})
        new = copy.deepcopy(old)
        del path(new, "/_health")["get"]
        [(_, change, detail)] = compat.breaks(old, new)
        self.assertEqual(change, "operation removed")
        self.assertIn("deprecated, sunset 2027-05-08T00:00:00Z", detail)

    def test_a_parameter_that_became_required(self):
        def edit(d):
            d["components"]["parameters"]["Limit"]["required"] = True

        self.assertEqual(changes(edit), [("GET /{collection}", "parameter query limit required")])

    def test_a_required_parameter_added(self):
        def edit(d):
            path(d, "/{collection}")["get"]["parameters"].append(
                {"name": "tenant", "in": "query", "required": True, "schema": {"type": "string"}}
            )

        self.assertEqual(changes(edit), [("GET /{collection}", "parameter query tenant required")])

    def test_a_parameter_removed(self):
        def edit(d):
            del path(d, "/{collection}")["get"]["parameters"][1]

        self.assertEqual(changes(edit), [("GET /{collection}", "parameter query order removed")])

    def test_a_body_field_that_became_required(self):
        def edit(d):
            schemas(d)["Near"]["required"].append("limit")

        self.assertEqual(changes(edit), [("POST /{collection}", "request field limit required")])

    def test_a_required_body_field_added(self):
        def edit(d):
            schemas(d)["Near"]["properties"]["field"] = {"type": "string"}
            schemas(d)["Near"]["required"].append("field")

        self.assertEqual(changes(edit), [("POST /{collection}", "request field field required")])

    def test_a_body_that_became_required(self):
        def edit(d):
            path(d, "/{collection}")["post"]["requestBody"]["required"] = False

        old, new = document(), document()
        edit(old)
        self.assertEqual(
            [(op, c) for op, c, _ in compat.breaks(old, new)], [("POST /{collection}", "request body required")]
        )

    def test_a_body_field_removed(self):
        def edit(d):
            del schemas(d)["Near"]["properties"]["where"]

        self.assertEqual(changes(edit), [("POST /{collection}", "request field where removed")])

    def test_a_request_type_narrowed(self):
        cases = {
            # A oneOf's alternative no longer taken.
            "vector": lambda p: p.update(vector={"type": "array", "items": {"type": "number"}, "minItems": 1}),
            # A bound tightened.
            "limit": lambda p: p["limit"].update(maximum=100),
            # An enum's value gone.
            "mode": lambda p: p["mode"].update(enum=["fast"]),
            # A number where a oneOf took a boolean or an integer.
            "fuzzy": lambda p: p.update(fuzzy={"type": "boolean"}),
            # A pattern where any text was taken.
            "where": lambda p: p["where"].update(pattern="^[a-z]+$"),
        }
        for field, narrow in cases.items():
            with self.subTest(field=field):

                def edit(d):
                    narrow(schemas(d)["Near"]["properties"])

                self.assertEqual(changes(edit), [("POST /{collection}", f"request field {field} narrowed")])

    def test_a_parameters_type_narrowed(self):
        def edit(d):
            d["components"]["parameters"]["Order"]["schema"] = {"type": "string", "enum": ["asc", "desc"]}

        self.assertEqual(changes(edit), [("GET /{collection}", "parameter query order narrowed")])

    def test_a_request_media_type_removed(self):
        def edit(d):
            body = path(d, "/{collection}")["post"]["requestBody"]
            body["content"] = {"application/x-ndjson": body["content"]["application/json"]}

        self.assertEqual(changes(edit), [("POST /{collection}", "request application/json removed")])

    def test_a_response_status_removed(self):
        def edit(d):
            del path(d, "/{collection}")["get"]["responses"]["401"]

        self.assertEqual(changes(edit), [("GET /{collection}", "response 401 removed")])

    def test_a_response_field_removed(self):
        # Through a $ref and a oneOf: the faceted answer's count.
        def edit(d):
            del schemas(d)["Faceted"]["properties"]["count"]

        self.assertEqual(changes(edit), [("GET /{collection}", "response 200 field count removed")])

        def nested(d):
            del schemas(d)["Faceted"]["properties"]["facets"]["properties"]["n"]

        self.assertEqual(changes(nested), [("GET /{collection}", "response 200 field facets.n removed")])

    def test_a_response_alternative_removed_removes_its_fields(self):
        def edit(d):
            schemas(d)["Read"] = {"$ref": "#/components/schemas/Rows"}

        found = changes(edit)
        self.assertIn(("GET /{collection}", "response 200 field rows removed"), found)
        self.assertIn(("GET /{collection}", "response 200 field count removed"), found)

    def test_a_response_fields_type_changed(self):
        def edit(d):
            schemas(d)["Faceted"]["properties"]["count"] = {"type": "string"}

        self.assertEqual(changes(edit), [("GET /{collection}", "response 200 field count type changed")])

    def test_a_response_header_removed(self):
        def edit(d):
            path(d, "/{collection}")["get"]["responses"]["200"]["headers"] = {}

        self.assertEqual(changes(edit), [("GET /{collection}", "response 200 header X-Request-Id removed")])

    def test_a_security_requirement_added_where_there_was_none(self):
        # An operation asking no token takes the document's.
        def edit(d):
            del path(d, "/_health")["get"]["security"]

        old = document()
        new = copy.deepcopy(old)
        edit(new)
        [(op, change, detail)] = compat.breaks(old, new)
        self.assertEqual((op, change), ("GET /_health", "security added"))
        self.assertIn("no token", detail)

        # One taking no token among its ways no longer does.
        def optional(d):
            path(d, "/_metrics")["get"]["security"] = [{"token": []}, {"admin": []}]

        self.assertEqual(changes(optional), [("GET /_metrics", "security added")])

    def test_a_way_of_authenticating_removed(self):
        def edit(d):
            d["security"] = [{"token": []}]

        self.assertEqual(changes(edit), [("GET /{collection}", "security jwt removed"), ("POST /{collection}", "security jwt removed")])


class Additive(unittest.TestCase):
    """What a client relying on the old document still finds passes."""

    def test_additions_pass(self):
        edits = {
            "an operation": lambda d: d["paths"].update(
                {"/_whoami": {"get": {"operationId": "whoami", "responses": {"200": {"description": "Who"}}}}}
            ),
            "a method on a path": lambda d: path(d, "/_health").update(
                head={"operationId": "healthHead", "security": [], "responses": {"200": {"description": "Up"}}}
            ),
            "an optional parameter": lambda d: path(d, "/{collection}")["get"]["parameters"].append(
                {"name": "select", "in": "query", "schema": {"type": "string"}}
            ),
            "an optional body field": lambda d: schemas(d)["Near"]["properties"].update(ef={"type": "integer"}),
            "a response status": lambda d: path(d, "/{collection}")["get"]["responses"].update(
                {"304": {"description": "Not modified"}}
            ),
            "a response field": lambda d: schemas(d)["Faceted"]["properties"].update(took={"type": "integer"}),
            "a response header": lambda d: path(d, "/{collection}")["get"]["responses"]["200"]["headers"].update(
                {"Fennec-Seq": {"schema": {"type": "integer"}}}
            ),
            "an enum's value": lambda d: schemas(d)["Near"]["properties"]["mode"]["enum"].append("hybrid"),
            "a bound loosened": lambda d: schemas(d)["Near"]["properties"]["limit"].update(maximum=100000),
            "a bound dropped": lambda d: schemas(d)["Near"]["properties"]["limit"].pop("minimum"),
            "a type widened": lambda d: schemas(d)["Near"]["properties"]["where"].update(type=["string", "null"]),
            "an integer taken as a number": lambda d: schemas(d)["Near"]["properties"]["limit"].update(
                type="number"
            ),
            "a oneOf's alternative": lambda d: schemas(d)["Near"]["properties"]["vector"]["oneOf"].append(
                {"type": "object"}
            ),
            "a required field made optional": lambda d: schemas(d)["Near"].update(required=[]),
            "a parameter made optional": lambda d: d["components"]["parameters"]["Limit"].update(required=False),
            "a way of authenticating": lambda d: d["security"].append({"admin": []}),
            "a token no longer asked": lambda d: path(d, "/_metrics")["get"].update(security=[]),
            "deprecated": lambda d: path(d, "/_health")["get"].update(
                deprecated=True, **{"x-fennec-sunset": "2027-05-08T00:00:00Z"}
            ),
            "a description": lambda d: d["info"].update(description="more words"),
            "a placeholder renamed": lambda d: d["paths"].update({"/{name}": d["paths"].pop("/{collection}")}),
            "the version": lambda d: d["info"].update(version="2"),
        }
        for what, edit in edits.items():
            with self.subTest(what):
                self.assertEqual(changes(edit), [])


def gate(old, new, allowed=(), base=None):
    """`check`'s failures and what it said, holding `new` to `old` (and to
    `base`'s where given) for each kind."""
    said = []
    built = {k: new for k in compat.KINDS}
    baseline = {k: old for k in compat.KINDS}
    bases = None if base is None else {k: base for k in compat.KINDS}
    failures = compat.check(built, baseline, bases, allowed=list(allowed), out=said.append)
    return failures, "\n".join(said)


def removed():
    """The document with `GET /_health` gone, its version moved."""
    new = document()
    del path(new, "/_health")["get"]
    new["info"]["version"] = "2"
    return new


def entry(kind="file", version="2", change="operation removed", reason="replaced by GET /_live"):
    return {"version": version, "kind": kind, "operation": "GET /_health", "change": change, "reason": reason}


class Gate(unittest.TestCase):
    """What lets a break through, and what says the baseline is behind."""

    def test_the_baseline_itself_passes(self):
        self.assertEqual(gate(document(), document())[0], 0)

    def test_a_break_fails_naming_its_kind_operation_change_and_the_entry_to_write(self):
        failures, said = gate(document(), removed())
        self.assertEqual(failures, 3)
        self.assertIn("BREAKING against the committed baseline (integrations/openapi/file.json)", said)
        self.assertIn("file: GET /_health: operation removed", said)
        self.assertIn('"kind": "node"', said)
        self.assertIn('"change": "operation removed"', said)
        self.assertIn("name it in BREAKS (tools/openapi_compat.py)", said)

    def test_a_break_breaks_names_and_moves_the_version_for_passes(self):
        allowed = [entry(kind) for kind in compat.KINDS]
        failures, said = gate(document(), removed(), allowed)
        # It passes as a break, and the baseline is then behind.
        self.assertNotIn("BREAKING", said)
        self.assertIn("allowed by BREAKS: replaced by GET /_live", said)
        self.assertEqual(failures, 3)
        self.assertIn("run make openapi-golden", said)
        # Written again, it holds.
        self.assertEqual(gate(removed(), removed(), allowed)[0], 0)

    def test_a_break_breaks_names_without_moving_the_version_fails(self):
        new = removed()
        new["info"]["version"] = "1"
        failures, said = gate(document(), new, [entry(kind, version="1") for kind in compat.KINDS])
        self.assertEqual(failures, 3)
        self.assertIn("moves DOCUMENT_VERSION", said)

    def test_an_entry_of_another_version_is_history_and_allows_nothing(self):
        new = removed()
        new["info"]["version"] = "3"
        failures, said = gate(document(), new, [entry(kind, version="2") for kind in compat.KINDS])
        self.assertEqual(failures, 3)
        self.assertIn('"version": "3"', said)

    def test_an_entry_names_its_kind_and_change(self):
        failures, _ = gate(document(), removed(), [entry("file"), entry("node", change="security added")])
        # node's names another change and router has none: theirs fail.
        self.assertEqual(failures, 2 + 1)

    def test_an_addition_not_written_into_the_baseline_fails(self):
        new = document()
        new["paths"]["/_whoami"] = {"get": {"operationId": "whoami", "responses": {"200": {"description": "Who"}}}}
        failures, said = gate(document(), new)
        self.assertEqual(failures, 3)
        self.assertNotIn("BREAKING", said)
        self.assertIn("the document changed: GET /_whoami added", said)
        self.assertIn("run make openapi-golden", said)

    def test_a_baseline_written_again_past_a_break_fails_against_the_base(self):
        # The committed baseline is the new document, as `--write` would
        # have left it had BREAKS been bypassed by hand; main's still has
        # the operation.
        new = removed()
        failures, said = gate(new, new, base=document())
        self.assertEqual(failures, 3)
        self.assertIn("BREAKING against the base branch's baseline", said)
        self.assertIn("never by writing the baseline alone", said)
        # Named in BREAKS, it passes both ways.
        allowed = [entry(kind) for kind in compat.KINDS]
        self.assertEqual(gate(new, new, allowed, base=document())[0], 0)

    def test_a_break_both_baselines_show_is_told_once_against_the_committed_one(self):
        failures, said = gate(document(), removed(), base=document())
        self.assertEqual(failures, 3)
        self.assertNotIn("the base branch's baseline", said)
        self.assertNotIn("written again past", said)

    def test_a_base_with_no_baseline_holds_nothing(self):
        failures, said = gate(document(), document(), base=None)
        self.assertEqual(failures, 0)
        bases = {k: None for k in compat.KINDS}
        said = []
        failures = compat.check(
            {k: document() for k in compat.KINDS},
            {k: document() for k in compat.KINDS},
            bases,
            allowed=[],
            out=said.append,
        )
        self.assertEqual(failures, 0)
        self.assertIn("no document in the base branch's baseline", "\n".join(said))

    def test_an_entry_without_its_reason_fails(self):
        allowed = [entry(kind, reason="<why, and what a caller does instead>") for kind in compat.KINDS]
        failures, said = gate(removed(), removed(), allowed)
        self.assertEqual(failures, 3)
        self.assertIn("no reason given", said)


class Writing(unittest.TestCase):
    """`--write` writes the baseline only where nothing breaks unnamed."""

    def run_write(self, old, new):
        with tempfile.TemporaryDirectory() as tmp:
            built, baseline = os.path.join(tmp, "built"), os.path.join(tmp, "baseline")
            os.makedirs(built)
            os.makedirs(baseline)
            for kind in compat.KINDS:
                with open(os.path.join(built, f"{kind}.json"), "w") as f:
                    json.dump(new, f)
                with open(os.path.join(baseline, f"{kind}.json"), "w") as f:
                    f.write(compat.written(old))
            with contextlib.redirect_stdout(io.StringIO()):
                code = compat.main(["--built", built, "--baseline", baseline, "--write"])
            with open(os.path.join(baseline, "file.json")) as f:
                return code, json.load(f)

    def test_an_addition_is_written(self):
        new = document()
        new["paths"]["/_whoami"] = {"get": {"operationId": "whoami", "responses": {"200": {"description": "Who"}}}}
        code, kept = self.run_write(document(), new)
        self.assertEqual(code, 0)
        self.assertEqual(kept, new)

    def test_a_break_is_not_written(self):
        code, kept = self.run_write(document(), removed())
        self.assertEqual(code, 1)
        self.assertEqual(kept, document())


class Committed(unittest.TestCase):
    """The committed baseline: three documents of OpenAPI 3.1, each its own
    baseline, and BREAKS well formed."""

    def test_the_baseline_is_three_documents_holding_to_themselves(self):
        root = os.path.join(HERE, "..", compat.BASELINE)
        for kind in compat.KINDS:
            with self.subTest(kind):
                with open(os.path.join(root, f"{kind}.json"), encoding="utf-8") as f:
                    text = f.read()
                doc = json.loads(text)
                self.assertEqual(doc["openapi"], "3.1.0")
                self.assertEqual(text, compat.written(doc), "written as --write writes it")
                self.assertEqual(compat.breaks(doc, doc), [])
                self.assertGreater(len(compat.operations(doc)), 40)

    def test_breaks_is_well_formed(self):
        self.assertEqual(compat.well_formed(compat.BREAKS), [])


if __name__ == "__main__":
    unittest.main()
