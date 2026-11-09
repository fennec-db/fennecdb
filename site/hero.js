/* The home hero's product: the page, the phone and fennec-server's terminal
   going through one 14 s story, over a sky of stars that answer it.

   The page searches its own copy of the menu and sends no request; the
   phone goes offline, still searches, and adds a row that waits in its queue;
   back online the row is sent once, the terminal logs it, and the page's list
   takes it in, its Desserts count rolling to 2.

   Two canvases, one loop. The stars are drawn behind the dunes (`.hero-stars`,
   in the scene) and the windows in front of them (`.hero-win`, in the hero's
   right column), so the dunes, the sand and their parallax stay the page's
   own (`site.js`) and this draws neither. Each event of the story sends a
   brightness wave through the stars from the window where it happened; the
   pointer brightens the stars near it and moves them aside, shifts the
   windows a little by depth, and a click sends a wave of its own. No line is
   drawn between anything: the waves are light in the stars, not wires.

   A frame draws the story from its time alone, as `motion.js` draws a scene.
   The windows' frames and shadows (a 50 px blur, the dearest thing on the
   canvas) are painted once into sprites and blitted. It sleeps off screen and
   while the tab is hidden (`awake`, site.js's), and under reduced motion it
   draws one frame, the story's end. */

const P = { night: '#070A18', win: '#120D22', bar: '#1D1430', field: '#150F26', rule: '#3A2A3F', star: '#FFF4DC', sun: '#F4A93C', hot: '#FFCE73', ember: '#FF7A3D', oasis: '#4FE0C4', sand: '#F0E2C4', sand2: '#CDB894', dim: '#96856A' };
const SANS = '"Bricolage Grotesque", system-ui, sans-serif';
const MONO = '"IBM Plex Mono", ui-monospace, monospace';
const clamp = (x, a = 0, b = 1) => Math.min(b, Math.max(a, x));
const span = (t, a, b) => clamp((t - a) / (b - a));
const ease = (x) => 1 - Math.pow(1 - clamp(x), 3);
const lerp = (a, b, k) => a + (b - a) * k;
const flash = (t, a, len = 0.9, rest = 0) => t < a ? 0 : lerp(1, rest, ease((t - a) / len));
const rng = (seed) => () => ((seed = (seed * 16807) % 2147483647) / 2147483647);

// The windows' stage, and where each stands on it.
const SW = 740, SH = 640;
const PAGE = { x: 30, y: 24, w: 500, h: 336 };
const PHONE = { x: 512, y: 164, w: 200, h: 404 };
const TERM = { x: 20, y: 388, w: 480, h: 226 };
const PAD = 70; // a shadow's reach, in a sprite

const STARS = (() => { const r = rng(11); return Array.from({ length: 300 }, () => ({ u: r(), v: r(), z: 0.25 + r() * 0.75, s: r() * 1.3 + 0.35, ph: r() * 6.28 })); })();

const MENU = [
  { s: 'Mercimek soup', cat: 'Soups', hit: false },
  { s: 'Sütlaç', cat: 'Desserts', hit: true },
  { s: 'Fırın sütlaç', cat: 'Desserts', hit: true, added: true },
  { s: 'Baklava', cat: 'Desserts', hit: false },
  { s: 'Sütlaç ice cream', cat: 'Ice cream', hit: true },
];
// The story's beats, in seconds of its cycle: the page's, then the phone's.
const B = { type: 0.8, filter: 1.75, insert: 7.7 };
const F = { off: 3.4, type: 3.9, filter: 4.75, write: 5.4, on: 6.4, sent: 7.0 };
const CYCLE = 14;
const END = 9.5; // the frame that says it all, for a reader who asked for no motion
// The terminal's log: GETs a second apart, then the one write and its change.
const LOG = [
  [-1, '$ fennec-server --file app.fennec --http :8080', P.sand],
  [-1, 'listening on: http://0.0.0.0:8080', P.dim],
  ...[[0.6, 17], [2.4, 4], [4.0, 23], [5.6, 9], [9.0, 31], [10.6, 12], [12.2, 6]].map(([t, id]) => [t, `GET /menu/${id}   200   0.022 ms`, P.sand2]),
  [6.8, 'POST /query   200   Idempotency-Key: 7f3a9c', P.sand],
  [6.95, '  put menu {name: "Fırın sütlaç", category: "Desserts"}', P.oasis],
  [7.3, '  change 4182 → 2 screens following /menu', P.oasis],
].sort((a, b) => a[0] - b[0]);

/* ------------------------------------------------------------- drawing */

const rr = (c, x, y, w, h, r) => {
  r = Math.min(r, w / 2, h / 2);
  c.beginPath();
  c.moveTo(x + r, y); c.arcTo(x + w, y, x + w, y + h, r); c.arcTo(x + w, y + h, x, y + h, r);
  c.arcTo(x, y + h, x, y, r); c.arcTo(x, y, x + w, y, r); c.closePath();
};
const say = (c, s, x, y, { size = 14, weight = 400, color = P.sand, font = SANS, align = 'left', alpha = 1 } = {}) => {
  if (alpha <= 0 || !s) return;
  c.save();
  c.globalAlpha *= alpha;
  c.font = `${weight} ${size}px ${font}`; c.fillStyle = color; c.textAlign = align; c.textBaseline = 'alphabetic';
  c.fillText(s, x, y);
  c.restore();
};
const width = (c, s, size, font = SANS, weight = 400) => { c.font = `${weight} ${size}px ${font}`; return c.measureText(s).width; };
const box = (c, x, y, w, h, r, { fill = P.field, stroke = null, alpha = 1, line = 1.2, glow = 0 } = {}) => {
  if (alpha <= 0) return;
  c.save();
  c.globalAlpha *= alpha;
  rr(c, x, y, w, h, r);
  if (fill) { c.fillStyle = fill; c.fill(); }
  if (stroke) { if (glow > 0) { c.shadowColor = 'rgba(79,224,196,.9)'; c.shadowBlur = 18 * glow; } c.strokeStyle = stroke; c.lineWidth = line; c.stroke(); }
  c.restore();
};
const typed = (s, t, at, per = 0.13) => t < at ? '' : s.slice(0, Math.min(s.length, Math.floor((t - at) / per) + 1));
// A row's text with one word marked as highlight() marks it, the mark swept in by `k`.
const marked = (c, s, word, x, y, size, k, alpha) => {
  const i = s.toLowerCase().indexOf(word);
  if (i >= 0 && k > 0) {
    const x0 = x + width(c, s.slice(0, i), size), w = width(c, s.slice(i, i + word.length), size);
    box(c, x0 - 3, y - size * 0.92, (w + 6) * ease(k), size * 1.25, 4, { fill: 'rgba(244,169,60,.3)', alpha });
  }
  say(c, s, x, y, { size, color: P.sand, alpha });
  if (i >= 0 && k > 0.5) say(c, s.slice(i, i + word.length), x + width(c, s.slice(0, i), size), y, { size, color: P.hot, alpha });
};
// A number that rolls from one value to the next.
const roll = (c, a, b, x, y, k, size, color, align = 'right') => {
  if (k <= 0 || a === b) { say(c, String(k > 0 ? b : a), x, y, { size, font: MONO, color, align }); return; }
  const e = ease(k), w = width(c, String(Math.max(a, b)) + '0', size, MONO);
  c.save();
  c.beginPath(); c.rect(align === 'right' ? x - w : x, y - size * 1.1, w, size * 1.45); c.clip();
  say(c, String(a), x, y - e * size * 1.2, { size, font: MONO, color, align, alpha: 1 - e });
  say(c, String(b), x, y + (1 - e) * size * 1.2, { size, font: MONO, color: P.oasis, align, alpha: e });
  c.restore();
};
// A list whose rows fold away or open in place: each row `{ ..., f }`, f its height from 0 to 1.
const list = (c, rows, x, y, w, h, gap, draw) => {
  for (const r of rows) {
    if (r.f <= 0.001) continue;
    c.save();
    c.beginPath(); c.rect(x - 4, y - 2, w + 8, (h + 4) * r.f); c.clip();
    draw(r, x, y, r.f);
    c.restore();
    y += (h + gap) * r.f;
  }
};

// What a window is before anything is in it: its shadow, its body, its bar.
const windowShell = (c, w, h, title) => {
  c.save();
  c.shadowColor = 'rgba(0,0,0,.55)'; c.shadowBlur = 50; c.shadowOffsetY = 18;
  rr(c, 0, 0, w, h, 14); c.fillStyle = P.win; c.fill();
  c.restore();
  c.save();
  rr(c, 0, 0, w, h, 14); c.clip();
  c.fillStyle = P.bar; c.fillRect(0, 0, w, 32);
  c.restore();
  box(c, 0, 0, w, h, 14, { fill: null, stroke: P.rule });
  [P.ember, P.sun, P.oasis].forEach((col, i) => { c.fillStyle = col; c.beginPath(); c.arc(18 + i * 15, 16, 4.5, 0, 7); c.fill(); });
  if (title) say(c, title, w / 2, 21, { size: 12, font: MONO, color: P.dim, align: 'center' });
};
const phoneShell = (c, w, h) => {
  c.save();
  c.shadowColor = 'rgba(0,0,0,.6)'; c.shadowBlur = 50; c.shadowOffsetY = 18;
  rr(c, 0, 0, w, h, 30); c.fillStyle = P.win; c.fill();
  c.restore();
  box(c, 0, 0, w, h, 30, { fill: null, stroke: P.rule, line: 1.6 });
  box(c, w / 2 - 30, 10, 60, 12, 6, { fill: P.bar });
};

/* ------------------------------------------------------------ the hero */

export function start({ hero, stage, stars, still, awake }) {
  const wc = stage.getContext('2d'), sc = stars.getContext('2d');
  let wpx = 1, sw = 0, sh = 0;          // device pixels a stage unit is; the stars' css size
  const place = { x: 0, y: 0, k: 1 };   // the stage, in the stars' css pixels
  let sprites = null;
  const ptr = { on: false, x: 0, y: 0 };
  const cam = { x: 0, y: 0 };
  let clicks = [];

  const sprite = (w, h, paint) => {
    const c = document.createElement('canvas');
    c.width = Math.ceil((w + 2 * PAD) * wpx); c.height = Math.ceil((h + 2 * PAD) * wpx);
    const x = c.getContext('2d');
    x.setTransform(wpx, 0, 0, wpx, PAD * wpx, PAD * wpx);
    paint(x);
    return c;
  };
  const blit = (img, x, y) => wc.drawImage(img, x - PAD, y - PAD, img.width / wpx, img.height / wpx);

  const fit = () => {
    const dpr = Math.min(devicePixelRatio || 1, 2);
    const r = stage.getBoundingClientRect();
    stage.width = Math.max(1, Math.round(r.width * dpr));
    stage.height = Math.round(stage.width * SH / SW);
    wpx = stage.width / SW;
    const sd = Math.min(dpr, 1.5), hr = stars.getBoundingClientRect();
    sw = hr.width; sh = hr.height;
    stars.width = Math.max(1, Math.round(sw * sd)); stars.height = Math.max(1, Math.round(sh * sd));
    sc.setTransform(sd, 0, 0, sd, 0, 0);
    place.x = r.left - hr.left; place.y = r.top - hr.top; place.k = r.width / SW;
    sprites = {
      page: sprite(PAGE.w, PAGE.h, (x) => windowShell(x, PAGE.w, PAGE.h, '')),
      term: sprite(TERM.w, TERM.h, (x) => windowShell(x, TERM.w, TERM.h, 'fennec-server — app.fennec')),
      phone: sprite(PHONE.w, PHONE.h, (x) => phoneShell(x, PHONE.w, PHONE.h)),
    };
  };

  // The stars: behind the dunes, answering the story and the pointer.
  const drawStars = (t, tc, camX, camY, events) => {
    sc.clearRect(0, 0, sw, sh);
    const waves = events.map(([at, x, y]) => ({ x: place.x + x * place.k, y: place.y + y * place.k, age: tc - at }))
      .concat(clicks.map((k) => ({ x: k.x, y: k.y, age: t - k.t })));
    const reach = sh * 0.8;
    let color = '';
    for (const st of STARS) {
      let x = st.u * sw - camX * 2.2 * st.z, y = Math.pow(st.v, 1.3) * reach - camY * 2.2 * st.z;
      let lift = 0;
      for (const w of waves) {
        if (w.age < 0 || w.age > 2.6) continue;
        const d = Math.hypot(x - w.x, y - w.y), r = w.age * 560;
        lift = Math.max(lift, Math.exp(-Math.pow((d - r) / 46, 2)) * (1 - w.age / 2.6));
      }
      if (ptr.on) {
        const d = Math.hypot(x - ptr.x, y - ptr.y), near = Math.exp(-Math.pow(d / 130, 2));
        lift = Math.max(lift, near * 0.85);
        if (d > 1) { x += ((x - ptr.x) / d) * near * 10; y += ((y - ptr.y) / d) * near * 10; }
      }
      const twk = still ? 0.28 : 0.2 + 0.16 * Math.sin(t * 1.3 + st.ph);
      sc.globalAlpha = clamp(twk + lift * 0.8);
      const col = lift > 0.25 ? P.oasis : P.star;
      if (col !== color) { sc.fillStyle = col; color = col; }
      sc.beginPath(); sc.arc(x, y, st.s * st.z + lift * 1.6, 0, 7); sc.fill();
    }
    sc.globalAlpha = 1;
  };

  const drawWindows = (t, tc, cx, cy) => {
    const c = wc;
    c.setTransform(wpx, 0, 0, wpx, 0, 0);
    c.clearRect(0, 0, SW, SH);
    const fadeIn = still ? 1 : ease(span(tc, 0, 0.4)) * (1 - ease(span(tc, CYCLE - 0.6, CYCLE)));
    const bx = PAGE.x + cx, by = PAGE.y + cy, bw = PAGE.w, bh = PAGE.h;
    const px = PHONE.x + cx * 1.4, py = PHONE.y + cy * 1.4, pw = PHONE.w, ph = PHONE.h;
    const tx = TERM.x + cx * 0.7, ty = TERM.y + cy * 0.7, tw = TERM.w, th = TERM.h;

    // The page: a search box over the page's own copy of the menu.
    c.save();
    c.globalAlpha = ease(span(t, 0.2, 0.9));
    blit(sprites.page, bx, by);
    c.globalAlpha *= fadeIn;
    box(c, bx + 72, by + 7, 300, 18, 9, { fill: P.field });
    say(c, 'my-app.com/menu', bx + 86, by + 20, { size: 12, font: MONO, color: P.dim });
    say(c, 'Menu', bx + 24, by + 66, { size: 20, weight: 600, color: P.star });
    const q = typed('sutlac', tc, B.type);
    const focus = tc > B.type - 0.4 && tc < B.filter + 1.4;
    box(c, bx + 24, by + 82, 300, 38, 19, { fill: P.field, stroke: focus ? P.sun : P.rule });
    if (q) say(c, q, bx + 44, by + 107, { size: 15, font: MONO, color: P.star, alpha: fadeIn });
    else say(c, 'Search the menu', bx + 44, by + 107, { size: 15, font: MONO, color: P.dim });
    if (focus && !still && Math.floor(t * 2.2) % 2 === 0) box(c, bx + 46 + width(c, q, 15, MONO), by + 92, 2, 19, 1, { fill: P.sun });
    const fold = ease(span(tc, B.filter, B.filter + 0.45));
    const rowsB = MENU.map((m) => ({ ...m, f: m.added ? ease(span(tc, B.insert, B.insert + 0.45)) : m.hit ? 1 : 1 - fold }));
    list(c, rowsB, bx + 24, by + 134, 300, 36, 8, (r, x, y) => {
      const fresh = r.added ? flash(tc, B.insert + 0.2, 1.4, 0) : 0;
      box(c, x, y, 300, 36, 8, { fill: P.field, stroke: fresh > 0.05 ? P.oasis : null, glow: fresh });
      marked(c, r.s, 'sütlaç', x + 14, y + 23, 15, r.hit ? span(tc, B.filter + 0.4, B.filter + 0.8) : 0, fadeIn);
      say(c, r.cat, x + 288, y + 23, { size: 11, font: MONO, color: P.dim, align: 'right', alpha: fadeIn });
    });
    // Its facets, counted beside the rows.
    say(c, 'category', bx + 348, by + 107, { size: 12, font: MONO, color: P.dim });
    const desserts = 2 - fold + ease(span(tc, B.insert + 0.3, B.insert + 0.7));
    const facets = [
      { s: 'Desserts', n: desserts, f: 1 },
      { s: 'Ice cream', n: 1, f: 1 },
      { s: 'Soups', n: 1, f: 1 - fold },
    ];
    list(c, facets, bx + 348, by + 128, 128, 30, 8, (r, x, y) => {
      say(c, r.s, x, y + 14, { size: 14, color: P.sand });
      if (r.s === 'Desserts') {
        if (tc < B.insert) roll(c, 2, 1, x + 128, y + 14, span(tc, B.filter, B.filter + 0.45), 13, P.sand2);
        else roll(c, 1, 2, x + 128, y + 14, span(tc, B.insert + 0.3, B.insert + 0.7), 13, P.sand2);
      } else say(c, String(r.n), x + 128, y + 14, { size: 13, font: MONO, color: P.sand2, align: 'right' });
      box(c, x, y + 21, 64 * r.n, 5, 3, { fill: P.sun });
    });
    say(c, 'searched in the page’s own copy · no request sent', bx + 24, by + bh - 18, { size: 12, font: MONO, color: P.oasis, alpha: ease(span(tc, B.filter + 0.6, B.filter + 1.0)) * fadeIn });
    c.restore();

    // The server's terminal: a log that keeps going.
    c.save();
    c.globalAlpha = ease(span(t, 0.35, 1.05));
    blit(sprites.term, tx, ty);
    c.globalAlpha *= Math.max(0.35, fadeIn);
    const shown = LOG.filter((l) => l[0] <= tc);
    const n = shown.length, last = shown[n - 1];
    const p = last && last[0] >= 0 ? ease(span(tc, last[0], last[0] + 0.25)) : 1;
    const ROWS = 7, off = Math.max(0, n - 1 + p - ROWS);
    c.save();
    c.beginPath(); c.rect(tx, ty + 34, tw, th - 38); c.clip();
    shown.forEach(([at, s, col], i) => {
      const y = ty + 58 + (i - off) * 24;
      const hi = at >= 6.8 ? flash(tc, at, 1.6, 0) : 0;
      if (hi > 0.02) box(c, tx + 8, y - 16, tw - 16, 22, 5, { fill: `rgba(79,224,196,${0.14 * hi})` });
      say(c, s, tx + 18, y, { size: 13, font: MONO, color: col, alpha: (i === n - 1 ? p : 1) * (at >= 0 ? fadeIn : 1) });
    });
    c.restore();
    c.restore();

    // The phone: its replica, with the network and without it.
    c.save();
    c.globalAlpha = ease(span(t, 0.5, 1.2));
    blit(sprites.phone, px, py);
    c.globalAlpha *= fadeIn;
    const offline = tc >= F.off && tc < F.on;
    say(c, '9:41', px + 22, py + 40, { size: 11, font: MONO, color: P.sand2 });
    box(c, px + pw - 78, py + 27, 58, 18, 9, { fill: P.field, stroke: offline ? P.ember : P.oasis, line: 1.1 });
    say(c, offline ? 'offline' : 'online', px + pw - 49, py + 40, { size: 10.5, font: MONO, color: offline ? P.ember : P.oasis, align: 'center' });
    say(c, 'Menu', px + 16, py + 72, { size: 17, weight: 600, color: P.star });
    const fq = typed('sutlac', tc, F.type);
    box(c, px + 14, py + 84, pw - 28, 32, 16, { fill: P.field, stroke: tc > F.type - 0.3 && tc < F.filter + 1 ? P.sun : P.rule });
    if (fq) say(c, fq, px + 30, py + 105, { size: 13, font: MONO, color: P.star, alpha: fadeIn });
    else say(c, 'Search', px + 30, py + 105, { size: 13, font: MONO, color: P.dim });
    const ffold = ease(span(tc, F.filter, F.filter + 0.45));
    const wrote = ease(span(tc, F.write, F.write + 0.45));
    const rowsP = MENU.map((m) => ({ ...m, f: m.added ? wrote : m.hit ? 1 : 1 - ffold }));
    list(c, rowsP, px + 14, py + 128, pw - 28, 32, 7, (r, x, y) => {
      const pending = r.added && tc < F.sent;
      box(c, x, y, pw - 28, 32, 7, { fill: P.field, stroke: r.added ? (pending ? P.sun : P.oasis) : null, glow: r.added ? flash(tc, F.sent, 1.2, 0) : 0 });
      marked(c, r.s, 'sütlaç', x + 10, y + 21, 13, r.hit ? span(tc, F.filter + 0.3, F.filter + 0.7) : 0, fadeIn);
      if (r.added) say(c, pending ? (tc >= F.on ? 'sending' : 'queued') : 'synced', x + pw - 38, y + 20, { size: 9.5, font: MONO, color: pending ? P.sun : P.oasis, align: 'right', alpha: tc < F.sent + 2.5 ? 1 : 1 - ease(span(tc, F.sent + 2.5, F.sent + 3)) });
    });
    say(c, 'on the device', px + pw - 16, py + 72, { size: 10.5, font: MONO, color: P.oasis, align: 'right', alpha: ease(span(tc, F.filter + 0.5, F.filter + 0.8)) * fadeIn });
    const queued = tc >= F.write && tc < F.sent ? 1 : 0;
    say(c, 'queue in its file', px + 16, py + ph - 66, { size: 11, font: MONO, color: P.dim });
    roll(c, tc < F.sent ? 0 : 1, tc < F.sent ? 1 : 0, px + pw - 16, py + ph - 66, tc < F.sent ? span(tc, F.write, F.write + 0.4) : span(tc, F.sent, F.sent + 0.4), 11, queued ? P.sun : P.dim);
    const press = 1 - 0.06 * Math.sin(Math.PI * span(tc, F.write - 0.15, F.write + 0.1));
    c.save();
    c.translate(px + pw / 2, py + ph - 38); c.scale(press, press);
    box(c, -(pw - 28) / 2, -18, pw - 28, 36, 18, { fill: P.sun });
    say(c, 'Add Fırın sütlaç', 0, 5, { size: 13, weight: 600, color: P.night, align: 'center' });
    c.restore();
    c.restore();

    // Where things happened, for the stars: the page filtering and then taking
    // the row in, the phone coming back online, the server's change.
    return [
      [B.filter, bx + 160, by + 105], [F.on + 0.1, px + pw / 2, py + 40], [7.3, tx + tw / 2, ty + 120], [B.insert, bx + 160, by + 220],
    ];
  };

  let t = 0, last = 0, running = false;
  const draw = (dt) => {
    const tc = still ? END : t % CYCLE;
    // The windows lean with the pointer, by depth; the stars more, the other way.
    const tx0 = ptr.on ? (ptr.x - sw / 2) * 0.012 : 0, ty0 = ptr.on ? (ptr.y - sh / 2) * 0.012 : 0;
    cam.x = lerp(cam.x, tx0, 0.08); cam.y = lerp(cam.y, ty0, 0.08);
    clicks = clicks.filter((k) => t - k.t < 2.7);
    const events = drawWindows(still ? 99 : t, tc, cam.x / place.k, cam.y / place.k);
    drawStars(t, tc, cam.x, cam.y, still ? [] : events);
  };
  const frame = (now) => {
    const dt = Math.min(0.1, (now - last) / 1000);
    last = now;
    t += dt;
    draw(dt);
    running = awake.on && !document.hidden;
    if (running) requestAnimationFrame(frame);
  };
  const wake = () => { if (!running && !still) { running = true; last = performance.now(); requestAnimationFrame(frame); } };

  fit();
  draw(0);
  new ResizeObserver(() => { fit(); if (still || !running) draw(0); }).observe(hero);
  if (still) return;
  awake.subs.push(wake);
  wake();

  const at = (e) => {
    const r = stars.getBoundingClientRect();
    return [e.clientX - r.left, e.clientY - r.top];
  };
  hero.addEventListener('pointermove', (e) => { ptr.on = true; [ptr.x, ptr.y] = at(e); });
  hero.addEventListener('pointerleave', () => { ptr.on = false; });
  hero.addEventListener('pointerdown', (e) => {
    // A click on a button or a link is that, not a wave.
    if (e.target.closest?.('a, button')) return;
    const [x, y] = at(e);
    clicks.push({ x, y, t });
  });
}
