// generated from the fennecdb schema: schema.fennecql
// fennec types --lang csharp schema.fennecql > FennecSchema.cs
// Do not edit by hand -- regenerate when the schema changes.

#nullable enable
using System.Text.Json;
using System.Text.Json.Serialization;

namespace FennecSchema;

/// <summary>A row of <c>articles</c>.</summary>
public sealed record Articles(
    [property: JsonPropertyName("id")] long Id,
    /* text collate tr @text(k1=0.9, b=0.4, prefix=6, chars) required */ [property: JsonPropertyName("title")] string Title,
    /* int @hash */ [property: JsonPropertyName("year")] long? Year,
    /* float @sorted */ [property: JsonPropertyName("score")] double? Score,
    /* bool required */ [property: JsonPropertyName("draft")] bool Draft,
    /* timestamp @ttl(30d) */ [property: JsonPropertyName("published")] string? Published,
    /* bytes */ [property: JsonPropertyName("cover")] int[]? Cover,
    /* json */ [property: JsonPropertyName("meta")] JsonElement? Meta,
    /* geo @geo */ [property: JsonPropertyName("loc")] double[]? Loc,
    /* vector<384> @hnsw(cosine, m=8, ef_construction=200, ef_search=100) */ [property: JsonPropertyName("embed")] float[]? Embed,
    /* vector<4, f16> @hnsw(l2, m=16, ef_construction=200, ef_search=100, quant=int8) */ [property: JsonPropertyName("small")] float[]? Small,
    /* sparse<30522> @inverted */ [property: JsonPropertyName("splade")] string? Splade,
    /* [text] @hash */ [property: JsonPropertyName("tags")] string[]? Tags,
    /* [int] required */ [property: JsonPropertyName("counts")] long[] Counts,
    /* text nocase @unique */ [property: JsonPropertyName("slug")] string? Slug);

/// <summary>A row of <c>product_reviews</c>.</summary>
public sealed record ProductReviews(
    [property: JsonPropertyName("id")] long Id,
    /* int @hash required */ [property: JsonPropertyName("product_id")] long ProductId,
    /* int */ [property: JsonPropertyName("stars")] long? Stars,
    /* text collate und */ [property: JsonPropertyName("notes")] string? Notes);

/// <summary>A row of <c>kişiler</c>.</summary>
public sealed record Kişiler(
    [property: JsonPropertyName("id")] long Id,
    /* text */ [property: JsonPropertyName("ad")] string? Ad,
    /* int */ [property: JsonPropertyName("yaş")] long? Yaş);

/// <summary>A row of <c>text</c>.</summary>
public sealed record Text(
    [property: JsonPropertyName("id")] long Id,
    /* text */ [property: JsonPropertyName("body")] string? Body);

/// <summary>The arguments of <c>article_count</c>: No parameters, and a count.</summary>
public sealed record ArticleCountArgs();

/// <summary>A row of what <c>article_count</c> answers.</summary>
public sealed record ArticleCountRow(
    /* a count */ [property: JsonPropertyName("count")] long Count);

/// <summary>The arguments of <c>every_type</c>: Every parameter type, and two with a default.</summary>
public sealed record EveryTypeArgs(
    /* bool */ [property: JsonPropertyName("flag")] bool Flag,
    /* int */ [property: JsonPropertyName("year")] long Year,
    /* float */ [property: JsonPropertyName("score")] double Score,
    /* text */ [property: JsonPropertyName("title")] string Title,
    /* bytes */ [property: JsonPropertyName("cover")] int[] Cover,
    /* timestamp */ [property: JsonPropertyName("at")] string At,
    /* json */ [property: JsonPropertyName("meta")] JsonElement Meta,
    /* geo */ [property: JsonPropertyName("loc")] double[] Loc,
    /* vector<384> */ [property: JsonPropertyName("embed")] float[] Embed,
    /* vector<4, f16> */ [property: JsonPropertyName("small")] float[] Small,
    /* sparse<30522> */ [property: JsonPropertyName("splade")] string Splade,
    /* [text] */ [property: JsonPropertyName("tags")] string[] Tags,
    /* [int] */ [property: JsonPropertyName("counts")] long[] Counts,
    /* int = 10 */ [property: JsonPropertyName("limit"), JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] long? Limit = null,
    /* text = null */ [property: JsonPropertyName("note"), JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Note = null);

/// <summary>A row of what <c>every_type</c> answers.</summary>
public sealed record EveryTypeRow(
    [property: JsonPropertyName("id")] long Id,
    /* text collate tr @text(k1=0.9, b=0.4, prefix=6, chars) required */ [property: JsonPropertyName("title")] string Title,
    /* int @hash */ [property: JsonPropertyName("year")] long? Year,
    /* bool required */ [property: JsonPropertyName("draft")] bool Draft,
    /* timestamp @ttl(30d) */ [property: JsonPropertyName("published")] string? Published,
    /* bytes */ [property: JsonPropertyName("cover")] int[]? Cover,
    /* json */ [property: JsonPropertyName("meta")] JsonElement? Meta,
    /* geo @geo */ [property: JsonPropertyName("loc")] double[]? Loc,
    /* vector<384> @hnsw(cosine, m=8, ef_construction=200, ef_search=100) */ [property: JsonPropertyName("embed")] float[]? Embed,
    /* vector<4, f16> @hnsw(l2, m=16, ef_construction=200, ef_search=100, quant=int8) */ [property: JsonPropertyName("small")] float[]? Small,
    /* sparse<30522> @inverted */ [property: JsonPropertyName("splade")] string? Splade,
    /* [text] @hash */ [property: JsonPropertyName("tags")] string[]? Tags,
    /* [int] required */ [property: JsonPropertyName("counts")] long[] Counts,
    /* text nocase @unique */ [property: JsonPropertyName("slug")] string? Slug);

/// <summary>The arguments of <c>forget_reviews</c>.</summary>
public sealed record ForgetReviewsArgs(
    /* int */ [property: JsonPropertyName("product")] long Product);

// `forget_reviews` answers the number of rows it wrote: no row type.

/// <summary>The arguments of <c>headlines</c>.</summary>
public sealed record HeadlinesArgs(
    /* int */ [property: JsonPropertyName("year")] long Year);

/// <summary>A row of what <c>headlines</c> answers.</summary>
public sealed record HeadlinesRow(
    /* not typed: `title` given another name */ [property: JsonPropertyName("headline")] JsonElement? Headline,
    /* not typed: an expression */ [property: JsonPropertyName("doubled")] JsonElement? Doubled,
    /* not typed: a path into `meta` */ [property: JsonPropertyName("meta.lang")] JsonElement? MetaLang,
    /* not typed: the rows of `product_reviews` that `lookup` attached */ [property: JsonPropertyName("product_reviews")] JsonElement? ProductReviews);

/// <summary>The arguments of <c>move_reviews</c>: Parameters named as Python, Swift, Kotlin and Dart keywords: */ and /* in a comment.</summary>
public sealed record MoveReviewsArgs(
    /* int */ [property: JsonPropertyName("from")] long From,
    /* int */ [property: JsonPropertyName("to")] long To,
    /* bool = true */ [property: JsonPropertyName("in"), JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] bool? In = null,
    /* text = null */ [property: JsonPropertyName("class"), JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Class = null);

/// <summary>A row of what <c>move_reviews</c> answers.</summary>
public sealed record MoveReviewsRow(
    [property: JsonPropertyName("id")] long Id,
    /* int @hash required */ [property: JsonPropertyName("product_id")] long ProductId);

/// <summary>The arguments of <c>my_reviews</c>. The claims it reads come from the token, never the arguments: <c>sub</c> text, <c>cognito:groups</c> [text].</summary>
public sealed record MyReviewsArgs(
    /* int */ [property: JsonPropertyName("product")] long Product);

/// <summary>A row of what <c>my_reviews</c> answers.</summary>
public sealed record MyReviewsRow(
    [property: JsonPropertyName("id")] long Id,
    /* int @hash required */ [property: JsonPropertyName("product_id")] long ProductId,
    /* int */ [property: JsonPropertyName("stars")] long? Stars,
    /* text collate und */ [property: JsonPropertyName("notes")] string? Notes);

/// <summary>The arguments of <c>rate</c>.</summary>
public sealed record RateArgs(
    /* int */ [property: JsonPropertyName("review")] long Review,
    /* int */ [property: JsonPropertyName("stars")] long Stars);

/// <summary>A row of what <c>rate</c> answers.</summary>
public sealed record RateRow(
    [property: JsonPropertyName("id")] long Id,
    /* int */ [property: JsonPropertyName("stars")] long? Stars);

/// <summary>The arguments of <c>reviews_by_stars</c>.</summary>
public sealed record ReviewsByStarsArgs();

/// <summary>A row of what <c>reviews_by_stars</c> answers.</summary>
public sealed record ReviewsByStarsRow(
    /* int */ [property: JsonPropertyName("stars")] long? Stars,
    /* a count */ [property: JsonPropertyName("count")] long Count);
