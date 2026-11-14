// Text no store compresses away, for persist.js and file-worker.js to fill
// a quota with: Firefox's IndexedDB compresses what it stores (snappy), and
// a row of one repeated letter would have cost it almost nothing.
// `crypto.getRandomValues` hands out 64 KiB a call; xorshift32 is plenty.

const ABC = new TextEncoder().encode('ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/');
let s = 0x9e3779b9;

/** `n` characters of 64, drawn at random, made 4 KiB at a time. */
export function noise(n) {
  const parts = [];
  for (let at = 0; at < n; at += 4096) {
    const codes = new Uint8Array(Math.min(4096, n - at));
    for (let i = 0; i < codes.length; i++) {
      s ^= s << 13;
      s ^= s >>> 17;
      s ^= s << 5;
      codes[i] = ABC[s & 63];
    }
    parts.push(String.fromCharCode(...codes));
  }
  return parts.join('');
}
