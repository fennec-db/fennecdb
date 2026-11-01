"""The query builder: held to integrations/builder-golden.json -- the text and
parameters the JavaScript builder makes of every chain there -- and run
against the server, where each answer is the answer to the same statement
written by hand."""

import array
import asyncio
import json
import os
from datetime import datetime
from pathlib import Path

import pytest

from conftest import TOKEN, URL, fresh
from fennecdb import (
    AsyncClient,
    AsyncQuery,
    Call,
    FacetCount,
    FennecError,
    Param,
    Query,
    and_,
    bucket,
    call,
    collection,
    count_distinct,
    distance,
    expr,
    first,
    inc,
    last,
    not_,
    or_,
    param,
    raw,
)

# run-tests.sh mounts the file beside the package in its container.
GOLDEN = Path(os.environ.get("FENNEC_GOLDEN") or Path(__file__).parents[2] / "builder-golden.json")
CASES = json.loads(GOLDEN.read_text(encoding="utf-8"))


def arg(x, held=None):
    """A step's argument as this builder takes it; `held` is the case's
    params by name, so each `$param` of a name is the one `param()`, and an
    inner query's share the case's."""
    held = {} if held is None else held

    def a(v):
        return arg(v, held)

    if isinstance(x, list):
        return [a(v) for v in x]
    if not isinstance(x, dict):
        return x
    if "$or" in x:
        return or_(*[a(c) for c in x["$or"]])
    if "$and" in x:
        return and_(*[a(c) for c in x["$and"]])
    if "$not" in x:
        return not_(a(x["$not"]))
    if "$raw" in x:
        return raw(x["$raw"][0], *[a(p) for p in x["$raw"][1:]])
    if "$inc" in x:
        return inc(x["$inc"])
    if "$expr" in x:
        return expr(x["$expr"][0], *[a(p) for p in x["$expr"][1:]])
    if "$bucket" in x:
        return bucket(*x["$bucket"])
    if "$distance" in x:
        return distance(x["$distance"][0], a(x["$distance"][1]))
    if "$countDistinct" in x:
        return count_distinct(x["$countDistinct"])
    if "$first" in x:
        return first(*x["$first"])
    if "$last" in x:
        return last(*x["$last"])
    if "$as" in x:
        return a(x["$as"][0]).as_(x["$as"][1])
    if "$date" in x:
        return datetime.fromisoformat(x["$date"].replace("Z", "+00:00"))
    if "$f32" in x:
        return array.array("f", x["$f32"])
    if "$param" in x:
        name, v = x["$param"]
        if name not in held:
            held[name] = param(a(v))
        return held[name]
    if "$query" in x:
        return chain(x["$query"], held)
    return {k: a(v) for k, v in x.items()}


def chain(steps, held):
    """An inner query's steps, `from` first, as the query they make: built
    by the step dispatcher a case's chain goes through, bound to no client."""
    first, *rest = steps
    assert first["op"] == "from", f"an inner query starts with from, not {first['op']}"
    q = collection(*[arg(v, held) for v in first.get("args", [])])
    for step in rest:
        q = step_of(q, step["op"], step.get("args", []), held)
    return q


SNAKE = {"parentKey": "parent_key", "ifAbsent": "if_absent"}


def kwargs(opts, held=None):
    return {SNAKE.get(k, k): arg(v, held) for k, v in (opts or {}).items()}


class Recorder:
    """A client that answers as the server would and keeps what it was sent."""

    def __init__(self):
        self.sent = []

    def query(self, text, params=None):
        self.sent.append((text, params))
        if text.split(" ", 1)[0] in ("put", "insert", "set", "del"):
            return {"affected": 0}
        return [{"count": 0}] if text.endswith(" count") else []


ENDPOINTS = {"rows", "first", "count", "explain", "insert", "insertNew", "update", "upsert", "delete"}

# Each writing step's builder method and the endpoint beside it, and how
# many documents and patches it takes before its options.
WRITES = {
    "toInsert": ("to_insert", 1),
    "insert": ("insert", 1),
    "toInsertNew": ("to_insert_new", 1),
    "insertNew": ("insert_new", 1),
    "toUpdate": ("to_update", 1),
    "update": ("update", 1),
    "toUpsert": ("to_upsert", 2),
    "upsert": ("upsert", 2),
    "toDelete": ("to_delete", 0),
    "delete": ("delete", 0),
}


def run(steps):
    rec = Recorder()
    first, *rest = steps
    if first["op"] == "call":
        return run_call(first, rest)
    assert first["op"] == "from"
    held = {}
    try:
        q = Query(*[arg(a, held) for a in first.get("args", [])], rec)
        for i, step in enumerate(rest):
            op, args = step["op"], step.get("args", [])
            last = i == len(rest) - 1
            if op == "toFennecQL":
                text, params = q.to_fennecql()
            elif op in WRITES:
                name, n = WRITES[op]
                given = [arg(a, held) for a in args[:n]]
                out = getattr(q, name)(*given, **kwargs(args[n] if len(args) > n else None, held))
                text, params = out if op.startswith("to") else rec.sent[-1]
            elif op in ENDPOINTS:
                getattr(q, op)()
                text, params = rec.sent[-1]
            else:
                q = step_of(q, op, args, held)
                continue
            assert last, f"{op} ends a chain"
            assert len(rec.sent) <= 1
            return {"text": text, "params": json.loads(json.dumps(params))}
        raise AssertionError("a chain ends with a statement")
    except FennecError as e:
        assert e.status == 0, e
        return {"error": str(e)}


def run_call(first, rest):
    """A call's chain: `call` (the name, its arguments or null, `{claims}`)
    then `toCall`."""
    assert [step["op"] for step in rest] == ["toCall"]
    name, *more = [arg(a) for a in first["args"]]
    try:
        text, params = call(name, *more[:1], **kwargs(first["args"][2] if len(first["args"]) > 2 else None)).to_call()
    except FennecError as e:
        assert e.status == 0, e
        return {"error": str(e)}
    return {"text": text, "params": json.loads(json.dumps(params))}


def step_of(q, op, args, held):
    def a(v):
        return arg(v, held)

    def opts(i):
        return kwargs(args[i] if len(args) > i else None, held)

    if op in ("select", "group"):
        return getattr(q, op)(*[a(v) for v in args])
    if op == "where":
        return q.where(*[a(v) for v in args])
    if op == "orWhere":
        return q.or_where(*[a(v) for v in args])
    if op in ("near", "rerank"):
        return getattr(q, op)(args[0], a(args[1]), **opts(2))
    if op == "fuse":
        return q.fuse(**opts(0))
    if op == "lookup":
        return q.lookup(args[0], **opts(1))
    if op == "order":
        return q.order(*[a(v) for v in args[:2]], **opts(2))
    if op == "highlight":
        return q.highlight(args[0], **opts(1))
    if op == "snippet":
        return q.snippet(args[0], args[1], **opts(2))
    if op == "facet":
        return q.facet(args[0], **opts(1))
    if op == "match":
        return q.match(args[0], a(args[1]), **opts(2))
    if op in ("limit", "offset"):
        return getattr(q, op)(*args)
    if op == "require":
        return q.require(args[0], **opts(1))
    raise AssertionError(f"no builder step {op}")


def test_the_golden_file_holds_enough_cases():
    assert len(CASES) >= 60
    assert sum("error" in k for k in CASES) >= 20


@pytest.mark.parametrize("case", CASES, ids=[k["name"] for k in CASES])
def test_a_golden_case(case):
    want = {"error": case["error"]} if "error" in case else {"text": case["text"], "params": case["params"]}
    assert run(case["steps"]) == want


def test_the_golden_file_holds_the_calls():
    calls = [k for k in CASES if k["steps"][0]["op"] == "call"]
    assert len(calls) >= 20
    assert sum("error" in k for k in calls) >= 7


def test_a_query_is_immutable_and_can_be_branched():
    base = collection("articles").where("year", ">=", 2024)
    a = base.where("tags", "has", "rust")
    b = base.limit(3)
    assert base.to_fennecql() == ("get articles where year >= $1", [2024])
    assert a.to_fennecql()[0] == "get articles where year >= $1 and tags has $2"
    assert b.to_fennecql() == ("get articles where year >= $1 limit $2", [2024, 3])


def test_a_call_is_immutable_and_its_values_are_normalised_as_a_query_s():
    c = call("since", {"at": datetime(2026, 9, 19, 12, 34, 56, 789000), "embed": array.array("f", [0.5, -0.25, 1])})
    assert isinstance(c, Call) and c.name == "since"
    assert c.to_call() == (
        "call since $1",
        [{"at": "2026-09-19T12:34:56.789Z", "embed": [0.5, -0.25, 1.0]}],
    )
    # Asked again it says the same, and nothing of it can be set.
    assert c.to_call() == c.to_call()
    for change in (lambda: setattr(c, "_name", "other"), lambda: setattr(c, "extra", 1), lambda: delattr(c, "_args")):
        with pytest.raises(AttributeError):
            change()
    # Any Mapping is an object of values by name.
    from types import MappingProxyType

    assert call("t", MappingProxyType({"a": (1, 2)})).to_call() == ("call t $1", [{"a": [1, 2]}])
    with pytest.raises(FennecError, match="cannot be used as a fennecdb value"):
        call("t", {"a": object()}).to_call()


def test_a_call_s_name_is_checked_and_its_arguments_are_an_object():
    for name in ["stamp\n", "a b", "a-b", "", "1x", "ü", None, 5, b"stamp"]:
        with pytest.raises(FennecError, match="invalid statement name") as e:
            Call(name)
        assert e.value.status == 0
    for args in ([1, 2], (1, 2), "text", 5, True):
        with pytest.raises(FennecError, match="a call's arguments are an object of values by name"):
            call("t", args)
    for claims in (["sub"], "alice", 0, False):
        with pytest.raises(FennecError, match="a call's claims are an object of values by name"):
            call("t", None, claims=claims)
    # An empty object is one argument, None none; claims are keyword-only.
    assert call("t", {}).to_call() == ("call t $1", [{}])
    assert call("t", None, claims={}).to_call() == ("call t as $1", [{}])
    with pytest.raises(TypeError):
        call("t", None, {"sub": "x"})


def test_a_reason_that_utf8_cannot_send_is_refused_as_the_builder_s():
    # A lone surrogate is a str to Python and no text to UTF-8: refused as
    # a reason of no bytes, a FennecError, never the UnicodeEncodeError.
    for reason in ["\ud800", "a\udfffb"]:
        with pytest.raises(FennecError, match="a text of 1 to 200 bytes"):
            collection("t").where("id", 1).to_delete(require=1, reason=reason)
        with pytest.raises(FennecError, match="a text of 1 to 200 bytes"):
            collection("t").require(1, reason=reason)


def test_an_unbound_query_has_text_but_cannot_run():
    q = collection("t")
    assert q.to_fennecql() == ("get t", [])
    with pytest.raises(FennecError, match="not bound"):
        q.rows()


def test_a_tuple_a_numpy_like_list_and_a_naive_datetime_are_values():
    class Vec:
        def tolist(self):
            return [0.5, 0.25]

    q = collection("t").where("v", "=", Vec()).where("t", "in", ("a", "b"))
    q = q.where("at", ">=", datetime(2026, 1, 2, 3, 4, 5, 678000))
    assert q.to_fennecql()[1] == [[0.5, 0.25], "a", "b", "2026-01-02T03:04:05.678Z"]
    with pytest.raises(FennecError, match="cannot be used as a fennecdb value"):
        collection("t").where("v", "=", object()).to_fennecql()


def test_a_param_is_one_parameter_by_its_identity():
    amount = param(300)
    assert isinstance(amount, Param) and amount.value == 300
    q = collection("accounts").where({"name": "ada", "balance": {"gte": amount}})
    assert q.to_update({"balance": expr("balance - ?", amount)}, require=1) == (
        "set accounts {balance: balance - $1} where name = $2 and balance >= $1 require 1",
        [300, "ada"],
    )
    # Two params of one value are two parameters, as two values are.
    assert collection("t").where("a", param(1)).where("b", param(1)).to_fennecql() == (
        "get t where a = $1 and b = $2",
        [1, 1],
    )
    # Every text numbers afresh: asked twice, a query says the same.
    twice = collection("t").where("a", amount).where("b", "in", [amount, 7])
    assert twice.to_fennecql() == twice.to_fennecql() == ("get t where a = $1 and b in [$1, $2]", [300, 7])
    # A param is a value, never an operator map, and a point's or metres'
    # check looks through it to its value.
    assert collection("t").where({"a": param({"gte": 1})}).to_fennecql() == ("get t where a = $1", [{"gte": 1}])
    at, far = param([13.4, 52.5]), param(500)
    assert collection("t").where({"loc": {"distance": {"from": at, "lte": far}}}).order(
        distance("loc", at)
    ).to_fennecql() == ("get t where distance(loc, $1) <= $2 order distance(loc, $1) asc", [[13.4, 52.5], 500])
    with pytest.raises(FennecError, match="distance compares metres"):
        collection("t").where({"loc": {"distance": {"from": at, "lte": param("far")}}})
    with pytest.raises(FennecError, match="distance takes"):
        collection("t").where({"loc": {"distance": {"from": param("here"), "lte": 5}}})
    # A param is a value of its own: not one of another param, not an
    # element inside a value, and never changed once made.
    with pytest.raises(FennecError, match=r"param\(\) takes a value, not another param\(\)"):
        param(amount)
    with pytest.raises(FennecError, match="cannot be used as a fennecdb value"):
        collection("t").where("a", [amount]).to_fennecql()
    with pytest.raises(AttributeError):
        amount.value = 1


def test_insert_new_sends_insert_and_takes_no_if_absent():
    rec = Recorder()
    q = Query("docs", rec)
    assert q.insert_new({"id": 1, "title": "a"}) == 0
    assert q.insert_new([]) == 0
    assert q.insert_new([], returning=True) == []
    assert rec.sent == [("insert docs {id: $1, title: $2}", [1, "a"])]
    # `if absent` passes over a held id, which is insert's: this one refuses it.
    with pytest.raises(TypeError):
        q.to_insert_new({"id": 1}, if_absent=True)

    class Awaited:
        def __init__(self):
            self.sent = []

        async def query(self, text, params=None):
            self.sent.append((text, params))
            return {"affected": 1}

    client = Awaited()
    assert asyncio.run(AsyncQuery("docs", client).insert_new({"id": 1}, require=1)) == 1
    assert client.sent == [("insert docs {id: $1} require 1", [1])]


def test_an_inner_query_comes_from_a_client_or_from_none():
    rec = Recorder()
    want = ("get orders where customer in (get customers select id where country = $1)", ["TR"])
    for inner in (
        Query("customers", rec).select("id").where("country", "TR"),
        AsyncQuery("customers", object()).select("id").where("country", "TR"),
        collection("customers").select("id").where("country", "TR"),
    ):
        assert collection("orders").where("customer", "in", inner).to_fennecql() == want
        assert collection("orders").where({"customer": {"in": inner}}).to_fennecql() == want
    # Its text goes inside the query around it, never on its own.
    assert rec.sent == []


# ---------------------------------------------------------- against the server


@pytest.fixture()
def shelf(client):
    name = fresh("shelf")
    notes = fresh("notes")
    client.query(
        f"create collection {name} (title text, year int @sorted, lang text @hash, "
        "tags [text], body text @text, embed vector<3> @hnsw(cosine))"
    )
    client.query(f"create collection {notes} (doc_id int @hash, stars int)")
    docs = client.collection(name)
    assert (
        docs.insert(
            [
                {"title": "Night at the oasis", "year": 2024, "lang": "en", "tags": ["desert"],
                 "body": "a night under the stars at the oasis", "embed": [0.1, 0.2, 0.3]},
                {"title": "Dunes", "year": 2021, "lang": "en", "tags": ["desert", "sand"],
                 "body": "dunes move with the wind", "embed": [0.9, 0.1, 0.0]},
                {"title": "Kum", "year": 2023, "lang": "tr", "tags": ["sand"],
                 "body": "kum ve rüzgar", "embed": [0.2, 0.8, 0.1]},
            ]
        )
        == 3
    )
    client.collection(notes).insert([{"doc_id": 1, "stars": 5}, {"doc_id": 1, "stars": 3}, {"doc_id": 3, "stars": 4}])
    try:
        yield name, notes
    finally:
        client.query(f"drop collection if exists {name}")
        client.query(f"drop collection if exists {notes}")


def test_builder_answers_are_the_texts_answers(client, shelf):
    name, notes = shelf
    docs = client.collection(name)
    v = [0.1, 0.2, 0.3]
    pairs = [
        (docs.select("title").where("year", ">=", 2022).order("year", "desc"),
         f"get {name} select title where year >= $1 order year desc", [2022]),
        (docs.select("title").where({"lang": "en", "tags": {"has": "sand"}}),
         f"get {name} select title where lang = $1 and tags has $2", ["en", "sand"]),
        (docs.select("title").where(or_({"lang": "tr"}, {"year": {"lt": 2022}})).order("title"),
         f"get {name} select title where lang = $1 or year < $2 order title asc", ["tr", 2022]),
        (docs.select("title").near("embed", v).limit(2),
         f"get {name} select title near embed $1 limit $2", [v, 2]),
        (docs.select("title").match("body", "oasis stars"),
         f"get {name} select title match body $1", ["oasis stars"]),
        (docs.select("title").where("id", "in", [1, 3]).lookup(notes, on="doc_id", select=["stars"], order=[("stars", "desc")]),
         f"get {name} select title where id in [$1, $2] lookup {notes} on doc_id select stars order stars desc", [1, 3]),
        (docs.select("lang", "count(*)").group("lang").order("lang"),
         f"get {name} select lang, count(*) group lang order lang asc", []),
    ]
    for q, text, params in pairs:
        assert q.to_fennecql() == (text, params)
        assert q.rows() == client.query(text, params)
        assert q.rows()  # an answer worth comparing
    assert docs.where("lang", "en").count() == client.query(f"get {name} where lang = $1 count", ["en"])[0]["count"] == 2
    assert docs.order("year").first()["title"] == "Dunes"
    assert docs.where("lang", "xx").first() is None
    assert any("near" in line or "hnsw" in line.lower() for line in docs.near("embed", v).limit(1).explain())


def test_builder_writes_are_the_texts_writes(client, shelf):
    name, _ = shelf
    docs = client.collection(name)
    assert docs.where("lang", "tr").update({"year": 2025}) == 1
    assert client.query(f"get {name} select year where lang = $1", ["tr"]) == [{"year": 2025}]
    with pytest.raises(FennecError) as e:
        docs.delete()
    assert e.value.status == 0
    assert docs.where("year", "<", 2022).delete() == 1
    assert docs.count() == 2
    assert docs.delete(all=True) == 2
    assert docs.count() == 0


def test_the_async_builder_awaits_the_same_answers(client, shelf):
    name, notes = shelf

    async def go():
        async with AsyncClient(URL, TOKEN) as db:
            docs = db.collection(name)
            q = docs.select("title").where("year", ">=", 2022).order("year", "desc")
            assert await q.rows() == client.query(*q.to_fennecql())
            assert await docs.where("lang", "en").count() == 2
            assert (await docs.order("year").first())["title"] == "Dunes"
            assert await db.collection(notes).where("doc_id", 3).update({"stars": 1}) == 1
            assert await db.collection(notes).insert({"doc_id": 2, "stars": 2}) == 1
            assert await db.collection(notes).where("stars", "<=", 2).delete() == 2

    asyncio.run(go())


def test_marks_and_facets_come_back_where_the_server_puts_them(client):
    name = fresh("marks")
    client.query(f"create collection {name} (body text @text, kind text, meta json)")
    try:
        client.collection(name).insert(
            [
                {"body": "rust and go", "kind": "lang", "meta": {"lang": "en"}},
                {"body": "rust compiler", "kind": "tool", "meta": {"lang": "en"}},
                {"body": "python", "kind": "lang", "meta": {"lang": "tr"}},
            ]
        )
        docs = client.collection(name)
        rows = docs.select("kind").highlight("body").match("body", "rust").rows()
        # Offsets into the text, UTF-16 code units: `rust` is the first four.
        assert sorted((r["kind"], r["highlight(body)"]) for r in rows) == [
            ("lang", [[0, 4]]),
            ("tool", [[0, 4]]),
        ]
        assert rows.facets == {}
        tagged = docs.select("kind").highlight("body", pre="<b>", post="</b>").match("body", "rust")
        assert sorted(r["highlight(body)"] for r in tagged.rows()) == [
            "<b>rust</b> and go",
            "<b>rust</b> compiler",
        ]
        snip = docs.snippet("body", 5).match("body", "compiler").first()["snippet(body)"]
        assert snip == {"marks": [[5, 13]], "text": "rust compiler"}

        # Counted over every row matched, the page holding one of them.
        page = docs.select("kind").facet("kind").facet("meta.lang", top=1).order("kind").limit(1).rows()
        assert page == [{"kind": "lang"}]
        assert page.facets == {
            "kind": [FacetCount("lang", 2), FacetCount("tool", 1)],
            "meta.lang": [FacetCount("en", 2)],
        }
        assert docs.facet("kind").count() == 3
        # The client hands the same: `Rows` for a query with facets, the
        # bare list for one without, and a batch's result as it came.
        got = client.query(f"get {name} select kind order kind limit 1 facet kind")
        assert got == [{"kind": "lang"}] and got.facets["kind"][0] == ("lang", 2)
        assert type(client.query(f"get {name} select kind")) is list
        out = client.batch([(f"get {name} select kind limit 1 facet kind", [])])
        assert out["results"][0]["facets"]["kind"] == [
            {"value": "lang", "count": 2},
            {"value": "tool", "count": 1},
        ]

        async def go():
            async with AsyncClient(URL, TOKEN) as db:
                got = await db.collection(name).where("kind", "lang").facet("meta.lang").rows()
                assert len(got) == 2
                assert sorted(got.facets["meta.lang"]) == [("en", 1), ("tr", 1)]
                raw = await db.query(f"get {name} limit 1 facet kind top 1")
                assert raw.facets == {"kind": [FacetCount("lang", 2)]}

        asyncio.run(go())
    finally:
        client.query(f"drop collection if exists {name}")
