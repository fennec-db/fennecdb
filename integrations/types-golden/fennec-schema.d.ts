// generated from the fennecdb schema: schema.fennecql
// fennec types schema.fennecql > fennec-schema.d.ts
// Do not edit by hand -- regenerate when the schema changes.

/** `timestamp`: ISO-8601 text when read; a Date/number is accepted when writing. */
export type Timestamp = string & { readonly __fennec: 'timestamp' };
/** `sparse<N>`: pgvector's text form, `{1:0.5,3:0.25}/N`, indices from 1. */
export type Sparse = string & { readonly __fennec: 'sparse' };
/** `vector<N>`: an array of numbers in JSON. */
export type Vector = number[] & { readonly __fennec: 'vector' };
/** `geo`: a point, its longitude and latitude in degrees. */
export type Point = [lon: number, lat: number];
/** `json`: any value JSON holds; a path reads into it, `'meta.lang'`. */
export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
/** `bytes`: an array of bytes in JSON. */
export type Bytes = number[] & { readonly __fennec: 'bytes' };

export type FennecSchema = {
  articles: {
    /** text @text(k1=0.9, b=0.4, prefix=6, chars) required */
    title: string;
    /** int @hash */
    year: number | null;
    /** float @sorted */
    score: number | null;
    /** bool required */
    draft: boolean;
    /** timestamp @ttl(30d) */
    published: Timestamp | null;
    /** bytes */
    cover: Bytes | null;
    /** json */
    meta: Json | null;
    /** geo @geo */
    loc: Point | null;
    /** vector<384> @hnsw(cosine, m=8, ef_search=100) */
    embed: Vector | null;
    /** vector<4, f16> @hnsw(l2, m=16, ef_search=100, quant=int8) */
    small: Vector | null;
    /** sparse<30522> @inverted */
    splade: Sparse | null;
    /** [text] @hash */
    tags: string[] | null;
    /** [int] required */
    counts: number[];
    /** text @unique */
    slug: string | null;
  };

  product_reviews: {
    /** int @hash required */
    product_id: number;
    /** int */
    stars: number | null;
    /** text */
    notes: string | null;
  };

  'kişiler': {
    /** text */
    ad: string | null;
    /** int */
    'yaş': number | null;
  };

  text: {
    /** text */
    body: string | null;
  };
};

/**
 * The named statements: what a call of each takes (`args`, one with a
 * default may be left out) and a row of what it answers (`row`, `unknown`
 * where it answers the rows it wrote, or rows not typed yet).
 */
export type FennecStatements = {
  /** No parameters, and a count. */
  article_count: {
    args: {};
    row: {
      /** a count */
      count: number;
    };
  };

  /** Every parameter type, and two with a default. */
  every_type: {
    args: {
      /** bool */
      flag: boolean;
      /** int */
      year: number;
      /** float */
      score: number;
      /** text */
      title: string;
      /** bytes */
      cover: Bytes;
      /** timestamp */
      at: Timestamp;
      /** json */
      meta: Json;
      /** geo */
      loc: Point;
      /** vector<384> */
      embed: Vector;
      /** vector<4, f16> */
      small: Vector;
      /** sparse<30522> */
      splade: Sparse;
      /** [text] */
      tags: string[];
      /** [int] */
      counts: number[];
      /** int = 10 */
      limit?: number | null;
      /** text = null */
      note?: string | null;
    };
    row: {
      id: number;
      /** text collate tr @text(k1=0.9, b=0.4, prefix=6, chars) required */
      title: string;
      /** int @hash */
      year: number | null;
      /** bool required */
      draft: boolean;
      /** timestamp @ttl(30d) */
      published: Timestamp | null;
      /** bytes */
      cover: Bytes | null;
      /** json */
      meta: Json | null;
      /** geo @geo */
      loc: Point | null;
      /** vector<384> @hnsw(cosine, m=8, ef_construction=200, ef_search=100) */
      embed: Vector | null;
      /** vector<4, f16> @hnsw(l2, m=16, ef_construction=200, ef_search=100, quant=int8) */
      small: Vector | null;
      /** sparse<30522> @inverted */
      splade: Sparse | null;
      /** [text] @hash */
      tags: string[] | null;
      /** [int] required */
      counts: number[];
      /** text nocase @unique */
      slug: string | null;
    };
  };

  /** `forget_reviews` answers the number of rows it wrote: no row type. */
  forget_reviews: {
    args: {
      /** int */
      product: number;
    };
    row: unknown;
  };

  headlines: {
    args: {
      /** int */
      year: number;
    };
    row: {
      /** not typed: `title` given another name */
      headline: Json | null;
      /** not typed: an expression */
      doubled: Json | null;
      /** not typed: a path into `meta` */
      'meta.lang': Json | null;
      /** not typed: the rows of `product_reviews` that `lookup` attached */
      product_reviews: Json | null;
    };
  };

  /** Parameters named as Python, Swift, Kotlin and Dart keywords: *\/ and /* in a comment. */
  move_reviews: {
    args: {
      /** int */
      from: number;
      /** int */
      to: number;
      /** bool = true */
      in?: boolean | null;
      /** text = null */
      class?: string | null;
    };
    row: {
      id: number;
      /** int @hash required */
      product_id: number;
    };
  };

  /** The claims it reads come from the token, never the arguments: `sub` text, `cognito:groups` [text]. */
  my_reviews: {
    args: {
      /** int */
      product: number;
    };
    row: {
      id: number;
      /** int @hash required */
      product_id: number;
      /** int */
      stars: number | null;
      /** text collate und */
      notes: string | null;
    };
  };

  rate: {
    args: {
      /** int */
      review: number;
      /** int */
      stars: number;
    };
    row: {
      id: number;
      /** int */
      stars: number | null;
    };
  };

  reviews_by_stars: {
    args: {};
    row: {
      /** int */
      stars: number | null;
      /** a count */
      count: number;
    };
  };
};
