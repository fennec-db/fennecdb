package com.fennecdb

import kotlinx.coroutines.runBlocking
import org.junit.jupiter.api.DynamicTest
import org.junit.jupiter.api.TestFactory
import java.io.File
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The query builder held to integrations/builder-golden.json: the text and
 * parameters the JavaScript builder makes of every chain there, refusals by
 * their message, the endpoints' statements read off a recording executor.
 */
class GoldenTest {
    companion object {
        val cases: List<Row> by lazy {
            val path = System.getProperty("fennec.golden")
                ?: generateSequence(File("").absoluteFile) { it.parentFile }
                    .map { File(it, "integrations/builder-golden.json") }
                    .first { it.exists() }.path
            @Suppress("UNCHECKED_CAST")
            Json.parse(File(path).readText()) as List<Row>
        }

        /** The `$param`s of the case being run, by name: each use of a name is the one `param(...)`. */
        private var held = HashMap<String, Param>()

        /**
         * An argument as a Kotlin caller hands it over: `$date` a Date, `$f32`
         * a FloatArray, `$param` a [param] (the same one wherever its name
         * comes again), `$query` an inner query, an object a map.
         */
        fun value(v: Any?): Any? = when (v) {
            is Row -> when {
                v.containsKey("\$param") -> v.list("\$param")!!.let { p -> held.getOrPut(p[0] as String) { param(value(p[1])) } }
                v.containsKey("\$query") -> query(v)
                v.containsKey("\$date") -> java.util.Date(java.time.Instant.parse(v.string("\$date")).toEpochMilli())
                v.containsKey("\$f32") -> v.list("\$f32")!!.let { l -> FloatArray(l.size) { (l[it] as Number).toFloat() } }
                v.containsKey("\$inc") -> Computed.inc(v["\$inc"])
                v.containsKey("\$expr") -> v.list("\$expr")!!.let { Computed.expr(it[0] as String, *it.drop(1).map(::value).toTypedArray()) }
                v.containsKey("\$bucket") -> v.list("\$bucket")!!.let {
                    Computed.bucket(it[0] as String, it[1] as String, it.getOrNull(2) as String?)
                }
                v.containsKey("\$countDistinct") -> Computed.countDistinct(v.string("\$countDistinct")!!)
                v.containsKey("\$distance") -> v.list("\$distance")!!.let { Computed.distance(it[0] as String, value(it[1])) }
                v.containsKey("\$first") -> v.list("\$first")!!.let { Computed.first(it[0] as String, it.getOrNull(1) as String?) }
                v.containsKey("\$last") -> v.list("\$last")!!.let { Computed.last(it[0] as String, it.getOrNull(1) as String?) }
                v.containsKey("\$as") -> v.list("\$as")!!.let { (value(it[0]) as Computed).alias(it[1] as String) }
                else -> LinkedHashMap<String, Any?>().also { m -> v.forEach { (k, x) -> m[k] = value(x) } }
            }
            is List<*> -> v.map(::value)
            else -> v
        }

        /** An inner query of `in`: the steps of a chain from `from`, unended. */
        fun query(v: Row): Query {
            val steps = v.list("\$query")!!.map { it as Row }
            var q = Query.from(steps[0].list("args")!![0] as String)
            for (s in steps.drop(1)) q = step(q, s.string("op")!!, s.list("args") ?: emptyList())
            return q
        }

        fun cond(v: Any?): Any {
            val r = v as Row
            r.list("\$or")?.let { return Cond.or(*it.map(::cond).toTypedArray()) }
            r.list("\$and")?.let { return Cond.and(*it.map(::cond).toTypedArray()) }
            if (r.containsKey("\$not")) return Cond.not(cond(r["\$not"]))
            r.list("\$raw")?.let { return Cond.raw(it[0] as String, *it.drop(1).map(::value).toTypedArray()) }
            return value(r)!!
        }

        fun opt(a: List<Any?>, at: Int, name: String): Any? = (a.getOrNull(at) as? Row)?.get(name)

        fun names(v: Any?): List<String>? = when (v) {
            null -> null
            is String -> listOf(v)
            else -> (v as List<*>).map { it as String }
        }

        fun keys(v: Any?): List<SortKey> = when (v) {
            null -> emptyList()
            is String -> listOf(SortKey(v))
            else -> (v as List<*>).map { k ->
                if (k is String) {
                    SortKey(k)
                } else {
                    val a = k as List<*>
                    SortKey(a[0] as String, a.getOrNull(1) as? String ?: "asc", (a.getOrNull(2) as? Row)?.string("collate"))
                }
            }
        }

        fun step(q: Query, op: String, a: List<Any?>): Query = when {
            op == "select" -> q.select(a.map { value(it)!! })
            op == "where" && a.size == 3 -> q.where(a[0] as String, a[1] as String, value(a[2]))
            op == "where" && a.size == 2 -> q.where(a[0] as String, value(a[1]))
            op == "where" -> q.where(cond(a[0]))
            op == "orWhere" && a.size == 3 -> q.orWhere(a[0] as String, a[1] as String, value(a[2]))
            op == "orWhere" && a.size == 2 -> q.orWhere(a[0] as String, value(a[1]))
            op == "orWhere" -> q.orWhere(cond(a[0]))
            op == "near" -> q.near(a[0] as String, value(a[1]), opt(a, 2, "ef") as Long?, opt(a, 2, "exact") as Boolean? ?: false)
            op == "rerank" -> q.rerank(a[0] as String, value(a[1]), opt(a, 2, "candidates") as Long?)
            op == "match" -> q.matchOf(a[0] as String, value(a[1]), opt(a, 2, "fuzzy"), opt(a, 2, "prefix"))
            op == "fuse" -> q.fuse(opt(a, 0, "k") as Long?, opt(a, 0, "candidates") as Long?)
            // One key as itself, several (or none) as the list they are.
            op == "group" && a.size == 1 -> (value(a[0]) as Any).let { if (it is List<*>) q.group(it.map { k -> k!! }) else q.group(it) }
            op == "group" -> q.group(a.map { value(it)!! })
            // A key is a field's name, or an expression: `$expr`, `$distance`, `$as` (refused).
            op == "order" -> a[0].let { key ->
                val direction = a.getOrNull(1) as? String ?: "asc"
                val collate = opt(a, 2, "collate") as String?
                if (key is String) q.order(key, direction, collate) else q.order(value(key) as Computed, direction, collate)
            }
            op == "limit" -> q.limit(a[0] as Long)
            op == "offset" -> q.offset(a[0] as Long)
            op == "require" -> q.require(a[0] as Long, opt(a, 1, "reason") as String?)
            // Through the untyped call: the file holds a tag that is no text.
            op == "highlight" -> q.mark(a[0] as String, null, null, opt(a, 1, "pre"), opt(a, 1, "post"))
            op == "snippet" -> q.mark(a[0] as String, a[1] as Long, opt(a, 2, "ellipsis"), opt(a, 2, "pre"), opt(a, 2, "post"))
            op == "facet" -> q.facetOf(a[0] as String, opt(a, 1, "top") as Long?, opt(a, 1, "ranges") as List<Any?>?, opt(a, 1, "disjunctive"))
            op == "lookup" -> q.lookup(
                a[0] as String,
                // A name, or a list of them: a key over several fields too.
                on = names(opt(a, 1, "on")).orEmpty(),
                parentKey = names(opt(a, 1, "parentKey")),
                // A column, or a list of them: an aggregate, an expression too.
                select = opt(a, 1, "select")?.let { s -> if (s is String) listOf(s) else (s as List<*>).map { value(it)!! } },
                group = opt(a, 1, "group")?.let { g -> if (g is String) listOf(g) else (g as List<*>).map { value(it)!! } },
                where = opt(a, 1, "where")?.let(::cond),
                required = opt(a, 1, "required") as Boolean? ?: false,
                order = keys(opt(a, 1, "order")),
                limit = opt(a, 1, "limit") as Long?,
                offset = opt(a, 1, "offset") as Long?,
            )
            else -> error("no builder step $op")
        }

        /**
         * A chain that starts with `call(name, args, {claims})` and ends in
         * `toCall`. The arguments and the claims go through the untyped
         * check, which a list or a text cannot be handed to the typed
         * factory by (the file holds both, refused by their message).
         */
        private fun call(steps: List<Row>): Triple<String?, List<Any?>?, String?> {
            val a = steps[0].list("args")!!
            val made = Call.made(a[0] as String, value(a.getOrNull(1)), opt(a, 2, "claims")?.let(::value))
            val op = steps.getOrNull(1)?.string("op") ?: return Triple(null, null, "a chain ends with a statement")
            if (op != "toCall") error("no call step $op")
            val st = made.toCall()
            return Triple(st.text, st.params, null)
        }

        /** The statement a chain makes, or the builder's refusal. */
        fun run(steps: List<Row>): Triple<String?, List<Any?>?, String?> = runBlocking {
            var sent: Statement? = null
            held = HashMap()
            try {
                if (steps[0].string("op") == "call") return@runBlocking call(steps)
                var q = Query.from(steps[0].list("args")!![0] as String).bind { text, params ->
                    sent = Statement(text, params)
                    when {
                        Regex("^(put|insert|set|del) ").containsMatchIn(text) -> Answer.Affected(0)
                        text.endsWith(" count") -> Answer.Rows(listOf("count"), listOf(Row(mapOf("count" to 0L))))
                        else -> Answer.Rows(emptyList(), emptyList())
                    }
                }
                for (s in steps.drop(1)) {
                    val op = s.string("op")!!
                    val a = s.list("args") ?: emptyList()
                    val all = { at: Int -> opt(a, at, "all") as Boolean? ?: false }
                    val require = { at: Int -> (opt(a, at, "require") as Number?)?.toLong() }
                    val reason = { at: Int -> opt(a, at, "reason") as String? }
                    // JavaScript's `returning: true` is every field, `*` here.
                    val returning = { at: Int ->
                        when (val r = opt(a, at, "returning")) {
                            null, false -> null
                            true -> listOf("*")
                            else -> (r as List<*>).map { it as String }
                        }
                    }
                    val made: Statement? = when (op) {
                        "toFennecQL" -> q.toFennecQL()
                        "toInsert" -> q.toInsert(
                            value(a[0]), opt(a, 1, "ifAbsent") as Boolean? ?: false, require(1), returning(1), reason(1))
                        "toInsertNew" -> q.toInsertNew(value(a[0]), require(1), returning(1), reason(1))
                        "toUpdate" -> q.toUpdate(value(a[0]), all(1), require(1), returning(1), reason(1))
                        "toUpsert" -> q.toUpsert(value(a[0]), value(a[1]), require(2), returning(2), reason(2))
                        "toDelete" -> q.toDelete(all(0), require(0), returning(0), reason(0))
                        "rows" -> null.also { q.rows() }
                        "first" -> null.also { q.first() }
                        "count" -> null.also { q.count() }
                        "explain" -> null.also { q.explain() }
                        "insert" -> null.also {
                            val absent = opt(a, 1, "ifAbsent") as Boolean? ?: false
                            when (val r = returning(1)) {
                                null -> q.insert(value(a[0]), absent, require(1), reason(1))
                                else -> q.insertReturning(value(a[0]), r, absent, require(1), reason(1))
                            }
                        }
                        "insertNew" -> null.also {
                            when (val r = returning(1)) {
                                null -> q.insertNew(value(a[0]), require(1), reason(1))
                                else -> q.insertNewReturning(value(a[0]), r, require(1), reason(1))
                            }
                        }
                        "update" -> null.also {
                            when (val r = returning(1)) {
                                null -> q.update(value(a[0]), all(1), require(1), reason(1))
                                else -> q.updateReturning(value(a[0]), r, all(1), require(1), reason(1))
                            }
                        }
                        "upsert" -> null.also {
                            when (val r = returning(2)) {
                                null -> q.upsert(value(a[0]), value(a[1]), require(2), reason(2))
                                else -> q.upsertReturning(value(a[0]), value(a[1]), r, require(2), reason(2))
                            }
                        }
                        "delete" -> null.also {
                            when (val r = returning(0)) {
                                null -> q.delete(all(0), require(0), reason(0))
                                else -> q.deleteReturning(r, all(0), require(0), reason(0))
                            }
                        }
                        else -> {
                            q = step(q, op, a)
                            continue
                        }
                    }
                    val st = made ?: sent
                    return@runBlocking Triple(st?.text, st?.params, null)
                }
                Triple(null, null, "a chain ends with a statement")
            } catch (e: FennecException) {
                Triple(null, null, e.message)
            }
        }

        /** JSON compared by value: a number the file writes as 1 and the builder holds as 1.0 is one number. */
        fun same(a: Any?, b: Any?): Boolean = when {
            a is Number && b is Number -> a.toDouble() == b.toDouble()
            a is FloatArray -> same(a.map { it.toDouble() }, b)
            a is List<*> && b is List<*> -> a.size == b.size && a.indices.all { same(a[it], b[it]) }
            a is Map<*, *> && b is Map<*, *> -> a.size == b.size && a.all { (k, v) -> b.containsKey(k) && same(v, b[k]) }
            else -> a == b
        }
    }

    @Test
    fun theFileHoldsEnoughCases() {
        assertTrue(cases.size >= 150, "${cases.size} cases")
    }

    @Suppress("UNCHECKED_CAST")
    @TestFactory
    fun golden(): List<DynamicTest> = cases.map { c ->
        DynamicTest.dynamicTest(c.string("name")) {
            val (text, params, error) = run(c.list("steps") as List<Row>)
            val want = c.string("error")
            if (want != null) {
                assertEquals(want, error, "made $text")
            } else {
                assertNull(error)
                assertEquals(c.string("text"), text)
                assertTrue(same(Json.parse(Json.write(params)), c["params"]), "params ${Json.write(params)}, want ${c["params"]}")
            }
        }
    }

    @Test
    fun theBuilderAnswersAsTheText() = runBlocking {
        val db = Fennec.memory()
        db.execute(
            "create collection shelf (title text, year int @sorted, lang text @hash, tags [text], body text @text, embed vector<3> @hnsw(cosine)); " +
                "create collection notes (doc_id int @hash, stars int)",
        )
        val shelf = db.from("shelf")
        assertEquals(
            3,
            shelf.insert(
                listOf(
                    mapOf("title" to "Night at the oasis", "year" to 2024, "lang" to "en", "tags" to listOf("desert"), "body" to "a night under the stars at the oasis", "embed" to floatArrayOf(0.1f, 0.2f, 0.3f)),
                    mapOf("title" to "Dunes", "year" to 2021, "lang" to "en", "tags" to listOf("desert", "sand"), "body" to "dunes move with the wind", "embed" to floatArrayOf(0.9f, 0.1f, 0f)),
                    mapOf("title" to "Kum", "year" to 2023, "lang" to "tr", "tags" to listOf("sand"), "body" to "kum ve rüzgar", "embed" to floatArrayOf(0.2f, 0.8f, 0.1f)),
                ),
            ),
        )
        db.from("notes").insert(listOf(mapOf("doc_id" to 1, "stars" to 5), mapOf("doc_id" to 1, "stars" to 3), mapOf("doc_id" to 3, "stars" to 4)))
        val v = floatArrayOf(0.1f, 0.2f, 0.3f)
        val pairs = listOf(
            Triple(shelf.select("title").where("year", ">=", 2022).order("year", "desc"), "get shelf select title where year >= \$1 order year desc", listOf<Any?>(2022)),
            Triple(shelf.select("title").where(mapOf("lang" to "en", "tags" to mapOf("has" to "sand"))), "get shelf select title where lang = \$1 and tags has \$2", listOf("en", "sand")),
            Triple(shelf.select("title").where(Cond.or(Cond.cmp("lang", "=", "tr"), Cond.cmp("year", "<", 2022))).order("title"), "get shelf select title where lang = \$1 or year < \$2 order title asc", listOf("tr", 2022)),
            Triple(shelf.select("title").near("embed", v).limit(2), "get shelf select title near embed \$1 limit \$2", listOf(v, 2)),
            Triple(shelf.select("title").match("body", "oasis stars"), "get shelf select title match body \$1", listOf("oasis stars")),
            Triple(
                shelf.select("title").where("id", "in", listOf(1, 3)).lookup("notes", on = "doc_id", select = listOf("stars"), order = listOf(SortKey("stars", "desc"))),
                "get shelf select title where id in [\$1, \$2] lookup notes on doc_id select stars order stars desc", listOf(1, 3),
            ),
            Triple(shelf.select("lang", "count(*)").group("lang").order("lang"), "get shelf select lang, count(*) group lang order lang asc", listOf()),
        )
        for ((q, text, params) in pairs) {
            assertEquals(text, q.toFennecQL().text)
            val got = q.rows()
            assertTrue(got.isNotEmpty(), text)
            assertEquals(db.query(text, *params.toTypedArray()), got, text)
        }
        assertEquals(2, shelf.where("lang", "en").count())
        assertEquals("Dunes", shelf.order("year").first()?.string("title"))
        assertTrue(shelf.near("embed", v).limit(1).explain().isNotEmpty())
        assertEquals(1, shelf.where("lang", "tr").update(mapOf("year" to 2025)))
        kotlin.test.assertFailsWith<FennecException> { shelf.delete() }
        assertEquals(1, shelf.where("year", "<", 2022).delete())
        assertEquals(2, shelf.delete(all = true))
        db.close()
    }

    @Test
    fun aQueryIsAValueToBranchFrom() {
        val b = Query.from("articles").where("year", ">=", 2024)
        assertEquals("get articles where year >= \$1", b.toFennecQL().text)
        assertEquals("get articles where year >= \$1 and tags has \$2", b.where("tags", "has", "rust").toFennecQL().text)
        assertEquals("get articles where year >= \$1 limit \$2", b.limit(3).toFennecQL().text)
        assertEquals(listOf("articles"), b.reads)
        assertEquals(listOf("articles", "notes"), b.lookup("notes", on = "article_id").reads)
        assertNull(b.where(Cond.raw("id in (get x select id)")).reads)
    }

    @Test
    fun anInnerQueryIsReadToo() {
        val inner = Query.from("customers").select("id")
        val q = Query.from("orders").where("customer", "in", inner)
        assertEquals(listOf("orders", "customers"), q.reads)
        assertEquals(listOf("orders", "customers", "notes"), q.lookup("notes", on = "order_id").reads)
        // An inner query that may read one more makes the whole unknown.
        assertNull(Query.from("orders").where("customer", "in", inner.where(Cond.raw("id in (get x select id)"))).reads)
        // Only an `in` takes a query: any other operator refuses it as a value.
        val e = kotlin.test.assertFailsWith<FennecException> { Query.from("orders").where("customer", "=", inner).toFennecQL() }
        assertEquals("this object cannot be used as a fennecdb value (a query is one only as an `in`'s list)", e.message)
    }

    @Test
    fun aParamIsOneParameterByIdentity() {
        val p = param(300)
        val st = Query.from("accounts").where("a", ">=", p).where("b", "<", p).where("c", "=", param(300)).toFennecQL()
        assertEquals("get accounts where a >= \$1 and b < \$1 and c = \$2", st.text)
        assertEquals(listOf<Any?>(300, 300), st.params)
        // A param is a value, never a map of operators, and not another param's value.
        assertEquals("get accounts where a = \$1", Query.from("accounts").where("a", p).toFennecQL().text)
        assertEquals("param() takes a value, not another param()", kotlin.test.assertFailsWith<FennecException> { param(p) }.message)
        assertEquals("get accounts where a in [\$1, \$2, \$1]", Query.from("accounts").where("a", "in", listOf(p, 1, p)).toFennecQL().text)
    }
}
