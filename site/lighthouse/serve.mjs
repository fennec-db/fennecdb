// site/dist served as Cloudflare serves it, for Lighthouse: clean URLs
// (`/docs/fennecql` is docs/fennecql.html, `/docs/` its index), the cache
// headers of the `_headers` file build.py wrote, and brotli/gzip for text
// (the edge compresses; a server that did not would fail Lighthouse's
// text-compression audit for a reason the site does not have).
// Listens on port 0 and prints `listening on: http://<address>` to stderr,
// the line every harness of the repo reads its port from.
import { createServer } from 'node:http';
import { existsSync, readFileSync, statSync } from 'node:fs';
import { extname, join, normalize, resolve } from 'node:path';
import { brotliCompressSync, gzipSync } from 'node:zlib';

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json',
  '.svg': 'image/svg+xml',
  '.wasm': 'application/wasm',
  '.woff2': 'font/woff2',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.webp': 'image/webp',
  '.txt': 'text/plain; charset=utf-8',
  '.gz': 'application/gzip',
};
const COMPRESSIBLE = /^(text\/|application\/(json|wasm)|image\/svg)/;

export function serve(dist) {
  const root = resolve(dist);
  // _headers: a path pattern, then indented `Name: value` lines; `*` is a
  // splat, `! Name` removes a header an earlier rule set.
  const rules = [];
  const file = join(root, '_headers');
  if (existsSync(file)) {
    let rule;
    for (const line of readFileSync(file, 'utf8').split('\n')) {
      if (!line.trim()) continue;
      if (!/^\s/.test(line)) {
        const re = new RegExp('^' + line.trim().replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*') + '$');
        rules.push((rule = { re, set: [], drop: [] }));
      } else if (rule) {
        const t = line.trim();
        if (t.startsWith('!')) rule.drop.push(t.slice(1).trim().toLowerCase());
        else rule.set.push([t.slice(0, t.indexOf(':')).trim(), t.slice(t.indexOf(':') + 1).trim()]);
      }
    }
  }
  const find = (p) => {
    for (const c of [p, p + '.html', join(p, 'index.html')]) {
      try {
        if (statSync(c).isFile()) return c;
      } catch {
        /* next spelling */
      }
    }
    return null;
  };
  return createServer((req, res) => {
    const path = decodeURIComponent(new URL(req.url, 'http://x').pathname);
    const full = normalize(join(root, path));
    const hit = full.startsWith(root) ? find(full) : null;
    const body = readFileSync(hit ?? join(root, '404.html'));
    const type = TYPES[extname(hit ?? '.html')] ?? 'application/octet-stream';
    const headers = { 'content-type': type };
    for (const r of rules) {
      if (!r.re.test(path)) continue;
      for (const [k, v] of r.set) headers[k.toLowerCase()] = v;
      for (const k of r.drop) delete headers[k];
    }
    let out = body;
    const accept = req.headers['accept-encoding'] ?? '';
    if (COMPRESSIBLE.test(type) && body.length > 1024) {
      if (accept.includes('br')) {
        out = brotliCompressSync(body);
        headers['content-encoding'] = 'br';
      } else if (accept.includes('gzip')) {
        out = gzipSync(body);
        headers['content-encoding'] = 'gzip';
      }
      headers.vary = 'Accept-Encoding';
    }
    headers['content-length'] = out.length;
    res.writeHead(hit ? 200 : 404, headers);
    res.end(req.method === 'HEAD' ? undefined : out);
  });
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const server = serve(process.argv[2] ?? '../dist');
  server.listen(0, '127.0.0.1', () => console.error(`listening on: http://127.0.0.1:${server.address().port}`));
}
