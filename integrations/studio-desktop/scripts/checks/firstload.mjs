// A studio window's first load that ended with nothing committed is loaded
// again (Linux).
//
// On the Linux runners a window's first load sometimes ended 5 to 22 ms after
// its will-navigate, before anything of the page committed, and the window
// stayed blank: WebKitGTK failed the load with no error page to draw, and
// Electrobun 2.0.4 hands the app such a load as the did-navigate its
// load-changed FINISHED sends, its load-failed reaching no handler. The app
// loads the page again (src/bun/firstload.ts). The runners' was a cancel
// (WebKitNetworkError 302, as the load started), which cannot be had on
// demand, so this makes a load end the same way: the window's server is
// reached through a proxy here whose first answer to the page is 204 No
// Content, which WebKit stops as it starts too (WebKitPolicyError 102, "Frame
// load interrupted"), drawing no error page, and every request after it goes
// through to the server.
import assert from 'node:assert/strict';
import { request, createServer } from 'node:http';
import { assertClean, cmd, data, lines, log, quit, startApp, startServer, step, waitFor, waitStudio } from './lib.mjs';

assertClean('before');
const SMALL = data('small', 10);
const TOKEN = 'firstload-check-token';
const server = await startServer(['--file', SMALL, '--studio', '--http', '127.0.0.1:0'], { FENNEC_HTTP_TOKEN: TOKEN });
const upstream = new URL(server.origin);

// The page's first request is answered 204; the rest are the server's, streams included.
let dropped = 0;
const proxy = createServer((req, res) => {
  if (dropped === 0 && req.method === 'GET' && req.url === '/_studio/') {
    dropped++;
    res.writeHead(204).end();
    return;
  }
  const out = request({ host: upstream.hostname, port: upstream.port, method: req.method, path: req.url, headers: req.headers }, (back) => {
    res.writeHead(back.statusCode ?? 502, back.headers);
    back.pipe(res);
  });
  out.on('error', () => res.destroy());
  req.pipe(out);
});
await new Promise((r) => proxy.listen(0, '127.0.0.1', r));
const origin = `http://127.0.0.1:${proxy.address().port}`;

const app = startApp('firstload');
await waitFor((l) => l.what === 'ready', 60000, 'the app ready');

step('a first load that committed nothing is loaded again, and the studio is there');
const KEY = 'store:dropped';
const n = lines().length;
cmd({ cmd: 'store-window', key: KEY, origin, token: TOKEN });
await waitStudio(KEY, n);
const mine = lines()
  .slice(n)
  .filter((l) => l.key === KEY);
const first = mine.findIndex((l) => l.what === 'did-navigate');
assert.equal(dropped, 1, 'the page was first answered 204');
assert.ok(first >= 0 && !mine.slice(0, first).some((l) => l.what === 'did-commit-navigation'), 'the first load ended with nothing committed');
const again = mine.filter((l) => l.what === 'load-again');
assert.deepEqual(
  again.map((l) => l.tries),
  [1],
  'loaded again once',
);
log(`the load again ${again[0].at - mine[first].at} ms after the dropped one ended; the studio loaded ${mine.find((l) => l.detail?.type === 'state' && l.detail.loaded).at - again[0].at} ms after it`);

step('quit');
// The server first: a quit holds the app to leaving no server of its own, and this one is the check's.
await server.stop();
proxy.closeAllConnections();
await new Promise((r) => proxy.close(r));
await quit(app);
log('clean');
