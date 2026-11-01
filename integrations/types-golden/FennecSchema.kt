// generated from the fennecdb schema: schema.fennecql
// fennec types --lang kotlin schema.fennecql > FennecSchema.kt
// Do not edit by hand -- regenerate when the schema changes.

package fennecschema

import com.fennecdb.Row

/** A row of `articles`. */
data class Articles(
    val id: Long,
    val title: String, // text collate tr @text(k1=0.9, b=0.4, prefix=6, chars) required
    val year: Long?, // int @hash
    val score: Double?, // float @sorted
    val draft: Boolean, // bool required
    val published: String?, // timestamp @ttl(30d)
    val cover: List<Int>?, // bytes
    val meta: Any?, // json
    val loc: List<Double>?, // geo @geo
    val embed: List<Float>?, // vector<384> @hnsw(cosine, m=8, ef_construction=200, ef_search=100)
    val small: List<Float>?, // vector<4, f16> @hnsw(l2, m=16, ef_construction=200, ef_search=100, quant=int8)
    val splade: String?, // sparse<30522> @inverted
    val tags: List<String>?, // [text] @hash
    val counts: List<Long>, // [int] required
    val slug: String?, // text nocase @unique
) {
    companion object {
        /** The row as the binding reads it. */
        fun from(row: Row) = Articles(
            id = (row["id"] as Number).toLong(),
            title = row["title"]!!.let { v -> (v as String) },
            year = row["year"]?.let { v -> (v as Number).toLong() },
            score = row["score"]?.let { v -> (v as Number).toDouble() },
            draft = row["draft"]!!.let { v -> (v as Boolean) },
            published = row["published"]?.let { v -> (v as String) },
            cover = row["cover"]?.let { v -> (v as List<*>).map { (it as Number).toInt() } },
            meta = row["meta"]?.let { v -> v },
            loc = row["loc"]?.let { v -> (v as List<*>).map { (it as Number).toDouble() } },
            embed = row["embed"]?.let { v -> (v as List<*>).map { (it as Number).toFloat() } },
            small = row["small"]?.let { v -> (v as List<*>).map { (it as Number).toFloat() } },
            splade = row["splade"]?.let { v -> (v as String) },
            tags = row["tags"]?.let { v -> (v as List<*>).map { v -> (v as String) } },
            counts = row["counts"]!!.let { v -> (v as List<*>).map { v -> (v as Number).toLong() } },
            slug = row["slug"]?.let { v -> (v as String) },
        )
    }
}

/** A row of `product_reviews`. */
data class ProductReviews(
    val id: Long,
    val productId: Long, // int @hash required
    val stars: Long?, // int
    val notes: String?, // text collate und
) {
    companion object {
        /** The row as the binding reads it. */
        fun from(row: Row) = ProductReviews(
            id = (row["id"] as Number).toLong(),
            productId = row["product_id"]!!.let { v -> (v as Number).toLong() },
            stars = row["stars"]?.let { v -> (v as Number).toLong() },
            notes = row["notes"]?.let { v -> (v as String) },
        )
    }
}

/** A row of `kişiler`. */
data class Kişiler(
    val id: Long,
    val ad: String?, // text
    val yaş: Long?, // int
) {
    companion object {
        /** The row as the binding reads it. */
        fun from(row: Row) = Kişiler(
            id = (row["id"] as Number).toLong(),
            ad = row["ad"]?.let { v -> (v as String) },
            yaş = row["yaş"]?.let { v -> (v as Number).toLong() },
        )
    }
}

/** A row of `text`. */
data class Text(
    val id: Long,
    val body: String?, // text
) {
    companion object {
        /** The row as the binding reads it. */
        fun from(row: Row) = Text(
            id = (row["id"] as Number).toLong(),
            body = row["body"]?.let { v -> (v as String) },
        )
    }
}

/** The arguments of `article_count`: No parameters, and a count. */
class ArticleCountArgs {
    /** The arguments as a call takes them: one left `null` is left out, for its default. */
    fun toMap(): Map<String, Any?> = emptyMap()
}

/** A row of what `article_count` answers. */
data class ArticleCountRow(
    val count: Long, // a count
) {
    companion object {
        /** The row as the binding reads it. */
        fun from(row: Row) = ArticleCountRow(
            count = row["count"]!!.let { v -> (v as Number).toLong() },
        )
    }
}

/** The arguments of `every_type`: Every parameter type, and two with a default. */
data class EveryTypeArgs(
    val flag: Boolean, // bool
    val year: Long, // int
    val score: Double, // float
    val title: String, // text
    val cover: List<Int>, // bytes
    val at: String, // timestamp
    val meta: Any, // json
    val loc: List<Double>, // geo
    val embed: List<Float>, // vector<384>
    val small: List<Float>, // vector<4, f16>
    val splade: String, // sparse<30522>
    val tags: List<String>, // [text]
    val counts: List<Long>, // [int]
    val limit: Long? = null, // int = 10
    val note: String? = null, // text = null
) {
    /** The arguments as a call takes them: one left `null` is left out, for its default. */
    fun toMap(): Map<String, Any?> {
        val m = mutableMapOf<String, Any?>()
        m["flag"] = this.flag
        m["year"] = this.year
        m["score"] = this.score
        m["title"] = this.title
        m["cover"] = this.cover
        m["at"] = this.at
        m["meta"] = this.meta
        m["loc"] = this.loc
        m["embed"] = this.embed
        m["small"] = this.small
        m["splade"] = this.splade
        m["tags"] = this.tags
        m["counts"] = this.counts
        if (this.limit != null) m["limit"] = this.limit
        if (this.note != null) m["note"] = this.note
        return m
    }
}

/** A row of what `every_type` answers. */
data class EveryTypeRow(
    val id: Long,
    val title: String, // text collate tr @text(k1=0.9, b=0.4, prefix=6, chars) required
    val year: Long?, // int @hash
    val draft: Boolean, // bool required
    val published: String?, // timestamp @ttl(30d)
    val cover: List<Int>?, // bytes
    val meta: Any?, // json
    val loc: List<Double>?, // geo @geo
    val embed: List<Float>?, // vector<384> @hnsw(cosine, m=8, ef_construction=200, ef_search=100)
    val small: List<Float>?, // vector<4, f16> @hnsw(l2, m=16, ef_construction=200, ef_search=100, quant=int8)
    val splade: String?, // sparse<30522> @inverted
    val tags: List<String>?, // [text] @hash
    val counts: List<Long>, // [int] required
    val slug: String?, // text nocase @unique
) {
    companion object {
        /** The row as the binding reads it. */
        fun from(row: Row) = EveryTypeRow(
            id = (row["id"] as Number).toLong(),
            title = row["title"]!!.let { v -> (v as String) },
            year = row["year"]?.let { v -> (v as Number).toLong() },
            draft = row["draft"]!!.let { v -> (v as Boolean) },
            published = row["published"]?.let { v -> (v as String) },
            cover = row["cover"]?.let { v -> (v as List<*>).map { (it as Number).toInt() } },
            meta = row["meta"]?.let { v -> v },
            loc = row["loc"]?.let { v -> (v as List<*>).map { (it as Number).toDouble() } },
            embed = row["embed"]?.let { v -> (v as List<*>).map { (it as Number).toFloat() } },
            small = row["small"]?.let { v -> (v as List<*>).map { (it as Number).toFloat() } },
            splade = row["splade"]?.let { v -> (v as String) },
            tags = row["tags"]?.let { v -> (v as List<*>).map { v -> (v as String) } },
            counts = row["counts"]!!.let { v -> (v as List<*>).map { v -> (v as Number).toLong() } },
            slug = row["slug"]?.let { v -> (v as String) },
        )
    }
}

/** The arguments of `forget_reviews`. */
data class ForgetReviewsArgs(
    val product: Long, // int
) {
    /** The arguments as a call takes them: one left `null` is left out, for its default. */
    fun toMap(): Map<String, Any?> {
        val m = mutableMapOf<String, Any?>()
        m["product"] = this.product
        return m
    }
}

// `forget_reviews` answers the number of rows it wrote: no row type.

/** The arguments of `headlines`. */
data class HeadlinesArgs(
    val year: Long, // int
) {
    /** The arguments as a call takes them: one left `null` is left out, for its default. */
    fun toMap(): Map<String, Any?> {
        val m = mutableMapOf<String, Any?>()
        m["year"] = this.year
        return m
    }
}

/** A row of what `headlines` answers. */
data class HeadlinesRow(
    val headline: Any?, // not typed: `title` given another name
    val doubled: Any?, // not typed: an expression
    val metaLang: Any?, // not typed: a path into `meta`
    val productReviews: Any?, // not typed: the rows of `product_reviews` that `lookup` attached
) {
    companion object {
        /** The row as the binding reads it. */
        fun from(row: Row) = HeadlinesRow(
            headline = row["headline"]?.let { v -> v },
            doubled = row["doubled"]?.let { v -> v },
            metaLang = row["meta.lang"]?.let { v -> v },
            productReviews = row["product_reviews"]?.let { v -> v },
        )
    }
}

/** The arguments of `move_reviews`: Parameters named as Python, Swift, Kotlin and Dart keywords: *\/ and /\* in a comment. */
data class MoveReviewsArgs(
    val from: Long, // int
    val to: Long, // int
    val `in`: Boolean? = null, // bool = true
    val class_: String? = null, // text = null
) {
    /** The arguments as a call takes them: one left `null` is left out, for its default. */
    fun toMap(): Map<String, Any?> {
        val m = mutableMapOf<String, Any?>()
        m["from"] = this.from
        m["to"] = this.to
        if (this.`in` != null) m["in"] = this.`in`
        if (this.class_ != null) m["class"] = this.class_
        return m
    }
}

/** A row of what `move_reviews` answers. */
data class MoveReviewsRow(
    val id: Long,
    val productId: Long, // int @hash required
) {
    companion object {
        /** The row as the binding reads it. */
        fun from(row: Row) = MoveReviewsRow(
            id = (row["id"] as Number).toLong(),
            productId = row["product_id"]!!.let { v -> (v as Number).toLong() },
        )
    }
}

/** The arguments of `my_reviews`. The claims it reads come from the token, never the arguments: `sub` text, `cognito:groups` [text]. */
data class MyReviewsArgs(
    val product: Long, // int
) {
    /** The arguments as a call takes them: one left `null` is left out, for its default. */
    fun toMap(): Map<String, Any?> {
        val m = mutableMapOf<String, Any?>()
        m["product"] = this.product
        return m
    }
}

/** A row of what `my_reviews` answers. */
data class MyReviewsRow(
    val id: Long,
    val productId: Long, // int @hash required
    val stars: Long?, // int
    val notes: String?, // text collate und
) {
    companion object {
        /** The row as the binding reads it. */
        fun from(row: Row) = MyReviewsRow(
            id = (row["id"] as Number).toLong(),
            productId = row["product_id"]!!.let { v -> (v as Number).toLong() },
            stars = row["stars"]?.let { v -> (v as Number).toLong() },
            notes = row["notes"]?.let { v -> (v as String) },
        )
    }
}

/** The arguments of `rate`. */
data class RateArgs(
    val review: Long, // int
    val stars: Long, // int
) {
    /** The arguments as a call takes them: one left `null` is left out, for its default. */
    fun toMap(): Map<String, Any?> {
        val m = mutableMapOf<String, Any?>()
        m["review"] = this.review
        m["stars"] = this.stars
        return m
    }
}

/** A row of what `rate` answers. */
data class RateRow(
    val id: Long,
    val stars: Long?, // int
) {
    companion object {
        /** The row as the binding reads it. */
        fun from(row: Row) = RateRow(
            id = (row["id"] as Number).toLong(),
            stars = row["stars"]?.let { v -> (v as Number).toLong() },
        )
    }
}

/** The arguments of `reviews_by_stars`. */
class ReviewsByStarsArgs {
    /** The arguments as a call takes them: one left `null` is left out, for its default. */
    fun toMap(): Map<String, Any?> = emptyMap()
}

/** A row of what `reviews_by_stars` answers. */
data class ReviewsByStarsRow(
    val stars: Long?, // int
    val count: Long, // a count
) {
    companion object {
        /** The row as the binding reads it. */
        fun from(row: Row) = ReviewsByStarsRow(
            stars = row["stars"]?.let { v -> (v as Number).toLong() },
            count = row["count"]!!.let { v -> (v as Number).toLong() },
        )
    }
}
