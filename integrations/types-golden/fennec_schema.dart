// generated from the fennecdb schema: schema.fennecql
// fennec types --lang dart schema.fennecql > fennec_schema.dart
// Do not edit by hand -- regenerate when the schema changes.

/// A row of `articles`.
class Articles {
  final int id;
  /// text collate tr @text(k1=0.9, b=0.4, prefix=6, chars) required
  final String title;
  /// int @hash
  final int? year;
  /// float @sorted
  final double? score;
  /// bool required
  final bool draft;
  /// timestamp @ttl(30d)
  final String? published;
  /// bytes
  final List<int>? cover;
  /// json
  final Object? meta;
  /// geo @geo
  final List<double>? loc;
  /// vector<384> @hnsw(cosine, m=8, ef_construction=200, ef_search=100)
  final List<double>? embed;
  /// vector<4, f16> @hnsw(l2, m=16, ef_construction=200, ef_search=100, quant=int8)
  final List<double>? small;
  /// sparse<30522> @inverted
  final String? splade;
  /// [text] @hash
  final List<String>? tags;
  /// [int] required
  final List<int> counts;
  /// text nocase @unique
  final String? slug;

  const Articles({required this.id, required this.title, this.year, this.score, required this.draft, this.published, this.cover, this.meta, this.loc, this.embed, this.small, this.splade, this.tags, required this.counts, this.slug});

  /// The row as JSON reads it.
  factory Articles.fromJson(Map<String, Object?> j) => Articles(
        id: j['id'] as int,
        title: switch (j['title']) { final v => v as String },
        year: switch (j['year']) { null => null, final v => v as int },
        score: switch (j['score']) { null => null, final v => (v as num).toDouble() },
        draft: switch (j['draft']) { final v => v as bool },
        published: switch (j['published']) { null => null, final v => v as String },
        cover: switch (j['cover']) { null => null, final v => [for (final e in v as List) e as int] },
        meta: switch (j['meta']) { null => null, final v => v },
        loc: switch (j['loc']) { null => null, final v => [for (final e in v as List) (e as num).toDouble()] },
        embed: switch (j['embed']) { null => null, final v => [for (final e in v as List) (e as num).toDouble()] },
        small: switch (j['small']) { null => null, final v => [for (final e in v as List) (e as num).toDouble()] },
        splade: switch (j['splade']) { null => null, final v => v as String },
        tags: switch (j['tags']) { null => null, final v => [for (final v in v as List) v as String] },
        counts: switch (j['counts']) { final v => [for (final v in v as List) v as int] },
        slug: switch (j['slug']) { null => null, final v => v as String },
      );
}

/// A row of `product_reviews`.
class ProductReviews {
  final int id;
  /// int @hash required
  final int productId;
  /// int
  final int? stars;
  /// text collate und
  final String? notes;

  const ProductReviews({required this.id, required this.productId, this.stars, this.notes});

  /// The row as JSON reads it.
  factory ProductReviews.fromJson(Map<String, Object?> j) => ProductReviews(
        id: j['id'] as int,
        productId: switch (j['product_id']) { final v => v as int },
        stars: switch (j['stars']) { null => null, final v => v as int },
        notes: switch (j['notes']) { null => null, final v => v as String },
      );
}

/// A row of `kişiler`.
class Ki_iler {
  final int id;
  /// text
  final String? ad;
  /// int
  final int? ya_;

  const Ki_iler({required this.id, this.ad, this.ya_});

  /// The row as JSON reads it.
  factory Ki_iler.fromJson(Map<String, Object?> j) => Ki_iler(
        id: j['id'] as int,
        ad: switch (j['ad']) { null => null, final v => v as String },
        ya_: switch (j['yaş']) { null => null, final v => v as int },
      );
}

/// A row of `text`.
class Text {
  final int id;
  /// text
  final String? body;

  const Text({required this.id, this.body});

  /// The row as JSON reads it.
  factory Text.fromJson(Map<String, Object?> j) => Text(
        id: j['id'] as int,
        body: switch (j['body']) { null => null, final v => v as String },
      );
}

/// The arguments of `article_count`: No parameters, and a count.
class ArticleCountArgs {
  const ArticleCountArgs();

  /// The arguments as a call takes them: one left `null` is left out, for its default.
  Map<String, Object?> toJson() => {};
}

/// A row of what `article_count` answers.
class ArticleCountRow {
  /// a count
  final int count;

  const ArticleCountRow({required this.count});

  /// The row as JSON reads it.
  factory ArticleCountRow.fromJson(Map<String, Object?> j) => ArticleCountRow(
        count: switch (j['count']) { final v => v as int },
      );
}

/// The arguments of `every_type`: Every parameter type, and two with a default.
class EveryTypeArgs {
  /// bool
  final bool flag;
  /// int
  final int year;
  /// float
  final double score;
  /// text
  final String title;
  /// bytes
  final List<int> cover;
  /// timestamp
  final String at;
  /// json
  final Object meta;
  /// geo
  final List<double> loc;
  /// vector<384>
  final List<double> embed;
  /// vector<4, f16>
  final List<double> small;
  /// sparse<30522>
  final String splade;
  /// [text]
  final List<String> tags;
  /// [int]
  final List<int> counts;
  /// int = 10
  final int? limit;
  /// text = null
  final String? note;

  const EveryTypeArgs({required this.flag, required this.year, required this.score, required this.title, required this.cover, required this.at, required this.meta, required this.loc, required this.embed, required this.small, required this.splade, required this.tags, required this.counts, this.limit, this.note});

  /// The arguments as a call takes them: one left `null` is left out, for its default.
  Map<String, Object?> toJson() => {
        'flag': flag,
        'year': year,
        'score': score,
        'title': title,
        'cover': cover,
        'at': at,
        'meta': meta,
        'loc': loc,
        'embed': embed,
        'small': small,
        'splade': splade,
        'tags': tags,
        'counts': counts,
        if (limit != null) 'limit': limit,
        if (note != null) 'note': note,
      };
}

/// A row of what `every_type` answers.
class EveryTypeRow {
  final int id;
  /// text collate tr @text(k1=0.9, b=0.4, prefix=6, chars) required
  final String title;
  /// int @hash
  final int? year;
  /// bool required
  final bool draft;
  /// timestamp @ttl(30d)
  final String? published;
  /// bytes
  final List<int>? cover;
  /// json
  final Object? meta;
  /// geo @geo
  final List<double>? loc;
  /// vector<384> @hnsw(cosine, m=8, ef_construction=200, ef_search=100)
  final List<double>? embed;
  /// vector<4, f16> @hnsw(l2, m=16, ef_construction=200, ef_search=100, quant=int8)
  final List<double>? small;
  /// sparse<30522> @inverted
  final String? splade;
  /// [text] @hash
  final List<String>? tags;
  /// [int] required
  final List<int> counts;
  /// text nocase @unique
  final String? slug;

  const EveryTypeRow({required this.id, required this.title, this.year, required this.draft, this.published, this.cover, this.meta, this.loc, this.embed, this.small, this.splade, this.tags, required this.counts, this.slug});

  /// The row as JSON reads it.
  factory EveryTypeRow.fromJson(Map<String, Object?> j) => EveryTypeRow(
        id: j['id'] as int,
        title: switch (j['title']) { final v => v as String },
        year: switch (j['year']) { null => null, final v => v as int },
        draft: switch (j['draft']) { final v => v as bool },
        published: switch (j['published']) { null => null, final v => v as String },
        cover: switch (j['cover']) { null => null, final v => [for (final e in v as List) e as int] },
        meta: switch (j['meta']) { null => null, final v => v },
        loc: switch (j['loc']) { null => null, final v => [for (final e in v as List) (e as num).toDouble()] },
        embed: switch (j['embed']) { null => null, final v => [for (final e in v as List) (e as num).toDouble()] },
        small: switch (j['small']) { null => null, final v => [for (final e in v as List) (e as num).toDouble()] },
        splade: switch (j['splade']) { null => null, final v => v as String },
        tags: switch (j['tags']) { null => null, final v => [for (final v in v as List) v as String] },
        counts: switch (j['counts']) { final v => [for (final v in v as List) v as int] },
        slug: switch (j['slug']) { null => null, final v => v as String },
      );
}

/// The arguments of `forget_reviews`.
class ForgetReviewsArgs {
  /// int
  final int product;

  const ForgetReviewsArgs({required this.product});

  /// The arguments as a call takes them: one left `null` is left out, for its default.
  Map<String, Object?> toJson() => {
        'product': product,
      };
}

// `forget_reviews` answers the number of rows it wrote: no row type.

/// The arguments of `headlines`.
class HeadlinesArgs {
  /// int
  final int year;

  const HeadlinesArgs({required this.year});

  /// The arguments as a call takes them: one left `null` is left out, for its default.
  Map<String, Object?> toJson() => {
        'year': year,
      };
}

/// A row of what `headlines` answers.
class HeadlinesRow {
  /// not typed: `title` given another name
  final Object? headline;
  /// not typed: an expression
  final Object? doubled;
  /// not typed: a path into `meta`
  final Object? metaLang;
  /// not typed: the rows of `product_reviews` that `lookup` attached
  final Object? productReviews;

  const HeadlinesRow({this.headline, this.doubled, this.metaLang, this.productReviews});

  /// The row as JSON reads it.
  factory HeadlinesRow.fromJson(Map<String, Object?> j) => HeadlinesRow(
        headline: switch (j['headline']) { null => null, final v => v },
        doubled: switch (j['doubled']) { null => null, final v => v },
        metaLang: switch (j['meta.lang']) { null => null, final v => v },
        productReviews: switch (j['product_reviews']) { null => null, final v => v },
      );
}

/// The arguments of `move_reviews`: Parameters named as Python, Swift, Kotlin and Dart keywords: */ and /* in a comment.
class MoveReviewsArgs {
  /// int
  final int from;
  /// int
  final int to;
  /// bool = true
  final bool? in_;
  /// text = null
  final String? class_;

  const MoveReviewsArgs({required this.from, required this.to, this.in_, this.class_});

  /// The arguments as a call takes them: one left `null` is left out, for its default.
  Map<String, Object?> toJson() => {
        'from': from,
        'to': to,
        if (in_ != null) 'in': in_,
        if (class_ != null) 'class': class_,
      };
}

/// A row of what `move_reviews` answers.
class MoveReviewsRow {
  final int id;
  /// int @hash required
  final int productId;

  const MoveReviewsRow({required this.id, required this.productId});

  /// The row as JSON reads it.
  factory MoveReviewsRow.fromJson(Map<String, Object?> j) => MoveReviewsRow(
        id: j['id'] as int,
        productId: switch (j['product_id']) { final v => v as int },
      );
}

/// The arguments of `my_reviews`. The claims it reads come from the token, never the arguments: `sub` text, `cognito:groups` [text].
class MyReviewsArgs {
  /// int
  final int product;

  const MyReviewsArgs({required this.product});

  /// The arguments as a call takes them: one left `null` is left out, for its default.
  Map<String, Object?> toJson() => {
        'product': product,
      };
}

/// A row of what `my_reviews` answers.
class MyReviewsRow {
  final int id;
  /// int @hash required
  final int productId;
  /// int
  final int? stars;
  /// text collate und
  final String? notes;

  const MyReviewsRow({required this.id, required this.productId, this.stars, this.notes});

  /// The row as JSON reads it.
  factory MyReviewsRow.fromJson(Map<String, Object?> j) => MyReviewsRow(
        id: j['id'] as int,
        productId: switch (j['product_id']) { final v => v as int },
        stars: switch (j['stars']) { null => null, final v => v as int },
        notes: switch (j['notes']) { null => null, final v => v as String },
      );
}

/// The arguments of `rate`.
class RateArgs {
  /// int
  final int review;
  /// int
  final int stars;

  const RateArgs({required this.review, required this.stars});

  /// The arguments as a call takes them: one left `null` is left out, for its default.
  Map<String, Object?> toJson() => {
        'review': review,
        'stars': stars,
      };
}

/// A row of what `rate` answers.
class RateRow {
  final int id;
  /// int
  final int? stars;

  const RateRow({required this.id, this.stars});

  /// The row as JSON reads it.
  factory RateRow.fromJson(Map<String, Object?> j) => RateRow(
        id: j['id'] as int,
        stars: switch (j['stars']) { null => null, final v => v as int },
      );
}

/// The arguments of `reviews_by_stars`.
class ReviewsByStarsArgs {
  const ReviewsByStarsArgs();

  /// The arguments as a call takes them: one left `null` is left out, for its default.
  Map<String, Object?> toJson() => {};
}

/// A row of what `reviews_by_stars` answers.
class ReviewsByStarsRow {
  /// int
  final int? stars;
  /// a count
  final int count;

  const ReviewsByStarsRow({this.stars, required this.count});

  /// The row as JSON reads it.
  factory ReviewsByStarsRow.fromJson(Map<String, Object?> j) => ReviewsByStarsRow(
        stars: switch (j['stars']) { null => null, final v => v as int },
        count: switch (j['count']) { final v => v as int },
      );
}
