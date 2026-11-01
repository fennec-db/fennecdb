// A collection's schema: the statements that make it, its fields, its
// indexes over several fields, and the changes a person makes to it -- an
// index, one over several fields made or dropped, a field added, renamed or
// dropped, the collection dropped.
//
// Every change is shown before it runs, twice: the exact statement it will
// send, and what the server's declared-schema plan (`POST /_schema/plan`)
// says of the schema as it would be after it -- the statements the engine
// would write for it, or what it refuses to guess (a field gone is a drop
// or a rename? it does not say). A change that destroys data runs only
// once its name is typed. The statement goes through `/batch` under an
// `Idempotency-Key`, as the studio's other writes do, with the page's
// token: the server refuses a schema change to a scoped one.

import { h, fill } from './dom.js';
import * as S from './statements-views.js';
import { call, refused, statementsBlock, dialog } from './kit.js';
import { readInput, multiline } from './values.js';

/** An index as `/_schema` describes it, `{kind: 'hnsw', metric: 'cosine', m: 16}`, written as FennecQL names it. */
function indexText(ix) {
  if (!ix) return '';
  const { kind, ...rest } = ix;
  const args = Object.entries(rest).map(([k, v]) => (v === true ? k : `${k}=${v}`));
  return `@${kind}${args.length ? `(${args.join(', ')})` : ''}`;
}

export function mount(host, ctx) {
  const title = h('h1', { class: 'view-title' });
  const actions = h('div', { class: 'sc-actions' });
  const note = h('p', { class: 'sc-note' });
  const body = h('div', { class: 'sc-body' });
  fill(host, h('div', { class: 'sc' }, h('div', { class: 'sc-bar' }, h('div', { class: 'view-head' }, title, h('span', { class: 'view-count' }, 'schema')), actions), note, body));

  let name = null;
  let described = null;
  let texts = new Map();
  let reading = 0;
  // The database's named statements, as `namedStatements` read them.
  let named = null;

  /** Whether this token may change the schema: the server's own, or an open server. */
  const owner = () => ctx.state.who?.kind === 'full' || ctx.state.who?.kind === 'open';

  async function load(next) {
    name = next;
    const mine = ++reading;
    fill(title, name ?? 'No collection');
    fill(actions);
    if (!name) {
      const got = await namedStatements();
      if (mine !== reading) return;
      named = got;
      fill(body, h('p', { class: 'hint' }, 'Pick a collection on the left.'), statementsPart());
      return;
    }
    let schema;
    let text;
    let got;
    try {
      [schema, text, got] = await Promise.all([call(ctx, '/_schema'), call(ctx, '/_schema?as=fennecql'), namedStatements()]);
      if (!schema.ok) throw refused(schema);
      if (!text.ok) throw refused(text);
    } catch (e) {
      if (mine === reading) fill(body, h('p', { class: 'ed-error' }, ctx.explain(e)));
      return;
    }
    if (mine !== reading) return;
    described = schema.json;
    texts = S.createTexts(text.json?.fennecql);
    named = got;
    draw();
  }

  // ------------------------------------------------------------- named statements

  /**
   * The database's named statements as this token may know them: each row
   * of `statements` to the server's token -- its signature, whether it
   * writes, its comment and why it is broken -- and to a scoped one the
   * names `/_whoami` says it may call, `statements` being the server
   * token's. The database in the page holds none it can run.
   */
  async function namedStatements() {
    if (ctx.state.db.local) return { note: 'Named statements are a server’s: the database in this page is built without them.' };
    if (!owner()) return { names: ctx.state.who?.calls ?? [] };
    const q = S.listStatements();
    try {
      const r = await call(ctx, '/query', { method: 'POST', body: JSON.stringify({ query: q.text, params: q.params }) });
      if (!r.ok) throw refused(r);
      return { rows: Array.isArray(r.json) ? r.json : [] };
    } catch (e) {
      return { error: ctx.explain(e) };
    }
  }

  function statementsPart() {
    const head = h('h2', {}, 'Named statements');
    const hint = (text) => [head, h('p', { class: 'hint' }, text)];
    if (!named) return null;
    if (named.note) return hint(named.note);
    if (named.error) return [head, h('p', { class: 'ed-error' }, named.error)];
    const callBtn = (n, signature, broken = false) =>
      h('button', { type: 'button', class: 'btn ghost small', 'aria-label': `Call ${n}`, disabled: broken, onclick: () => callForm(n, signature) }, 'Call');
    let rows;
    if (named.names) {
      if (!named.names.length) return hint('Your token may call no named statement: a call line of the server’s policy grants one.');
      rows = named.names.map((n) => h('tr', {}, h('td', { class: 'mono' }, n), h('td', { class: 'muted' }, 'granted to your token'), h('td', { class: 'sc-row-actions' }, callBtn(n, null))));
    } else {
      if (!named.rows.length) return hint('The database holds none: create statement in the query editor defines one.');
      rows = named.rows.map((s) =>
        h(
          'tr',
          { dataset: { statement: s.name } },
          h('td', { class: 'mono' }, s.name, h('span', { class: 'sc-paths' }, s.writes ? 'writes' : 'reads')),
          h('td', { class: 'mono muted' }, s.signature ?? ''),
          h('td', {}, s.broken ? h('span', { class: 'ed-error' }, `Broken: ${s.broken}`) : (s.comment ?? '')),
          h(
            'td',
            { class: 'sc-row-actions' },
            callBtn(s.name, s.signature ?? '()', Boolean(s.broken)),
            h('button', { type: 'button', class: 'btn ghost small', 'aria-label': `The definition of ${s.name}`, onclick: () => definition(s.name) }, 'Definition'),
          ),
        ),
      );
    }
    return [head, h('table', { class: 'sc-fields sc-statements' }, h('tbody', {}, rows))];
  }

  /** A statement's definition, as `describe statement` gives it, and its replacement opened in the editor. */
  async function definition(n) {
    const q = S.describeStatement(n);
    let r;
    try {
      r = await call(ctx, '/query', { method: 'POST', body: JSON.stringify({ query: q.text, params: q.params }) });
      if (!r.ok) throw refused(r);
    } catch (e) {
      ctx.say(ctx.explain(e), 'error');
      return;
    }
    const s = (Array.isArray(r.json) ? r.json : [])[0] ?? {};
    const text = String(s.text ?? '');
    const d = dialog(
      `Statement ${n}`,
      statementsBlock(text.split('\n')),
      s.broken ? h('p', { class: 'form-error' }, `Broken: ${s.broken}`) : null,
      h('p', { class: 'hint' }, `Answers ${(s.answers ?? []).join(', ') || 'no field'}; reads ${(s.depends ?? []).join(', ') || 'nothing'}.`),
      h(
        'div',
        { class: 'dialog-actions' },
        h('button', { type: 'button', class: 'btn', onclick: () => d.close() }, 'Close'),
        h('button', { type: 'button', class: 'btn primary', onclick: () => (d.close(), ctx.query(S.replacing(text))) }, 'Replace it in the query editor'),
      ),
    );
    d.classList.add('wide');
  }

  /**
   * The call form: an input for each argument the signature names, typed
   * as it is, or -- to a token not shown the signature -- the arguments as
   * a JSON object; the statement it sends written out below them. It runs
   * in the query editor, its answer in the grid, under an `Idempotency-Key`
   * as the studio's writes are.
   */
  function callForm(n, signature) {
    const params = signature === null ? null : S.signatureParams(signature).params;
    const inputs = new Map();
    const statement = h('div');
    const problem = h('p', { class: 'form-error', role: 'alert' });
    let current = null;
    const fields = (params ?? [{ name: null, type: 'json' }]).map((p) => {
      const id = `sc-arg-${p.name ?? 'all'}`;
      const multi = multiline(p.type);
      const input = h(multi ? 'textarea' : 'input', { id, class: 'mono', rows: multi ? 3 : undefined, autocomplete: 'off', placeholder: p.name ? (p.optional ? 'its default when empty' : '') : '{"from": 1}' });
      inputs.set(p.name, input);
      const label = p.name ? [h('span', { class: 'form-name' }, `$${p.name}`), h('span', { class: 'form-type' }, p.optional ? `${p.type}, optional` : p.type)] : [h('span', { class: 'form-name' }, 'Arguments'), h('span', { class: 'form-type' }, 'a JSON object')];
      return h('div', { class: 'form-row' }, h('label', { for: id }, ...label), input);
    });
    const update = () => {
      try {
        let args = {};
        if (params) {
          for (const p of params) {
            const text = inputs.get(p.name).value;
            if (p.optional && text.trim() === '') continue;
            args[p.name] = readInput(text, p.type, `$${p.name}`);
          }
        } else {
          try {
            args = JSON.parse(inputs.get(null).value.trim() || '{}');
          } catch {
            throw new S.StatementError('the arguments are a JSON object: {"from": 1}');
          }
        }
        current = S.callStatement(n, args);
        fill(statement, statementsBlock([current.text]), h('pre', { class: 'statement' }, `$1 = ${JSON.stringify(current.params[0])}`));
        fill(problem);
      } catch (e) {
        current = null;
        fill(statement);
        fill(problem, ctx.explain(e));
      }
      go.disabled = !current;
    };
    const go = h('button', { type: 'submit', class: 'btn primary' }, 'Call');
    const form = h(
      'form',
      {
        class: 'form',
        oninput: update,
        onsubmit: (e) => {
          e.preventDefault();
          if (!current) return;
          d.close();
          ctx.query(current.text, JSON.stringify(current.params), true, crypto.randomUUID());
        },
      },
      fields,
      h('h3', {}, 'The statement'),
      statement,
      problem,
      h('div', { class: 'dialog-actions' }, h('button', { type: 'button', class: 'btn', onclick: () => d.close() }, 'Cancel'), go),
    );
    const d = dialog(`Call ${n}`, form);
    update();
    form.querySelector('input, textarea')?.focus();
  }

  function collection() {
    return described?.collections?.find((c) => c.name === name) ?? null;
  }

  function draw() {
    const c = collection();
    const lines = texts.get(name) ?? [];
    fill(
      note,
      owner()
        ? 'Each change shows the statement it sends and the declared-schema plan of it before it runs.'
        : 'Your token reads this schema. Changing it takes the server’s token.',
    );
    if (!c) {
      fill(body, h('p', { class: 'hint' }, `${name} is the database's own collection, or your token does not read it.`), statementsPart());
      return;
    }
    if (owner()) {
      fill(
        actions,
        h('button', { type: 'button', class: 'btn', onclick: () => change('index') }, 'Add index'),
        h('button', { type: 'button', class: 'btn', onclick: () => change('composite') }, 'Add index over fields'),
        h('button', { type: 'button', class: 'btn', onclick: () => change('add') }, 'Add field'),
        h('button', { type: 'button', class: 'btn ghost danger', onclick: () => change('drop') }, 'Drop collection'),
      );
    }
    const copy = h(
      'button',
      {
        type: 'button',
        class: 'btn ghost small',
        onclick: () => navigator.clipboard?.writeText(lines.join('\n')).then(() => ctx.say('Copied the statements.'), () => ctx.say('The browser did not allow the copy.', 'error')),
      },
      'Copy',
    );
    const rows = c.fields.map((f) => {
      const kinds = S.indexKinds(f.type);
      const idx = f.index ? indexText(f.index) : null;
      const paths = (f.paths ?? []).map((p) => `${f.name}.${p.path} ${indexText(p.index)}`);
      return h(
        'tr',
        { dataset: { field: f.name } },
        h('td', { class: 'mono' }, f.name),
        h('td', { class: 'mono muted' }, f.type, f.collate ? ` collate ${f.collate}` : '', f.nocase ? ' nocase' : '', f.required ? ' required' : ''),
        h('td', { class: 'mono sun' }, idx ?? '', paths.length ? h('span', { class: 'sc-paths' }, paths.join(', ')) : null),
        h(
          'td',
          { class: 'sc-row-actions' },
          owner()
            ? [
                !f.index && kinds.length ? h('button', { type: 'button', class: 'btn ghost small', 'aria-label': `Add an index on ${f.name}`, onclick: () => change('index', f.name) }, 'Index') : null,
                h('button', { type: 'button', class: 'btn ghost small', 'aria-label': `Rename ${f.name}`, onclick: () => change('rename', f.name) }, 'Rename'),
                h('button', { type: 'button', class: 'btn ghost small danger', 'aria-label': `Drop ${f.name}`, onclick: () => change('drop-field', f.name) }, 'Drop'),
              ]
            : null,
        ),
      );
    });
    // The indexes over several fields, each as `create collection` writes it.
    const composites = (c.indexes ?? []).map((ix) => {
      const fields = ix.keys.map((k) => k.field);
      const text = `@${ix.kind}(${ix.keys.map((k) => (k.desc ? `${k.field} desc` : k.field)).join(', ')})`;
      return h(
        'tr',
        {},
        h('td', { class: 'mono sun' }, text),
        h(
          'td',
          { class: 'sc-row-actions' },
          owner() ? h('button', { type: 'button', class: 'btn ghost small danger', 'aria-label': `Drop ${text}`, onclick: () => change('drop-index', fields) }, 'Drop') : null,
        ),
      );
    });
    fill(
      body,
      h('div', { class: 'sc-head' }, h('h2', {}, 'As FennecQL'), copy, h('button', { type: 'button', class: 'btn ghost small', onclick: () => ctx.query(`get ${name} limit 100`) }, 'Query it')),
      statementsBlock(lines, 'statement q-block sc-create'),
      h('h2', {}, 'Fields'),
      h(
        'table',
        { class: 'sc-fields' },
        h('thead', {}, h('tr', {}, h('th', {}, 'Field'), h('th', {}, 'Type'), h('th', {}, 'Index'), h('th', {}, h('span', { class: 'sr' }, 'Changes')))),
        h('tbody', {}, h('tr', {}, h('td', { class: 'mono' }, 'id'), h('td', { class: 'mono muted' }, 'int'), h('td', { class: 'mono muted' }, 'its own'), h('td')), rows),
      ),
      composites.length
        ? [
            h('h2', {}, 'Indexes over several fields'),
            h('table', { class: 'sc-fields sc-composites' }, h('thead', {}, h('tr', {}, h('th', {}, 'Index'), h('th', {}, h('span', { class: 'sr' }, 'Changes')))), h('tbody', {}, composites)),
          ]
        : null,
      statementsPart(),
    );
  }

  // ------------------------------------------------------------- a change

  /**
   * The dialog for one change: its inputs, the statement they make, the
   * plan of it, and -- for a drop -- the name to type. `op` is `index`,
   * `composite`, `drop-index`, `add`, `rename`, `drop-field` or `drop`;
   * `field` a field, or a dropped index's fields.
   */
  function change(op, field = null) {
    const c = collection();
    if (!c) return;
    const inputs = h('div', { class: 'form-fields sc-form' });
    const statement = h('div', { class: 'sc-statement' });
    const plan = h('div', { class: 'sc-plan', 'aria-live': 'polite' });
    const problem = h('p', { class: 'form-error', role: 'alert' });
    const destructive = op === 'drop' || op === 'drop-field';
    const confirm = h('input', { id: 'sc-confirm', type: 'text', spellcheck: 'false', autocomplete: 'off' });
    const confirmRow = h('div', { class: 'sc-confirm' }, h('label', { for: 'sc-confirm' }));
    const apply = h(
      'button',
      { type: 'submit', class: `btn ${destructive ? 'danger' : 'primary'}`, disabled: true },
      { index: 'Create index', composite: 'Create index', 'drop-index': 'Drop index', add: 'Add field', rename: 'Rename field', 'drop-field': 'Drop field', drop: 'Drop collection' }[op],
    );
    const box = (id, label, el, hint) => h('div', { class: 'form-row' }, h('label', { for: id }, h('span', { class: 'form-name' }, label), hint ? h('span', { class: 'form-type' }, hint) : null), el);
    const select = (id, options, value) => {
      const s = h('select', { id }, options.map((o) => (Array.isArray(o) ? h('option', { value: o[0], selected: o[0] === value }, o[1]) : h('option', { value: o, selected: o === value }, o))));
      return s;
    };
    const fieldsWith = (pred) => c.fields.filter(pred).map((f) => [f.name, `${f.name}  ${f.type}`]);

    // The inputs each change takes, built again when what they offer
    // depends on another (an index's options on its kind).
    const v = {};
    const build = () => {
      const rows = [];
      if (op === 'index') {
        const can = c.fields.filter((f) => !f.index && S.indexKinds(f.type).length);
        v.field ??= field ?? can[0]?.name ?? null;
        const f = c.fields.find((x) => x.name === v.field);
        rows.push(box('sc-field', 'Field', (v.fieldEl = select('sc-field', can.map((x) => [x.name, `${x.name}  ${x.type}`]), v.field))));
        const kinds = f ? S.indexKinds(f.type) : [];
        if (!kinds.includes(v.kind)) v.kind = kinds[0] ?? null;
        rows.push(box('sc-kind', 'Index', (v.kindEl = select('sc-kind', kinds.map((k) => [k, `@${k}`]), v.kind))));
        rows.push(...options(v.kind));
        if (!can.length) rows.push(h('p', { class: 'hint' }, 'Every field that takes an index has one.'));
      } else if (op === 'composite') {
        rows.push(box('sc-keys', 'Fields', (v.keysEl = h('input', { id: 'sc-keys', type: 'text', value: v.keys ?? '', placeholder: 'owner, created desc', spellcheck: 'false', autocomplete: 'off' })), 'in order, each asc or desc'));
        rows.push(box('sc-kind', 'Index', (v.kindEl = select('sc-kind', [['sorted', '@sorted'], ['unique', '@unique']], v.kind ?? 'sorted'))));
        rows.push(h('p', { class: 'hint' }, 'An equality on its first fields and a range or an order on the next one read a run of it, and a limit stops there.'));
      } else if (op === 'drop-index') {
        rows.push(h('p', { class: 'hint' }, `The index on (${field.join(', ')}) is taken off. No row changes; a read it answered is answered by another index or the scan.`));
      } else if (op === 'add') {
        rows.push(box('sc-name', 'Name', (v.nameEl = h('input', { id: 'sc-name', type: 'text', value: v.name ?? '', spellcheck: 'false', autocomplete: 'off' }))));
        const types = h('datalist', { id: 'sc-types' }, ['text', 'int', 'float', 'bool', 'timestamp', 'json', 'bytes', '[text]', 'vector<768>', 'sparse<30522>', 'geo'].map((t) => h('option', { value: t })));
        rows.push(box('sc-type', 'Type', h('div', {}, (v.typeEl = h('input', { id: 'sc-type', type: 'text', value: v.type ?? 'text', list: 'sc-types', spellcheck: 'false', autocomplete: 'off' })), types)));
        let kinds = [];
        try {
          kinds = S.indexKinds(S.typeText(v.type ?? 'text'));
        } catch {
          /* said below, with the statement */
        }
        if (v.kind && !kinds.includes(v.kind)) v.kind = '';
        rows.push(box('sc-kind', 'Index', (v.kindEl = select('sc-kind', [['', 'none'], ...kinds.map((k) => [k, `@${k}`])], v.kind ?? ''))));
        rows.push(...options(v.kind));
        if ((v.type ?? 'text').trim() === 'text') {
          rows.push(box('sc-collate', 'Collation', (v.collateEl = select('sc-collate', [['', 'bytes'], ['und', 'und, ICU’s root'], ['tr', 'tr, Turkish']], v.collate ?? ''))));
          rows.push(box('sc-nocase', 'Case', h('label', { class: 'sc-check' }, (v.nocaseEl = h('input', { id: 'sc-nocase', type: 'checkbox', checked: !!v.nocase })), ' compared by its case fold (nocase)')));
        }
      } else if (op === 'rename') {
        v.field ??= field ?? c.fields[0]?.name;
        rows.push(box('sc-field', 'Field', (v.fieldEl = select('sc-field', fieldsWith(() => true), v.field))));
        rows.push(box('sc-name', 'New name', (v.nameEl = h('input', { id: 'sc-name', type: 'text', value: v.name ?? '', spellcheck: 'false', autocomplete: 'off' }))));
      } else if (op === 'drop-field') {
        v.field ??= field ?? c.fields[0]?.name;
        rows.push(box('sc-field', 'Field', (v.fieldEl = select('sc-field', fieldsWith(() => true), v.field))));
        rows.push(h('p', { class: 'sc-warn' }, 'Its values are gone from every read at once, and from the file at the next compact. Its index goes with it.'));
      } else {
        rows.push(h('p', { class: 'sc-warn' }, `Every row of ${c.name}, its indexes and its schema are deleted. This cannot be undone.`));
      }
      fill(inputs, rows);
    };
    const options = (kind) => {
      if (kind === 'ttl') return [box('sc-ttl', 'Expires after', (v.ttlEl = h('input', { id: 'sc-ttl', type: 'text', value: v.ttl ?? '30d', spellcheck: 'false', autocomplete: 'off' })), '30s, 15m, 12h, 7d')];
      if (kind === 'text') {
        return [
          box('sc-prefix', 'Prefixes', (v.prefixEl = h('input', { id: 'sc-prefix', type: 'text', inputmode: 'numeric', value: v.prefix ?? '', placeholder: 'off', spellcheck: 'false', autocomplete: 'off' })), 'up to N characters, for inflected words'),
          box('sc-chars', 'Characters', h('label', { class: 'sc-check' }, (v.charsEl = h('input', { id: 'sc-chars', type: 'checkbox', checked: !!v.chars })), ' each character of Han, kana and Hangul')),
        ];
      }
      if (kind === 'hnsw') {
        return [
          box('sc-metric', 'Metric', (v.metricEl = select('sc-metric', ['cosine', 'l2', 'dot'], v.metric ?? 'cosine'))),
          box('sc-quant', 'Codes', (v.quantEl = select('sc-quant', [['none', 'full vectors'], ['int8', 'int8'], ['bit', 'bit (cosine)']], v.quant ?? 'none'))),
        ];
      }
      return [];
    };
    /** What the inputs say now, read into `v`. */
    const read = () => {
      for (const k of ['field', 'kind', 'name', 'type', 'ttl', 'prefix', 'metric', 'quant', 'collate', 'keys']) {
        const el = v[`${k}El`];
        if (el && el.isConnected) v[k] = el.value;
      }
      if (v.charsEl?.isConnected) v.chars = v.charsEl.checked;
      if (v.nocaseEl?.isConnected) v.nocase = v.nocaseEl.checked;
    };
    const spec = () => (v.kind ? { kind: v.kind, ttl: v.ttl, prefix: v.prefix, chars: v.chars, metric: v.metric, quant: v.quant } : null);
    /** The statement, and the change the plan is asked of. */
    const made = () => {
      switch (op) {
        case 'index':
          if (!v.field) throw new S.StatementError('no field here takes an index');
          return [S.createIndex(c.name, v.field, spec()), { op, collection: c.name, field: v.field, index: spec() }];
        case 'add':
          return [
            S.addField(c.name, v.name ?? '', v.type ?? '', { index: spec(), collate: v.collate || null, nocase: !!v.nocase }),
            { op, collection: c.name, field: v.name, type: v.type, index: spec(), collate: v.collate || null, nocase: !!v.nocase },
          ];
        case 'composite': {
          const keys = S.compositeKeys(v.keys ?? '');
          const unique = v.kind === 'unique';
          return [S.createComposite(c.name, keys, unique), { op, collection: c.name, keys, unique }];
        }
        case 'drop-index':
          return [S.dropComposite(c.name, field), { op, collection: c.name, fields: field }];
        case 'rename':
          return [S.renameField(c.name, v.field, v.name ?? ''), { op, collection: c.name, from: v.field, to: v.name }];
        case 'drop-field':
          return [S.dropField(c.name, v.field), { op, collection: c.name, field: v.field }];
        default:
          return [S.dropCollection(c.name), { op, collection: c.name }];
      }
    };
    const must = () => (op === 'drop' ? c.name : op === 'drop-field' ? v.field : null);

    let current = null;
    let planned = false;
    let timer;
    let asked = 0;
    let shown = null;
    const update = () => {
      read();
      let stmt;
      let ch;
      let body;
      try {
        [stmt, ch] = made();
        body = JSON.stringify(S.planned(described, ch));
      } catch (e) {
        shown = null;
        planned = false;
        current = null;
        fill(statement, h('p', { class: 'form-error' }, ctx.explain(e)));
        fill(plan);
        refresh();
        return;
      }
      // The same change as the one shown keeps its plan: asked again, the
      // button that applies it would wait for the answer once more.
      if (shown === stmt.text + body) return refresh();
      shown = stmt.text + body;
      fill(problem);
      planned = false;
      current = stmt;
      fill(statement, statementsBlock([stmt.text]));
      const need = must();
      confirmRow.hidden = !need;
      if (need) fill(confirmRow.firstChild, 'Type ', h('code', {}, need), ' to confirm');
      clearTimeout(timer);
      const mine = ++asked;
      fill(plan, h('p', { class: 'hint' }, 'Asking for the plan…'));
      timer = setTimeout(async () => {
        let r;
        try {
          r = await call(ctx, '/_schema/plan', { method: 'POST', body });
        } catch (e) {
          r = { ok: false, status: 0, json: { error: e.message }, text: e.message };
        }
        if (mine !== asked) return;
        fill(plan, planOf(r));
        planned = true;
        refresh();
      }, 200);
      refresh();
    };
    const refresh = () => {
      const need = must();
      apply.disabled = !current || !planned || (need !== null && confirm.value !== need);
    };

    const form = h(
      'form',
      {
        class: 'form',
        oninput: (e) => (e.target === confirm ? refresh() : update()),
        onchange: (e) => {
          // A choice that changes what the others offer draws them again.
          // Nothing else: a text box's change comes as it loses the focus,
          // to the very button that applies it, and asking the plan again
          // there held the button back.
          if (['sc-field', 'sc-kind', 'sc-type'].includes(e.target.id)) {
            read();
            build();
            update();
          }
        },
        onsubmit: async (e) => {
          e.preventDefault();
          if (apply.disabled || !current) return;
          apply.disabled = true;
          try {
            await ctx.state.db.batch([[current.text, current.params]], { idempotencyKey: crypto.randomUUID() });
          } catch (err) {
            fill(problem, ctx.explain(err));
            refresh();
            return;
          }
          d.close();
          ctx.say(`Ran: ${current.text}`);
          await ctx.refresh();
          if (op === 'drop') await load(ctx.state.current?.name ?? null);
          else await load(name);
        },
      },
      inputs,
      h('h3', {}, 'The statement'),
      statement,
      h('h3', {}, 'The declared-schema plan'),
      plan,
      confirmRow,
      problem,
      h('div', { class: 'dialog-actions' }, h('button', { type: 'button', class: 'btn', onclick: () => d.close() }, 'Cancel'), apply),
    );
    const titles = {
      index: `Add an index to ${c.name}`,
      composite: `Add an index over several fields of ${c.name}`,
      'drop-index': `Drop an index of ${c.name}`,
      add: `Add a field to ${c.name}`,
      rename: `Rename a field of ${c.name}`,
      'drop-field': `Drop a field of ${c.name}`,
      drop: `Drop ${c.name}?`,
    };
    const d = dialog(titles[op], form);
    d.classList.add('wide');
    confirmRow.append(confirm);
    build();
    update();
    (inputs.querySelector('input, select') ?? confirm).focus();
  }

  /** `/_schema/plan`'s answer in words: what it would write, and what it will not guess. */
  function planOf(r) {
    if (!r.ok) return h('p', { class: 'form-error' }, `The plan was refused (HTTP ${r.status}): ${r.json?.error ?? r.text}`);
    const p = r.json ?? {};
    const out = [];
    if (p.statements?.length) {
      out.push(h('p', { class: 'hint' }, 'The engine would bring the schema to it with:'), statementsBlock(p.statements));
    }
    for (const x of p.refusals ?? []) {
      out.push(h('div', { class: 'sc-refusal' }, h('p', {}, h('b', {}, x.kind.replace(/_/g, ' ')), x.field ? ` (${x.field})` : '', `: ${x.message}`), x.fix ? h('p', { class: 'hint' }, x.fix) : null));
    }
    if (!out.length) out.push(h('p', { class: 'hint' }, 'Nothing: a collection the description leaves out is one the plan leaves alone.'));
    return out;
  }

  return {
    open(next) {
      if (next !== name || !described) load(next);
    },
    focus: () => host.querySelector('button, [tabindex]')?.focus(),
    hide() {},
    close() {},
    /** For a schema changed elsewhere. */
    reload: () => load(name),
  };
}
