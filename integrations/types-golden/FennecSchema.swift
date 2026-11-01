// generated from the fennecdb schema: schema.fennecql
// fennec types --lang swift schema.fennecql > FennecSchema.swift
// Do not edit by hand -- regenerate when the schema changes.

import Foundation

/// `json`: any value JSON holds.
public indirect enum JSON: Codable, Sendable, Equatable {
    case null, bool(Bool), number(Double), string(String), array([JSON]), object([String: JSON])

    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let b = try? c.decode(Bool.self) { self = .bool(b) }
        else if let n = try? c.decode(Double.self) { self = .number(n) }
        else if let s = try? c.decode(String.self) { self = .string(s) }
        else if let a = try? c.decode([JSON].self) { self = .array(a) }
        else { self = .object(try c.decode([String: JSON].self)) }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case .bool(let b): try c.encode(b)
        case .number(let n): try c.encode(n)
        case .string(let s): try c.encode(s)
        case .array(let a): try c.encode(a)
        case .object(let o): try c.encode(o)
        }
    }
}

/// A row of `articles`.
public struct Articles: Codable, Sendable, Equatable {
    public var id: Int64
    public var title: String  // text collate tr @text(k1=0.9, b=0.4, prefix=6, chars) required
    public var year: Int64?  // int @hash
    public var score: Double?  // float @sorted
    public var draft: Bool  // bool required
    public var published: String?  // timestamp @ttl(30d)
    public var cover: [UInt8]?  // bytes
    public var meta: JSON?  // json
    public var loc: [Double]?  // geo @geo
    public var embed: [Float]?  // vector<384> @hnsw(cosine, m=8, ef_construction=200, ef_search=100)
    public var small: [Float]?  // vector<4, f16> @hnsw(l2, m=16, ef_construction=200, ef_search=100, quant=int8)
    public var splade: String?  // sparse<30522> @inverted
    public var tags: [String]?  // [text] @hash
    public var counts: [Int64]  // [int] required
    public var slug: String?  // text nocase @unique
}

/// A row of `product_reviews`.
public struct ProductReviews: Codable, Sendable, Equatable {
    public var id: Int64
    public var productId: Int64  // int @hash required
    public var stars: Int64?  // int
    public var notes: String?  // text collate und

    enum CodingKeys: String, CodingKey {
        case id
        case productId = "product_id"
        case stars
        case notes
    }
}

/// A row of `kişiler`.
public struct Kişiler: Codable, Sendable, Equatable {
    public var id: Int64
    public var ad: String?  // text
    public var yaş: Int64?  // int
}

/// A row of `text`.
public struct Text: Codable, Sendable, Equatable {
    public var id: Int64
    public var body: String?  // text
}

/// The arguments of `article_count`: No parameters, and a count.
public struct ArticleCountArgs: Codable, Sendable, Equatable {
    public init() {}
}

/// A row of what `article_count` answers.
public struct ArticleCountRow: Codable, Sendable, Equatable {
    public var count: Int64  // a count
}

/// The arguments of `every_type`: Every parameter type, and two with a default.
public struct EveryTypeArgs: Codable, Sendable, Equatable {
    public var flag: Bool  // bool
    public var year: Int64  // int
    public var score: Double  // float
    public var title: String  // text
    public var cover: [UInt8]  // bytes
    public var at: String  // timestamp
    public var meta: JSON  // json
    public var loc: [Double]  // geo
    public var embed: [Float]  // vector<384>
    public var small: [Float]  // vector<4, f16>
    public var splade: String  // sparse<30522>
    public var tags: [String]  // [text]
    public var counts: [Int64]  // [int]
    public var limit: Int64?  // int = 10
    public var note: String?  // text = null

    public init(flag: Bool, year: Int64, score: Double, title: String, cover: [UInt8], at: String, meta: JSON, loc: [Double], embed: [Float], small: [Float], splade: String, tags: [String], counts: [Int64], limit: Int64? = nil, note: String? = nil) {
        self.flag = flag
        self.year = year
        self.score = score
        self.title = title
        self.cover = cover
        self.at = at
        self.meta = meta
        self.loc = loc
        self.embed = embed
        self.small = small
        self.splade = splade
        self.tags = tags
        self.counts = counts
        self.limit = limit
        self.note = note
    }
}

/// A row of what `every_type` answers.
public struct EveryTypeRow: Codable, Sendable, Equatable {
    public var id: Int64
    public var title: String  // text collate tr @text(k1=0.9, b=0.4, prefix=6, chars) required
    public var year: Int64?  // int @hash
    public var draft: Bool  // bool required
    public var published: String?  // timestamp @ttl(30d)
    public var cover: [UInt8]?  // bytes
    public var meta: JSON?  // json
    public var loc: [Double]?  // geo @geo
    public var embed: [Float]?  // vector<384> @hnsw(cosine, m=8, ef_construction=200, ef_search=100)
    public var small: [Float]?  // vector<4, f16> @hnsw(l2, m=16, ef_construction=200, ef_search=100, quant=int8)
    public var splade: String?  // sparse<30522> @inverted
    public var tags: [String]?  // [text] @hash
    public var counts: [Int64]  // [int] required
    public var slug: String?  // text nocase @unique
}

/// The arguments of `forget_reviews`.
public struct ForgetReviewsArgs: Codable, Sendable, Equatable {
    public var product: Int64  // int

    public init(product: Int64) {
        self.product = product
    }
}

// `forget_reviews` answers the number of rows it wrote: no row type.

/// The arguments of `headlines`.
public struct HeadlinesArgs: Codable, Sendable, Equatable {
    public var year: Int64  // int

    public init(year: Int64) {
        self.year = year
    }
}

/// A row of what `headlines` answers.
public struct HeadlinesRow: Codable, Sendable, Equatable {
    public var headline: JSON?  // not typed: `title` given another name
    public var doubled: JSON?  // not typed: an expression
    public var metaLang: JSON?  // not typed: a path into `meta`
    public var productReviews: JSON?  // not typed: the rows of `product_reviews` that `lookup` attached

    enum CodingKeys: String, CodingKey {
        case headline
        case doubled
        case metaLang = "meta.lang"
        case productReviews = "product_reviews"
    }
}

/// The arguments of `move_reviews`: Parameters named as Python, Swift, Kotlin and Dart keywords: */ and /* in a comment.
public struct MoveReviewsArgs: Codable, Sendable, Equatable {
    public var from: Int64  // int
    public var to: Int64  // int
    public var `in`: Bool?  // bool = true
    public var `class`: String?  // text = null

    public init(from: Int64, to: Int64, `in`: Bool? = nil, `class`: String? = nil) {
        self.from = from
        self.to = to
        self.`in` = `in`
        self.`class` = `class`
    }
}

/// A row of what `move_reviews` answers.
public struct MoveReviewsRow: Codable, Sendable, Equatable {
    public var id: Int64
    public var productId: Int64  // int @hash required

    enum CodingKeys: String, CodingKey {
        case id
        case productId = "product_id"
    }
}

/// The arguments of `my_reviews`. The claims it reads come from the token, never the arguments: `sub` text, `cognito:groups` [text].
public struct MyReviewsArgs: Codable, Sendable, Equatable {
    public var product: Int64  // int

    public init(product: Int64) {
        self.product = product
    }
}

/// A row of what `my_reviews` answers.
public struct MyReviewsRow: Codable, Sendable, Equatable {
    public var id: Int64
    public var productId: Int64  // int @hash required
    public var stars: Int64?  // int
    public var notes: String?  // text collate und

    enum CodingKeys: String, CodingKey {
        case id
        case productId = "product_id"
        case stars
        case notes
    }
}

/// The arguments of `rate`.
public struct RateArgs: Codable, Sendable, Equatable {
    public var review: Int64  // int
    public var stars: Int64  // int

    public init(review: Int64, stars: Int64) {
        self.review = review
        self.stars = stars
    }
}

/// A row of what `rate` answers.
public struct RateRow: Codable, Sendable, Equatable {
    public var id: Int64
    public var stars: Int64?  // int
}

/// The arguments of `reviews_by_stars`.
public struct ReviewsByStarsArgs: Codable, Sendable, Equatable {
    public init() {}
}

/// A row of what `reviews_by_stars` answers.
public struct ReviewsByStarsRow: Codable, Sendable, Equatable {
    public var stars: Int64?  // int
    public var count: Int64  // a count
}
