// A webhook consumer's test event: its statement, under `statements.js`'s
// rules, the event it begins from and what keeps it from running. Fetched
// when Test event is clicked: in `schema.js` it held the schema view past
// the 25 000 bytes gzipped a view is held to (`statements.test.mjs`).

import { StatementError, statementName, signatureParams } from './statements-views.js';

/** A webhook consumer called with an event, `call <name> $1`: run as a delivery is, unsigned and keeping no key. */
export function testEvent(n, event) {
  statementName(n);
  if (event === null || typeof event !== 'object' || Array.isArray(event)) throw new StatementError('an event is a JSON object: {"type": "..."}');
  return { text: `call ${n} $1`, params: [event] };
}

// A value of each type a parameter reads, since the call refuses a path
// holding `null`: a list's empty, a `json` one's an object (one each), a
// `bytes` one's a text, its bytes. A point or a vector begins `null`.
const BLANK = { int: 0, float: 0, bool: false, text: '', timestamp: 0, bytes: '' };
const blank = (type) => BLANK[type] ?? (type === 'json' ? Object.create(null) : type[0] === '[' ? [] : null);

// A key plain or quoted (`metadata."order-id"`); a path begun after no
// dot, `data.event.x` being a path through a key `event`; a text as the
// lexer reads it, `\` escaping the character after it and `''` a quote.
const KEY = String.raw`(?:[\p{Alphabetic}\p{N}_]+|"[^"]+")`;
const START = String.raw`(?<![.\p{Alphabetic}\p{N}_])`;
const TEXT = String.raw`'(?:[^'\\]|\\[^]|'')*'|"(?:[^"\\]|\\[^])*"`;
const COMPARED = new RegExp(String.raw`${TEXT}|${START}event\.(${KEY}(?:\.${KEY})*)\s*=\s*(${TEXT}|true|false|-?\d[\d_]*(?:\.\d+)?)`, 'gu');
const ESCAPED = { n: '\n', t: '\t', r: '\r', 0: '\0' };

/** A literal's value, a text's as FennecQL reads it. */
function literal(v) {
  if (v === 'true' || v === 'false') return v === 'true';
  if (v[0] !== "'" && v[0] !== '"') return Number(v.replaceAll('_', ''));
  return v.slice(1, -1).replace(v[0] === "'" ? /\\([^])|''/g : /\\([^])/g, (_, c) => (c === undefined ? "'" : (ESCAPED[c] ?? c)));
}

/**
 * A test event to begin from: each path a parameter reads holding a value of
 * its type, each `event.<path> = <literal>` of the condition its literal.
 * Objects of no prototype, so a key `__proto__` is a key.
 */
export function eventSkeleton(signature, condition) {
  const event = Object.create(null);
  const put = (keys, v) => {
    let o = event;
    for (const k of keys.slice(0, -1)) o = o[k] = typeof o[k] === 'object' && o[k] ? o[k] : Object.create(null);
    o[keys.at(-1)] = v;
  };
  // A signature's keys are plain, joined by dots, which no key holds.
  for (const p of signatureParams(signature).params) if (p.from?.startsWith('event.')) put(p.from.slice(6).split('.'), blank(p.type));
  // A text is matched whole: nothing in one reads as a comparison.
  for (const [, path, v] of String(condition ?? '').matchAll(COMPARED)) {
    if (path) put(path.match(/"[^"]+"|[^.]+/g).map((k) => (k[0] === '"' ? k.slice(1, -1) : k)), literal(v));
  }
  return event;
}

/**
 * What the dialog says first: `said`, how it runs and whether its
 * condition reads `hook.event`, `null` here, which `= 'push'` passes over
 * and `!= 'ping'` does not; `never`, the parameters reading a `hook.`
 * value with no default, which make its call refused (400) whatever the
 * event holds.
 */
export function notes(signature, condition) {
  const held = signatureParams(signature).params.filter((p) => p.from?.startsWith('hook.') && !p.optional);
  const asks = [...String(condition ?? '').matchAll(new RegExp(String.raw`${TEXT}|${START}hook\.event(?![\p{Alphabetic}\p{N}_])`, 'gu'))].some(([m]) => m[0] === 'h');
  return {
    said: `It runs as a delivery would, unsigned and keeping no key: hook.id, hook.at and hook.event are null.${asks ? " Its condition reads hook.event, so hook.event = '...' passes it over." : ''}`,
    never: held.length ? `A test event never runs it: ${held.map((p) => `$${p.name} reads ${p.from}`).join(', ')}, with no default.` : null,
  };
}
