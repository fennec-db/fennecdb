// The docs' TypeScript examples type-checked: `make docs-types`, which
// `make types-check` runs too. Every `<pre data-lang="ts">` under
// site/content is written out as a file of its own and run through tsc
// under --strict against web/fennec.d.ts, integrations/react/index.d.ts and
// integrations/cloudflare/index.d.ts, so a change to the API that breaks
// an example fails here rather than in a reader's editor.
//
// An example that leans on something it does not show -- the `db` a page
// opened above it, a vector, a DOM element -- finds it declared in
// docs/prelude/<page>.d.ts, one file a page, global declarations alone. The
// collections the examples use are docs/fennec-schema.d.ts, what `fennec
// types` makes of docs/schema.fennecql, made again by
//
//   node docs.mjs --schema     # with target/debug/fennec, or FENNEC=<binary>
//
// Each example is a module of its own (`export {}` appended), so two may
// both declare `db`; the pages' examples are compiled a page at a time, so
// each page's prelude says what `db` is there.

import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative } from 'node:path';

const here = new URL('.', import.meta.url).pathname;
const root = join(here, '../..');
const content = join(root, 'site/content');
const out = join(here, 'docs/out');

if (process.argv.includes('--schema')) {
  const fennec = process.env.FENNEC ?? join(root, 'target/debug/fennec');
  const dir = mkdtempSync(join(tmpdir(), 'fennec-docs-'));
  try {
    for (const line of readFileSync(join(here, 'docs/schema.fennecql'), 'utf8').split('\n')) {
      if (line.trim() && !line.startsWith('--')) execFileSync(fennec, ['docs.fennec', '-c', line], { cwd: dir });
    }
    const types = execFileSync(fennec, ['types', 'docs.fennec'], { cwd: dir, encoding: 'utf8' });
    writeFileSync(join(here, 'docs/fennec-schema.d.ts'), types);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

const PRE = /<pre([^>]*)>([\s\S]*?)<\/pre>/g;
const unescape = (s) =>
  s.replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&amp;/g, '&');
const lineAt = (text, i) => text.slice(0, i).split('\n').length;

function pages(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? pages(join(dir, e.name)) : e.name.endsWith('.html') ? [join(dir, e.name)] : [],
  );
}

// Every page's TypeScript blocks, each with the line its code starts on.
const found = [];
for (const file of pages(content).sort()) {
  const text = readFileSync(file, 'utf8');
  const blocks = [];
  for (const m of text.matchAll(PRE)) {
    if (!/\bdata-lang="ts"/.test(m[1])) continue;
    const at = m.index + 4 + m[1].length + 1;
    let code = unescape(m[2]);
    let line = lineAt(text, at);
    if (code.startsWith('\n')) {
      code = code.slice(1);
      line += 1;
    }
    blocks.push({ code, line });
  }
  if (blocks.length) found.push({ file, name: relative(content, file).replace(/\.html$/, '').replace(/\//g, '-'), blocks });
}

rmSync(out, { recursive: true, force: true });
const tsc = join(here, 'node_modules/.bin/tsc');
const up = (dir, to) => relative(dir, to).replace(/\\/g, '/');
let failed = false;
let count = 0;

for (const page of found) {
  const dir = join(out, page.name);
  mkdirSync(dir, { recursive: true });
  // What an example imports by a relative path, as a page beside the module
  // would: './fennec.js', './schema.js', './client.js' and the schema `fennec types` wrote
  // beside it.
  writeFileSync(join(dir, 'fennec.d.ts'), `export * from '${up(dir, join(root, 'web/fennec.js'))}';\n`);
  writeFileSync(join(dir, 'schema.d.ts'), `export * from '${up(dir, join(root, 'web/schema.js'))}';\n`);
  writeFileSync(join(dir, 'client.d.ts'), `export * from '${up(dir, join(root, 'web/client.js'))}';\n`);
  // The tables an example declared above it, as the app keeps them in a module of its own.
  writeFileSync(join(dir, 'tables.d.ts'), `export * from '${up(dir, join(here, 'docs/tables.js'))}';\n`);
  writeFileSync(join(dir, 'fennec-schema.d.ts'), `export * from '${up(dir, join(here, 'docs/fennec-schema.js'))}';\n`);

  const files = [];
  page.blocks.forEach((b, i) => {
    const name = `${String(i + 1).padStart(2, '0')}.tsx`;
    // The header takes the first line, so the code's line n is the file's n + 1.
    writeFileSync(join(dir, name), `// ${relative(root, page.file)}:${b.line}\n${b.code}\nexport {};\n`);
    files.push(name);
  });
  const prelude = join(here, 'docs/prelude', `${page.name}.d.ts`);
  if (existsSync(prelude)) files.push(up(dir, prelude));

  // A Worker's examples see the Workers runtime's globals, not the DOM's:
  // serverless.html's, and cloud.html's one TypeScript example, the Worker
  // in front of the objects (`crypto.subtle.timingSafeEqual` is the
  // runtime's, which the DOM's `SubtleCrypto` has none of).
  const workers = page.name === 'docs-serverless' || page.name === 'docs-cloud';
  const modules = join(here, 'node_modules');
  const tsconfig = {
    compilerOptions: {
      strict: true,
      noEmit: true,
      target: 'es2022',
      module: 'nodenext',
      moduleResolution: 'nodenext',
      jsx: 'react-jsx',
      lib: workers ? ['es2022'] : ['es2022', 'dom', 'dom.iterable'],
      typeRoots: [up(dir, modules)],
      types: workers ? ['@cloudflare/workers-types'] : [],
      paths: {
        '@fennecdb/web': [up(dir, join(root, 'web/fennec.d.ts'))],
        '@fennecdb/web/schema': [up(dir, join(root, 'web/schema.d.ts'))],
        '@fennecdb/web/client': [up(dir, join(root, 'web/client.d.ts'))],
        '@fennecdb/react': [up(dir, join(root, 'integrations/react/index.d.ts'))],
        '@fennecdb/cloudflare': [up(dir, join(root, 'integrations/cloudflare/index.d.ts'))],
        react: [up(dir, join(modules, '@types/react/index.d.ts'))],
        'react/jsx-runtime': [up(dir, join(modules, '@types/react/jsx-runtime.d.ts'))],
      },
    },
    files,
  };
  writeFileSync(join(dir, 'tsconfig.json'), `${JSON.stringify(tsconfig, null, 2)}\n`);

  try {
    execFileSync(tsc, ['-p', join(dir, 'tsconfig.json'), '--pretty', 'false'], { cwd: dir, encoding: 'utf8' });
  } catch (e) {
    failed = true;
    // Each error pointed at the page and the line it is on there.
    const where = /^(\d\d)\.tsx\((\d+),(\d+)\)/;
    for (const line of `${e.stdout ?? ''}${e.stderr ?? ''}`.split('\n')) {
      const m = line.match(where);
      if (!m) {
        if (line.trim()) console.error(line);
        continue;
      }
      const b = page.blocks[Number(m[1]) - 1];
      console.error(`${relative(root, page.file)}:${b.line + Number(m[2]) - 2}:${m[3]}${line.slice(m[0].length)}`);
    }
  }
  count += page.blocks.length;
}

if (failed) process.exit(1);
console.log(`docs: ${count} TypeScript examples on ${found.length} pages type-check`);
