// What the examples on sync.html take from the page around them.

/** The replica the page opened above the example. */
declare const db: import('../../../fennec.js').FennecSync<import('../fennec-schema.js').FennecSchema>;
/** The page's own drawing, handed the rows. */
declare function draw(rows: { title: string | null }[]): void;
/** The page's sign-in: whose it is, the token it holds, and a fresh one from the issuer. */
declare const auth: { userId: string; token(): Promise<string>; refresh(): Promise<string> };
/** The signed-in account's id, which a replica's `persist` key names. */
declare const userId: string;
/** The page's own way of telling its user something. */
declare function toast(text: string): void;
