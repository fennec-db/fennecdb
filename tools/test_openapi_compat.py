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
    parameters and a header, a write with a body, an operation asking no
    token, one served under a flag, a refusal whose body requires a field
    through an `allOf`, the shared components behind `$ref`s."""
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
                        "503": {"$ref": "#/components/responses/Unavailable"},
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
            "/_changes/consumers/{consumer}": {
                "parameters": [{"$ref": "#/components/parameters/consumer"}],
                "patch": {
                    "operationId": "moveConsumer",
                    "x-fennec-flag": "--cdc",
                    "security": [{"token": []}],
                    "requestBody": {
                        "required": True,
                        "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Since"}}},
                    },
                    "responses": {
                        "200": {
                            "description": "Moved",
                            "headers": {"Fennec-Seq": {"$ref": "#/components/headers/FennecSeq"}},
                        }
                    },
                },
            },
        },
        "components": {
            "securitySchemes": {
                "token": {"type": "http", "scheme": "bearer"},
                "jwt": {"type": "http", "scheme": "bearer", "bearerFormat": "JWT"},
                "admin": {"type": "http", "scheme": "bearer"},
            },
            "parameters": {
                "collection": {"name": "collection", "in": "path", "required": True, "schema": {"type": "string"}},
                "consumer": {"name": "consumer", "in": "path", "required": True, "schema": {"type": "string"}},
                "Limit": {"name": "limit", "in": "query", "schema": {"type": "integer", "minimum": 0}},
                "Order": {"name": "order", "in": "query", "schema": {"type": "string"}},
            },
            "headers": {"FennecSeq": {"schema": {"type": "integer"}}},
            "responses": {
                "Unavailable": {
                    "description": "Come back later",
                    "headers": {"Retry-After": {"schema": {"type": "integer"}}},
                    "content": {
                        "application/json": {
                            "schema": {"allOf": [{"$ref": "#/components/schemas/Error"}, {"required": ["retry_after"]}]}
                        }
                    },
                }
            },
            "schemas": {
                "Error": {
                    "type": "object",
                    "required": ["error"],
                    "properties": {"error": {"type": "string"}, "retry_after": {"type": "integer"}},
                },
                "Since": {"type": "object", "properties": {"since": {"type": "integer", "minimum": 0}}},
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
                        "ef": {"type": "integer", "format": "int32", "minimum": 0},
                        "weight": {"type": "number", "format": "float", "multipleOf": 0.5},
                        "candidates": {"allOf": [{"type": "integer", "minimum": 1}, {"maximum": 10000}]},
                        "filter": {"type": "object", "properties": {"year": {"type": "integer"}}},
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

    def test_a_flag_added_or_grown(self):
        # An operation every server served now asks a flag: a server started
        # without it answers 404, the operation gone for it.
        def always(d):
            path(d, "/_health")["get"]["x-fennec-flag"] = "--studio"

        old = document()
        new = copy.deepcopy(old)
        always(new)
        [(op, change, detail)] = compat.breaks(old, new)
        self.assertEqual((op, change), ("GET /_health", "flag added"))
        self.assertIn("served under every server", detail)
        self.assertIn("without --studio answers 404", detail)

        # A flag grown: served under --cdc, now under --cdc and --lease.
        def grown(d):
            path(d, "/_changes/consumers/{consumer}")["patch"]["x-fennec-flag"] = "--cdc --lease"

        self.assertEqual(changes(grown), [("PATCH /_changes/consumers/{consumer}", "flag added")])

        # One flag for another.
        def other(d):
            path(d, "/_changes/consumers/{consumer}")["patch"]["x-fennec-flag"] = "--changes"

        self.assertEqual(changes(other), [("PATCH /_changes/consumers/{consumer}", "flag added")])

    def test_a_response_field_no_longer_required(self):
        # Unavailable's retry_after, required through its allOf: an answer
        # may now leave it out.
        def edit(d):
            d["components"]["responses"]["Unavailable"]["content"]["application/json"]["schema"] = {
                "$ref": "#/components/schemas/Error"
            }

        self.assertEqual(changes(edit), [("GET /{collection}", "response 503 field retry_after no longer required")])

        # The body's own required field, through the $ref.
        def own(d):
            schemas(d)["Error"]["required"] = []

        self.assertEqual(changes(own), [("GET /{collection}", "response 503 field error no longer required")])

    def test_a_path_parameter_narrowed(self):
        cases = {
            "type": {"type": "integer"},
            "enum": {"type": "string", "enum": ["a", "b"]},
            "pattern": {"type": "string", "pattern": "^[a-z]+$"},
            "maxLength": {"type": "string", "maxLength": 64},
            "minLength": {"type": "string", "minLength": 1},
        }
        for what, schema in cases.items():
            with self.subTest(what):

                def edit(d):
                    d["components"]["parameters"]["collection"]["schema"] = schema

                self.assertEqual(
                    changes(edit),
                    [
                        ("GET /{collection}", "parameter path collection narrowed"),
                        ("POST /{collection}", "parameter path collection narrowed"),
                    ],
                )

    def test_a_path_parameter_renamed(self):
        # The same route to a client filling it by place; a generated one
        # names its argument by it.
        def edit(d):
            item = d["paths"].pop("/{collection}")
            item["parameters"] = [{"$ref": "#/components/parameters/name"}]
            d["paths"]["/{name}"] = item
            d["components"]["parameters"]["name"] = {
                "name": "name",
                "in": "path",
                "required": True,
                "schema": {"type": "string"},
            }

        old = document()
        new = copy.deepcopy(old)
        edit(new)
        found = compat.breaks(old, new)
        self.assertEqual(
            [(op, c) for op, c, _ in found],
            [
                ("GET /{collection}", "parameter path collection renamed"),
                ("POST /{collection}", "parameter path collection renamed"),
            ],
        )
        self.assertIn("{collection} -> {name}", found[0][2])

    def test_an_operation_id_renamed(self):
        def edit(d):
            path(d, "/_health")["get"]["operationId"] = "live"

        old = document()
        new = copy.deepcopy(old)
        edit(new)
        [(op, change, detail)] = compat.breaks(old, new)
        self.assertEqual((op, change), ("GET /_health", "operationId renamed"))
        self.assertIn("health -> live", detail)

    def test_additional_properties_closed_or_given_a_schema(self):
        # Absent (any field taken) to false.
        def closed(d):
            schemas(d)["Near"]["additionalProperties"] = False

        self.assertEqual(changes(closed), [("POST /{collection}", "request body narrowed")])

        # Absent to a schema: a field beyond those listed held to it.
        def typed(d):
            schemas(d)["Near"]["additionalProperties"] = {"type": "string"}

        self.assertEqual(changes(typed), [("POST /{collection}", "request field * narrowed")])

        # true to a schema, within a field.
        def nested(d):
            schemas(d)["Near"]["properties"]["filter"]["additionalProperties"] = {"type": "integer"}

        old = document()
        schemas(old)["Near"]["properties"]["filter"]["additionalProperties"] = True
        new = copy.deepcopy(old)
        nested(new)
        self.assertEqual(
            [(op, c) for op, c, _ in compat.breaks(old, new)],
            [("POST /{collection}", "request field filter.* narrowed")],
        )

    def test_every_part_of_an_all_of_is_held(self):
        # A tighter bound in the second part counts, as one in the first.
        def later(d):
            schemas(d)["Near"]["properties"]["candidates"]["allOf"][1]["maximum"] = 100

        self.assertEqual(changes(later), [("POST /{collection}", "request field candidates narrowed")])

        # A part added, bounding what the first took.
        def added(d):
            schemas(d)["Near"]["properties"]["candidates"]["allOf"].append({"minimum": 10})

        self.assertEqual(changes(added), [("POST /{collection}", "request field candidates narrowed")])

        # A field both parts list is held to both.
        def both(d):
            schemas(d)["Near"] = {"allOf": [schemas(d)["Near"], {"properties": {"limit": {"maximum": 50}}}]}

        self.assertEqual(changes(both), [("POST /{collection}", "request field limit narrowed")])

    def test_a_security_schemes_definition_changed(self):
        # A client authenticates by the definition, not the name: one entry
        # a scheme, whichever operations ask it.
        def edit(d):
            d["components"]["securitySchemes"]["token"] = {"type": "apiKey", "in": "header", "name": "X-Token"}

        old = document()
        new = copy.deepcopy(old)
        edit(new)
        found = compat.breaks(old, new)
        self.assertEqual({(op, c) for op, c, _ in found}, {("securitySchemes token", "definition changed")})
        self.assertIn("type 'http' -> 'apiKey'", "\n".join(d for _, _, d in found))

        # Its scheme's case is not a change; a scheme no operation uses
        # changes nothing.
        def case(d):
            d["components"]["securitySchemes"]["token"]["scheme"] = "Bearer"
            d["components"]["securitySchemes"]["unused"] = {"type": "http", "scheme": "basic"}

        self.assertEqual(changes(case), [])

    def test_a_ways_scopes_grown_or_an_oauth_flow_changed(self):
        old = document()
        old["components"]["securitySchemes"]["oauth"] = {
            "type": "oauth2",
            "flows": {
                "clientCredentials": {"tokenUrl": "https://id.example/token", "scopes": {"read": "", "write": ""}}
            },
        }
        path(old, "/_health")["get"]["security"] = [{"oauth": ["read"]}]
        new = copy.deepcopy(old)
        path(new, "/_health")["get"]["security"] = [{"oauth": ["read", "write"]}]
        self.assertEqual(
            [(op, c) for op, c, _ in compat.breaks(old, new)], [("GET /_health", "security oauth scopes added")]
        )
        # Fewer scopes asked takes every caller it took.
        fewer = copy.deepcopy(old)
        path(fewer, "/_health")["get"]["security"] = [{"oauth": []}]
        self.assertEqual(compat.breaks(old, fewer), [])
        # A flow's URL moved, a scope gone.
        moved = copy.deepcopy(old)
        flow = moved["components"]["securitySchemes"]["oauth"]["flows"]["clientCredentials"]
        flow["tokenUrl"] = "https://id.example/v2/token"
        del flow["scopes"]["write"]
        found = compat.breaks(old, moved)
        self.assertEqual({(op, c) for op, c, _ in found}, {("securitySchemes oauth", "definition changed")})
        self.assertEqual(len(found), 2)

    def test_a_response_headers_schema_changed(self):
        def edit(d):
            d["components"]["headers"]["FennecSeq"]["schema"] = {"type": "string"}

        self.assertEqual(
            changes(edit),
            [("PATCH /_changes/consumers/{consumer}", "response 200 header Fennec-Seq type changed")],
        )

        def unavailable(d):
            d["components"]["responses"]["Unavailable"]["headers"]["Retry-After"]["schema"] = {"type": "string"}

        self.assertEqual(changes(unavailable), [("GET /{collection}", "response 503 header Retry-After type changed")])

        # A header that was required no longer is.
        old = document()
        old["components"]["responses"]["Unavailable"]["headers"]["Retry-After"]["required"] = True
        self.assertEqual(
            [(op, c) for op, c, _ in compat.breaks(old, document())],
            [("GET /{collection}", "response 503 header Retry-After no longer required")],
        )

    def test_a_default_changed(self):
        # The documents write none; one written is held.
        old = document()
        old["components"]["parameters"]["Limit"]["schema"]["default"] = 100
        new = copy.deepcopy(old)
        new["components"]["parameters"]["Limit"]["schema"]["default"] = 10
        self.assertEqual(
            [(op, c) for op, c, _ in compat.breaks(old, new)],
            [("GET /{collection}", "parameter query limit default changed")],
        )
        del new["components"]["parameters"]["Limit"]["schema"]["default"]
        self.assertEqual(
            [(op, c) for op, c, _ in compat.breaks(old, new)],
            [("GET /{collection}", "parameter query limit default changed")],
        )

    def test_a_pattern_changed_says_a_loosened_one_is_named_too(self):
        old = document()
        schemas(old)["Near"]["properties"]["where"]["pattern"] = "^[a-z]{1,8}$"
        new = copy.deepcopy(old)
        schemas(new)["Near"]["properties"]["where"]["pattern"] = "^[a-z]+$"
        [(_, change, detail)] = compat.breaks(old, new)
        self.assertEqual(change, "request field where narrowed")
        self.assertIn("cannot be told", detail)
        self.assertIn("named in BREAKS too", detail)


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
            "a format widened to int64": lambda d: schemas(d)["Near"]["properties"]["ef"].update(format="int64"),
            "a format widened to double": lambda d: schemas(d)["Near"]["properties"]["weight"].update(format="double"),
            "a format dropped": lambda d: schemas(d)["Near"]["properties"]["ef"].pop("format"),
            "a multipleOf by a divisor": lambda d: schemas(d)["Near"]["properties"]["weight"].update(multipleOf=0.25),
            "a status taken into its range": lambda d: path(d, "/_health")["get"].update(
                responses={"2XX": {"description": "Up"}}
            ),
            "a flag dropped": lambda d: path(d, "/_changes/consumers/{consumer}")["patch"].pop("x-fennec-flag"),
            "a response field made required": lambda d: schemas(d)["Error"]["required"].append("retry_after"),
            "an allOf's bound loosened": lambda d: schemas(d)["Near"]["properties"]["candidates"]["allOf"][1].update(
                maximum=20000
            ),
            "additionalProperties opened": lambda d: schemas(d)["Near"]["properties"]["filter"].update(
                additionalProperties=True
            ),
            "the version": lambda d: d["info"].update(version="2"),
            "a scheme's case": lambda d: d["components"]["securitySchemes"]["jwt"].update(scheme="Bearer"),
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

    def test_a_version_moved_back_reopens_no_old_entry(self):
        # main is at 3; BREAKS's entry of 2 is history. A pull request
        # setting the version back to 2 and breaking what 2 broke fails.
        old = document()
        old["info"]["version"] = "3"
        new = removed()
        new["info"]["version"] = "2"
        failures, said = gate(old, new, [entry(kind, version="2") for kind in compat.KINDS])
        self.assertIn("went back from 3 to 2", said)
        self.assertIn("not past the baseline's 3", said)
        self.assertEqual(failures, 3 + 3)

    def test_a_version_moved_back_with_nothing_broken_fails(self):
        old = document()
        old["info"]["version"] = "3"
        new = document()
        new["info"]["version"] = "2"
        failures, said = gate(old, new)
        self.assertIn("only moves forward", said)
        # Each kind: its version, and its baseline left behind.
        self.assertEqual(failures, 3 + 3)

    def test_a_version_is_a_whole_number(self):
        new = removed()
        new["info"]["version"] = "2-beta"
        failures, said = gate(document(), new, [entry(kind, version="2-beta") for kind in compat.KINDS])
        self.assertIn("not a whole number", said)
        self.assertNotIn("allowed by BREAKS", said)
        self.assertEqual(
            compat.well_formed([entry(version="2-beta")]), ["BREAKS[0]: version '2-beta' is not a whole number's text"]
        )

    def test_a_version_moved_past_by_more_than_one_passes(self):
        new = removed()
        new["info"]["version"] = "10"
        allowed = [entry(kind, version="10") for kind in compat.KINDS]
        failures, said = gate(document(), new, allowed)
        self.assertNotIn("BREAKING", said)
        self.assertIn("allowed by BREAKS", said)

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


class BreaksFrom(unittest.TestCase):
    """main's checker reads a pull request's BREAKS as data, never running
    its code, and holds the documents by them (CI's base pass)."""

    def checker(self, tmp, text, name="openapi_compat.py"):
        path = os.path.join(tmp, name)
        with open(path, "w") as f:
            f.write(text)
        return path

    def test_breaks_are_read_as_a_literal(self):
        with tempfile.TemporaryDirectory() as tmp:
            listed = self.checker(tmp, 'import sys\nBREAKS = [{"version": "2", "kind": "file"}]\nsys.exit(9)\n')
            self.assertEqual(compat.breaks_in(listed), [{"version": "2", "kind": "file"}])
            # Code, never run: refused.
            run = self.checker(tmp, "BREAKS = __import__('os').listdir('.')\n", "run.py")
            with self.assertRaises(ValueError):
                compat.breaks_in(run)
            with self.assertRaises(ValueError):
                compat.breaks_in(self.checker(tmp, "OTHER = []\n", "other.py"))

    def test_the_pass_holds_the_documents_by_the_breaks_it_reads(self):
        with tempfile.TemporaryDirectory() as tmp:
            built, baseline = os.path.join(tmp, "built"), os.path.join(tmp, "baseline")
            os.makedirs(built)
            os.makedirs(baseline)
            for kind in compat.KINDS:
                with open(os.path.join(built, f"{kind}.json"), "w") as f:
                    json.dump(removed(), f)
                with open(os.path.join(baseline, f"{kind}.json"), "w") as f:
                    f.write(compat.written(document()))
            args = ["--built", built, "--baseline", baseline]
            quiet = contextlib.redirect_stdout(io.StringIO())
            # This checker's own BREAKS name no such break.
            with quiet:
                self.assertEqual(compat.main(args), 1)
            # The pull request's do: the break passes, and its baseline is
            # behind.
            pr = self.checker(tmp, f"BREAKS = {[entry(kind) for kind in compat.KINDS]!r}\n")
            said = io.StringIO()
            with contextlib.redirect_stdout(said):
                self.assertEqual(compat.main(args + ["--breaks-from", pr]), 1)
            self.assertIn("allowed by BREAKS", said.getvalue())
            self.assertNotIn("BREAKING", said.getvalue())
            # None to read is no pass at all.
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(compat.main(args + ["--breaks-from", os.path.join(tmp, "none.py")]), 2)

    def test_the_committed_breaks_are_a_literal(self):
        self.assertEqual(compat.breaks_in(os.path.join(HERE, "openapi_compat.py")), compat.BREAKS)


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
