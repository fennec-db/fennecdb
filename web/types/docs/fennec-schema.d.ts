// generated from the fennecdb schema: docs.fennec
// fennec types docs.fennec > fennec-schema.d.ts
// Do not edit by hand -- regenerate when the schema changes.

/** `timestamp`: ISO-8601 text when read; a Date/number is accepted when writing. */
export type Timestamp = string & { readonly __fennec: 'timestamp' };
/** `vector<N>`: an array of numbers in JSON. */
export type Vector = number[] & { readonly __fennec: 'vector' };
/** `geo`: a point, its longitude and latitude in degrees. */
export type Point = [lon: number, lat: number];
/** `json`: any value JSON holds; a path reads into it, `'meta.lang'`. */
export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

export type FennecSchema = {
  articles: {
    /** text */
    title: string | null;
    /** text @text(k1=0.9, b=0.4) */
    body: string | null;
    /** int @sorted */
    year: number | null;
    /** text */
    category: string | null;
    /** [text] */
    tags: string[] | null;
    /** timestamp */
    published: Timestamp | null;
    /** vector<384> @hnsw(cosine, m=16, ef_search=100) */
    embed: Vector | null;
    /** json */
    meta: Json | null;
  };

  docs: {
    /** text @text(k1=0.9, b=0.4) */
    title: string | null;
    /** int @sorted */
    year: number | null;
    /** vector<384> @hnsw(cosine, m=16, ef_search=100) */
    embed: Vector | null;
  };

  notes: {
    /** text */
    text: string | null;
  };

  todos: {
    /** text @hash */
    key: string | null;
    /** text */
    title: string | null;
    /** bool @hash */
    done: boolean | null;
    /** timestamp @sorted */
    at: Timestamp | null;
  };

  tasks: {
    /** text @hash */
    key: string | null;
    /** text */
    title: string | null;
    /** text @hash */
    status: string | null;
    /** int @sorted */
    priority: number | null;
    /** bool @hash */
    done: boolean | null;
  };

  people: {
    /** text @sorted */
    name: string | null;
  };

  products: {
    /** text @text(k1=0.9, b=0.4) */
    name: string | null;
    /** int @sorted */
    price: number | null;
    /** text @hash */
    brand: string | null;
    /** text */
    color: string | null;
    /** [text] */
    tags: string[] | null;
  };

  reviews: {
    /** int @hash */
    product_id: number | null;
    /** int */
    stars: number | null;
    /** text */
    text: string | null;
    /** timestamp @sorted */
    created: Timestamp | null;
  };

  shops: {
    /** text */
    name: string | null;
  };

  orders: {
    /** int @hash */
    shop_id: number | null;
    /** text */
    code: string | null;
  };

  lines: {
    /** int @hash */
    order_id: number | null;
    /** text */
    item: string | null;
    /** int */
    quantity: number | null;
  };

  places: {
    /** text */
    name: string | null;
    /** text @hash */
    kind: string | null;
    /** geo @geo */
    loc: Point | null;
  };

  accounts: {
    /** text @hash */
    owner: string | null;
    /** int */
    balance: number | null;
  };
};

/**
 * The named statements: what a call of each takes (`args`, one with a
 * default may be left out) and a row of what it answers (`row`, `unknown`
 * where it answers the rows it wrote, or rows not typed yet).
 */
export type FennecStatements = {
  /** Moves money, or says why not. */
  transfer: {
    args: {
      /** int */
      from: number;
      /** int */
      to: number;
      /** int */
      amount: number;
    };
    row: {
      id: number;
      /** int */
      balance: number | null;
    };
  };
};
