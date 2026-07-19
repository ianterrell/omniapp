/* OmniApp admin — vanilla single-page app.
 *
 * Architecture:
 *   boot()               loads /api/project, applies theme, builds nav, starts router
 *   router()             parses location.hash → dispatches to a page renderer
 *   page renderers       renderViewPage / showRecord / showEdit / showCreate / resolveBy
 *   renderView()         dispatches on view.type → one renderer per type (table, board, …)
 *
 * A monotonically increasing request token (reqToken) guards against stale async
 * renders when the user navigates while a fetch is in flight. Every routed render
 * captures the token and bails after each await if a newer render has started.
 *
 * Record presentation is driven by the display DSL: models carry named blocks
 * of layout nodes (grid/stack/card/section/field/resource/outputs) rendered by
 * renderNode(). The `detail` block lays out the record page; other blocks
 * render records wherever they appear in lists (view display.item → the
 * model's `card` block → the built-in recordCard heuristic).
 */

'use strict';

const state = { project: null, view: null, recordsByModel: {} };
let reqToken = 0;

const $ = s => document.querySelector(s);
const esc = v => String(v ?? '').replace(/[&<>"']/g, c => (
  { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

async function api(url, options) {
  const r = await fetch(url, options);
  if (!r.ok) { let x; try { x = await r.json(); } catch {} throw new Error(x?.error || `${r.status} ${r.statusText}`); }
  return r.status === 204 ? null : r.json();
}

/* =========================================================================
 * Theme — derive the sidebar palette from three config base colors.
 * ====================================================================== */

function hexToRgb(h) {
  h = String(h).replace('#', '');
  if (h.length === 3) h = h.split('').map(c => c + c).join('');
  return [parseInt(h.slice(0, 2), 16), parseInt(h.slice(2, 4), 16), parseInt(h.slice(4, 6), 16)];
}
function rgbToHex(c) {
  return '#' + c.map(x => Math.round(Math.max(0, Math.min(255, x))).toString(16).padStart(2, '0')).join('');
}
function mix(a, b, t) { return a.map((x, i) => x + (b[i] - x) * t); } // t: 0→a, 1→b
function luminance(c) {
  const [r, g, b] = c.map(v => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function applyTheme(theme) {
  const white = [255, 255, 255], black = [0, 0, 0];
  const accent = hexToRgb(theme?.accent || '#245c47');
  const sidebar = hexToRgb(theme?.sidebar || '#17231e');
  const paper = hexToRgb(theme?.background || '#f6f7f4');
  const dark = luminance(sidebar) < 0.4; // dark sidebar → light text, and vice-versa
  const toward = dark ? white : black;
  const set = (k, v) => document.documentElement.style.setProperty(k, v);
  set('--accent', rgbToHex(accent));
  set('--accent-soft', rgbToHex(mix(accent, white, 0.85)));
  set('--paper', rgbToHex(paper));
  set('--sidebar', rgbToHex(sidebar));
  set('--sidebar-ink', rgbToHex(mix(sidebar, toward, dark ? 0.94 : 0.88)));
  set('--sidebar-muted', rgbToHex(mix(sidebar, toward, dark ? 0.5 : 0.5)));
  set('--sidebar-hover', rgbToHex(mix(sidebar, toward, dark ? 0.12 : 0.09)));
  set('--sidebar-mark', rgbToHex(mix(sidebar, toward, 0.42)));
  set('--sidebar-mark-ink', rgbToHex(mix(mix(sidebar, accent, 0.5), toward, dark ? 0.62 : 0.55)));
}

/* =========================================================================
 * Boot + navigation
 * ====================================================================== */

// Map of viewName → { index, label, views: [...] } for views that live in a
// nav group, so the router can render the group's tab strip + highlight.
const viewGroups = new Map();
// Ordered list of { label, target, key } describing the sidebar buttons.
let navButtons = [];

async function boot() {
  try {
    state.project = await api('/api/project');
  } catch (e) { fatal(e.message); return; }
  const cfg = state.project.config;
  document.title = cfg.name || 'OmniApp';
  $('#brandMark').textContent = (cfg.name || 'O').trim().charAt(0) || 'O';
  $('#brandName').textContent = cfg.name || 'OmniApp';
  $('#projectDesc').textContent = cfg.description || '';
  applyTheme(cfg.theme);
  buildNav();
  buildRouteTables();
  window.addEventListener('hashchange', router);
  // Delegated handlers for display-DSL dynamics (copy buttons, checklists,
  // expandable resource rows).
  document.addEventListener('click', e => {
    const button = e.target.closest('.copy-btn');
    if (button) { e.preventDefault(); handleCopyClick(button); return; }
    // Expandable rows toggle anywhere on the row except real links.
    const row = e.target.closest('tr.expand-row');
    if (row && !e.target.closest('a')) {
      e.preventDefault();
      const detail = row.nextElementSibling;
      if (!detail?.classList.contains('expand-detail')) return;
      const toggle = row.querySelector('.expand-toggle');
      const open = detail.hidden;
      detail.hidden = !open;
      if (toggle) {
        toggle.textContent = open ? '▾' : '▸';
        toggle.setAttribute('aria-expanded', String(open));
      }
    }
  });
  document.addEventListener('change', e => {
    const box = e.target.closest('.check-toggle');
    if (box) handleCheckToggle(box);
  });
  if (!defaultRoute()) {
    setHeader(cfg.name || 'OmniApp', '', '');
    $('#page').innerHTML = '<div class="empty">No models or views are defined yet.</div>';
    return;
  }
  if (!location.hash || location.hash === '#' || location.hash === '#/') {
    history.replaceState(null, '', '#' + defaultRoute());
  }
  router();
}

// Resolve a view by route name. Synthetic table views over a model are
// addressable as "@ModelName" so models without any view stay routable.
function resolveView(name) {
  if (state.project.views[name]) return state.project.views[name];
  if (name && name[0] === '@') {
    const model = state.project.models[name.slice(1)];
    if (model) return synthView(model);
  }
  return null;
}
function synthView(model) {
  return {
    name: '@' + model.name, model: model.name, type: 'table',
    fields: Object.keys(model.fields), query: { order: [], page_size: 50 },
    __synthetic: true,
  };
}

// Build the sidebar buttons + the viewGroups index from config.navigation.
// If navigation is empty, list every view flat (else every model synthesized).
function buildNav() {
  const { config, views, models } = state.project;
  viewGroups.clear();
  navButtons = [];
  const nav = config.navigation || [];

  if (nav.length) {
    nav.forEach((item, index) => {
      if (item.views && item.views.length) {
        const label = item.label || 'Group';
        navButtons.push({ label, target: '#/views/' + encodeURIComponent(item.views[0]), key: 'g' + index });
        item.views.forEach(v => viewGroups.set(v, { index, label, views: item.views }));
      } else if (item.view) {
        const v = views[item.view];
        const label = item.label || v?.label || item.view;
        navButtons.push({ label, target: '#/views/' + encodeURIComponent(item.view), key: 's' + index });
      }
    });
  } else if (Object.keys(views).length) {
    for (const v of Object.values(views)) {
      navButtons.push({ label: v.label || v.name, target: '#/views/' + encodeURIComponent(v.name), key: v.name });
    }
  } else {
    for (const m of Object.values(models)) {
      navButtons.push({ label: m.label || m.name, target: '#/views/@' + encodeURIComponent(m.name), key: '@' + m.name });
    }
  }

  const el = $('#nav');
  el.innerHTML = '';
  for (const b of navButtons) {
    const btn = document.createElement('button');
    btn.textContent = b.label;
    btn.dataset.key = b.key;
    btn.onclick = () => { location.hash = b.target; };
    el.append(btn);
  }
}

function defaultRoute() {
  const { config, views, models } = state.project;
  const nav = config.navigation || [];
  for (const item of nav) {
    if (item.views && item.views.length) return 'views/' + encodeURIComponent(item.views[0]);
    if (item.view) return 'views/' + encodeURIComponent(item.view);
  }
  const first = Object.keys(views)[0];
  if (first) return 'views/' + encodeURIComponent(first);
  const model = Object.keys(models)[0];
  if (model) return 'views/@' + encodeURIComponent(model);
  return null;
}

// Highlight the sidebar button owning the active view (group or single).
function highlightNav(viewName) {
  const group = viewGroups.get(viewName);
  let activeKey = null;
  if (group) activeKey = 'g' + group.index;
  else for (const b of navButtons) {
    if (b.target === '#/views/' + encodeURIComponent(viewName)) { activeKey = b.key; break; }
  }
  document.querySelectorAll('#nav button').forEach(btn =>
    btn.classList.toggle('active', btn.dataset.key === activeKey));
}

// Render the horizontal group tab strip above the header, if the active view
// belongs to a nav group; otherwise clear it.
function renderSubnav(viewName) {
  const strip = $('#subnav');
  const group = viewGroups.get(viewName);
  if (!group) { strip.innerHTML = ''; return; }
  strip.innerHTML = group.views.map(v => {
    const view = state.project.views[v];
    const label = view?.label || v;
    const active = v === viewName ? ' active' : '';
    return `<a class="tab${active}" href="#/views/${encodeURIComponent(v)}">${esc(label)}</a>`;
  }).join('');
}

/* =========================================================================
 * Router
 * ====================================================================== */

/* Nested-route tables (Rails-style): a root model's `route` claims a first
 * segment; a nested model's `route` claims a segment under its parent. */
const rootRoutes = new Map();   // 'books' → 'Book'
const childRoutes = new Map();  // 'Book' → Map('docs' → 'BookDoc')

function buildRouteTables() {
  rootRoutes.clear();
  childRoutes.clear();
  for (const m of Object.values(state.project.models)) {
    if (!m.route) continue;
    if (!m.parent) { rootRoutes.set(m.route, m.name); continue; }
    const ref = m.fields[m.parent]?.reference;
    if (!ref) continue;
    if (!childRoutes.has(ref.model)) childRoutes.set(ref.model, new Map());
    childRoutes.get(ref.model).set(m.route, m.name);
  }
}

function router() {
  const raw = location.hash.replace(/^#\/?/, '');
  const [pathPart, queryPart] = raw.split('?');
  const seg = pathPart.split('/').filter(s => s.length); // raw, still encoded
  const query = new URLSearchParams(queryPart || '');
  const dec = s => { try { return decodeURIComponent(s); } catch { return s; } };

  if (!seg.length) { const d = defaultRoute(); if (d) { location.hash = d; } return; }

  if (rootRoutes.has(dec(seg[0]))) {
    routeNested(seg.map(dec));
    return;
  }

  if (seg[0] === 'views') {
    const name = dec(seg[1] || '');
    // `f.<field>=<value>` params are runtime equality filters on the view.
    const filters = {};
    for (const [k, v] of query.entries()) if (k.startsWith('f.')) filters[k.slice(2)] = v;
    renderViewPage(name, { page: parseInt(query.get('page'), 10) || 1, q: query.get('q') || '', filters });
    return;
  }
  if (seg[0] === 'records') {
    const model = dec(seg[1] || '');
    if (seg[2] === 'new') { showCreate(model, query); return; }
    if (seg[2] === 'by') { resolveBy(model, dec(seg[3] || ''), dec(seg[4] || '')); return; }
    const key = dec(seg[2] || '');
    if (seg[3] === 'edit') { showEdit(model, key); return; }
    showRecord(model, key);
    return;
  }
  fatal('Unknown route: ' + esc(raw));
}

// Walk a nested route: /books/rude/docs/idea[/tab-or-edit]. Segments resolve
// records pairwise (route + identity); one trailing segment is a tab of the
// final record, or `edit`. A record below a tabbed ancestor renders inside
// that ancestor's tab (header + tab bar stay put).
async function routeNested(segs) {
  const t = ++reqToken;
  setHeader('Loading…', '', '');
  $('#page').innerHTML = '<div class="loading">Loading record…</div>';
  try {
    if (segs.length < 2) { fatal('Missing record identifier.'); return; }
    let modelName = rootRoutes.get(segs[0]);
    let record = await findByIdentity(modelName, null, segs[1]);
    const chain = [{ modelName, record, route: segs[0] }];
    let i = 2;
    let tab = null;
    while (i < segs.length) {
      const s = segs[i];
      const model = state.project.models[modelName];
      if (i === segs.length - 1) {
        if (s === 'edit') { if (t === reqToken) showEdit(modelName, record.key); return; }
        const match = (model.tabs || []).find(tb => tb_route(tb) === s);
        if (match) { tab = tb_route(match); i += 1; continue; }
      }
      const childModel = childRoutes.get(modelName)?.get(s);
      if (!childModel || i + 1 >= segs.length) { fatal(`Unknown route segment “${esc(s)}”.`); return; }
      record = await findByIdentity(childModel, modelName, segs[i + 1], record);
      modelName = childModel;
      chain.push({ modelName, record, route: s });
      i += 2;
    }
    if (t !== reqToken) return;
    // The outermost tabbed ancestor whose child segment matches one of its
    // tabs anchors the page; anything deeper renders inside that tab.
    for (let a = 0; a < chain.length - 1; a++) {
      const anchorModel = state.project.models[chain[a].modelName];
      const childRoute = chain[a + 1].route;
      if (anchorModel.tabs?.some(tb => tb_route(tb) === childRoute)) {
        showRecordInTab(chain, a, childRoute, t);
        return;
      }
    }
    showRecord(modelName, record.key, record, tab);
  } catch (e) {
    if (t !== reqToken) return;
    fatal(e.message);
  }
}

// A child record shown inside its ancestor's tab: the ancestor's title and
// tab bar stay, and a within-tab breadcrumb trail sits under the tabs.
async function showRecordInTab(chain, anchorIndex, tabRoute, token) {
  const t = token ?? ++reqToken;
  const models = state.project.models;
  const anchor = chain[anchorIndex];
  const tail = chain.slice(anchorIndex + 1);
  const leaf = tail[tail.length - 1];
  const anchorModel = models[anchor.modelName];
  const leafModel = models[leaf.modelName];
  keepNavForRecord(anchor.modelName);
  const hasOutputs = Object.keys(leafModel.outputs || {}).length > 0;
  const [rels, outs] = await Promise.all([
    api(`/api/models/${encodeURIComponent(leaf.modelName)}/record/relationships?key=${encodeURIComponent(leaf.record.key)}`).catch(() => ({ outbound: [], inbound: [] })),
    hasOutputs
      ? api(`/api/models/${encodeURIComponent(leaf.modelName)}/record/outputs?key=${encodeURIComponent(leaf.record.key)}`).catch(() => ({ outputs: [] }))
      : Promise.resolve({ outputs: [] }),
  ]);
  if (t !== reqToken) return;
  resetPageDynamics();
  setHeader(recordTitle(anchor.record, anchorModel), '', recordActions(leaf.record));
  $('#subtitle').innerHTML = `<span class="badge plain">${esc(anchorModel.label || anchorModel.name)}</span>`;
  const trail = tail.map((entry, i) => {
    const title = recordTitle(entry.record, models[entry.modelName]);
    return i === tail.length - 1
      ? `<span class="crumb-current">${esc(title)}</span>`
      : `<a href="${recordHref(entry.record)}">${esc(title)}</a>`;
  }).join('<span class="crumb-sep">›</span>');
  const crumbs = `<div class="tab-crumbs"><span class="crumbs">${trail}</span><span class="badge plain">${esc(leafModel.label || leafModel.name)}</span></div>`;
  const ctx = buildRecordCtx(leafModel, leaf.record, rels, outs);
  const inner = renderDetailBlock(leafModel, ctx, 'detail');
  $('#page').innerHTML = `${recordTabsBar(anchorModel, anchor.record, tabRoute)}${crumbs}<div class="detail">${inner}</div>`;
  fillMarkdown();
  fillResources();
  wireDelete(leaf.record, recordTitle(leaf.record, leafModel));
}

function tb_route(tab) { return tab.route || tab.block; }

// One record by its URL identity, scoped to its parent when nested. The
// parent scope filter uses the parent record's identity value, which is what
// the child's back-reference stores (enforced by route validation).
async function findByIdentity(modelName, parentModelName, value, parentRecord) {
  const model = state.project.models[modelName];
  if (!model.identity) {
    return api(`/api/models/${encodeURIComponent(modelName)}/record?key=${encodeURIComponent(value)}`);
  }
  const filters = [{ field: model.identity, op: 'eq', value }];
  if (parentModelName && parentRecord) {
    const parentModel = state.project.models[parentModelName];
    const scope = parentModel.identity ? parentRecord.values[parentModel.identity] : parentRecord.key;
    filters.push({ field: model.parent, op: 'eq', value: scope });
  }
  const res = await api(`/api/models/${encodeURIComponent(modelName)}/records?page=1&page_size=2&filter=${encodeURIComponent(JSON.stringify(filters))}`);
  if (!res.records?.length) {
    const label = state.project.models[modelName].label || modelName;
    throw new Error(`No ${label.toLowerCase().replace(/s$/, '')} “${value}”.`);
  }
  return res.records[0];
}

// Resolve a reference value to a canonical record, then rewrite the URL.
async function resolveBy(model, field, value) {
  const t = ++reqToken;
  setHeader('Resolving…', '', '');
  $('#subnav').innerHTML = '';
  $('#page').innerHTML = '<div class="loading">Resolving reference…</div>';
  try {
    const rec = await api(`/api/models/${encodeURIComponent(model)}/record?key=${encodeURIComponent(value)}`);
    if (t !== reqToken) return;
    history.replaceState(null, '', recordHref(rec));
    showRecord(rec.model, rec.key, rec);
  } catch (e) {
    if (t !== reqToken) return;
    fatal(`Could not resolve ${esc(field)} = ${esc(value)}: ${esc(e.message)}`);
  }
}

/* =========================================================================
 * Shared helpers: headers, titles, links, value formatting
 * ====================================================================== */

function setHeader(title, subtitle, actionsHtml) {
  $('#title').textContent = title || '';
  $('#subtitle').textContent = subtitle || '';
  $('#actions').innerHTML = actionsHtml || '';
}

function fatal(message) {
  $('#subnav').innerHTML = '';
  setHeader('Something went wrong', '', '');
  $('#page').innerHTML = `<div class="error-panel"><div class="error">${esc(message)}</div></div>`;
}

function pluralize(word) {
  if (!word) return word;
  if (/[^aeiou]y$/i.test(word)) return word.slice(0, -1) + 'ies';
  if (/(s|x|z|ch|sh)$/i.test(word)) return word + 'es';
  return word + 's';
}

function modelLabel(name) { const m = state.project.models[name]; return m?.label || name; }

// Record title shared by breadcrumbs, cards, and the detail header: the
// model's configured title field when set, else the heuristic.
function recordTitle(record, model) {
  const v = record.values || {};
  if (model.title && v[model.title] != null && v[model.title] !== '') return String(v[model.title]);
  if (v.title != null && v.title !== '') return String(v.title);
  if (v.name != null && v.name !== '') return String(v.name);
  for (const [f, def] of Object.entries(model.fields)) {
    if (def.required && def.type === 'string' && v[f] != null && v[f] !== '') return String(v[f]);
  }
  return record.key;
}

function recordHref(record) {
  return nestedHref(record) || `#/records/${encodeURIComponent(record.model)}/${encodeURIComponent(record.key)}`;
}

// Canonical Rails-style URL (#/books/rude/docs/idea), derivable purely from
// the record's own values: every ancestor's URL segment is the value some
// field on THIS record stores by referencing that ancestor at its identity
// field. Returns null when the chain isn't fully routed or derivable.
function nestedHref(record) {
  const models = state.project?.models || {};
  const model = models[record.model];
  if (!model?.route) return null;
  const leaf = model.identity ? record.values[model.identity] : record.key;
  if (leaf == null || leaf === '') return null;
  let segs = [model.route, String(leaf)];
  let cur = model;
  for (let depth = 0; depth < 6 && cur.parent; depth++) {
    const ancestor = models[cur.fields[cur.parent]?.reference?.model];
    if (!ancestor?.route || !ancestor.identity) return null;
    const source = Object.entries(model.fields).find(([, d]) =>
      d.reference?.model === ancestor.name && d.reference.field === ancestor.identity && !d.reference.many);
    const value = source && record.values[source[0]];
    if (value == null || value === '' || typeof value === 'object') return null;
    segs = [ancestor.route, String(value), ...segs];
    cur = ancestor;
  }
  if (cur.parent) return null;
  return '#/' + segs.map(encodeURIComponent).join('/');
}

function assetUrl(path) { return '/files/' + String(path).split('/').map(encodeURIComponent).join('/'); }
const IMG = ['jpg', 'jpeg', 'png', 'gif', 'webp', 'avif'];
const VID = ['mp4', 'webm', 'mov', 'm4v', 'ogv'];
const AUD = ['mp3', 'wav', 'ogg', 'm4a', 'flac'];

// `opts.link` (default true): wrap images in a target=_blank full-file link.
// Set false inside linked gallery cards so we don't nest <a> in <a>.
function assetPreview(path, compact = false, opts = {}) {
  const url = assetUrl(path), ext = String(path).split('.').pop().toLowerCase();
  const link = opts.link !== false;
  if (IMG.includes(ext)) {
    const cls = compact ? 'asset-thumb' : (opts.fill ? 'asset-tile' : 'asset-preview');
    const img = `<img class="${cls}" src="${esc(url)}" alt="${esc(path)}" loading="lazy">`;
    if (!link) return img;
    return `<a class="asset-full" href="${esc(url)}" target="_blank" rel="noopener" title="Open full image">${img}</a>`;
  }
  if (compact) return `<span class="badge plain">${VID.includes(ext) ? 'Video' : AUD.includes(ext) ? 'Audio' : 'Asset'}</span>`;
  if (VID.includes(ext)) return `<video class="asset-preview" src="${esc(url)}" controls preload="metadata"></video>`;
  if (AUD.includes(ext)) return `<audio class="asset-preview" src="${esc(url)}" controls preload="metadata"></audio>`;
  return `<a class="asset-link" href="${esc(url)}" target="_blank" rel="noopener">Open ${esc(path)}</a>`;
}

function fmtDate(value, type) {
  const d = new Date(value);
  if (isNaN(d)) return esc(value);
  if (type === 'date_time') return d.toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
  return d.toLocaleDateString(undefined, { dateStyle: 'medium' });
}

// Link(s) for a reference value using the `by/<field>/<value>` resolver route.
function refLink(value, field) {
  const ref = field.reference;
  if (ref == null || value == null) return esc(value ?? '—');
  const vals = Array.isArray(value) ? value : [value];
  return vals.map(v =>
    `<a class="ref" href="#/records/${encodeURIComponent(ref.model)}/by/${encodeURIComponent(ref.field)}/${encodeURIComponent(v)}">${esc(refName(v, ref))}</a>`
  ).join(', ');
}

// The human title of a referenced record (falls back to the raw key/slug when
// the referenced records haven't been cached — see ensureRecords).
function refName(value, ref) {
  if (value == null) return '';
  const records = ref && state.recordsByModel[ref.model];
  const hit = records && records.find(r => r.values[ref.field] === value);
  return hit ? recordTitle(hit, state.project.models[ref.model] || { fields: {} }) : String(value);
}

// Fetch and cache every record of a model, so reference cells can show titles
// instead of slugs. Cached after the first call.
async function ensureRecords(modelName) {
  if (state.recordsByModel[modelName]) return;
  try {
    const res = await api(`/api/models/${encodeURIComponent(modelName)}/records?page_size=1000`);
    state.recordsByModel[modelName] = res.records || [];
  } catch {
    state.recordsByModel[modelName] = [];
  }
}

// Load the referenced records for every reference field a view displays, so
// their cells render human names.
async function ensureViewRefs(view, model) {
  const models = new Set();
  for (const f of viewFields(view, model)) {
    const ref = model.fields[f]?.reference?.model;
    if (ref) models.add(ref);
  }
  await Promise.all([...models].map(ensureRecords));
}

// Compact, single-line-ish value formatting for table cells & card metadata.
function formatValue(value, field) {
  if (value == null || value === '') return '<span class="dash">—</span>';
  const type = field?.type;
  if (type === 'reference') return refLink(value, field);
  if (type === 'asset') return assetPreview(value, true);
  if (type === 'boolean' || typeof value === 'boolean') return value ? 'Yes' : 'No';
  if (type === 'date' || type === 'date_time') return esc(fmtDate(value, type));
  if (Array.isArray(value)) return `<span class="badge-row">${value.map(x => `<span class="badge plain">${esc(x)}</span>`).join('')}</span>`;
  if (type === 'json' || (typeof value === 'object')) return esc(JSON.stringify(value));
  const s = String(value);
  return esc(s.length > 120 ? s.slice(0, 120) + '…' : s);
}

// The first asset field named in `fields` (else any model asset field).
function imageField(model, fields) {
  const list = (fields && fields.length ? fields : Object.keys(model.fields));
  for (const f of list) if (model.fields[f]?.type === 'asset') return f;
  for (const f of Object.keys(model.fields)) if (model.fields[f].type === 'asset') return f;
  return null;
}

// A clickable card: thumbnail (if an image asset is present) + title + up to
// three secondary formatted values.
function recordCard(record, model, fields) {
  const imgF = imageField(model, fields);
  const imgV = imgF && record.values[imgF];
  const isImg = imgV && IMG.includes(String(imgV).split('.').pop().toLowerCase());
  const thumb = isImg
    ? `<img class="card-thumb" src="${esc(assetUrl(imgV))}" alt="" loading="lazy">`
    : `<div class="thumb-ph">${esc(recordTitle(record, model).charAt(0) || '?')}</div>`;
  const secondary = (fields && fields.length ? fields : Object.keys(model.fields))
    .filter(f => f !== imgF && f !== 'title' && f !== 'name')
    .slice(0, 3)
    .map(f => `<div><span class="k">${esc(model.fields[f]?.label || f)}</span> ${formatValue(record.values[f], model.fields[f])}</div>`)
    .join('');
  return `<a class="card" href="${recordHref(record)}">${isImg || imgF ? thumb : ''}` +
    `<div class="card-title">${esc(recordTitle(record, model))}</div>` +
    `<div class="card-meta">${secondary}</div></a>`;
}

/* =========================================================================
 * Display DSL — recursive renderer for model display blocks.
 * ====================================================================== */

const MAX_NODE_DEPTH = 8;
const SPACE = { sm: '8px', md: '14px', lg: '22px', xl: '32px' };
const BP_SUFFIX = { default: 'base', sm: 'sm', md: 'md', lg: 'lg', xl: 'xl' };

// Per-render payload stores for delegated handlers (reset on every page build).
let copyPayloads = [];
let checkPayloads = [];
let mdQueue = [];
let relQueue = [];

function resetPageDynamics() {
  copyPayloads = [];
  checkPayloads = [];
  mdQueue = [];
  relQueue = [];
}

// A block is one node or a list (implicit stack).
function blockNodes(block) { return Array.isArray(block) ? block : [block]; }

// Emit `--name-<bp>` custom properties for an int-or-breakpoint-map value.
function responsiveVars(name, value, out) {
  if (value == null) return;
  if (typeof value === 'number') { out.push(`--${name}-base:${value}`); return; }
  for (const [bp, suffix] of Object.entries(BP_SUFFIX)) {
    if (value[bp] != null) out.push(`--${name}-${suffix}:${value[bp]}`);
  }
}

function gapVars(gap, out) {
  if (gap == null) return;
  if (typeof gap === 'string') {
    out.push(`--cgap:${SPACE[gap]}`, `--rgap:${SPACE[gap]}`);
    return;
  }
  if (gap.column) out.push(`--cgap:${SPACE[gap.column]}`);
  if (gap.row) out.push(`--rgap:${SPACE[gap.row]}`);
}

function styleAttr(vars) { return vars.length ? ` style="${vars.join(';')}"` : ''; }

// Render one DSL node. ctx: { model, record, rels, outs, outboundByField,
// depth } — rels/outs are null when a block renders as a collection item.
function renderNode(node, ctx) {
  if (!node || ctx.depth > MAX_NODE_DEPTH) return '';
  const vars = [];
  responsiveVars('span', node.span, vars);
  const kids = children =>
    (children || []).map(child => renderNode(child, ctx)).join('');
  switch (node.type) {
    case 'grid': {
      responsiveVars('cols', node.columns, vars);
      gapVars(node.gap, vars);
      return `<div class="dgrid"${styleAttr(vars)}>${kids(node.children)}</div>`;
    }
    case 'stack': {
      gapVars(node.gap, vars);
      return `<div class="dstack"${styleAttr(vars)}>${kids(node.children)}</div>`;
    }
    case 'card': {
      if (node.padding) vars.push(`--pad:${SPACE[node.padding]}`);
      const title = node.title ? `<h3 class="d-heading">${esc(node.title)}</h3>` : '';
      return `<div class="dcard"${styleAttr(vars)}>${title}${kids(node.children)}</div>`;
    }
    case 'section': {
      return `<section class="dsection"${styleAttr(vars)}>` +
        `<h3 class="d-heading">${esc(node.title)}</h3>${kids(node.children)}</section>`;
    }
    case 'divider':
      return `<hr class="ddivider"${styleAttr(vars)}>`;
    case 'field':
      return fieldNode(node, ctx, vars);
    case 'resource':
      return resourceNode(node, ctx, vars);
    case 'outputs':
      return outputsNode(node, ctx, vars);
    default:
      return ''; // action_group (reserved) and anything unknown
  }
}

function metaValue(record, name) {
  return { 'meta.key': record.key, 'meta.path': record.path, 'meta.model': record.model }[name];
}
const META_LABELS = { 'meta.key': 'Key', 'meta.path': 'Path', 'meta.model': 'Model' };

function fieldNode(node, ctx, vars) {
  const names = node.name ? [node.name] : (node.fields || []);
  const def = node.name ? ctx.model.fields[node.name] : null;
  const raw = node.name
    ? (node.name.startsWith('meta.') ? metaValue(ctx.record, node.name) : ctx.record.values[node.name])
    : null;
  const isEmpty = v => v == null || v === '' || (Array.isArray(v) && !v.length);

  let value;
  if (node.format === 'badge' || node.format === 'badges') {
    const badges = names
      .flatMap(n => {
        const rawValue = n.startsWith('meta.') ? metaValue(ctx.record, n) : ctx.record.values[n];
        if (isEmpty(rawValue)) return [];
        const field = ctx.model.fields[n];
        return (Array.isArray(rawValue) ? rawValue : [rawValue]).map(v =>
          field?.type === 'reference' ? refName(v, field.reference) : v);
      })
      .map(v => `<span class="badge plain">${esc(v)}</span>`)
      .join('');
    value = badges ? `<span class="badge-row">${badges}</span>` : null;
  } else if (isEmpty(raw)) {
    value = null;
  } else {
    value = formatFieldValue(raw, def, node, ctx);
  }
  if (value == null) value = `<span class="dash">${esc(node.empty ?? '—')}</span>`;

  const actions = (node.actions || []).map(action => copyButton(action, raw)).join('');
  if (node.format === 'title') {
    const title = ctx.titleHref ? `<a href="${ctx.titleHref}">${value}</a>` : value;
    return `<div class="kv"${styleAttr(vars)}><div class="d-title">${title}</div></div>`;
  }
  // `label: ""` suppresses the label row entirely (images, full-bleed prose).
  const label = node.label
    ?? def?.label
    ?? META_LABELS[node.name]
    ?? node.name
    ?? '';
  const head = label === '' && !actions ? '' : `<div class="k">${esc(label)}${actions}</div>`;
  return `<div class="kv"${styleAttr(vars)}>${head}<div class="v">${value}</div></div>`;
}

function formatFieldValue(value, def, node, ctx) {
  switch (node.format) {
    case 'title': return esc(String(value));
    case 'markdown': {
      const index = mdQueue.push(String(value)) - 1;
      return `<div class="prose" data-md="${index}">${esc(String(value))}</div>`;
    }
    case 'code': return `<code class="d-code">${esc(String(value))}</code>`;
    case 'date': return esc(fmtDate(value, def?.type === 'date_time' ? 'date_time' : 'date'));
    case 'relative_time': return relTime(value);
    case 'chips': {
      const items = listItems(value);
      if (!items.length) return null; // → the node's `empty` placeholder
      return `<span class="chip-row">${items.map(x => `<span class="chip">${esc(x)}</span>`).join('')}</span>`;
    }
    case 'list': {
      const items = listItems(value);
      if (!items.length) return null;
      return `<ul class="d-list">${items.map(x => `<li>${esc(x)}</li>`).join('')}</ul>`;
    }
    case 'links': return linksHtml(value);
    case 'template':
      return esc(node.template || '').replace(/\{\{\s*value\s*\}\}/g, esc(String(value)));
    case 'text': return detailValue(value, def || {}, node.name, ctx.outboundByField || new Map(), ctx);
    default: // type-derived
      return detailValue(value, def || {}, node.name, ctx.outboundByField || new Map(), ctx);
  }
}

// Array-ish values as display strings, dropping blank entries.
function listItems(value) {
  const items = Array.isArray(value) ? value : [value];
  return items
    .map(x => x != null && typeof x === 'object' ? JSON.stringify(x) : String(x ?? ''))
    .filter(s => s.trim() !== '');
}

// A JSON links value: {label: url}, [url, ...], or [{label, url}, ...].
function linksHtml(value) {
  let entries;
  if (Array.isArray(value)) {
    entries = value.map(v => (v && typeof v === 'object') ? [v.label ?? v.url, v.url] : [v, v]);
  } else if (value && typeof value === 'object') {
    entries = Object.entries(value);
  } else {
    entries = [[String(value), String(value)]];
  }
  return entries
    .filter(([, url]) => url != null)
    .map(([label, url]) =>
      `<div class="d-link"><span class="k2">${esc(label)}:</span> ` +
      `<a href="${esc(url)}" target="_blank" rel="noopener">${esc(url)}</a></div>`)
    .join('');
}

function relTime(value) {
  const d = new Date(value);
  if (isNaN(d)) return esc(String(value));
  const seconds = (Date.now() - d.getTime()) / 1000;
  const abs = Math.abs(seconds);
  const units = [[31536000, 'year'], [2592000, 'month'], [604800, 'week'],
    [86400, 'day'], [3600, 'hour'], [60, 'minute']];
  for (const [size, name] of units) {
    if (abs >= size) {
      const n = Math.floor(abs / size);
      return esc(seconds >= 0 ? `${n} ${name}${n === 1 ? '' : 's'} ago` : `in ${n} ${name}${n === 1 ? '' : 's'}`);
    }
  }
  return 'just now';
}

// `copy` field action → button resolved through the delegated click handler.
function copyButton(action, raw) {
  if (action.type !== 'copy' || raw == null) return '';
  const index = copyPayloads.push({ value: raw, as: action.value || 'text' }) - 1;
  return `<button type="button" class="copy-btn" data-copy="${index}">${esc(action.label || 'Copy')}</button>`;
}

async function handleCopyClick(button) {
  const payload = copyPayloads[+button.dataset.copy];
  if (!payload) return;
  const { value, as } = payload;
  let text;
  try {
    if (as === 'html') {
      const res = await api('/api/render/markdown', {
        method: 'POST', headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ texts: [String(value)] }),
      });
      text = res.html[0];
    } else if (as === 'lines') {
      text = (Array.isArray(value) ? value : [value]).map(String).join('\n');
    } else if (as === 'json') {
      text = JSON.stringify(value, null, 2);
    } else {
      text = typeof value === 'object' ? JSON.stringify(value) : String(value);
    }
    await navigator.clipboard.writeText(text);
    const original = button.textContent;
    button.textContent = 'Copied';
    setTimeout(() => { button.textContent = original; }, 1200);
  } catch (e) {
    alert('Copy failed: ' + e.message);
  }
}

// Checklist rows toggle their boolean via the merge semantics of the update
// API (only the checked field is sent), then re-render the page.
async function handleCheckToggle(box) {
  const payload = checkPayloads[+box.dataset.check];
  if (!payload) return;
  const { record, field } = payload;
  box.disabled = true;
  try {
    await api(`/api/models/${encodeURIComponent(record.model)}/record?key=${encodeURIComponent(record.key)}`, {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ revision: record.revision, values: { [field]: box.checked } }),
    });
    router(); // refresh the page (records + revisions changed)
  } catch (e) {
    box.checked = !box.checked;
    box.disabled = false;
    alert(e.message);
  }
}

// Fill `format: markdown` placeholders from one batched render request.
async function fillMarkdown() {
  if (!mdQueue.length) return;
  const texts = mdQueue.slice();
  try {
    const res = await api('/api/render/markdown', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ texts }),
    });
    document.querySelectorAll('[data-md]').forEach(el => {
      const html = res.html[+el.dataset.md];
      if (html != null) { el.innerHTML = html; el.removeAttribute('data-md'); }
    });
  } catch { /* placeholders already show the raw text */ }
}

// Fill nested resource placeholders. Item blocks and expand rows render
// without relationship data, so their resource nodes queue up here; each
// distinct record's relationships are fetched once, then the node re-renders
// in place. Hydrated content can queue further placeholders (and markdown),
// so the loop drains the queue and re-runs the markdown fill as needed.
// renderNode's depth cap bounds the recursion.
async function fillResources() {
  let cursor = 0;
  while (cursor < relQueue.length) {
    const batch = relQueue.slice(cursor);
    const start = cursor;
    cursor = relQueue.length;
    const fetches = new Map();
    for (const { ctx } of batch) {
      const id = `${ctx.record.model}\n${ctx.record.key}`;
      if (!fetches.has(id)) {
        fetches.set(id, api(`/api/models/${encodeURIComponent(ctx.record.model)}/record/relationships?key=${encodeURIComponent(ctx.record.key)}`).catch(() => null));
      }
    }
    const mdBefore = mdQueue.length;
    for (let i = 0; i < batch.length; i++) {
      const { node, ctx, vars } = batch[i];
      const rels = await fetches.get(`${ctx.record.model}\n${ctx.record.key}`);
      const el = document.querySelector(`[data-rel="${start + i}"]`);
      if (!el) continue; // navigated away, or an earlier pass replaced it
      if (!rels) { el.remove(); continue; }
      el.outerHTML = resourceNode(node, { ...ctx, rels }, vars);
    }
    if (mdQueue.length > mdBefore) fillMarkdown();
  }
  wireResourceSort();
}

/* ---- resource + outputs nodes (outputs on the record page only) ---- */

// Does a record satisfy one resource-node filter? Missing values read as null,
// matching the server's filter semantics.
function recordMatchesFilter(record, filter) {
  const actual = record.values[filter.field];
  const present = actual != null;
  switch (filter.op) {
    case 'is_null': return !present;
    case 'is_not_null': return present;
    case 'eq': return present && actual === filter.value;
    case 'not_eq': return !(present && actual === filter.value);
    case 'in': return present && Array.isArray(filter.value) && filter.value.includes(actual);
    case 'contains':
      if (Array.isArray(actual)) return actual.includes(filter.value);
      return present && String(actual).includes(String(filter.value));
    case 'lt': return present && actual < filter.value;
    case 'lte': return present && actual <= filter.value;
    case 'gt': return present && actual > filter.value;
    case 'gte': return present && actual >= filter.value;
    default: return true;
  }
}

// The related records for a resource node, from the relationships payload.
function resourceRecords(node, ctx) {
  const inbound = ctx.rels?.inbound || [];
  let records = inbound
    .filter(link => link.record.model === node.model && (!node.via || link.field === node.via))
    .map(link => link.record);
  for (const filter of node.filters || []) {
    records = records.filter(r => recordMatchesFilter(r, filter));
  }
  for (const order of [...(node.order || [])].reverse()) {
    const dir = order.direction === 'desc' ? -1 : 1;
    records = records.slice().sort((a, b) => {
      const [x, y] = [a.values[order.field], b.values[order.field]];
      if (x == null && y == null) return 0;
      if (x == null) return 1;
      if (y == null) return -1;
      return (x < y ? -1 : x > y ? 1 : 0) * dir;
    });
  }
  return records;
}

// Depth-first order for a self-referencing record set: children group under
// their parent (sibling order preserved from the sorted input); roots and
// orphans stay at the top level.
function treeDepths(records, related, treeField) {
  const target = related.fields[treeField]?.reference?.field;
  if (!target) return records.map(record => ({ record, depth: 0 }));
  const ids = new Set(records.map(r => String(r.values[target])));
  const children = new Map();
  const roots = [];
  for (const r of records) {
    const p = r.values[treeField];
    if (p != null && p !== '' && ids.has(String(p))) {
      if (!children.has(String(p))) children.set(String(p), []);
      children.get(String(p)).push(r);
    } else {
      roots.push(r);
    }
  }
  const out = [];
  const walk = (record, depth) => {
    if (depth > 12) return;
    out.push({ record, depth });
    for (const child of children.get(String(record.values[target])) || []) walk(child, depth + 1);
  };
  roots.forEach(r => walk(r, 0));
  return out;
}

// The reference field on the related model pointing at ctx.model, honoring
// `via`; used to prefill create forms.
function resourceRefField(node, ctx) {
  const related = state.project.models[node.model];
  if (!related) return null;
  if (node.via) return related.fields[node.via] ? node.via : null;
  for (const [name, def] of Object.entries(related.fields)) {
    if (def.type === 'reference' && def.reference?.model === ctx.model.name) return name;
  }
  return null;
}

function resourceActions(node, ctx) {
  return (node.actions || []).map(action => {
    if (action.type === 'navigate') {
      let suffix = '';
      if (action.filtered) {
        // Pin the view to this record via the related model's back-reference.
        const refField = resourceRefField(node, ctx);
        const refDef = refField && state.project.models[node.model].fields[refField];
        const refValue = refDef ? (ctx.record.values[refDef.reference.field] ?? ctx.record.key) : null;
        if (refField && refValue != null) suffix = `?f.${encodeURIComponent(refField)}=${encodeURIComponent(refValue)}`;
      }
      return `<a class="d-action" href="#/views/${encodeURIComponent(action.view)}${suffix}">${esc(action.label || 'View all')}</a>`;
    }
    if (action.type === 'create') {
      const refField = resourceRefField(node, ctx);
      const refDef = refField && state.project.models[node.model].fields[refField];
      const refValue = refDef ? (ctx.record.values[refDef.reference.field] ?? ctx.record.key) : null;
      const query = refField && refValue != null
        ? `?${encodeURIComponent(refField)}=${encodeURIComponent(refValue)}` : '';
      return `<a class="d-action" href="#/records/${encodeURIComponent(node.model)}/new${query}">${esc(action.label || 'New')}</a>`;
    }
    return '';
  }).join('');
}

// Model labels are conventionally already plural ("Todos", "Book changes"),
// so counting means singularizing for 1 rather than pluralizing for many.
function countNoun(n, label) {
  const noun = label.toLowerCase();
  if (n === 1) {
    if (/ies$/.test(noun)) return noun.slice(0, -3) + 'y';
    if (/[^s]s$/.test(noun)) return noun.slice(0, -1);
    return noun;
  }
  return /s$/.test(noun) ? noun : pluralize(noun);
}

function resourceNode(node, ctx, vars) {
  if (!ctx.rels) {
    // Items and expand rows render before their record's relationships are
    // known; fillResources() hydrates the placeholder after the page builds.
    if (!ctx.record) return '';
    const index = relQueue.push({ node, ctx, vars }) - 1;
    return `<div class="dres" data-rel="${index}"${styleAttr(vars)}></div>`;
  }
  const related = state.project.models[node.model];
  if (!related) return '';
  const all = resourceRecords(node, ctx);
  const records = node.limit ? all.slice(0, node.limit) : all;
  const more = all.length - records.length;

  let body;
  const display = node.display || 'item';
  if (display === 'summary') {
    body = all.length
      ? `<div class="d-summary">${all.length} ${esc(countNoun(all.length, related.label || related.name))}</div>`
      : `<div class="d-summary dash">${esc(node.empty ?? 'None yet.')}</div>`;
  } else if (!all.length) {
    body = `<div class="d-summary dash">${esc(node.empty ?? 'None yet.')}</div>`;
  } else if (display === 'checklist') {
    // `sortable` names an integer field made drag-reorderable; rows become
    // draggable divs (a label would swallow drags) that renumber on drop.
    const sortField = node.sortable;
    const rows = records.map(r => {
      const done = !!r.values[node.check];
      const index = checkPayloads.push({ record: r, field: node.check }) - 1;
      const drag = sortField
        ? ` draggable="true" data-key="${esc(r.key)}" data-revision="${esc(r.revision ?? '')}" data-sortval="${esc(r.values[sortField] ?? '')}"`
        : '';
      const tag = sortField ? 'div' : 'label';
      return `<${tag} class="check-row${sortField ? ' check-item' : ''}${done ? ' done' : ''}"${drag}>` +
        `<input type="checkbox" class="check-toggle" data-check="${index}"${done ? ' checked' : ''}>` +
        `<a href="${recordHref(r)}"${sortField ? ' draggable="false"' : ''}>${esc(recordTitle(r, related))}</a></${tag}>`;
    }).join('');
    body = sortField
      ? `<div class="check-sortable" data-model="${esc(node.model)}" data-sort="${esc(sortField)}">${rows}</div>`
      : rows;
  } else if (display === 'table' || display === 'tree') {
    const columns = node.fields && node.fields.length
      ? node.fields.map(c => typeof c === 'string' ? { field: c } : c)
      : Object.keys(related.fields).slice(0, 3).map(field => ({ field }));
    const expandable = (node.expand || []).length > 0;
    const head = (expandable ? '<th class="expand-cell"></th>' : '') + columns.map(c =>
      `<th>${esc(c.label || related.fields[c.field]?.label || c.field)}</th>`).join('') +
      (expandable ? '<th class="details-cell"></th>' : '');
    const depths = display === 'tree' ? treeDepths(records, related, node.tree) : null;
    const ordered = depths ? depths.map(d => d.record) : records;
    const rows = ordered.map((r, rowIndex) => {
      const indent = depths ? '<span class="tree-indent"></span>'.repeat(depths[rowIndex].depth) : '';
      const cells = columns.map((c, i) => {
        const raw = c.field.startsWith('meta.') ? metaValue(r, c.field) : r.values[c.field];
        const formatted = c.format
          ? formatFieldValue(raw, related.fields[c.field], c, { model: related, record: r })
          : formatValue(raw, related.fields[c.field]);
        let inner = raw == null || raw === '' ? '<span class="dash">—</span>' : formatted;
        if (i === 0) inner = indent + inner;
        // Expandable rows toggle on click, so the record link moves to a
        // Details cell instead of wrapping the first column.
        return i === 0 && !expandable
          ? `<td><a class="cell" href="${recordHref(r)}">${inner}</a></td>`
          : `<td class="plain">${inner}</td>`;
      }).join('');
      if (!expandable) return `<tr>${cells}</tr>`;
      // Detail rendered up front (hidden) so markdown placeholders hydrate in
      // the page's one batched render; the toggle only flips visibility.
      const detailCtx = { model: related, record: r, rels: null, outs: null, outboundByField: new Map(), depth: ctx.depth + 1 };
      const detail = (node.expand || []).map(n => renderNode(n, detailCtx)).join('');
      const toggle = `<td class="plain expand-cell"><button class="expand-toggle" type="button" aria-expanded="false">▸</button></td>`;
      const details = `<td class="plain details-cell"><a class="d-action" href="${recordHref(r)}">Details</a></td>`;
      return `<tr class="expand-row">${toggle}${cells}${details}</tr>` +
        `<tr class="expand-detail" hidden><td class="plain" colspan="${columns.length + 2}">${detail}</td></tr>`;
    }).join('');
    body = `<div class="table-shell mini"><table><thead><tr>${head}</tr></thead><tbody>${rows}</tbody></table></div>`;
  } else { // item
    const block = related.display?.[node.item || 'card'];
    const items = records.map(r => recordItem(r, related, block, ctx.depth + 1)).join('');
    const gridVars = [];
    responsiveVars('cols', node.columns ?? 1, gridVars);
    body = `<div class="dgrid" style="${gridVars.join(';')}">${items}</div>`;
  }

  const moreNote = more > 0 ? `<div class="dres-more">and ${more} more…</div>` : '';
  const heading = node.title || node.actions?.length
    ? `<div class="dres-head"><h3 class="d-heading">${esc(node.title || '')}</h3><span class="dres-actions">${resourceActions(node, ctx)}</span></div>`
    : '';
  const boxed = display === 'item' ? `${body}${moreNote}` : `<div class="panel">${body}${moreNote}</div>`;
  return `<div class="dres"${styleAttr(vars)}>${heading}${boxed}</div>`;
}

function outputsNode(node, ctx, vars) {
  if (!ctx.outs) return '';
  const outputs = ctx.outs.outputs || [];
  const heading = node.title ? `<div class="dres-head"><h3 class="d-heading">${esc(node.title)}</h3></div>` : '';
  let body;
  if (!outputs.length) {
    body = '<div class="d-summary dash">No outputs generated yet.</div>';
  } else if (node.display === 'list') {
    body = outputs.map(o => {
      const status = o.exists ? '<span class="ok">✓</span>' : '<span class="missing">—</span>';
      const files = (o.files || []).map(f =>
        `<div class="output-row output-file"><span class="badge"></span><span class="path">${esc(f)}</span><span class="ok">✓</span></div>`).join('');
      return `<div class="output-row"><span class="badge">${esc(o.name)}</span><span class="path">${esc(o.path)}</span>${status}</div>${files}`;
    }).join('');
  } else {
    const rows = outputs.map(o => {
      const status = o.exists
        ? `<span class="ok">${o.is_file ? 'file' : `directory · ${(o.files || []).length} file${(o.files || []).length === 1 ? '' : 's'}`}</span>`
        : '<span class="missing">not generated</span>';
      const files = (o.files || []).map(f =>
        `<tr class="output-file"><td class="plain"></td><td class="plain"><span class="path">${esc(f)}</span></td><td class="plain"><span class="ok">file</span></td></tr>`).join('');
      return `<tr><td class="plain"><span class="badge">${esc(o.name)}</span></td><td class="plain"><span class="path">${esc(o.path)}</span></td><td class="plain">${status}</td></tr>${files}`;
    }).join('');
    body = `<div class="table-shell mini"><table><thead><tr><th>Output</th><th>Path</th><th>Status</th></tr></thead><tbody>${rows}</tbody></table></div>`;
  }
  return `<div class="dres"${styleAttr(vars)}>${heading}<div class="panel">${body}</div></div>`;
}

/* ---- record items: a record rendered through a display block ---- */

// The block a view uses for each record in card-ish contexts.
function itemBlock(view, model) {
  const name = view?.display?.item;
  if (name && model.display?.[name]) return model.display[name];
  return model.display?.card || null;
}

// A calendar's lower agenda may use a roomier block than its compact day
// cells. It falls back through the regular item selection.
function agendaBlock(view, model) {
  const name = view?.display?.agenda;
  if (name && model.display?.[name]) return model.display[name];
  return itemBlock(view, model);
}

// True when a block's visible content is only asset field(s) — an image
// gallery card. One card/stack wrapper level is allowed around the fields.
function imageOnlyBlock(block, model) {
  let nodes = blockNodes(block);
  if (nodes.length === 1 && ['card', 'stack'].includes(nodes[0]?.type) && nodes[0].children)
    nodes = blockNodes(nodes[0].children);
  return nodes.length > 0 && nodes.every(n =>
    n?.type === 'field' && model.fields[n.name]?.type === 'asset');
}

// True when a block contains a resource node at any depth. Such a block
// renders links of its own, so the item cannot be one big anchor.
function blockHasResource(block) {
  const walk = node => !!node && (node.type === 'resource' ||
    (node.children || []).some(walk));
  return blockNodes(block).some(walk);
}

// Render one record through a block (else the built-in card), linked.
// Image-only blocks get the square gallery-tile treatment; every other block
// keeps the regular card chrome. Blocks with nested resources get a plain
// wrapper with the title linked instead of an anchor around the whole item.
function recordItem(record, model, block, depth = 0) {
  if (!block || depth > MAX_NODE_DEPTH) return recordCard(record, model, []);
  const tile = imageOnlyBlock(block, model);
  const nested = blockHasResource(block);
  // linkAssets false: avoid nested anchors (card → record; image link only on detail).
  // assetFill: gallery tiles should stretch the image to the cell.
  const ctx = {
    model, record, rels: null, outs: null, outboundByField: new Map(), depth,
    linkAssets: false,
    assetFill: tile,
    titleHref: nested ? recordHref(record) : null,
  };
  const inner = blockNodes(block).map(node => renderNode(node, ctx)).join('');
  return nested
    ? `<div class="item-card">${inner}</div>`
    : `<a class="item-link${tile ? ' gallery-item' : ''}" href="${recordHref(record)}">${inner}</a>`;
}

function paginationHtml(view, result, q, filters) {
  if (result.pages <= 1) return '';
  const base = p => `#/views/${encodeURIComponent(view.name)}?page=${p}${q ? '&q=' + encodeURIComponent(q) : ''}${filterParams(filters)}`;
  const prev = result.page > 1
    ? `<a class="btn" href="${base(result.page - 1)}">Previous</a>`
    : `<span class="btn" aria-disabled="true" style="opacity:.45">Previous</span>`;
  const next = result.page < result.pages
    ? `<a class="btn" href="${base(result.page + 1)}">Next</a>`
    : `<span class="btn" aria-disabled="true" style="opacity:.45">Next</span>`;
  return `<div class="pagination">${prev}<span>Page ${result.page} of ${result.pages}</span>${next}</div>`;
}

/* =========================================================================
 * View page
 * ====================================================================== */

async function renderViewPage(name, opts) {
  const t = ++reqToken;
  const view = resolveView(name);
  if (!view) { fatal(`No view named “${esc(name)}”.`); return; }
  const model = state.project.models[view.model];
  if (!model) { fatal(`View “${esc(name)}” references unknown model “${esc(view.model)}”.`); return; }
  state.view = view;

  highlightNav(view.name);
  renderSubnav(view.name);

  // A form-type view is a create form, not a record list.
  if (view.type === 'form') {
    setHeader(view.label || view.name, model.description || `New ${model.label || model.name}`, '');
    renderFormPage(model, null, formFieldOrder(view, model), rec => location.hash = recordHref(rec));
    return;
  }

  const newBtn = `<a class="btn primary" href="#/records/${encodeURIComponent(view.model)}/new">New ${esc((model.label || model.name).toLowerCase())}</a>`;
  setHeader(view.label || view.name, model.description || `${model.label || model.name} · ${view.type} view`, newBtn);

  const filters = opts.filters || {};
  const chips = Object.entries(filters).map(([field, value]) => {
    const rest = filterParams(Object.fromEntries(Object.entries(filters).filter(([f]) => f !== field)));
    const clearHref = `#/views/${encodeURIComponent(view.name)}?page=1${opts.q ? '&q=' + encodeURIComponent(opts.q) : ''}${rest}`;
    const label = model.fields[field]?.label || field;
    return `<span class="filter-chip">${esc(label)}: ${esc(String(value))}<a href="${clearHref}" title="Clear filter">×</a></span>`;
  }).join('');
  const placeholder = `Search ${esc((model.label || model.name).toLowerCase())}…`;
  $('#page').innerHTML =
    `<div class="toolbar"><input class="search" id="search" type="search" placeholder="${placeholder}" value="${esc(opts.q)}">${chips}<span class="count" id="count"></span></div>` +
    `<div id="viewBody"><div class="loading">Loading records…</div></div>`;

  // Debounced server-side search: update the hash (replaceState → no history
  // spam), reset to page 1, and refetch.
  let debounce;
  const input = $('#search');
  input.oninput = () => {
    clearTimeout(debounce);
    debounce = setTimeout(() => {
      const q = input.value.trim();
      const href = `#/views/${encodeURIComponent(view.name)}?page=1${q ? '&q=' + encodeURIComponent(q) : ''}${filterParams(filters)}`;
      history.replaceState(null, '', href);
      fetchAndRender(view, model, 1, q, filters);
    }, 250);
  };

  await fetchAndRender(view, model, opts.page, opts.q, filters, t);
}

// `f.<field>=<value>` hash-param string for a filter set (leading &).
function filterParams(filters) {
  return Object.entries(filters || {}).map(([field, value]) =>
    `&f.${encodeURIComponent(field)}=${encodeURIComponent(value)}`).join('');
}

async function fetchAndRender(view, model, page, q, filters, token) {
  const t = token ?? ++reqToken;
  const body = $('#viewBody');
  if (!body) return;
  const isReal = !!state.project.views[view.name];
  const route = isReal
    ? `/api/views/${encodeURIComponent(view.name)}/records`
    : `/api/models/${encodeURIComponent(view.model)}/records`;
  const entries = Object.entries(filters || {});
  const filterParam = entries.length
    ? '&filter=' + encodeURIComponent(JSON.stringify(entries.map(([field, value]) => ({ field, op: 'eq', value }))))
    : '';
  try {
    const result = await api(`${route}?page=${page}&q=${encodeURIComponent(q || '')}${filterParam}`);
    if (t !== reqToken) return;
    const count = $('#count');
    if (count) count.textContent = `${result.total} record${result.total === 1 ? '' : 's'}`;
    // Grouped list results carry `groups` instead of a flat `records` page.
    const empty = result.grouped ? !result.groups.length : !result.records.length;
    if (empty) {
      body.innerHTML = q || entries.length
        ? `<div class="empty">No records match.</div>`
        : `<div class="empty">No records yet.<br><a class="btn primary" href="#/records/${encodeURIComponent(view.model)}/new">New ${esc((model.label || model.name).toLowerCase())}</a></div>`;
      return;
    }
    await ensureViewRefs(view, model);
    if (t !== reqToken) return;
    resetPageDynamics();
    body.innerHTML = renderView(view, model, result)
      + (result.grouped ? '' : paginationHtml(view, result, q, filters));
    wireDynamic(view, model, result);
    fillMarkdown();
    fillResources();
  } catch (e) {
    if (t !== reqToken) return;
    body.innerHTML = `<div class="error">${esc(e.message)}</div>`;
  }
}

// Dispatch to the per-type renderer. Renderers return an HTML string.
function renderView(view, model, result) {
  if (result.grouped) return renderGroupedList(view, model, result);
  const records = result.records;
  switch (view.type) {
    case 'board': return renderBoard(view, model, records);
    case 'cards': return renderCards(view, model, records);
    case 'calendar': return `<div id="calMount"></div>`; // wired in wireDynamic
    case 'timeline': return renderTimeline(view, model, records);
    case 'tree': return renderTree(view, model, records);
    case 'table':
    default: return renderTable(view, model, records);
  }
}

// Post-render wiring for renderers that need JS (calendar month nav).
function wireDynamic(view, model, result) {
  if (view.type === 'calendar') {
    const mount = $('#calMount');
    if (mount) renderCalendar(mount, view, model, result.records);
  } else if (view.type === 'board') {
    wireBoard();
  }
  if (result.grouped && view.check && view.sortable) wireGroupedSort();
}

// Drag-to-reorder one list of `.check-item` rows: dropping renumbers the
// visible rows (1..N) into `field` via PUT (see persistOrder). Dragging is
// scoped to a single list — `dragItem` lives in this closure, so a drag begun
// in one list is invisible to any other, which keeps grouped lists independent.
function wireSortableList(list, modelName, field) {
  let dragItem = null;
  list.addEventListener('dragstart', e => {
    const item = e.target.closest('.check-item');
    if (!item || item.parentElement !== list) return;
    dragItem = item;
    item.classList.add('dragging');
    e.dataTransfer.effectAllowed = 'move';
    try { e.dataTransfer.setData('text/plain', item.dataset.key); } catch { /* Safari */ }
  });
  list.addEventListener('dragend', () => {
    if (dragItem) dragItem.classList.remove('dragging');
    dragItem = null;
  });
  list.addEventListener('dragover', e => {
    if (!dragItem) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    const after = itemAfter(list, e.clientY);
    if (after == null) list.appendChild(dragItem);
    else list.insertBefore(dragItem, after);
  });
  list.addEventListener('drop', e => {
    if (!dragItem) return;
    e.preventDefault();
    persistOrder(list, modelName, field);
  });
}

// Grouped checklist views: each group is its own sortable list (drag within a
// group only), all rewriting the same integer field.
function wireGroupedSort() {
  const container = $('#viewBody .grouped-checklist');
  if (!container || !container.dataset.sort) return;
  container.querySelectorAll('.check-list.sortable').forEach(list =>
    wireSortableList(list, container.dataset.model, container.dataset.sort));
}

// Resource-block checklists on a record detail page hydrate after the page
// builds (see fillResources), so wire them once their rows exist. Idempotent:
// a re-render or a repeated fillResources pass skips already-wired lists.
function wireResourceSort() {
  document.querySelectorAll('.dres .check-sortable').forEach(list => {
    if (list.dataset.wired) return;
    list.dataset.wired = '1';
    wireSortableList(list, list.dataset.model, list.dataset.sort);
  });
}

// The row a drop at vertical position y should land before (null → append).
function itemAfter(list, y) {
  const items = [...list.querySelectorAll('.check-item:not(.dragging)')];
  for (const item of items) {
    const box = item.getBoundingClientRect();
    if (y < box.top + box.height / 2) return item;
  }
  return null;
}

// Renumber a group's rows 1..N and persist the ones whose value changed.
async function persistOrder(list, modelName, field) {
  const items = [...list.querySelectorAll('.check-item')];
  const changed = [];
  items.forEach((item, i) => {
    const next = String(i + 1);
    if (item.dataset.sortval !== next) changed.push({ item, next });
  });
  if (!changed.length) return;
  list.classList.add('saving');
  try {
    for (const { item, next } of changed) {
      const saved = await api(`/api/models/${encodeURIComponent(modelName)}/record?key=${encodeURIComponent(item.dataset.key)}`, {
        method: 'PUT',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ revision: item.dataset.revision, values: { [field]: Number(next) } }),
      });
      item.dataset.key = saved.key;
      item.dataset.revision = saved.revision ?? '';
      item.dataset.sortval = next;
    }
    router(); // refresh so grouping/order and top-N reflect the new ranking
  } catch (e) {
    list.classList.remove('saving');
    alert(e.message);
    router();
  }
}

function viewFields(view, model) {
  return view.fields && view.fields.length ? view.fields : Object.keys(model.fields);
}

/* ---- table ---- */
function tableHead(fields, model) {
  return fields.map(f => `<th>${esc(model.fields[f]?.label || f)}</th>`).join('');
}

function tableRows(fields, model, records) {
  return records.map(r => {
    const cells = fields.map(f => {
      const def = model.fields[f];
      // Reference cells stay as inline links; other cells become a full-cell
      // link to the record so the whole row is clickable (and middle-clicks).
      if (def?.type === 'reference') return `<td>${formatValue(r.values[f], def)}</td>`;
      return `<td><a class="cell" href="${recordHref(r)}">${formatValue(r.values[f], def)}</a></td>`;
    }).join('');
    return `<tr>${cells}</tr>`;
  }).join('');
}

function renderTable(view, model, records) {
  const fields = viewFields(view, model);
  return `<div class="table-shell"><table><thead><tr>${tableHead(fields, model)}</tr></thead><tbody>${tableRows(fields, model, records)}</tbody></table></div>`;
}

/* ---- grouped list (table/checklist with group_by, optional per-group limit) ---- */
function renderGroupedList(view, model, result) {
  const groups = result.groups || [];
  if (!groups.length) return `<div class="empty">Nothing here yet.</div>`;
  const noun = esc((view.group_by || 'group').split('.').pop());
  const groupLabel = g => (g.value == null || g.value === '') ? `No ${noun}` : esc(String(g.value));

  // Checklist style: big-checkbox rows (the checkbox is the only control),
  // optionally drag-sortable to rewrite an integer field.
  if (view.check) {
    const sortable = !!view.sortable;
    const sections = groups.map(g => {
      const rows = g.records.map(r => checklistRow(r, model, view)).join('');
      const more = g.total - g.records.length;
      const moreNote = more > 0 ? `<div class="dres-more">and ${more} more…</div>` : '';
      return `<section class="group">`
        + `<div class="board-col-head"><span>${groupLabel(g)}</span><span class="n">${g.total}</span></div>`
        + `<div class="check-list${sortable ? ' sortable' : ''}">${rows}${moreNote}</div>`
        + `</section>`;
    }).join('');
    return `<div class="grouped-checklist" data-model="${esc(model.name)}"`
      + `${sortable ? ` data-sort="${esc(view.sortable)}"` : ''}>${sections}</div>`;
  }

  // Table style (grouped rows).
  const fields = viewFields(view, model);
  return groups.map(g => {
    const more = g.total - g.records.length;
    const moreRow = more > 0
      ? `<tr class="group-more"><td colspan="${fields.length}" class="dres-more">and ${more} more…</td></tr>`
      : '';
    return `<section class="group">`
      + `<div class="board-col-head"><span>${groupLabel(g)}</span><span class="n">${g.total}</span></div>`
      + `<div class="table-shell"><table><thead><tr>${tableHead(fields, model)}</tr></thead>`
      + `<tbody>${tableRows(fields, model, g.records)}${moreRow}</tbody></table></div>`
      + `</section>`;
  }).join('');
}

// One checklist row: a big checkbox (the only interactive element — no title
// link) plus the record's title and any non-title view fields as muted meta.
function checklistRow(r, model, view) {
  const done = !!r.values[view.check];
  const index = checkPayloads.push({ record: r, field: view.check }) - 1;
  const meta = viewFields(view, model)
    .filter(f => f !== 'title' && f !== model.title && f !== view.check)
    .map(f => {
      const def = model.fields[f];
      if (def?.type === 'reference') return esc(refName(r.values[f], def.reference));
      const v = r.values[f];
      return v == null || v === '' ? '' : esc(String(v));
    })
    .filter(Boolean)
    .join(' · ');
  const sortAttrs = view.sortable
    ? ` draggable="true" data-key="${esc(r.key)}" data-revision="${esc(r.revision ?? '')}" data-sortval="${esc(r.values[view.sortable] ?? '')}"`
    : '';
  return `<div class="check-item${done ? ' done' : ''}"${sortAttrs}>`
    + `<input type="checkbox" class="check-toggle big" data-check="${index}"${done ? ' checked' : ''} aria-label="Mark done">`
    + `<span class="check-body"><span class="check-title">${esc(recordTitle(r, model))}</span>`
    + (meta ? `<span class="check-meta">${meta}</span>` : '')
    + `</span></div>`;
}

/* ---- board (kanban) ---- */
function renderBoard(view, model, records) {
  const gb = view.group_by;
  const def = gb && model.fields[gb];
  if (!def) return renderTable(view, model, records);
  const label = def.label || gb;
  const choices = (def.validation?.choices || []).map(String);
  const buckets = new Map();
  const order = [...choices];
  const NONE = ' none';
  for (const r of records) {
    let v = r.values[gb];
    v = (v == null || v === '') ? NONE : String(v);
    if (v !== NONE && !order.includes(v)) order.push(v); // out-of-choice value
    (buckets.get(v) || buckets.set(v, []).get(v)).push(r);
  }
  order.push(NONE);
  const block = itemBlock(view, model);
  // Each card is a draggable handle carrying the record's key and revision,
  // which wireBoard() needs to persist a move.
  const card = r =>
    `<div class="board-card" draggable="true" data-key="${esc(r.key)}" data-revision="${esc(r.revision ?? '')}">` +
    `${block ? recordItem(r, model, block) : recordCard(r, model, viewFields(view, model))}</div>`;
  const cols = order.map(v => {
    const items = buckets.get(v) || [];
    if (!items.length && v === NONE) return '';
    const title = v === NONE ? `No ${esc(label.toLowerCase())}` : esc(v);
    // The value a card takes on when dropped here ('' clears the field).
    const value = v === NONE ? '' : v;
    const cards = items.map(card).join('') || '<div class="board-empty">Empty</div>';
    return `<div class="board-col" data-value="${esc(value)}">` +
      `<div class="board-col-head"><span>${title}</span><span class="n">${items.length}</span></div>` +
      `<div class="board-col-body">${cards}</div></div>`;
  }).join('');
  return `<div class="board" data-model="${esc(model.name)}" data-field="${esc(gb)}">${cols}</div>`;
}

// Wire the board's drag-and-drop: dropping a card on a column writes that
// column's value to the record's group_by field through the merge-update API,
// with an optimistic move that reverts if the write fails.
function wireBoard() {
  const board = $('#viewBody .board');
  if (!board) return;
  const { model: modelName, field } = board.dataset;
  // Links inside a card are draggable by default and would become the drag
  // source (dragging their URL); the wrapper owns the drag instead.
  board.querySelectorAll('.board-card a').forEach(a => { a.draggable = false; });

  let dragCard = null;
  board.addEventListener('dragstart', e => {
    const card = e.target.closest('.board-card');
    if (!card) return;
    dragCard = card;
    card.classList.add('dragging');
    e.dataTransfer.effectAllowed = 'move';
    try { e.dataTransfer.setData('text/plain', card.dataset.key); } catch { /* Safari */ }
  });
  board.addEventListener('dragend', () => {
    if (dragCard) dragCard.classList.remove('dragging');
    dragCard = null;
    board.querySelectorAll('.drag-over').forEach(c => c.classList.remove('drag-over'));
  });
  board.querySelectorAll('.board-col').forEach(col => {
    col.addEventListener('dragover', e => {
      if (!dragCard) return; // ignore drags that didn't start on a card
      e.preventDefault();
      e.dataTransfer.dropEffect = 'move';
      col.classList.add('drag-over');
    });
    col.addEventListener('dragleave', e => {
      if (!col.contains(e.relatedTarget)) col.classList.remove('drag-over');
    });
    col.addEventListener('drop', e => {
      if (!dragCard) return;
      e.preventDefault();
      col.classList.remove('drag-over');
      if (dragCard.closest('.board-col') !== col) moveBoardCard(dragCard, col, modelName, field);
    });
  });
}

// Move a card to a column: patch the DOM immediately, then persist. On failure
// the card returns to its origin. The saved record's key/revision replace the
// card's (updating group_by can change both when it feeds the storage path).
async function moveBoardCard(card, col, modelName, field) {
  const from = card.closest('.board-col');
  const value = col.dataset.value; // '' → clear the field (the "none" column)
  card.classList.add('saving');
  col.querySelector('.board-col-body').appendChild(card);
  updateBoardCounts(from);
  updateBoardCounts(col);
  try {
    const saved = await api(`/api/models/${encodeURIComponent(modelName)}/record?key=${encodeURIComponent(card.dataset.key)}`, {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ revision: card.dataset.revision, values: { [field]: value === '' ? null : value } }),
    });
    card.dataset.key = saved.key;
    card.dataset.revision = saved.revision ?? '';
    card.classList.remove('saving');
  } catch (e) {
    from.querySelector('.board-col-body').appendChild(card);
    updateBoardCounts(from);
    updateBoardCounts(col);
    card.classList.remove('saving');
    alert(e.message);
  }
}

// Refresh a column's count badge and its empty placeholder after cards move.
function updateBoardCounts(col) {
  if (!col) return;
  const body = col.querySelector('.board-col-body');
  const cards = body.querySelectorAll('.board-card');
  const badge = col.querySelector('.board-col-head .n');
  if (badge) badge.textContent = cards.length;
  const empty = body.querySelector('.board-empty');
  if (!cards.length && !empty) body.insertAdjacentHTML('beforeend', '<div class="board-empty">Empty</div>');
  else if (cards.length && empty) empty.remove();
}

/* ---- cards ---- */
function renderCards(view, model, records) {
  const block = itemBlock(view, model);
  if (block) return `<div class="gallery">${records.map(r => recordItem(r, model, block)).join('')}</div>`;
  const fields = viewFields(view, model);
  return `<div class="gallery">${records.map(r => recordCard(r, model, fields)).join('')}</div>`;
}

/* ---- calendar ---- */
function calendarDateField(view, model) {
  for (const o of (view.query?.order || [])) {
    const t = model.fields[o.field]?.type;
    if (t === 'date' || t === 'date_time') return o.field;
  }
  for (const f of viewFields(view, model)) {
    const t = model.fields[f]?.type;
    if (t === 'date' || t === 'date_time') return f;
  }
  return null;
}

// Calendar views may name multiple date fields. The first non-empty, valid
// value wins per record (for example sent_at before scheduled_at). Without an
// explicit list, preserve the original query-order/view-field heuristic.
function calendarDateFields(view, model) {
  const configured = view.calendar?.date_fields || [];
  if (configured.length) return configured;
  const inferred = calendarDateField(view, model);
  return inferred ? [inferred] : [];
}

function localDateKey(date) {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`;
}

// Date-only fields are calendar literals and must not move under timezone
// conversion. Date-time fields are placed on the browser's local day.
function calendarDate(value, type) {
  if (value == null || value === '') return null;
  const raw = String(value);
  if (type === 'date') {
    const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(raw.slice(0, 10));
    if (match) {
      const date = new Date(+match[1], +match[2] - 1, +match[3]);
      return { date, key: match[0] };
    }
  }
  const date = new Date(value);
  return isNaN(date) ? null : { date, key: localDateKey(date) };
}

function clockLabel(date, type) {
  if (type !== 'date_time') return 'All day';
  return date.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' });
}

function calendarRecordDate(view, model, record) {
  for (const field of calendarDateFields(view, model)) {
    const raw = record.values[field];
    const type = model.fields[field]?.type;
    const parsed = calendarDate(raw, type);
    if (parsed) return { r: record, field, raw, type, ...parsed };
  }
  return null;
}

function calendarItemStyle(view, record) {
  const field = view.calendar?.color_by;
  if (!field) return '';
  const palette = view.calendar?.colors || {};
  const raw = record.values[field];
  const value = Array.isArray(raw) ? raw[0] : raw;
  const color = palette[String(value ?? '')] || palette.default;
  return color ? ` style="--cal-color:${esc(color)}"` : '';
}

function calendarItemPast(view, entry) {
  if (!view.calendar?.dim_past) return false;
  if (entry.type === 'date') return entry.key < localDateKey(new Date());
  return entry.date.getTime() < Date.now();
}

function calendarEvent(view, model, block, entry) {
  const time = entry.type === 'date_time'
    ? `<span class="cal-ev-time">${esc(clockLabel(entry.date, entry.type))}</span>`
    : '';
  const past = calendarItemPast(view, entry) ? ' past' : '';
  const style = calendarItemStyle(view, entry.r);
  if (block) {
    return `<div class="cal-ev${past}"${style} title="${esc(recordTitle(entry.r, model))}">${time}${recordItem(entry.r, model, block)}</div>`;
  }
  return `<a class="cal-ev${past}"${style} href="${recordHref(entry.r)}" title="${esc(recordTitle(entry.r, model))}">` +
    `${time}<span class="cal-ev-title">${esc(recordTitle(entry.r, model))}</span></a>`;
}

function renderCalendar(mount, view, model, records) {
  const dated = [], undated = [];
  for (const r of records) {
    const entry = calendarRecordDate(view, model, r);
    if (entry) dated.push(entry);
    else undated.push(r);
  }
  const byDay = new Map();
  for (const d of dated) (byDay.get(d.key) || byDay.set(d.key, []).get(d.key)).push(d);

  // Calendar navigation always starts where the user is, even when the first
  // query result lives in a past or future month.
  const today = new Date();
  const todayKey = localDateKey(today);
  let year = today.getFullYear(), month = today.getMonth();
  const block = itemBlock(view, model);
  const agendaHtml = view.agenda ? renderAgenda(view, model, dated, todayKey) : '';
  const undatedHtml = undated.length
    ? `<section class="undated"><h2>Undated</h2><div class="gallery">${undated.map(r => block ? recordItem(r, model, block) : recordCard(r, model, viewFields(view, model))).join('')}</div></section>`
    : '';

  mount.innerHTML =
    `<section class="cal-paper"><div class="cal-head"><h2 id="calMonth"></h2>` +
    `<div class="cal-nav"><button id="calToday" type="button">Today</button>` +
    `<button id="calPrev" type="button" aria-label="Previous month">‹</button>` +
    `<button id="calNext" type="button" aria-label="Next month">›</button></div></div>` +
    `<div class="cal-grid" id="calGrid"></div></section>${agendaHtml}${undatedHtml}`;

  const draw = () => {
    const monthName = new Date(year, month, 1).toLocaleDateString(undefined, { month: 'long', year: 'numeric' });
    const firstDow = new Date(year, month, 1).getDay();
    const days = new Date(year, month + 1, 0).getDate();
    const dow = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'].map(d => `<div class="cal-dow">${d}</div>`).join('');
    // At least five complete weeks, with adjacent-month dates visible like a
    // printed wall calendar instead of leaving dead blank cells.
    const cellCount = Math.max(35, Math.ceil((firstDow + days) / 7) * 7);
    const start = new Date(year, month, 1 - firstDow);
    let cells = '';
    for (let i = 0; i < cellCount; i++) {
      const date = new Date(start.getFullYear(), start.getMonth(), start.getDate() + i);
      const key = localDateKey(date);
      const outside = date.getMonth() !== month;
      const events = (byDay.get(key) || []).map(entry =>
        calendarEvent(view, model, block, entry)).join('');
      cells += `<div class="cal-cell${outside ? ' outside' : ''}${key === todayKey ? ' today' : ''}">` +
        `<div class="cal-date">${date.getDate()}</div>${events}</div>`;
    }
    mount.querySelector('#calMonth').textContent = monthName;
    mount.querySelector('#calGrid').innerHTML = dow + cells;
  };

  mount.querySelector('#calToday').onclick = () => {
    const now = new Date(); year = now.getFullYear(); month = now.getMonth(); draw(); fillMarkdown();
  };
  mount.querySelector('#calPrev').onclick = () => {
    month--; if (month < 0) { month = 11; year--; } draw(); fillMarkdown();
  };
  mount.querySelector('#calNext').onclick = () => {
    month++; if (month > 11) { month = 0; year++; } draw(); fillMarkdown();
  };
  draw();
}

// A calendar agenda is deliberately grouped by local day and starts at the
// beginning of today. This keeps all of today's schedule visible even after a
// time slot has passed, while excluding stale scheduled records from prior days.
function renderAgenda(view, model, dated, todayKey) {
  const groups = new Map();
  for (const entry of dated) {
    if (entry.key < todayKey) continue;
    (groups.get(entry.key) || groups.set(entry.key, []).get(entry.key)).push(entry);
  }
  const keys = [...groups.keys()].sort();
  if (!keys.length) {
    return `<section class="agenda"><div class="agenda-head"><h2>Upcoming</h2></div>` +
      `<div class="empty agenda-empty">Nothing scheduled from today forward.</div></section>`;
  }
  const block = agendaBlock(view, model);
  const today = new Date();
  const tomorrow = new Date(today.getFullYear(), today.getMonth(), today.getDate() + 1);
  const tomorrowKey = localDateKey(tomorrow);
  const sections = keys.map(key => {
    const [y, m, d] = key.split('-').map(Number);
    const date = new Date(y, m - 1, d);
    const prefix = key === todayKey ? 'Today · ' : key === tomorrowKey ? 'Tomorrow · ' : '';
    const label = prefix + date.toLocaleDateString(undefined, {
      weekday: 'long', month: 'long', day: 'numeric',
    });
    const entries = groups.get(key).slice().sort((a, b) => a.date - b.date);
    const rows = entries.map(entry => {
      const item = block
        ? recordItem(entry.r, model, block)
        : `<a class="card" href="${recordHref(entry.r)}"><div class="card-title">${esc(recordTitle(entry.r, model))}</div>` +
          `<div class="card-meta">${secondaryMeta(entry.r, model, view, entry.field)}</div></a>`;
      const past = calendarItemPast(view, entry) ? ' past' : '';
      return `<div class="agenda-row${past}"${calendarItemStyle(view, entry.r)}>` +
        `<time class="agenda-time" datetime="${esc(entry.raw)}">${esc(clockLabel(entry.date, entry.type))}</time>` +
        `<div class="agenda-item">${item}</div></div>`;
    }).join('');
    return `<section class="agenda-day"><h3>${esc(label)}</h3><div class="agenda-rows">${rows}</div></section>`;
  }).join('');
  return `<section class="agenda"><div class="agenda-head"><h2>Upcoming</h2></div>${sections}</section>`;
}

/* ---- timeline ---- */
function renderTimeline(view, model, records) {
  const field = calendarDateField(view, model);
  const type = model.fields[field]?.type;
  const dated = [], undated = [];
  for (const r of records) {
    const raw = field && r.values[field];
    const parsed = calendarDate(raw, type);
    if (parsed) dated.push({ r, ...parsed }); else undated.push(r);
  }
  const block = itemBlock(view, model);
  const item = r => block
    ? recordItem(r, model, block)
    : `<a class="card" href="${recordHref(r)}"><div class="card-title">${esc(recordTitle(r, model))}</div>` +
      `<div class="card-meta">${secondaryMeta(r, model, view, field)}</div></a>`;
  const row = (r, dateHtml) =>
    `<div class="tl-row"><div class="tl-date">${dateHtml}</div>` +
    `<div class="tl-body">${item(r)}</div></div>`;
  const rows = dated.map(entry => {
    const dt = `<span class="d">${entry.date.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })}</span>` +
      `${entry.date.getFullYear()}${type === 'date_time' ? `<br>${esc(clockLabel(entry.date, type))}` : ''}`;
    return row(entry.r, dt);
  }).join('');
  const tail = undated.map(r => row(r, '<span class="d">Undated</span>')).join('');
  return `<div class="timeline">${rows}${tail}</div>`;
}

function secondaryMeta(record, model, view, skipField) {
  return viewFields(view, model)
    .filter(f => f !== skipField && f !== 'title' && f !== 'name' && model.fields[f]?.type !== 'asset')
    .slice(0, 2)
    .map(f => `<div><span class="k">${esc(model.fields[f]?.label || f)}</span> ${formatValue(record.values[f], model.fields[f])}</div>`)
    .join('');
}

/* ---- tree ---- */
function renderTree(view, model, records) {
  const gb = view.group_by;
  const def = gb && model.fields[gb];
  const refField = def?.reference?.field;
  if (!def || !refField) return renderTable(view, model, records);

  const idOf = r => r.values[refField];
  const parentOf = r => r.values[gb];
  const ids = new Set(records.map(idOf).filter(v => v != null));
  const children = new Map(); // parentValue → [records]
  const roots = [];
  for (const r of records) {
    const p = parentOf(r);
    if (p == null || p === '' || !ids.has(p)) roots.push(r);
    else (children.get(p) || children.set(p, []).get(p)).push(r);
  }
  const fields = viewFields(view, model);
  const block = itemBlock(view, model);
  const visited = new Set();
  const node = r => {
    if (visited.has(r.key)) return ''; // cycle guard
    visited.add(r.key);
    const kids = children.get(idOf(r)) || [];
    const kidsHtml = kids.length ? `<div class="tree-children">${kids.map(node).join('')}</div>` : '';
    const card = block ? recordItem(r, model, block) : recordCard(r, model, fields);
    return `<div class="tree-node">${card}${kidsHtml}</div>`;
  };
  return `<div class="tree">${roots.map(node).join('')}</div>`;
}

/* =========================================================================
 * Record detail
 * ====================================================================== */

async function showRecord(modelName, key, prefetched, activeTab) {
  const t = ++reqToken;
  const model = state.project.models[modelName];
  if (!model) { fatal(`Unknown model “${esc(modelName)}”.`); return; }
  keepNavForRecord(modelName);
  setHeader('Loading…', '', '');
  $('#page').innerHTML = '<div class="loading">Loading record…</div>';

  const hasOutputs = Object.keys(model.outputs || {}).length > 0;
  try {
    const [record, rels, outs] = await Promise.all([
      prefetched
        ? Promise.resolve(prefetched)
        : api(`/api/models/${encodeURIComponent(modelName)}/record?key=${encodeURIComponent(key)}`),
      api(`/api/models/${encodeURIComponent(modelName)}/record/relationships?key=${encodeURIComponent(key)}`).catch(() => ({ outbound: [], inbound: [] })),
      hasOutputs
        ? api(`/api/models/${encodeURIComponent(modelName)}/record/outputs?key=${encodeURIComponent(key)}`).catch(() => ({ outputs: [] }))
        : Promise.resolve({ outputs: [] }),
    ]);
    if (t !== reqToken) return;
    // Legacy #/records/ URLs re-route to the canonical nested URL, which
    // also picks in-tab rendering for records under a tabbed ancestor.
    const canonical = nestedHref(record);
    if (canonical && location.hash.startsWith('#/records/')) {
      location.replace(canonical);
      return;
    }
    const crumbs = await breadcrumbTrail(model, rels);
    if (t !== reqToken) return;
    renderRecord(model, record, rels, outs, crumbs, activeTab);
  } catch (e) {
    if (t !== reqToken) return;
    fatal(e.message);
  }
}

// The ancestor chain for a nested model (model.parent names the reference
// field), nearest parent last. Each hop past the first needs that ancestor's
// own relationships to find the next parent up.
async function breadcrumbTrail(model, rels) {
  const crumbs = [];
  let currentModel = model;
  let currentRels = rels;
  for (let depth = 0; depth < 5; depth++) {
    const parentField = currentModel.parent;
    if (!parentField) break;
    const link = (currentRels?.outbound || []).find(l => l.field === parentField);
    if (!link) break;
    const parentModel = state.project.models[link.record.model];
    if (!parentModel) break;
    crumbs.unshift({ record: link.record, model: parentModel });
    if (!parentModel.parent) break;
    currentModel = parentModel;
    currentRels = await api(`/api/models/${encodeURIComponent(link.record.model)}/record/relationships?key=${encodeURIComponent(link.record.key)}`).catch(() => null);
    if (!currentRels) break;
  }
  return crumbs;
}

function renderRecord(model, record, rels, outs, crumbs = [], activeTab = null) {
  resetPageDynamics();
  const title = recordTitle(record, model);
  setHeader(title, '', recordActions(record));
  // With a trail, the final segment is the record itself (its own title, not
  // the collection's label); without one, the model badge says what this is.
  $('#subtitle').innerHTML = crumbs.length
    ? `<span class="crumbs">${crumbs.map(c =>
        `<a href="${recordHref(c.record)}">${esc(recordTitle(c.record, c.model))}</a>`
      ).join('<span class="crumb-sep">›</span>')}<span class="crumb-sep">›</span><span class="crumb-current">${esc(title)}</span></span>`
    : `<span class="badge plain">${esc(model.label || model.name)}</span>`;

  // With tabs, each tab renders one display block and the first tab is the
  // default. Without them, a `detail` display block lays out the whole page;
  // without that, the default is a stacked label-over-value grid plus the
  // automatic related/outputs sections.
  const ctx = buildRecordCtx(model, record, rels, outs);
  let tabsBar = '';
  let inner;
  if (model.tabs?.length) {
    const current = model.tabs.find(tb => tb_route(tb) === activeTab) || model.tabs[0];
    tabsBar = recordTabsBar(model, record, tb_route(current));
    inner = renderDetailBlock(model, ctx, current.block);
  } else {
    inner = renderDetailBlock(model, ctx, 'detail');
  }
  $('#page').innerHTML = `${tabsBar}<div class="detail">${inner}</div>`;
  fillMarkdown();
  fillResources();
  wireDelete(record, title);
}

// Outbound reference links, resolved to canonical records where the
// relationships endpoint gives us a match on field name.
function buildRecordCtx(model, record, rels, outs) {
  const outboundByField = new Map();
  for (const link of (rels?.outbound || [])) {
    (outboundByField.get(link.field) || outboundByField.set(link.field, []).get(link.field)).push(link.record);
  }
  return { model, record, rels, outs, outboundByField, depth: 0 };
}

function renderDetailBlock(model, ctx, name) {
  const block = model.display?.[name];
  return block
    ? blockNodes(block).map(node => renderNode(node, ctx)).join('')
    : defaultDetail(model, ctx.record, ctx) + relatedSection(model, ctx.rels) + outputsSection(model, ctx.outs);
}

function recordTabsBar(model, record, activeRoute) {
  if (!model.tabs?.length) return '';
  const base = recordHref(record);
  return `<div class="rec-tabs">` + model.tabs.map((tb, idx) => {
    const href = idx === 0 ? base : `${base}/${encodeURIComponent(tb_route(tb))}`;
    return `<a class="rec-tab${tb_route(tb) === activeRoute ? ' active' : ''}" href="${href}">${esc(tb.label)}</a>`;
  }).join('') + `</div>`;
}

function recordActions(record) {
  return `<a class="btn" href="${recordHref(record)}/edit">Edit</a>` +
    `<button class="btn danger" id="deleteBtn">Delete</button>`;
}

function wireDelete(record, title) {
  const btn = $('#deleteBtn');
  if (!btn) return;
  btn.onclick = async () => {
    if (!confirm(`Delete “${title}”? This removes it from disk.`)) return;
    try {
      await api(`/api/models/${encodeURIComponent(record.model)}/record?key=${encodeURIComponent(record.key)}&revision=${encodeURIComponent(record.revision)}`, { method: 'DELETE' });
      location.hash = backToViewHref();
    } catch (e) { alert(e.message); }
  };
}

// Default record page: every field as a label-over-value cell in a
// two-column grid; long-form values span the full width.
function defaultDetail(model, record, ctx) {
  const cells = Object.entries(model.fields).map(([name, def]) => {
    const value = record.values[name];
    const wide = def.type === 'text' || def.type === 'json' || def.type === 'asset';
    const inner = value == null || value === ''
      ? '<span class="dash">—</span>'
      : detailValue(value, def, name, ctx.outboundByField);
    return `<div class="kv${wide ? ' kv-wide' : ''}"><div class="k">${esc(def.label || name)}</div><div class="v">${inner}</div></div>`;
  }).join('');
  return `<div class="dgrid detail-default" style="--cols-base:1;--cols-md:2;--cgap:32px;--rgap:22px">${cells}</div>`;
}

// Detail-page value formatting: richer than a table cell.
// Optional `ctx` carries display flags (e.g. linkAssets for nested gallery cards).
function detailValue(value, def, name, outboundByField, ctx) {
  if (value == null || value === '') return '<span class="dash">—</span>';
  switch (def.type) {
    case 'text': return `<div class="prose">${esc(value)}</div>`;
    case 'asset': return assetPreview(value, false, {
      link: ctx?.linkAssets !== false,
      fill: !!ctx?.assetFill,
    });
    case 'boolean': return value ? 'Yes' : 'No';
    case 'json': return `<pre class="json">${esc(JSON.stringify(value, null, 2))}</pre>`;
    case 'date': case 'date_time': return esc(fmtDate(value, def.type));
    case 'reference': {
      const resolved = outboundByField.get(name);
      if (resolved && resolved.length) {
        return resolved.map(rec => `<a class="ref" href="${recordHref(rec)}">${esc(recordTitle(rec, state.project.models[rec.model]))}</a>`).join(', ');
      }
      return refLink(value, def);
    }
  }
  if (Array.isArray(value)) return `<span class="badge-row">${value.map(x => `<span class="badge plain">${esc(x)}</span>`).join('')}</span>`;
  return esc(value);
}

// Inbound relationships → tabs grouped by (source model, field).
function relatedSection(model, rels) {
  const inbound = rels.inbound || [];
  if (!inbound.length) return '';
  const groups = new Map(); // `${srcModel}|${field}` → { srcModel, field, records }
  for (const link of inbound) {
    const src = link.record.model;
    const gkey = `${src}|${link.field}`;
    if (!groups.has(gkey)) groups.set(gkey, { srcModel: src, field: link.field, records: [] });
    groups.get(gkey).records.push(link.record);
  }
  const list = [...groups.values()];
  const tabs = list.map((g, i) => {
    const srcModel = state.project.models[g.srcModel];
    // Disambiguate when the source model has 2+ references into this model.
    const refCount = srcModel ? Object.values(srcModel.fields).filter(f => f.type === 'reference' && f.reference?.model === model.name).length : 0;
    let label = pluralize(modelLabel(g.srcModel));
    if (refCount >= 2) label += ` · ${srcModel.fields[g.field]?.label || g.field}`;
    return `<button data-tab="${i}"${i === 0 ? ' class="active"' : ''}>${esc(label)} <span class="badge plain">${g.records.length}</span></button>`;
  }).join('');
  const panels = list.map((g, i) => {
    const srcModel = state.project.models[g.srcModel];
    const block = srcModel?.display?.card || null;
    const cards = g.records.map(r =>
      block ? recordItem(r, srcModel, block) : recordCard(r, srcModel, [])).join('');
    return `<div class="tabpanel" data-panel="${i}"${i === 0 ? '' : ' hidden'}>${cards}</div>`;
  }).join('');
  // Tab switching is wired after insertion via event delegation.
  setTimeout(wireTabs, 0);
  return `<div class="related"><div class="tabstrip">${tabs}</div>${panels}</div>`;
}

function wireTabs() {
  const strip = document.querySelector('.related .tabstrip');
  if (!strip) return;
  strip.onclick = e => {
    const btn = e.target.closest('button[data-tab]');
    if (!btn) return;
    const idx = btn.dataset.tab;
    strip.querySelectorAll('button').forEach(b => b.classList.toggle('active', b === btn));
    document.querySelectorAll('.related .tabpanel').forEach(p => { p.hidden = p.dataset.panel !== idx; });
  };
}

function outputsSection(model, outs) {
  if (!Object.keys(model.outputs || {}).length) return '';
  const rows = (outs.outputs || []).map(o => {
    const status = o.exists
      ? `<span class="ok">${o.is_file ? 'file' : 'directory'}</span>`
      : `<span class="missing">not generated</span>`;
    return `<div class="output-row"><span class="badge">${esc(o.name)}</span><span class="path">${esc(o.path)}</span>${status}</div>`;
  }).join('') || '<div class="output-row missing">No outputs generated yet.</div>';
  return `<div class="outputs"><h3>Generated outputs</h3>${rows}</div>`;
}

// After leaving a record, return to the active view if we have one, else home.
function backToViewHref() {
  if (state.view) return `#/views/${encodeURIComponent(state.view.name)}`;
  const d = defaultRoute();
  return d ? '#/' + d.replace(/^\//, '') : '#/';
}
function keepNavForRecord(modelName) {
  // Keep the sidebar/subnav from the view the user came from, if it targets
  // this model. On direct hits, light up the first nav view showing the
  // record's root ancestor (a changelog entry highlights the books view).
  if (state.view && state.view.model === modelName) {
    highlightNav(state.view.name);
    renderSubnav(state.view.name);
    return;
  }
  const models = state.project.models;
  let m = models[modelName];
  for (let depth = 0; depth < 6 && m?.parent; depth++) {
    m = models[m.fields[m.parent]?.reference?.model];
  }
  if (!m) return;
  const view = Object.values(state.project.views).find(v => v.model === m.name);
  if (view) {
    highlightNav(view.name);
    renderSubnav(view.name);
  }
}

/* =========================================================================
 * Forms — create + edit (full page)
 * ====================================================================== */

// Field order for a form-type view: view.fields first, then any required
// model fields not already listed.
function formFieldOrder(view, model) {
  const listed = view.fields && view.fields.length ? [...view.fields] : Object.keys(model.fields);
  for (const [f, def] of Object.entries(model.fields)) {
    if (def.required && !listed.includes(f)) listed.push(f);
  }
  return listed;
}

// Build the per-field inputs. Returns an HTML string; inputs use id "f-<name>".
function buildFormFields(model, record, order) {
  const names = order && order.length ? order : Object.keys(model.fields);
  return names.map(name => {
    const field = model.fields[name];
    if (!field) return '';
    let value = record?.values[name] ?? field.default ?? '';
    if (field.type === 'date_time' && value) {
      const d = new Date(value);
      if (!isNaN(d)) value = d.toISOString().slice(0, 16);
    }
    const req = field.required ? ' required' : '';
    const id = 'f-' + name;
    let input;
    if (field.type === 'boolean') {
      input = `<input id="${esc(id)}" type="checkbox"${value ? ' checked' : ''}>`;
    } else if (field.type === 'enum') {
      const choices = (field.validation?.choices || []).map(c => {
        const s = String(c);
        return `<option${s === String(value) ? ' selected' : ''}>${esc(s)}</option>`;
      }).join('');
      input = `<select id="${esc(id)}"${req}><option value=""></option>${choices}</select>`;
    } else if (field.type === 'text' || field.type === 'json') {
      const text = field.type === 'json' && value !== '' ? JSON.stringify(value, null, 2) : value;
      input = `<textarea id="${esc(id)}"${req}>${esc(text)}</textarea>`;
    } else {
      const type = { integer: 'number', number: 'number', date: 'date', date_time: 'datetime-local' }[field.type] || 'text';
      const ro = field.source?.kind === 'asset' ? ' readonly' : '';
      input = `<input id="${esc(id)}" type="${type}" value="${esc(value)}"${req}${ro}>`;
    }
    const desc = field.description ? ` <small>— ${esc(field.description)}</small>` : '';
    const preview = field.type === 'asset' && value ? assetPreview(value, false) : '';
    return `<div class="field"><label class="flabel" for="${esc(id)}">${esc(field.label || name)}${field.required ? ' *' : ''}${desc}</label>${input}${preview}</div>`;
  }).join('');
}

// Read + coerce field values back out of the form.
function collectValues(model, order) {
  const names = order && order.length ? order : Object.keys(model.fields);
  const values = {};
  for (const name of names) {
    const field = model.fields[name];
    if (!field || field.source?.kind === 'asset') continue; // assets are read-only
    const el = document.getElementById('f-' + name);
    if (!el) continue;
    let value = field.type === 'boolean' ? el.checked : el.value;
    if (value === '') value = null;
    else if (field.type === 'integer') value = parseInt(value, 10);
    else if (field.type === 'number') value = parseFloat(value);
    else if (field.type === 'json') value = JSON.parse(value);
    else if (field.type === 'date_time') value = new Date(value).toISOString();
    values[name] = value;
  }
  return values;
}

async function showEdit(modelName, key) {
  const t = ++reqToken;
  const model = state.project.models[modelName];
  if (!model) { fatal(`Unknown model “${esc(modelName)}”.`); return; }
  keepNavForRecord(modelName);
  setHeader('Loading…', '', '');
  $('#page').innerHTML = '<div class="loading">Loading record…</div>';
  try {
    const record = await api(`/api/models/${encodeURIComponent(modelName)}/record?key=${encodeURIComponent(key)}`);
    if (t !== reqToken) return;
    setHeader(`Edit ${recordTitle(record, model)}`, model.label || model.name, '');
    renderFormPage(model, record, Object.keys(model.fields), rec => location.hash = recordHref(rec), record);
  } catch (e) {
    if (t !== reqToken) return;
    fatal(e.message);
  }
}

function showCreate(modelName, query) {
  ++reqToken;
  const model = state.project.models[modelName];
  if (!model) { fatal(`Unknown model “${esc(modelName)}”.`); return; }
  keepNavForRecord(modelName);
  setHeader(`New ${model.label || model.name}`, model.description || '', '');
  // Query params matching field names prefill the form (used by resource
  // `create` actions to fill in the back-reference).
  const prefill = {};
  for (const [name, value] of (query || new URLSearchParams())) {
    if (model.fields[name]) prefill[name] = value;
  }
  const seed = Object.keys(prefill).length ? { values: prefill } : null;
  renderFormPage(model, seed, Object.keys(model.fields), rec => location.hash = recordHref(rec));
}

// Shared form page for create + edit. `record` seeds initial values;
// `existing` (if present) enables PUT + Delete.
function renderFormPage(model, record, order, onSaved, existing) {
  const editing = existing;
  const deleteBtn = editing ? '<button type="button" class="btn danger" id="fDelete">Delete</button>' : '';
  const cancelHref = editing ? recordHref(editing) : backToViewHref();
  $('#page').innerHTML =
    `<form class="record-form" id="recordForm" novalidate>` +
    `<p class="form-error" id="formError"></p>` +
    buildFormFields(model, editing || record, order) +
    `<div class="form-actions">${deleteBtn ? `<span class="spacer">${deleteBtn}</span>` : '<span class="spacer"></span>'}` +
    `<a class="btn" href="${cancelHref}">Cancel</a>` +
    `<button type="submit" class="btn primary">Save</button></div></form>`;

  const errBox = $('#formError');
  $('#recordForm').onsubmit = async e => {
    e.preventDefault();
    errBox.textContent = '';
    let values;
    try { values = collectValues(model, order); }
    catch (err) { errBox.textContent = 'Invalid input: ' + err.message; return; }
    const body = JSON.stringify({ revision: editing?.revision ?? null, values });
    const method = editing ? 'PUT' : 'POST';
    const url = editing
      ? `/api/models/${encodeURIComponent(model.name)}/record?key=${encodeURIComponent(editing.key)}`
      : `/api/models/${encodeURIComponent(model.name)}/records`;
    try {
      const saved = await api(url, { method, headers: { 'content-type': 'application/json' }, body });
      onSaved(saved || editing);
    } catch (err) { errBox.textContent = err.message; }
  };

  if (editing) $('#fDelete').onclick = async () => {
    if (!confirm(`Delete “${recordTitle(editing, model)}”? This removes it from disk.`)) return;
    try {
      await api(`/api/models/${encodeURIComponent(model.name)}/record?key=${encodeURIComponent(editing.key)}&revision=${encodeURIComponent(editing.revision)}`, { method: 'DELETE' });
      location.hash = backToViewHref();
    } catch (err) { errBox.textContent = err.message; }
  };
}

/* ======================================================================= */

boot();
