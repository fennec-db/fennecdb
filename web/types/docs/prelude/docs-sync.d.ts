// What the examples on sync.html take from the page around them.

/** The replica the page opened above the example. */
declare const db: import('../../../fennec.js').FennecSync<import('../fennec-schema.js').FennecSchema>;
/** The page's own drawing, handed the rows. */
declare function draw(rows: { title: string | null }[]): void;
/** The page's sign-in, which hands out a fresh token, and its sign-out. */
declare const auth: { refresh(): Promise<string>; signOut(): Promise<void> };
/** The page's own way of telling its user something. */
declare function toast(text: string): void;
