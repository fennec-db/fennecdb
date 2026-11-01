// generated from the fennecdb schema: schema.fennecql
// fennec types --lang go schema.fennecql > fennec_schema.go
// Do not edit by hand -- regenerate when the schema changes.

package fennecschema

import "encoding/json"

// Articles is a row of `articles`.
type Articles struct {
	ID        int64           `json:"id"`
	Title     string          `json:"title"`     // text collate tr @text(k1=0.9, b=0.4, prefix=6, chars) required
	Year      *int64          `json:"year"`      // int @hash
	Score     *float64        `json:"score"`     // float @sorted
	Draft     bool            `json:"draft"`     // bool required
	Published *string         `json:"published"` // timestamp @ttl(30d)
	Cover     []int           `json:"cover"`     // bytes
	Meta      json.RawMessage `json:"meta"`      // json
	Loc       *[2]float64     `json:"loc"`       // geo @geo
	Embed     []float32       `json:"embed"`     // vector<384> @hnsw(cosine, m=8, ef_construction=200, ef_search=100)
	Small     []float32       `json:"small"`     // vector<4, f16> @hnsw(l2, m=16, ef_construction=200, ef_search=100, quant=int8)
	Splade    *string         `json:"splade"`    // sparse<30522> @inverted
	Tags      []string        `json:"tags"`      // [text] @hash
	Counts    []int64         `json:"counts"`    // [int] required
	Slug      *string         `json:"slug"`      // text nocase @unique
}

// ProductReviews is a row of `product_reviews`.
type ProductReviews struct {
	ID        int64   `json:"id"`
	ProductId int64   `json:"product_id"` // int @hash required
	Stars     *int64  `json:"stars"`      // int
	Notes     *string `json:"notes"`      // text collate und
}

// Kişiler is a row of `kişiler`.
type Kişiler struct {
	ID  int64   `json:"id"`
	Ad  *string `json:"ad"`  // text
	Yaş *int64  `json:"yaş"` // int
}

// Text is a row of `text`.
type Text struct {
	ID   int64   `json:"id"`
	Body *string `json:"body"` // text
}

// ArticleCountArgs are the arguments of `article_count`: No parameters, and a count.
type ArticleCountArgs struct{}

// ArticleCountRow is a row of what `article_count` answers.
type ArticleCountRow struct {
	Count int64 `json:"count"` // a count
}

// EveryTypeArgs are the arguments of `every_type`: Every parameter type, and two with a default.
type EveryTypeArgs struct {
	Flag   bool            `json:"flag"`            // bool
	Year   int64           `json:"year"`            // int
	Score  float64         `json:"score"`           // float
	Title  string          `json:"title"`           // text
	Cover  []int           `json:"cover"`           // bytes
	At     string          `json:"at"`              // timestamp
	Meta   json.RawMessage `json:"meta"`            // json
	Loc    [2]float64      `json:"loc"`             // geo
	Embed  []float32       `json:"embed"`           // vector<384>
	Small  []float32       `json:"small"`           // vector<4, f16>
	Splade string          `json:"splade"`          // sparse<30522>
	Tags   []string        `json:"tags"`            // [text]
	Counts []int64         `json:"counts"`          // [int]
	Limit  *int64          `json:"limit,omitempty"` // int = 10
	Note   *string         `json:"note,omitempty"`  // text = null
}

// EveryTypeRow is a row of what `every_type` answers.
type EveryTypeRow struct {
	ID        int64           `json:"id"`
	Title     string          `json:"title"`     // text collate tr @text(k1=0.9, b=0.4, prefix=6, chars) required
	Year      *int64          `json:"year"`      // int @hash
	Draft     bool            `json:"draft"`     // bool required
	Published *string         `json:"published"` // timestamp @ttl(30d)
	Cover     []int           `json:"cover"`     // bytes
	Meta      json.RawMessage `json:"meta"`      // json
	Loc       *[2]float64     `json:"loc"`       // geo @geo
	Embed     []float32       `json:"embed"`     // vector<384> @hnsw(cosine, m=8, ef_construction=200, ef_search=100)
	Small     []float32       `json:"small"`     // vector<4, f16> @hnsw(l2, m=16, ef_construction=200, ef_search=100, quant=int8)
	Splade    *string         `json:"splade"`    // sparse<30522> @inverted
	Tags      []string        `json:"tags"`      // [text] @hash
	Counts    []int64         `json:"counts"`    // [int] required
	Slug      *string         `json:"slug"`      // text nocase @unique
}

// ForgetReviewsArgs are the arguments of `forget_reviews`.
type ForgetReviewsArgs struct {
	Product int64 `json:"product"` // int
}

// `forget_reviews` answers the number of rows it wrote: no row type.

// HeadlinesArgs are the arguments of `headlines`.
type HeadlinesArgs struct {
	Year int64 `json:"year"` // int
}

// HeadlinesRow is a row of what `headlines` answers.
type HeadlinesRow struct {
	Headline       json.RawMessage `json:"headline"`        // not typed: `title` given another name
	Doubled        json.RawMessage `json:"doubled"`         // not typed: an expression
	MetaLang       json.RawMessage `json:"meta.lang"`       // not typed: a path into `meta`
	ProductReviews json.RawMessage `json:"product_reviews"` // not typed: the rows of `product_reviews` that `lookup` attached
}

// MoveReviewsArgs are the arguments of `move_reviews`: Parameters named as Python, Swift, Kotlin and Dart keywords: */ and /* in a comment.
type MoveReviewsArgs struct {
	From  int64   `json:"from"`            // int
	To    int64   `json:"to"`              // int
	In    *bool   `json:"in,omitempty"`    // bool = true
	Class *string `json:"class,omitempty"` // text = null
}

// MoveReviewsRow is a row of what `move_reviews` answers.
type MoveReviewsRow struct {
	ID        int64 `json:"id"`
	ProductId int64 `json:"product_id"` // int @hash required
}

// MyReviewsArgs are the arguments of `my_reviews`. The claims it reads come from the token, never the arguments: `sub` text, `cognito:groups` [text].
type MyReviewsArgs struct {
	Product int64 `json:"product"` // int
}

// MyReviewsRow is a row of what `my_reviews` answers.
type MyReviewsRow struct {
	ID        int64   `json:"id"`
	ProductId int64   `json:"product_id"` // int @hash required
	Stars     *int64  `json:"stars"`      // int
	Notes     *string `json:"notes"`      // text collate und
}

// RateArgs are the arguments of `rate`.
type RateArgs struct {
	Review int64 `json:"review"` // int
	Stars  int64 `json:"stars"`  // int
}

// RateRow is a row of what `rate` answers.
type RateRow struct {
	ID    int64  `json:"id"`
	Stars *int64 `json:"stars"` // int
}

// ReviewsByStarsArgs are the arguments of `reviews_by_stars`.
type ReviewsByStarsArgs struct{}

// ReviewsByStarsRow is a row of what `reviews_by_stars` answers.
type ReviewsByStarsRow struct {
	Stars *int64 `json:"stars"` // int
	Count int64  `json:"count"` // a count
}
