// TEMPORARY (spike, taken out before review): what WebView2 does on the
// runners with the arguments Electrobun 2.0.4 hard-codes, with
// WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS (elevated and not), and with a page's
// requests for a new window from each kind of frame. Prints facts; fails only
// where it could not ask.
import { spawnSync } from 'node:child_process';
import { createServer } from 'node:http';
import { appendFileSync, existsSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { BUN, CONTROL, D, EXE, HERE, POWERSHELL, REPORT, USER_DATA, cmd, evalIn, lines, mine, sleep, startApp, waitFor, log } from '../checks/lib.mjs';

const ps = (script, ms = 60000) => spawnSync(POWERSHELL, ['-NoProfile', '-NonInteractive', '-Command', script], { encoding: 'utf8', timeout: ms });
const say = (title, v) => log(`\n## ${title}\n${typeof v === 'string' ? v : JSON.stringify(v, null, 1)}`);

// ---- the runner
say('whoami /groups (integrity, Administrators)', spawnSync('whoami', ['/groups'], { encoding: 'utf8' }).stdout.split(/\r?\n/).filter((l) => /Mandatory|S-1-5-32-544/.test(l)).join('\n'));
say('UAC', spawnSync('reg', ['query', 'HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Policies\\System'], { encoding: 'utf8' }).stdout.split(/\r?\n/).filter((l) => /EnableLUA|FilterAdministratorToken|ConsentPromptBehaviorAdmin/.test(l)).join('\n'));
say('token elevation', ps("Add-Type -TypeDefinition 'using System;using System.Runtime.InteropServices;public static class E{[DllImport(\"advapi32.dll\")]static extern bool OpenProcessToken(IntPtr p,uint a,out IntPtr t);[DllImport(\"advapi32.dll\")]static extern bool GetTokenInformation(IntPtr t,int c,out int v,int l,out int r);[DllImport(\"kernel32.dll\")]static extern IntPtr GetCurrentProcess();public static string Get(){IntPtr t;OpenProcessToken(GetCurrentProcess(),8,out t);int e,r,ty;GetTokenInformation(t,20,out e,4,out r);GetTokenInformation(t,18,out ty,4,out r);return \"TokenIsElevated=\"+e+\" TokenElevationType=\"+ty;}}'; [E]::Get()").stdout.trim());
for (const key of ['HKLM\\SOFTWARE\\WOW6432Node\\Microsoft\\EdgeUpdate\\Clients\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'HKLM\\SOFTWARE\\Microsoft\\EdgeUpdate\\Clients\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}']) {
  const r = spawnSync('reg', ['query', key, '/v', 'pv'], { encoding: 'utf8' });
  if (r.status === 0) say(`WebView2 runtime (${key.includes('WOW') ? 'WOW6432' : 'native'})`, r.stdout.trim().split(/\r?\n/).pop());
}

// ---- does Bun's process.env reach the Win32 environment?
const probe = join(D, 'spike-env.ts');
writeFileSync(
  probe,
  `import { dlopen, FFIType, ptr } from "bun:ffi";
const k = dlopen("kernel32.dll", { GetEnvironmentVariableW: { args: [FFIType.ptr, FFIType.ptr, FFIType.u32], returns: FFIType.u32 }, SetEnvironmentVariableW: { args: [FFIType.ptr, FFIType.ptr], returns: FFIType.i32 } });
const w = (s) => Buffer.from(s + "\\0", "utf16le");
const get = (name) => { const n = w(name); const b = Buffer.alloc(2048); const len = k.symbols.GetEnvironmentVariableW(ptr(n), ptr(b), 1024); return len ? b.subarray(0, len * 2).toString("utf16le") : null; };
const out = { inheritedB: get("FENNEC_SPIKE_B") };
process.env.FENNEC_SPIKE_A = "set-by-process-env";
out.afterAssign = get("FENNEC_SPIKE_A");
delete process.env.FENNEC_SPIKE_B;
out.afterDelete = get("FENNEC_SPIKE_B");
const n = w("FENNEC_SPIKE_C"), v = w("set-by-ffi");
out.ffiSet = k.symbols.SetEnvironmentVariableW(ptr(n), ptr(v));
out.ffiReadBack = get("FENNEC_SPIKE_C");
out.processEnvSeesFfi = process.env.FENNEC_SPIKE_C ?? null;
console.log(JSON.stringify(out));
`,
);
const envProbe = spawnSync(BUN, [probe], { encoding: 'utf8', env: { ...process.env, FENNEC_SPIKE_B: 'inherited' }, timeout: 60000 });
say("Bun's process.env and the Win32 environment", envProbe.stdout.trim() + envProbe.stderr.trim());

// ---- the pages: a hostile "remote server" with no CSP, and another origin
const hits = [];
const html = (res, body, headers = {}) => {
  res.writeHead(200, { 'content-type': 'text/html; charset=utf-8', ...headers });
  res.end(body);
};
const hostile = createServer((req, res) => {
  hits.push(req.url);
  if (req.url.startsWith('/_studio/')) return html(res, '<!doctype html><title>hostile</title><body><p id="p">hostile page</p></body>');
  if (req.url.startsWith('/popup/')) return html(res, `<!doctype html><title>popup ${req.url.slice(7)}</title><p>popup`);
  if (req.url === '/frame') return html(res, '<!doctype html><title>same-origin frame</title><p>frame');
  res.writeHead(404);
  res.end();
});
const other = createServer((req, res) => {
  hits.push(`B${req.url}`);
  const u = new URL(req.url, 'http://x');
  if (u.pathname === '/secret') {
    res.writeHead(200, { 'content-type': 'text/plain' });
    return res.end('secret-from-B');
  }
  if (u.pathname === '/cors') {
    res.writeHead(200, { 'content-type': 'text/plain', 'access-control-allow-origin': '*' });
    return res.end('cors-from-B');
  }
  if (u.pathname === '/frame') return html(res, '<!doctype html><title>B frame</title><p>B');
  if (u.pathname === '/frame-open') {
    const k = u.searchParams.get('k');
    const target = u.searchParams.get('u');
    return html(res, `<!doctype html><script>var r;try{r=open(${JSON.stringify(target)});r=r===null?"null":typeof r}catch(e){r="threw "+e}parent.postMessage({k:${JSON.stringify(k)},bridge:typeof __electrobunSendToHost,openNative:/native code/.test(Function.prototype.toString.call(open)),returned:r},"*")</script>`);
  }
  res.writeHead(404);
  res.end();
});
const listen = (s) => new Promise((r) => s.listen(0, '127.0.0.1', () => r(`http://127.0.0.1:${s.address().port}`)));
const H = await listen(hostile);
const B = await listen(other);
say('servers', { H, B });

/** WebView2's processes of the test variant, the browser's command line first. */
function webview2() {
  const dir = USER_DATA.replace(/'/g, "''");
  const r = ps(`Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains('${dir}') } | ForEach-Object { '{0}|{1}' -f $_.ProcessId, $_.CommandLine }`);
  return r.stdout.split(/\r?\n/).filter(Boolean).map((l) => ({ pid: +l.slice(0, l.indexOf('|')), cl: l.slice(l.indexOf('|') + 1) }));
}
const switches = (cl) => (cl.match(/--[A-Za-z0-9-]+(=("[^"]*"|\S*))?/g) ?? []).filter((s) => !/user-data-dir|mojo|field-trial|metrics-shmem|embedded-browser-webview-dpi|webview-exe-version|variations/.test(s));

/** Every visible top-level window of the test variant's WebView2 processes, with its title. */
function windowsOfWebview2() {
  const pids = webview2().map((p) => p.pid);
  if (!pids.length) return [];
  const r = ps(
    `Add-Type -TypeDefinition 'using System;using System.Text;using System.Collections.Generic;using System.Runtime.InteropServices;public static class W{public delegate bool P(IntPtr h,IntPtr l);[DllImport("user32.dll")]static extern bool EnumWindows(P f,IntPtr l);[DllImport("user32.dll")]static extern uint GetWindowThreadProcessId(IntPtr h,out uint p);[DllImport("user32.dll")]static extern bool IsWindowVisible(IntPtr h);[DllImport("user32.dll",CharSet=CharSet.Unicode)]static extern int GetWindowText(IntPtr h,StringBuilder s,int n);public static string[] L(uint[] ids){var set=new HashSet<uint>(ids);var r=new List<string>();EnumWindows(delegate(IntPtr h,IntPtr l){uint p;GetWindowThreadProcessId(h,out p);if(set.Contains(p)&&IsWindowVisible(h)){var sb=new StringBuilder(512);GetWindowText(h,sb,512);r.Add(p+" "+sb);}return true;},IntPtr.Zero);return r.ToArray();}}'; [W]::L([uint32[]]@(${pids.join(',')}))`,
  );
  if (r.status !== 0) return [`(not listed: ${r.stderr})`];
  return r.stdout.split(/\r?\n/).filter(Boolean);
}

const U = (k) => `${H}/popup/${k}`;
const PROBE = `const probe = (cw) => { try { return { bridge: typeof cw.__electrobunSendToHost, openNative: /native code/.test(Function.prototype.toString.call(cw.open)), href: String(cw.location.href).slice(0, 40) }; } catch (e) { return { probe: 'threw ' + e }; } };
const ret = (w) => (w === null ? 'null' : w === undefined ? 'undefined' : typeof w);
const loaded = (f) => new Promise((r) => { f.addEventListener('load', r, { once: true }); });
const message = (k, ms) => new Promise((r) => { const t = setTimeout(() => r({ message: 'none in ' + ms + ' ms' }), ms); window.addEventListener('message', (e) => { if (e.data && e.data.k === k) { clearTimeout(t); r(e.data); } }); });`;
const KINDS = {
  'main-open': `return { returned: ret(window.open(${JSON.stringify(U('main-open'))})) };`,
  'blank-sync': `const f = document.createElement('iframe'); document.body.append(f); const cw = f.contentWindow; const i = probe(cw); i.returned = ret(cw.open(${JSON.stringify(U('blank-sync'))})); return i;`,
  'blank-index': `document.body.append(document.createElement('iframe')); const cw = window[window.length - 1]; const i = probe(cw); i.returned = ret(cw.open(${JSON.stringify(U('blank-index'))})); return i;`,
  'blank-load': `const f = document.createElement('iframe'); f.src = 'about:blank'; const l = loaded(f); document.body.append(f); await l; const i = probe(f.contentWindow); i.returned = ret(f.contentWindow.open(${JSON.stringify(U('blank-load'))})); return i;`,
  'srcdoc-load': `const f = document.createElement('iframe'); f.srcdoc = '<p>x</p>'; const l = loaded(f); document.body.append(f); await l; const i = probe(f.contentWindow); i.returned = ret(f.contentWindow.open(${JSON.stringify(U('srcdoc-load'))})); return i;`,
  'srcdoc-inline': `const m = message('srcdoc-inline', 5000); const f = document.createElement('iframe'); f.srcdoc = ${JSON.stringify(`<script>var r;try{r=open(${JSON.stringify(U('srcdoc-inline'))});r=r===null?"null":typeof r}catch(e){r="threw "+e}parent.postMessage({k:"srcdoc-inline",bridge:typeof __electrobunSendToHost,openNative:/native code/.test(Function.prototype.toString.call(open)),returned:r},"*")<\/script>`)}; document.body.append(f); return await m;`,
  'same-origin-load': `const f = document.createElement('iframe'); f.src = '/frame'; const l = loaded(f); document.body.append(f); await l; const i = probe(f.contentWindow); i.returned = ret(f.contentWindow.open(${JSON.stringify(U('same-origin-load'))})); return i;`,
  'cross-origin-inline': `const m = message('cross-origin-inline', 8000); const f = document.createElement('iframe'); f.src = ${JSON.stringify(`${B}/frame-open?k=cross-origin-inline&u=${encodeURIComponent(U('cross-origin-inline'))}`)}; document.body.append(f); return await m;`,
  'data-inline': `const m = message('data-inline', 5000); const f = document.createElement('iframe'); f.src = 'data:text/html,' + encodeURIComponent(${JSON.stringify(`<script>var r;try{r=open(${JSON.stringify(U('data-inline'))});r=r===null?"null":typeof r}catch(e){r="threw "+e}parent.postMessage({k:"data-inline",bridge:typeof __electrobunSendToHost,openNative:/native code/.test(Function.prototype.toString.call(open)),returned:r},"*")<\/script>`)}); document.body.append(f); return await m;`,
  'blob-inline': `const m = message('blob-inline', 5000); const b = new Blob([${JSON.stringify(`<script>var r;try{r=open(${JSON.stringify(U('blob-inline'))});r=r===null?"null":typeof r}catch(e){r="threw "+e}parent.postMessage({k:"blob-inline",bridge:typeof __electrobunSendToHost,openNative:/native code/.test(Function.prototype.toString.call(open)),returned:r},"*")<\/script>`)}], { type: 'text/html' }); const f = document.createElement('iframe'); f.src = URL.createObjectURL(b); document.body.append(f); return await m;`,
  'nested-blank': `const f = document.createElement('iframe'); document.body.append(f); const g = f.contentDocument.createElement('iframe'); f.contentDocument.body.append(g); const i = probe(g.contentWindow); i.returned = ret(g.contentWindow.open(${JSON.stringify(U('nested-blank'))})); return i;`,
  'docwrite': `const m = message('docwrite', 5000); const f = document.createElement('iframe'); document.body.append(f); f.contentDocument.write(${JSON.stringify(`<script>var r;try{r=open(${JSON.stringify(U('docwrite'))});r=r===null?"null":typeof r}catch(e){r="threw "+e}parent.postMessage({k:"docwrite",bridge:typeof __electrobunSendToHost,openNative:/native code/.test(Function.prototype.toString.call(open)),returned:r},"*")<\/script>`)}); f.contentDocument.close(); return await m;`,
  'child-docopen3': `const f = document.createElement('iframe'); document.body.append(f); let r; try { r = ret(f.contentDocument.open(${JSON.stringify(U('child-docopen3'))}, '', '')); } catch (e) { r = 'threw ' + e; } return { returned: r };`,
  'form-submit': `const fm = document.createElement('form'); fm.method = 'get'; fm.target = '_blank'; fm.action = ${JSON.stringify(U('form-submit'))}; document.body.append(fm); fm.submit(); return { submitted: true };`,
  'anchor-blank': `const a = document.createElement('a'); a.href = ${JSON.stringify(U('anchor-blank'))}; a.target = '_blank'; a.textContent = 'x'; document.body.append(a); a.click(); return { clicked: true };`,
  'anchor-shadow': `const host = document.createElement('div'); document.body.append(host); const root = host.attachShadow({ mode: 'closed' }); const a = document.createElement('a'); a.href = ${JSON.stringify(U('anchor-shadow'))}; a.target = '_blank'; a.textContent = 'x'; root.append(a); a.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true })); return { dispatched: true };`,
  'anchor-base': `const base = document.createElement('base'); base.target = '_blank'; document.head.append(base); const a = document.createElement('a'); a.href = ${JSON.stringify(U('anchor-base'))}; a.textContent = 'x'; document.body.append(a); a.click(); base.remove(); return { clicked: true };`,
  'main-docopen3': `let r; try { r = ret(document.open(${JSON.stringify(U('main-docopen3'))}, '', '')); } catch (e) { r = 'threw ' + e; } return { returned: r };`,
};

/** Starts the test variant de-elevated: a normal user's token at Medium integrity (safer.ps1), through a cmd file that sets its environment. */
function startDeElevated(name, env) {
  rmSync(REPORT, { force: true });
  writeFileSync(CONTROL, '');
  const all = { ELECTROBUN_CONSOLE: '1', FENNEC_DESKTOP_REPORT: REPORT, FENNEC_DESKTOP_CONTROL: CONTROL, ...env };
  const file = join(D, `spike-${name}.cmd`);
  const il = join(D, `spike-${name}-il.txt`);
  writeFileSync(file, ['@echo off', ...Object.entries(all).map(([k, v]) => `set "${k}=${v}"`), `whoami /groups > "${il}" 2>&1`, `"${EXE}" > "${join(D, 'logs', `spike-${name}.app.log`)}" 2>&1`, ''].join('\r\n'));
  const r = spawnSync(POWERSHELL, ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', join(HERE, 'scripts', 'spike', 'safer.ps1'), '-CommandLine', `cmd.exe /c "${file}"`], { encoding: 'utf8', timeout: 60000 });
  say(`${name}: started de-elevated`, `status ${r.status} pid ${r.stdout.trim()} ${r.stderr.trim().slice(0, 400)}`);
  return il;
}

async function config(name, { env = {}, deElevated = false }) {
  log(`\n\n========== ${name} (${deElevated ? 'de-elevated' : 'elevated, as the runner'}; ${JSON.stringify(env)})`);
  const before = hits.length;
  let il = null;
  if (deElevated) il = startDeElevated(name, env);
  else startApp(`spike-${name}`, env);
  try {
    await waitFor((l) => l.what === 'ready', 90000, 'ready');
  } catch (e) {
    say(`${name}: the app never said ready`, String(e).slice(0, 1500));
    return;
  }
  if (il) {
    await sleep(500);
    try {
      say(`${name}: the cmd's integrity`, (await import('node:fs')).readFileSync(il, 'utf8').split(/\r?\n/).filter((l) => /Mandatory|S-1-5-32-544/.test(l)).join('\n'));
    } catch (e) {
      say(`${name}: integrity not read`, String(e));
    }
  }
  cmd({ cmd: 'store-window', key: 'h', origin: H, token: 'spike-not-a-token' });
  await waitFor((l) => l.what === 'store-window-opened', 30000, 'the window');
  let title = '';
  for (let i = 0; i < 30 && title !== 'hostile'; i++) {
    try {
      title = (await evalIn('h', 'return document.title', 4000)).value;
    } catch {
      await sleep(500);
    }
  }
  say(`${name}: the hostile page's title`, title);

  const procs = webview2();
  const browser = procs.filter((p) => !/ --type=/.test(p.cl));
  say(`${name}: WebView2's browser process(es)`, browser.map((p) => `${p.pid}: ${p.cl.split(' --')[0]}\n  ${switches(p.cl).join('\n  ')}`).join('\n'));
  const renderer = procs.find((p) => / --type=renderer/.test(p.cl));
  if (renderer) say(`${name}: a renderer's switches`, switches(renderer.cl).join('\n'));

  const fetched = await evalIn(
    'h',
    `const out = {};
for (const [k, u] of [['secret', ${JSON.stringify(`${B}/secret`)}], ['cors', ${JSON.stringify(`${B}/cors`)}]]) {
  try { const r = await fetch(u); out[k] = 'read: ' + (await r.text()); } catch (e) { out[k] = 'refused: ' + e; }
}
try { const x = new XMLHttpRequest(); x.open('GET', ${JSON.stringify(`${B}/secret`)}, false); x.send(); out.xhr = 'read: ' + x.responseText; } catch (e) { out.xhr = 'refused: ' + e; }
const f = document.createElement('iframe'); f.src = ${JSON.stringify(`${B}/frame`)}; const l = new Promise((r) => f.onload = r); document.body.append(f); await l;
try { out.frameDom = 'read: ' + f.contentDocument.title; } catch (e) { out.frameDom = 'refused: ' + e; }
f.remove();
return out;`,
    30000,
  );
  say(`${name}: cross-origin reads from the page`, fetched);

  const results = {};
  for (const [k, js] of Object.entries(process.env.SPIKE_KINDS === '1' ? KINDS : {})) {
    try {
      const r = await evalIn('h', `${PROBE}\n${js}`, 15000);
      results[k] = r.ok ? r.value : `threw ${r.value}`;
    } catch (e) {
      results[k] = `no answer: ${String(e).split('\n')[0]}`;
    }
  }
  await sleep(4000);
  const popupHits = hits.slice(before).filter((u) => u.startsWith('/popup/')).map((u) => u.slice(7));
  const linkOuts = lines().filter((l) => l.what === 'page' && l.detail?.type === 'link-out').map((l) => l.detail.url.replace(`${H}/popup/`, ''));
  for (const k of Object.keys(KINDS)) results[k] = { ...(typeof results[k] === 'object' ? results[k] : { said: results[k] }), loadedInAWindow: popupHits.includes(k), linkOut: linkOuts.includes(k) };
  say(`${name}: each kind of request for a new window`, results);
  say(`${name}: WebView2's visible windows`, windowsOfWebview2());

  cmd({ cmd: 'quit' });
  const t0 = Date.now();
  while (Date.now() - t0 < 40000 && mine().length) await sleep(500);
  const left = mine();
  say(`${name}: left after quit (40 s)`, left.map((p) => `${p.pid} ${p.command.slice(0, 120)}`).join('\n') || 'none');
  for (const p of left) {
    try {
      process.kill(p.pid, 'SIGKILL');
    } catch {
      // gone
    }
  }
  const t1 = Date.now();
  while (Date.now() - t1 < 30000 && mine().length) await sleep(500);
}

// The data folder the runs share is made writable by the user's own SID, which a de-elevated token keeps (Administrators is deny-only in it).
const sid = spawnSync('whoami', ['/user', '/fo', 'csv', '/nh'], { encoding: 'utf8' }).stdout.trim().split(',').pop().replace(/"/g, '');
say('grant the user its own folders', spawnSync('icacls', [D, '/grant', `*${sid}:(OI)(CI)F`, '/T', '/Q'], { encoding: 'utf8' }).stdout.trim());

// The app takes every WEBVIEW2_ variable out of its environment (webview2.ts): started de-elevated
// (where WebView2 reads them) with two, its browser processes must show neither.
const ONE_STORE = join(D, 'one-store-for-all');
await config('basic-inherited', { deElevated: true, env: { WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: '--fennec-spike-marker', WEBVIEW2_USER_DATA_FOLDER: ONE_STORE } });
say('the folder WEBVIEW2_USER_DATA_FOLDER named', existsSync(ONE_STORE) ? 'made (the variable reached WebView2)' : 'never made');

appendFileSync(join(D, 'logs', 'spike-done'), 'done\n');
hostile.close();
other.close();
log('\nspike done');
process.exit(0);
