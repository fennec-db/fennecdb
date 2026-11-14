// Text no store compresses away, for persist.js and file-worker.js to fill
// a quota with: Firefox's IndexedDB compresses what it stores (snappy), and
// a row of one repeated letter would have cost it almost nothing.
// `crypto.getRandomValues` hands out 64 KiB a call; xorshift32 is plenty.

let s = 0x9e3779b9;

/** `n` characters of 64, drawn at random. */
export function noise(n) {
  const abc = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  const out = new Array(n);
  for (let i = 0; i < n; i++) {
    s ^= s << 13;
    s ^= s >>> 17;
    s ^= s << 5;
    out[i] = abc[s & 63];
  }
  return out.join('');
}
