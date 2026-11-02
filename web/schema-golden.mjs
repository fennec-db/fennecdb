// The schema's golden suite: integrations/schema-golden.json, held by every
// SDK that declares a schema in code, as builder-golden.json holds the
// builders. Three parts:
//
// - `declarations`: a declaration, as TypeScript writes it with
//   `@fennecdb/web/schema`, and the description it compiles to -- or the
//   refusal it meets. Another SDK writes each case in its own idiom and must
//   compile it to the same description, to the key: the description is what
//   the engine compares, and the one format every SDK speaks.
// - `texts`: a schema written as FennecQL, as any SDK may hand it over with
//   no builder of its own, and the statements an empty database is made
//   with -- every option written -- or the refusal it meets.
// - `plans`: a database, made by FennecQL statements, a description with its
//   migrations, and what the engine plans for them (`fennec_abi::schema`,
//   writing nothing): the statements, the migrations it would record, the
//   refusals with their messages. The Rust tests run every case
//   (crates/fennec-abi), and this file made them through the browser module:
//   the plan is the engine's, and one engine answers both.
//
//   node web/schema-golden.mjs           writes integrations/schema-golden.json
//   node web/schema-golden.mjs --check   fails if the file is not what it would write

import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { Fennec, FennecError } from './fennec.js';
import * as schema from './schema.js';

export const GOLDEN = fileURLToPath(new URL('../integrations/schema-golden.json', import.meta.url));

/** A declaration's source, run with the builders in scope. */
function declare(source) {
  const names = Object.keys(schema);
  return new Function(...names, `return (${source});`)(...names.map((n) => schema[n]));
}

// ------------------------------------------------------------ declarations

const declarations = [];
const d = (name, source) => declarations.push({ name, source });

d('every type', `fennecTable('t', {
  a: text(), b: integer(), c: doublePrecision(), d: boolean(), e: timestamp(), f: bytea(), g: json(),
  h: vector({ dimensions: 768 }), i: halfvec({ dimensions: 384 }), j: sparsevec({ dimensions: 30522 }),
  k: text().array(), l: integer().array(), m: timestamp().array(),
})`);
d('a required field', `fennecTable('t', { title: text().notNull(), n: integer().notNull() })`);
d('a collation', `fennecTable('t', { a: text().collate('tr'), b: text().collate('und').notNull(), c: text().collate('und').array() })`);
d('a unique column', `fennecTable('t', { email: text().unique().notNull() })`);
d('a column without case', `fennecTable('t', { email: text().nocase().unique().notNull(), name: text().collate('tr').nocase() })`);
d('an ordered index', `fennecTable('t', { at: timestamp() }, (t) => [index('t_at').on(t.at)])`);
d('a btree is ordered', `fennecTable('t', { n: integer() }, (t) => [index('t_n').using('btree', t.n)])`);
d('a hash index', `fennecTable('t', { n: integer() }, (t) => [index('t_n').using('hash', t.n)])`);
d('a unique index', `fennecTable('t', { s: text() }, (t) => [uniqueIndex('t_s').on(t.s)])`);
d('an expiry', `fennecTable('t', { seen: timestamp() }, (t) => [index('t_seen').on(t.seen).ttl('30m')])`);
d('an expiry in days', `fennecTable('t', { seen: timestamp() }, (t) => [index().on(t.seen).ttl('7d')])`);
d('an expiry in hours', `fennecTable('t', { seen: timestamp() }, (t) => [index().on(t.seen).ttl('12h')])`);
d('an expiry in seconds', `fennecTable('t', { seen: timestamp() }, (t) => [index().on(t.seen).ttl('45s')])`);
d('an expiry in milliseconds', `fennecTable('t', { seen: timestamp() }, (t) => [index().on(t.seen).ttl('1500ms')])`);
d('an expiry written in a smaller unit', `fennecTable('t', { seen: timestamp() }, (t) => [index().on(t.seen).ttl('120m')])`);
d('hnsw by cosine', `fennecTable('t', { e: vector({ dimensions: 3 }) }, (t) => [index('t_e').using('hnsw', t.e.op('vector_cosine_ops'))])`);
d('hnsw by l2, its options', `fennecTable('t', { e: vector({ dimensions: 3 }) }, (t) => [
  index('t_e').using('hnsw', t.e.op('vector_l2_ops')).with({ m: 8, ef_construction: 64, ef_search: 40 }),
])`);
d('hnsw by inner product over halfvec, int8', `fennecTable('t', { e: halfvec({ dimensions: 3 }) }, (t) => [
  index('t_e').using('hnsw', t.e.op('halfvec_ip_ops')).with({ quant: 'int8' }),
])`);
d('hnsw over bit codes', `fennecTable('t', { e: vector({ dimensions: 8 }) }, (t) => [index().using('hnsw', t.e).with({ quant: 'bit' })])`);
d('bm25', `fennecTable('t', { body: text() }, (t) => [index('t_body').using('bm25', t.body)])`);
d('bm25, its options', `fennecTable('t', { body: text() }, (t) => [
  index('t_body').using('bm25', t.body).with({ k1: 1.2, b: 0.75, prefix: 6, prefix_min: 4, chars: true }),
])`);
d('an inverted index', `fennecTable('t', { s: sparsevec({ dimensions: 100 }) }, (t) => [index('t_s').using('inverted', t.s)])`);
d('a point and its index', `fennecTable('places', { loc: geometry({ type: 'point' }), route: geo().array() }, (t) => [index('places_loc').using('gist', t.loc)])`);
d('a point of another srid', `fennecTable('t', { loc: geometry({ type: 'point', srid: 3857 }) })`);
d('a polygon', `fennecTable('t', { area: geometry({ type: 'polygon' }) })`);
d('a gist over a number', `fennecTable('t', { n: integer() }, (t) => [index().using('gist', t.n)])`);
d('indexes on paths', `fennecTable('docs', { meta: json() }, (t) => [
  index('docs_lang').using('hash', t.meta.path('lang')),
  uniqueIndex('docs_slug').on(t.meta.path('slug')),
  index('docs_rank').on(t.meta.path('source.rank')),
])`);
d('a name past ASCII', `fennecTable('kişiler', { 'ad': text(), 'yaş': integer() })`);
d('two indexes on a field', `fennecTable('t', { a: text().unique() }, (t) => [index().on(t.a)])`);
d('a text index beside a unique one', `fennecTable('t', { email: text().nocase().unique() }, (t) => [index('t_email_words').using('bm25', t.email)])`);
d('a hash index beside a text one', `fennecTable('t', { tag: text() }, (t) => [index('t_tag_words').using('bm25', t.tag).with({ prefix: 3 }), index('t_tag').using('hash', t.tag)])`);
d('two text indexes on a field', `fennecTable('t', { a: text() }, (t) => [index('x').using('bm25', t.a), index('y').using('bm25', t.a)])`);
d('two indexes of one name', `fennecTable('t', { a: text(), b: text() }, (t) => [index('x').on(t.a), index('x').on(t.b)])`);
d('an id declared', `fennecTable('t', { id: integer() })`);
d('a name FennecQL does not read', `fennecTable('t', { 'a-b': integer() })`);
d('no column', `fennecTable('t', {})`);
d('a default', `fennecTable('t', { n: integer().default(0) })`);
d('a primary key', `fennecTable('t', { n: integer().primaryKey() })`);
d('a collation of a number', `fennecTable('t', { n: integer().collate('tr') })`);
d('a list of json', `fennecTable('t', { m: json().array() })`);
d('an index on two columns', `fennecTable('t', { a: text(), b: text() }, (t) => [index().on(t.a, t.b)])`);
d('indexes over several fields', `fennecTable('posts', { owner: integer(), at: timestamp(), slug: text(), meta: json() }, (t) => [
  index('posts_feed').on(t.owner, t.at.desc()),
  uniqueIndex('posts_slug').on(t.owner, t.slug),
  index().using('btree', t.meta.path('lang'), t.at.desc()),
])`);
d('desc on an index of one column', `fennecTable('t', { a: text() }, (t) => [index().on(t.a.desc())])`);
d('a hash over two columns', `fennecTable('t', { a: text(), b: text() }, (t) => [index().using('hash', t.a, t.b)])`);
d('a vector in an index over several fields', `fennecTable('t', { a: text(), e: vector({ dimensions: 3 }) }, (t) => [index().on(t.a, t.e)])`);
d('one column twice in an index', `fennecTable('t', { a: text(), b: text() }, (t) => [index().on(t.a, t.a.desc())])`);
d('an expiry of a hash', `fennecTable('t', { at: timestamp() }, (t) => [index().using('hash', t.at).ttl('1h')])`);
d('an expiry of a number', `fennecTable('t', { n: integer() }, (t) => [index().on(t.n).ttl('1h')])`);
d('hnsw over text', `fennecTable('t', { a: text() }, (t) => [index().using('hnsw', t.a)])`);
d('bm25 over a number', `fennecTable('t', { n: integer() }, (t) => [index().using('bm25', t.n)])`);
d('an inverted index over a vector', `fennecTable('t', { e: vector({ dimensions: 3 }) }, (t) => [index().using('inverted', t.e)])`);
d('a bad duration', `fennecTable('t', { at: timestamp() }, (t) => [index().on(t.at).ttl('soon')])`);
d('a collation there is not', `fennecTable('t', { a: text().collate('fr') })`);
d('a number without case', `fennecTable('t', { n: integer().nocase() })`);
d('a list without case', `fennecTable('t', { a: text().nocase().array() })`);
d('a dimension of none', `fennecTable('t', { e: vector({ dimensions: 0 }) })`);
d('an option hnsw takes not', `fennecTable('t', { e: vector({ dimensions: 3 }) }, (t) => [index().using('hnsw', t.e).with({ prefix: 3 })])`);

// ------------------------------------------------------------------- texts

const texts = [];
const x = (name, fennecql) => texts.push({ name, fennecql });
x('a collection', 'create collection t (a text required, b int @hash)');
x('every index, options defaulted', `create collection t (
  a text @text, b int @sorted, c text @unique, d timestamp @ttl(1h), e vector<3> @hnsw(dot), f sparse<9> @inverted, g json,
  h geo @geo
)
create index on t (g.lang) @hash`);
x('two collections, comments between', `-- the schema
create collection a (x int)
-- and another
create collection b (y text collate und)`);
x('an index after its collection', 'create collection t (a int)\ncreate index on t (a) @sorted');
x('a collection twice', 'create collection t (a int)\ncreate collection t (b int)');
x('a field twice', 'create collection t (a int, a text)');
x('an id declared', 'create collection t (id int)');
x('two indexes on a field', 'create collection t (a int @hash)\ncreate index on t (a) @sorted');
x('two indexes declared on a field', 'create collection t (a text @unique @sorted)');
x('a text index beside a unique one', 'create collection t (email text nocase @text @unique, tag text @hash)\ncreate index on t (tag) @text');
x('an index on no collection', 'create index on t (a) @hash');
x('an index on no field', 'create collection t (a int)\ncreate index on t (b) @hash');
x('an expiry of a number', 'create collection t (a int @ttl(1h))');
x('hnsw over text', 'create collection t (a text @hnsw(cosine))');
x('a bad duration', 'create collection t (a timestamp @ttl(soon))');
x('a collation there is not', 'create collection t (a text collate fr)');
x('fields without case', 'create collection u (email text nocase @unique, name text collate tr nocase @sorted, org text nocase, @unique(org, name))');
x('a number without case', 'create collection t (n int nocase)');
x('a write in a schema', 'create collection t (a int)\nput t {a: 1}');
x('a path into a number', 'create collection t (a int)\ncreate index on t (a.b) @hash');
x('indexes over several fields', 'create collection t (a int, b text, m json, @sorted(a, b desc))\ncreate index on t (b, m.k) @unique');
x('an index over several fields on no field', 'create collection t (a int, @sorted(a, nope))');
x('an index over several fields twice', 'create collection t (a int, b int, @sorted(a, b))\ncreate index on t (a, b desc) @unique');

// ------------------------------------------------------------------- plans

const plans = [];
const p = (name, db, source, migrations = []) => plans.push({ name, db, source, migrations });
const TODOS = 'create collection todos (title text required, done bool @hash)';
const todos = `fennecTable('todos', { title: text().notNull(), done: boolean() }, (t) => [index().using('hash', t.done)])`;

p('an empty database is made', [], `{ todos: ${todos} }`);
p('the same schema plans nothing', [TODOS], `{ todos: ${todos} }`);
p('a collection is added', [TODOS], `{ todos: ${todos}, tags: fennecTable('tags', { name: text().unique() }) }`);
p('a field and its indexes are added', [TODOS], `{ todos: fennecTable('todos', {
  title: text().notNull(), done: boolean(), at: timestamp(), meta: json(),
}, (t) => [index().using('hash', t.done), index().on(t.at), index().using('hash', t.meta.path('lang'))]) }`);
p('an index is added where there is none', [TODOS], `{ todos: fennecTable('todos', { title: text().notNull(), done: boolean() }, (t) => [
  index().using('hash', t.done), index().using('bm25', t.title),
]) }`);
p('a unique index is added beside a text one', ['create collection users (email text @text)'], `{ users: fennecTable('users', { email: text().unique() }, (t) => [index().using('bm25', t.email)]) }`);
p('a unique index beside a text one taken off', ['create collection users (email text @unique @text)'], `{ users: fennecTable('users', { email: text() }, (t) => [index().using('bm25', t.email)]) }`);
p('a field the code lacks', [TODOS], `{ todos: fennecTable('todos', { title: text().notNull() }) }`);
p('a field renamed, it seems', [TODOS], `{ todos: fennecTable('todos', { name: text().notNull(), done: boolean() }, (t) => [index().using('hash', t.done)]) }`);
p('a rename, as a migration', [TODOS, 'put todos {title: "a"}'], `{ todos: fennecTable('todos', { name: text().notNull(), done: boolean() }, (t) => [index().using('hash', t.done)]) }`, [
  "rename('todos', 'title', 'name')",
]);
p('a type changed', [TODOS], `{ todos: fennecTable('todos', { title: integer().notNull(), done: boolean() }, (t) => [index().using('hash', t.done)]) }`);
p('a field made required', ['create collection todos (title text, done bool @hash)'], `{ todos: ${todos} }`);
p('a field no longer required', [TODOS], `{ todos: fennecTable('todos', { title: text(), done: boolean() }, (t) => [index().using('hash', t.done)]) }`);
p('a required field added', [TODOS], `{ todos: fennecTable('todos', { title: text().notNull(), done: boolean(), due: timestamp().notNull() }, (t) => [index().using('hash', t.done)]) }`);
p('a collation changed', [TODOS], `{ todos: fennecTable('todos', { title: text().notNull().collate('tr'), done: boolean() }, (t) => [index().using('hash', t.done)]) }`);
const USERS = 'create collection users (email text @unique)';
const users = `{ users: fennecTable('users', { email: text().nocase().unique() }) }`;
p('a field made nocase', [USERS], users);
p('a field made nocase, rebuilt', [USERS, 'put users [{email: "ada@x.com"}, {email: "Bob@x.com"}]'], users, ["rebuild('users', 'email')"]);
p('a field made nocase over two spellings of a value', [USERS, 'put users [{email: "ada@x.com"}, {email: "ADA@x.com"}]'], users, ["rebuild('users', 'email')"]);
p('an index taken off', [TODOS], `{ todos: fennecTable('todos', { title: text().notNull(), done: boolean() }) }`);
p('an index changed', [TODOS], `{ todos: fennecTable('todos', { title: text().notNull(), done: boolean().unique() }) }`);
p('an index changed, rebuilt', [TODOS, 'put todos [{title: "a", done: true}, {title: "b", done: false}]'], `{ todos: fennecTable('todos', { title: text().notNull(), done: boolean().unique() }) }`, [
  "rebuild('todos', 'done')",
]);
p('an expiry changed in place', ['create collection s (seen timestamp @ttl(30m))'], `{ s: fennecTable('s', { seen: timestamp() }, (t) => [index().on(t.seen).ttl('1h')]) }`);
p('hnsw options changed', ['create collection v (e vector<3> @hnsw(cosine))'], `{ v: fennecTable('v', { e: vector({ dimensions: 3 }) }, (t) => [index().using('hnsw', t.e.op('vector_cosine_ops')).with({ m: 32 })]) }`);
p('a path index taken off', ['create collection docs (meta json)', 'create index on docs (meta.lang) @hash'], `{ docs: fennecTable('docs', { meta: json() }) }`);
const POSTS = 'create collection posts (owner int, at timestamp, slug text)';
const posts = (...ix) => `{ posts: fennecTable('posts', { owner: integer(), at: timestamp(), slug: text() }, (t) => [${ix.join(', ')}]) }`;
p('an index over several fields is added', [POSTS], posts('index().on(t.owner, t.at.desc())', 'uniqueIndex().on(t.owner, t.slug)'));
p('an index over several fields as declared plans nothing', [POSTS, 'create index on posts (owner, at desc) @sorted'], posts('index().on(t.owner, t.at.desc())'));
p('an index over several fields changed', [POSTS, 'create index on posts (owner, at) @sorted'], posts('index().on(t.owner, t.at.desc())'));
p('an index over several fields taken off', [POSTS, 'create index on posts (owner, at) @sorted'], posts());
p('an index over several fields changed, dropped by a migration', [POSTS, 'create index on posts (owner, at) @sorted'], posts('index().on(t.owner, t.at.desc())'), [
  "dropIndex('posts', 'owner', 'at')",
]);
p('a collection the code lacks is left as it is', [TODOS, 'create collection gone (x int)'], `{ todos: ${todos} }`);
p('a dropped collection, as a migration', [TODOS, 'create collection gone (x int)'], `{ todos: ${todos} }`, ["dropTable('gone')"]);
p('the database\'s own collections are its own', [TODOS, 'create collection _idempotency (key text)'], `{ todos: ${todos} }`);
p('a field dropped before is declared again, a new field', [TODOS, 'alter collection todos drop field done'], `{ todos: ${todos} }`);
p('a field is loosened: a rebuild', [TODOS, 'put todos {title: "a"}'], `{ todos: fennecTable('todos', { title: text(), done: boolean() }, (t) => [index().using('hash', t.done)]) }`, ["rebuild('todos', 'title')"]);
p('migrations run in order', [TODOS, 'put todos {title: "a"}'], `{ todos: fennecTable('todos', { label: text().notNull(), done: boolean() }, (t) => [index().using('hash', t.done)]) }`, [
  "rename('todos', 'title', 'name')",
  "rename('todos', 'name', 'label')",
]);
p('a migration is refused', [TODOS], `{ todos: ${todos} }`, ["'compact'"]);
p('a migration fails, and none of them lands', [TODOS], `{ todos: fennecTable('todos', { name: text().notNull(), done: boolean() }, (t) => [index().using('hash', t.done)]) }`, [
  "rename('todos', 'title', 'name')",
  "dropTable('nothing')",
]);
const RECORDED = [
  'create collection _migrations (n int @unique, text text, at timestamp)',
  'put _migrations [{n: 1, text: "alter collection todos rename field title to name"}, {n: 2, text: "drop collection gone"}]',
];
p('an older app opens what a newer one migrated', ['create collection todos (name text required, done bool @hash)', ...RECORDED], `{ todos: fennecTable('todos', { name: text().notNull(), done: boolean() }, (t) => [index().using('hash', t.done)]) }`, [
  "rename('todos', 'title', 'name')",
]);
p('a migration edited after it ran', ['create collection todos (name text required, done bool @hash)', ...RECORDED], `{ todos: fennecTable('todos', { name: text().notNull(), done: boolean() }, (t) => [index().using('hash', t.done)]) }`, [
  "rename('todos', 'title', 'label')",
  "dropTable('gone')",
]);
p('the migrations recorded, nothing runs', ['create collection todos (name text required, done bool @hash)', ...RECORDED], `{ todos: fennecTable('todos', { name: text().notNull(), done: boolean() }, (t) => [index().using('hash', t.done)]) }`, [
  "rename('todos', 'title', 'name')",
  "dropTable('gone')",
]);
p('an empty database records its migrations, and runs none', [], `{ todos: ${todos} }`, ["rename('todos', 'name', 'title')", "dropTable('gone')"]);
p('an empty database, several collections', [], `{ a: fennecTable('a', { x: integer() }), b: fennecTable('b', { y: text().notNull() }, (t) => [index().on(t.y)]) }`);
p('an empty database holding only its own', ['create collection _idempotency (key text)'], `{ todos: ${todos} }`);
p('every difference at once', [TODOS, 'create collection gone (x int)'], `{ todos: fennecTable('todos', { name: integer(), done: boolean().notNull() }) }`);

// ------------------------------------------------------------------ writing

async function generate() {
  const out = { declarations: [], plans: [] };
  for (const { name, source } of declarations) {
    try {
      const table = declare(source);
      out.declarations.push({ name, source, collection: JSON.parse(JSON.stringify(table)), fennecql: table.toFennecQL() });
    } catch (e) {
      if (!(e instanceof FennecError)) throw e;
      out.declarations.push({ name, source, error: e.message });
    }
  }
  const wasm = await readFile(new URL('./fennec.wasm', import.meta.url));
  // The plan an empty or a made database gets, or the refusal: written
  // without `kind` and `applied`, which a plan always says alike.
  const planned = async (statements, description) => {
    const db = await Fennec.open(wasm);
    try {
      for (const sql of statements) db.run(sql);
      const { kind, applied, ...plan } = db.checkSchema(description, 'plan');
      void kind;
      void applied;
      return { plan };
    } catch (e) {
      if (!(e instanceof FennecError)) throw e;
      return { error: e.message };
    } finally {
      db.close();
    }
  };
  out.texts = [];
  for (const { name, fennecql } of texts) {
    const r = await planned([], { format: 1, fennecql });
    out.texts.push({ name, fennecql, ...(r.plan ? { statements: r.plan.statements } : r) });
  }
  for (const { name, db: statements, source, migrations } of plans) {
    const tables = declare(source);
    const description = schema.describe(tables, migrations.map((m) => declare(m)));
    // The module reads a schema as FennecQL; the Rust tests read both.
    const fennecql = schema.toFennecQL(tables);
    const r = await planned(statements, { ...description, collections: undefined, fennecql });
    out.plans.push({ name, db: statements, source, migrations, description, fennecql, ...r });
  }
  return out;
}

/** The file's text: one case a line, so a change reads as the cases it moved. */
export function format(out) {
  const part = (list) => `[\n${list.map((k) => `    ${JSON.stringify(k)}`).join(',\n')}\n  ]`;
  return `{\n  "format": 1,\n  "declarations": ${part(out.declarations)},\n  "texts": ${part(out.texts)},\n  "plans": ${part(out.plans)}\n}\n`;
}

export async function text() {
  for (const list of [declarations, texts, plans]) {
    if (new Set(list.map((k) => k.name)).size !== list.length) throw new Error('two cases share a name');
  }
  return format(await generate());
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const made = await text();
  if (process.argv.includes('--check')) {
    const have = await readFile(GOLDEN, 'utf8').catch(() => '');
    if (have !== made) {
      console.error(`${GOLDEN} is not what the declarations and the engine make: make schema-golden`);
      process.exit(1);
    }
  } else {
    await writeFile(GOLDEN, made);
    console.log(`${declarations.length} declarations, ${texts.length} texts, ${plans.length} plans -> ${GOLDEN}`);
  }
}
