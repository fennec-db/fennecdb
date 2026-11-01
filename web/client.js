// `@fennecdb/web/client`: fennecdb for a page whose queries run on a server.
//
// The HTTP client, the query builder and live queries over the server's
// subscriptions -- and no engine: it imports neither the module's glue nor
// persistence, files or the sync layer, so a bundle of it cannot hold them.
// That is the point of it more than the bytes: `connect` through
// `@fennecdb/web` never fetches the `.wasm` either (only `Fennec.open` does),
// and bundles 2.5 KB brotli larger, the glue a bundler cannot drop. The same builder as `@fennecdb/web`, so a query
// moves between the two unchanged.
//
//   import { connect } from '@fennecdb/web/client';
//   const db = connect('https://db.example.com', { token });
//   const rows = await db.from('docs').where('year', '>=', 2024).limit(10).rows();
//   const stop = db.live(db.from('docs').where('done', false), render);
//
// `web/fennec.client.test.js` fails if anything here reaches the engine.

export {
  FennecError, Query, Call, Param, from, call, or, and, not, raw, inc, expr, bucket, countDistinct, distance, first, last, param,
} from './builder.js';
export { FennecHttp, connect, sseEvents } from './http.js';
