'use strict';
// Boundrail model view. Reads the model through the local read-only API of
// `boundrail model view`, draws the SVG it renders, and follows changes over Server-Sent Events.

/** The token from the URL, kept for this tab so a reload works once it leaves the address bar. */
const TOKEN = (() => {
  const given = new URLSearchParams(location.search).get('t');
  try {
    if (given) sessionStorage.setItem('boundrail-token', given);
    return given || sessionStorage.getItem('boundrail-token') || '';
  } catch {
    return given || '';
  }
})();
history.replaceState(null, '', location.pathname + location.hash);

const FLOW_VIEWS = ['sequence', 'flowchart'];
const ALL_PARTS = ['model', 'proposals', 'state', 'history'];
const ZOOM_MIN = 0.05;
const ZOOM_MAX = 8;
const ZOOM_STEP = 1.25;
const WHEEL_RATE = 0.0015;
const KEY_STEP = 40;
const DRAG_PX = 4;
const FIT_PAD = 16;
const FIT_MAX = 1.25;
const TOP_PAD = 32;
const FIT_GLIDE = 150;
const phone = matchMedia('(max-width: 600px)');
const finePointer = matchMedia('(pointer: fine)');
const BACKOFF = { first: 1000, most: 30000 };

const app = {
  check: null, // `model check` of the active pane's state, null while it runs
  checkError: null,
  comparison: null,
  proposals: [],
  checkpoints: [],
  models: new Map(), // state key -> promise of a pane model
  interfaces: new Map(), // state key | module ID -> `model inspect`
  references: new Map(), // state key | module ID -> `model read --references`
  foreign: null, // the reference shown read-only: `{ label, data, error }`, null when none is
  // The revision the last change event named, null when the current state did not load; the
  // first load's until an event comes. Kept apart from later loads, which may race the events.
  eventRevision: undefined,
  currentFailed: false, // whether the last load of the current state failed
  selection: [], // qualified module IDs, in the order they were selected
  hits: [], // subjects a warning pointed at
  focus: false,
  filter: '',
  kinds: new Set(), // kinds shown; empty shows every kind
  hiddenRels: new Set(), // relationship kinds hidden on the canvas and in the detail lists
  page: null, // the number the server's event stream gave this page
  restoring: true,
  collapsed: new Set(),
  interfaceOpen: null,
  split: false,
  active: 0,
  panes: [],
  ticket: 0, // drops answers to requests a newer load replaced
  // The walkthrough being played: `{ id, walkthrough, index, error }`, null when none is.
  play: null,
  pictures: [], // the current state's picture recipes
  picturesError: null,
  // Each picture's `model pictures` entry by ID, null until asked for: checking draws them all.
  pictureStatus: null,
  // The open question to the agent, kept across redraws: `{ owner, target, question, answer,
  // busy }`, where `owner` is the module whose panel shows it, or `summary`.
  ask: null,
  explainAllowed: null, // promise of whether the server runs the agent for the page
};

// ---------------------------------------------------------------- helpers

/** An element with attributes, listeners (`on...`) and children; text never becomes markup. */
function h(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  if (tag === 'button') node.type = 'button';
  for (const [key, value] of Object.entries(props)) {
    if (value === null || value === undefined || value === false) continue;
    if (key.startsWith('on')) node.addEventListener(key.slice(2), value);
    else if (key === 'class') node.className = value;
    else if (key === 'text') node.textContent = value;
    else node.setAttribute(key, value === true ? '' : value);
  }
  for (const child of children.flat()) {
    if (child === null || child === undefined || child === false) continue;
    node.append(child instanceof Node ? child : String(child));
  }
  return node;
}

const $ = (selector) => document.querySelector(selector);
const note = (text) => h('p', { class: 'empty', text });
/** A list entry: a name, a quieter second word, and what clicking it does. */
const item = (props, name, kind, ...rest) => h('button', { class: 'item', ...props },
  h('span', { class: 'name', text: name }), kind ? h('span', { class: 'kind', text: kind }) : null, ...rest);
/** `[value, null]` when the promise keeps it, `[null, message]` when it breaks it. */
const attempt = (promise) => promise.then((value) => [value, null], (error) => [null, error.message]);

/** `/api/<path>` with the token, an array parameter repeated; a refusal becomes an Error
 * carrying its code and message. */
async function api(path, params = {}, asText = false, method = 'GET') {
  const query = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    for (const one of [value].flat()) {
      if (one !== null && one !== undefined && one !== '') query.append(key, one);
    }
  }
  const response = await fetch(`/api/${path}?${query}`, { method, headers: { 'X-Boundrail-Token': TOKEN } });
  if (!response.ok) {
    let message = `The server answered ${response.status}.`;
    try {
      const body = await response.json();
      if (body.error) message = body.error.code ? `${body.error.code}: ${body.error.message}` : body.error.message;
    } catch { /* not JSON: keep the status */ }
    throw new Error(message);
  }
  return asText ? response.text() : response.json();
}

function toast(text) {
  $('#toast').textContent = text;
  $('#toast').classList.add('show');
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => $('#toast').classList.remove('show'), 1800);
}

/** Rebuilds `container` with `build`, then gives focus back to the element with the same key. */
function keepFocus(container, build) {
  const key = container.contains(document.activeElement) ? document.activeElement.dataset.key : null;
  build();
  if (key) container.querySelector(`[data-key="${CSS.escape(key)}"]`)?.focus();
}

// ---------------------------------------------------------------- model queries

/** One state's `model read` with its modules by ID; `data` is null and `error` set when it failed. */
function paneModel(data, error) {
  return { data, error, modules: new Map((data?.modules ?? []).map((m) => [m.id, m])) };
}

const EMPTY = paneModel(null, 'Loading the model');
const activePane = () => app.panes[app.active];
/** The model the sidebar and detail panel show: the active pane's. */
const shown = () => activePane().model ?? EMPTY;
const nameOf = (model, id) => model.modules.get(id)?.name ?? id;
const kindName = (kind) => shown().data?.config?.['node-kinds']?.[kind]?.name ?? kind;
const relationshipKind = (kind) => shown().data?.config?.['relationship-kinds']?.[kind]?.name ?? kind;
const flows = (model) => (model.data?.collections ?? []).flatMap((c) => c.flows);
const TONES = ['ui', 'service', 'system', 'module', 'job', 'datastore', 'cache', 'app', 'external'];
const BORROWED = TONES.filter((tone) => tone !== 'external');
function toneOf(kind = '') {
  if (TONES.includes(kind)) return kind;
  let hash = 0;
  for (const c of kind) hash = (hash * 31 + c.charCodeAt(0)) >>> 0;
  return BORROWED[hash % BORROWED.length];
}
const HEX_COLOR = /^#(?:[0-9a-f]{3}|[0-9a-f]{6})$/i;
function paint(node, kind, model = shown()) {
  const color = model.data?.config?.['node-kinds']?.[kind]?.color;
  const configured = typeof color === 'string' && HEX_COLOR.test(color);
  node.style.setProperty('--tone', configured ? color : `var(--k-${toneOf(kind)})`);
  return node;
}
const kindDot = (kind) => paint(h('span', { class: 'dot', 'aria-hidden': 'true' }), kind);

function ancestors(model, id) {
  const found = [];
  for (let m = model.modules.get(id); m?.parent; m = model.modules.get(m.parent)) found.push(m.parent);
  return found;
}

function subtree(model, id) {
  const found = [id];
  for (let i = 0; i < found.length; i += 1) found.push(...(model.modules.get(found[i])?.children ?? []));
  return found;
}

/** The modules and relationships connected to `ids` or their parts, with their containers. */
function neighbourhood(model, ids) {
  const inside = new Set(ids.flatMap((id) => subtree(model, id)));
  const modules = new Set(inside);
  const relationships = new Set();
  for (const r of model.data?.relationships ?? []) {
    if (inside.has(r.from) || inside.has(r.to)) {
      relationships.add(r.id);
      modules.add(r.from);
      modules.add(r.to);
    }
  }
  for (const m of [...modules]) ancestors(model, m).forEach((a) => modules.add(a));
  return { modules, relationships };
}

const stateParams = (state) => (state.kind === 'current' ? {} : { [state.kind]: state.id });
const stateKey = (state) => (state.kind === 'current' ? 'current' : `${state.kind}:${state.id}`);

function parseState(value) {
  const [kind, ...rest] = value.split(':');
  return kind === 'current' ? { kind } : { kind, id: rest.join(':') };
}

function stateLabel(state) {
  if (state.kind === 'proposal') {
    const p = app.proposals.find((x) => x.id === state.id);
    return `Proposal ${p ? p.name : state.id}`;
  }
  if (state.kind === 'checkpoint') {
    const c = app.checkpoints.find((x) => x.id === state.id);
    return c ? `Checkpoint ${c.id}, ${c.doing}` : `Checkpoint ${state.id}`;
  }
  return 'Current state';
}

function againstLabel(against) {
  if (against === 'current') return 'the current state';
  const p = app.proposals.find((x) => x.id === against);
  return `proposal ${p ? p.name : against}`;
}

/** What the pane is compared with: what it names, or the current state for a proposal alone. */
const baseOf = (pane) => pane.against || (pane.state.kind === 'proposal' ? 'current' : '');

/** The proposal and base `model compare` takes for what the pane draws, or null. */
function comparePair(pane) {
  if (pane.state.kind === 'proposal') return { proposal: pane.state.id, against: baseOf(pane) };
  if (pane.state.kind === 'current' && pane.against) return { proposal: pane.against, against: 'current' };
  return null;
}

// ---------------------------------------------------------------- loading

/** The model of `state`, fetched once until the next change. */
function loadModel(state) {
  const key = stateKey(state);
  if (!app.models.has(key)) {
    const loading = attempt(api('model', stateParams(state))).then(([data, error]) => {
      // A load a later change replaced says nothing about the current state any more.
      if (state.kind === 'current' && app.models.get(key) === loading) {
        app.currentFailed = !data;
        if (app.eventRevision === undefined) app.eventRevision = data?.revision?.digest ?? null;
      }
      return paneModel(data, error);
    });
    app.models.set(key, loading);
  }
  return app.models.get(key);
}

function forget() {
  app.models.clear();
  app.interfaces.clear();
  app.references.clear();
}

async function loadLists() {
  const [[proposals], [checkpoints]] = await Promise.all([attempt(api('proposals')), attempt(api('checkpoints'))]);
  app.proposals = (proposals?.proposals ?? []).filter((p) => !p.error);
  app.checkpoints = checkpoints?.checkpoints ?? [];
  renderLists();
}

/** Reads the picture recipes again, without drawing them. A pane showing a picture follows its
 * recipe, or keeps what it shows once the picture is gone. Statuses are dropped: they may be out
 * of date. */
async function loadPictures() {
  const [data, error] = await attempt(api('pictures'));
  app.pictures = data?.pictures ?? [];
  app.picturesError = error;
  app.pictureStatus = null;
  for (const pane of app.panes) {
    const id = pane.beat?.picture;
    if (!id) continue;
    const picture = app.pictures.find((p) => p.id === id);
    if (picture) applyReading(pane, picture.reading, { picture: id });
    else pane.beat = null;
  }
  renderPictures();
}

/** A pane showing a saved view follows it after a reload, or keeps what it shows once the view
 * is gone. */
async function followViews() {
  const { data } = await loadModel({ kind: 'current' });
  for (const pane of app.panes) {
    const id = pane.beat?.saved;
    if (!id) continue;
    const view = (data?.views ?? []).find((v) => v.id === id);
    if (view) applyReading(pane, view.reading, { saved: id });
    else pane.beat = null;
  }
}

/** Asks the server to draw every picture and compare it with its file. */
async function checkPictures() {
  const [data, error] = await attempt(api('pictures', { status: 1 }));
  app.picturesError = error;
  app.pictureStatus = data ? new Map(data.pictures.map((p) => [p.id, p])) : null;
  renderPictures();
}

/** Loads `pane`'s state and draws it. */
async function loadPane(pane) {
  const ticket = ++pane.loads;
  const model = await loadModel(pane.state);
  if (ticket !== pane.loads) return;
  pane.model = model;
  const names = flows(model).map((f) => f.id);
  if (pane.flow && !names.includes(pane.flow)) pane.flow = null;
  if (FLOW_VIEWS.includes(pane.view) && !pane.flow) pane.flow = names[0] ?? null;
  await drawPane(pane);
}

/** Loads everything again, as on opening the page; a walkthrough being played stays on its beat. */
async function loadAll() {
  forget();
  app.currentFailed = false;
  await Promise.all([refreshPlay(), loadPictures(), followViews()]);
  await Promise.all([loadLists(), ...app.panes.map(loadPane)]);
  showActive();
  renderPlay();
}

/** Fills the top bar, the sidebar and the detail panel from the active pane. */
function showActive() {
  const ticket = ++app.ticket;
  renderHeader();
  renderControls();
  remember();
  renderTree();
  renderFlows();
  renderWalkthroughs();
  renderViews();
  renderDetail();
  app.panes.forEach(renderCrumbs);
  loadCheck(ticket);
  loadComparison(ticket);
}

async function loadCheck(ticket) {
  const state = activePane().state;
  app.check = null;
  app.checkError = state.kind === 'checkpoint'
    ? 'Checks compare a state with the code in the checkout, so checkpoints have none.'
    : null;
  renderWarnings();
  if (app.checkError || !shown().data) return;
  const [check, checkError] = await attempt(api('check', stateParams(state)));
  if (ticket !== app.ticket) return;
  Object.assign(app, { check, checkError });
  renderWarnings();
  renderTree();
  renderDetail();
}

async function loadComparison(ticket) {
  const pair = comparePair(activePane());
  app.comparison = null;
  $('#changes-section').hidden = !pair;
  if (!pair) return;
  renderChanges();
  const comparison = await api('compare', pair).catch((error) => ({ error: error.message }));
  if (ticket !== app.ticket) return;
  app.comparison = comparison;
  renderChanges();
}

// ---------------------------------------------------------------- top bar

function renderHeader() {
  const data = shown().data;
  const name = data?.manifest?.name ?? 'Model unavailable';
  $('#model-name').textContent = name;
  document.title = `${name}, Boundrail`;
  const digest = data?.revision?.digest ?? '';
  $('#revision').textContent = digest ? `rev ${digest.slice(0, 10)}` : '';
  $('#revision').title = digest ? `Revision ${digest}` : '';
}

/** Gives `select` these options, rebuilding them only when they changed, and sets its value. */
function fillSelect(select, options, value) {
  const signature = JSON.stringify(options);
  if (select.dataset.signature !== signature) {
    select.dataset.signature = signature;
    select.replaceChildren(...options.map(([group, list]) => {
      const nodes = list.map(([v, label]) => h('option', { value: v }, label));
      return group ? h('optgroup', { label: group }, nodes) : nodes;
    }).flat());
  }
  select.value = value;
}

function renderControls() {
  const pane = activePane();
  const proposals = app.proposals.map((p) => [`proposal:${p.id}`, `${p.name} (${p.status})`]);
  const checkpoints = app.checkpoints.map((c) => [`checkpoint:${c.id}`, `${c.doing} (${c.time.slice(0, 10)})`]);
  fillSelect($('#state'), [
    [null, [['current', 'Current state']]],
    ...(proposals.length ? [['Proposals', proposals]] : []),
    ...(checkpoints.length ? [['Checkpoints', checkpoints]] : []),
  ], stateKey(pane.state));
  const others = app.proposals.filter((p) => !(pane.state.kind === 'proposal' && p.id === pane.state.id));
  fillSelect($('#against'), [[null, [
    ['', 'Nothing'],
    ...(pane.state.kind === 'current' ? [] : [['current', 'Current state']]),
    ...others.map((p) => [p.id, `Proposal ${p.name}`]),
  ]]], pane.against);
  const noFlows = flows(shown()).length === 0;
  for (const button of document.querySelectorAll('[data-view]')) {
    const view = button.dataset.view;
    button.setAttribute('aria-pressed', String(view === pane.view));
    button.disabled = FLOW_VIEWS.includes(view) && noFlows;
    button.title = button.disabled ? 'This state has no flows' : '';
  }
  $('#depth').value = pane.depth;
  $('#depth').disabled = pane.view !== 'architecture';
  $('.stepper').hidden = pane.view !== 'architecture';
  $('#depth-down').disabled = $('#depth').disabled || pane.depth <= Number($('#depth').min);
  $('#depth-up').disabled = $('#depth').disabled || pane.depth >= Number($('#depth').max);
  const canFocus = app.selection.some((id) => shown().modules.has(id));
  $('#focus').setAttribute('aria-disabled', String(!canFocus));
  $('#focus').setAttribute('aria-pressed', String(app.focus && canFocus));
  $('#focus').title = canFocus ? 'Focus on selection: dim what it does not touch' : 'Select a module to focus';
  $('#split').setAttribute('aria-pressed', String(app.split));
}

function setLive(on) {
  const dot = $('#live');
  dot.className = `live ${on ? 'on' : 'off'}`;
  dot.title = on ? 'Live: the view follows changes to the model' : 'Offline, reconnecting';
}

// ---------------------------------------------------------------- sidebar

function warningCounts() {
  const counts = new Map();
  for (const d of app.check?.diagnostics ?? []) {
    if (d.severity !== 'warning') continue;
    for (const s of d.subjects) counts.set(s, (counts.get(s) ?? 0) + 1);
  }
  return counts;
}

/** Modules the filter and kind chips keep, with the containers above them. */
function keptModules(model) {
  const text = app.filter.trim().toLowerCase();
  const kept = new Set();
  for (const m of model.modules.values()) {
    const named = !text || [m.name, ...m.former_names].some((n) => n.toLowerCase().includes(text));
    if (named && (app.kinds.size === 0 || app.kinds.has(m.kind))) {
      kept.add(m.id);
      ancestors(model, m.id).forEach((a) => kept.add(a));
    }
  }
  return kept;
}

/** The kind chips, rebuilt only when the kinds in the model change. */
function renderKinds() {
  const kinds = [...new Set([...shown().modules.values()].map((m) => m.kind))].sort();
  for (const kind of [...app.kinds]) if (!kinds.includes(kind)) app.kinds.delete(kind);
  const box = $('#kinds');
  if (box.dataset.kinds === kinds.join(' ')) return;
  box.dataset.kinds = kinds.join(' ');
  keepFocus(box, () => box.replaceChildren(...kinds.map((kind) => h('button', {
    'data-key': kind,
    'aria-pressed': String(app.kinds.has(kind)),
    onclick: (event) => {
      app.kinds[app.kinds.has(kind) ? 'delete' : 'add'](kind);
      event.currentTarget.setAttribute('aria-pressed', String(app.kinds.has(kind)));
      renderTree();
      app.panes.forEach(decorate);
    },
  }, kindDot(kind), kindName(kind)))));
}

const DASHES = { dashed: '6 4', dotted: '2 3' };

/** A short line drawn as relationship kind `kind` is: solid, dashed or dotted. */
function lineSample(kind) {
  const svg = svgNode('svg', { class: 'line', viewBox: '0 0 18 8', 'aria-hidden': 'true' });
  const dash = DASHES[shown().data?.config?.['relationship-kinds']?.[kind]?.dash];
  svg.append(svgNode('line', { x1: 1, y1: 4, x2: 17, y2: 4, ...(dash ? { 'stroke-dasharray': dash } : {}) }));
  return svg;
}

/** The relationship kind chips, one per kind the state uses; a pressed chip hides its kind. */
function renderRelKinds() {
  const kinds = [...new Set((shown().data?.relationships ?? []).map((r) => r.kind))].sort();
  for (const kind of [...app.hiddenRels]) if (!kinds.includes(kind)) app.hiddenRels.delete(kind);
  const box = $('#rel-kinds');
  box.hidden = !kinds.length;
  if (box.dataset.kinds === kinds.join(' ')) return;
  box.dataset.kinds = kinds.join(' ');
  const label = (kind) => `${app.hiddenRels.has(kind) ? 'Show' : 'Hide'} ${relationshipKind(kind)} relationships`;
  keepFocus(box, () => box.replaceChildren(...kinds.map((kind) => h('button', {
    'data-key': kind,
    'aria-pressed': String(app.hiddenRels.has(kind)),
    title: label(kind),
    onclick: (event) => {
      app.hiddenRels[app.hiddenRels.has(kind) ? 'delete' : 'add'](kind);
      event.currentTarget.setAttribute('aria-pressed', String(app.hiddenRels.has(kind)));
      event.currentTarget.title = label(kind);
      renderDetail();
      app.panes.forEach(decorate);
    },
  }, lineSample(kind), relationshipKind(kind)))));
}

function renderTree() {
  renderKinds();
  renderRelKinds();
  const list = $('#tree');
  const model = shown();
  keepFocus(list, () => {
    list.replaceChildren();
    if (!model.data) return list.append(h('li', {}, note(model.error)));
    const kept = keptModules(model);
    const filtering = app.filter.trim() !== '' || app.kinds.size > 0;
    const counts = warningCounts();
    const add = (ids, parent) => {
      for (const id of ids.filter((x) => kept.has(x))) {
        const m = model.modules.get(id);
        const parts = m.children.filter((c) => kept.has(c));
        const open = filtering || !app.collapsed.has(id);
        const toggle = parts.length ? h('button', {
          class: open ? 'toggle open' : 'toggle',
          'data-key': `toggle:${id}`,
          'aria-expanded': String(open),
          'aria-label': `Parts of ${m.name}`,
          onclick: () => { app.collapsed[app.collapsed.has(id) ? 'delete' : 'add'](id); renderTree(); },
        }) : h('span', { class: 'toggle leaf' });
        const chosen = app.selection.includes(id);
        const entry = item({
          class: chosen ? 'item selected' : 'item',
          'data-key': `item:${id}`,
          'aria-current': chosen ? 'true' : null,
          title: m.children.length ? 'Double-click to open its parts' : null,
          onclick: (event) => (adds(event) ? toggleSelect(id) : select(id, true)),
          ondblclick: () => drill(activePane(), id),
        }, m.name, kindName(m.kind),
        counts.get(id) ? h('span', { class: 'count', title: 'Warnings', text: counts.get(id) }) : null);
        entry.prepend(kindDot(m.kind));
        const li = h('li', {}, h('div', { class: 'row' }, toggle, entry));
        if (parts.length && open) li.append(add(parts, h('ul')));
        parent.append(li);
      }
      return parent;
    };
    add(model.data.roots, list);
    if (!list.children.length) {
      list.append(h('li', {}, note(model.modules.size ? 'No module matches the filter.' : 'This state has no modules yet.')));
    }
  });
}

function renderFlows() {
  const box = $('#flows');
  const pane = activePane();
  keepFocus(box, () => {
    box.replaceChildren();
    if (!flows(shown()).length) {
      return box.append(note('No flows yet.'));
    }
    for (const collection of shown().data.collections) {
      box.append(h('div', { class: 'collection', text: collection.name }));
      for (const flow of collection.flows) {
        const open = pane.view !== 'architecture' && pane.flow === flow.id;
        box.append(item({
          class: open ? 'item selected' : 'item',
          'data-key': flow.id,
          'aria-current': open ? 'true' : null,
          title: flow.summary,
          onclick: () => openFlow(flow.id),
        }, flow.name));
      }
    }
  });
}

/** The current state's walkthroughs, whatever the active pane shows: they belong to it. */
async function renderWalkthroughs() {
  const { data, error } = await loadModel({ kind: 'current' });
  const box = $('#walkthroughs');
  keepFocus(box, () => {
    box.replaceChildren();
    const list = data?.walkthroughs ?? [];
    if (!data) return box.append(note(error));
    if (!list.length) {
      return box.append(note('No walkthroughs yet.'));
    }
    for (const w of list) {
      const playing = app.play?.id === w.id;
      box.append(item({
        class: playing ? 'item selected' : 'item',
        'data-key': w.id,
        'aria-current': playing ? 'true' : null,
        title: w.description,
        onclick: () => playWalkthrough(w.id),
      }, w.name, `${w.beats} beat${w.beats === 1 ? '' : 's'}`));
    }
  });
}

/** The current state's saved views; opening one draws its reading in the active pane. */
async function renderViews() {
  const { data, error } = await loadModel({ kind: 'current' });
  const box = $('#views');
  const list = data?.views ?? [];
  markEmpty(box, Boolean(data) && !list.length);
  keepFocus(box, () => {
    box.replaceChildren();
    if (!data) return box.append(note(error));
    if (!list.length) return box.append(note('No saved views yet.'));
    const shown = activePane().beat?.saved;
    for (const v of list) {
      box.append(item({
        class: shown === v.id ? 'item selected' : 'item',
        'data-key': v.id,
        'aria-current': shown === v.id ? 'true' : null,
        title: v.description,
        onclick: () => showView(v.id),
      }, v.name));
    }
  });
}

/** Shows what saved view `id` reads in the active pane, as the server draws it. */
async function showView(id) {
  const { data } = await loadModel({ kind: 'current' });
  const view = (data?.views ?? []).find((v) => v.id === id);
  if (!view) return;
  stopPlaying();
  const pane = activePane();
  applyReading(pane, view.reading, { saved: id });
  await loadPane(pane);
  fit(pane);
  showActive();
}

/** The current state's pictures, with whether each file still matches what the model draws once
 * checked. */
function renderPictures() {
  const box = $('#pictures');
  markEmpty(box, !app.pictures.length && !app.picturesError);
  keepFocus(box, () => {
    box.replaceChildren();
    if (app.picturesError) box.append(note(app.picturesError));
    if (!app.pictures.length) return box.append(note('No pictures yet.'));
    for (const p of app.pictures) {
      const status = app.pictureStatus?.get(p.id);
      box.append(item({
        'data-key': p.id,
        title: status?.message ?? p.output,
        onclick: () => showPicture(p.id),
      }, p.name, status?.status));
    }
    box.append(h('button', { class: 'link', 'data-key': 'check', onclick: checkPictures }, 'Check files'));
  });
}

/** Shows what picture `id` draws in the active pane, as the server draws it, focus included. */
async function showPicture(id) {
  const picture = app.pictures.find((p) => p.id === id);
  if (!picture) return;
  stopPlaying();
  const pane = activePane();
  applyReading(pane, picture.reading, { picture: id });
  await loadPane(pane);
  fit(pane);
  showActive();
}

function renderWarnings() {
  const box = $('#warnings');
  box.replaceChildren();
  const diagnostics = app.check?.diagnostics ?? [];
  box.closest('section').hidden = !diagnostics.length;
  for (const [severity, heading] of [['warning', 'Warnings'], ['information', 'Information']]) {
    const group = diagnostics.filter((d) => d.severity === severity);
    if (!group.length) continue;
    box.append(h('div', { class: `severity ${severity}`, text: `${heading} (${group.length})` }));
    for (const d of group) {
      box.append(h('button', { class: 'diagnostic', onclick: () => focusSubjects(d.subjects) },
        d.message, h('code', { text: d.code })));
    }
  }
}

function renderChanges() {
  const box = $('#changes');
  box.replaceChildren();
  const c = app.comparison;
  if (!c) return box.append(note('Comparing'));
  if (c.error) return box.append(note(c.error));
  for (const section of ['modules', 'relationships', 'collections', 'flows', 'steps', 'walkthroughs']) {
    for (const verb of ['added', 'removed', 'changed']) {
      for (const entry of c[section]?.[verb] ?? []) {
        const label = `${verb} ${section.replace(/s$/, '')} ${entry.name ?? entry.id}`;
        const fields = entry.fields?.length ? `: ${entry.fields.join(', ')}` : '';
        box.append(section === 'modules'
          ? h('button', { class: 'diagnostic', onclick: () => select(entry.id, true) }, label + fields)
          : h('div', { class: 'diagnostic' }, label + fields));
      }
    }
  }
  if (!box.children.length) box.append(note('No differences.'));
}

function markEmpty(box, empty) {
  box.closest('section').dataset.empty = String(empty);
  const names = [...document.querySelectorAll('.sidebar section[data-empty="true"]')].map((s) => s.dataset.none);
  const line = $('#empties');
  line.hidden = !names.length;
  line.textContent = names.length ? `No ${names.length > 1 ? `${names.slice(0, -1).join(', ')} or ${names.at(-1)}` : names[0]} yet.` : '';
}

function aboutProposal(p) {
  const lines = [p.summary, ...(p.documents ?? []).map((d) => `${d.title}: ${d.path ?? d.url}`)];
  return lines.filter(Boolean).join('\n') || null;
}

function renderLists() {
  markEmpty($('#proposals'), !app.proposals.length);
  markEmpty($('#history'), !app.checkpoints.length);
  $('#proposals').replaceChildren(...(app.proposals.length
    ? app.proposals.map((p) => item({ title: aboutProposal(p), onclick: () => setState({ kind: 'proposal', id: p.id }) }, p.name, p.status))
    : [note('No proposals yet.')]));
  $('#history').replaceChildren(...(app.checkpoints.length
    ? app.checkpoints.map((c) => {
      const proposal = c.proposal && (app.proposals.find((p) => p.id === c.proposal)?.name ?? c.proposal);
      const date = c.time.slice(0, 10);
      return item({
        title: `${c.id} by ${c.writer} at ${c.time}${proposal ? `, in proposal ${proposal}` : ''}`,
        onclick: () => setState({ kind: 'checkpoint', id: c.id }),
      }, c.doing, proposal ? `${proposal}, ${date}` : date);
    })
    : [note('No checkpoints yet.')]));
}

// ---------------------------------------------------------------- detail panel

const link = (id) => h('button', { class: 'link', onclick: () => select(id, true) }, nameOf(shown(), id));
const icon = (path) => {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('viewBox', '0 0 16 16');
  svg.setAttribute('aria-hidden', 'true');
  svg.innerHTML = path;
  return svg;
};
const COPY_ICON = '<rect x="5.5" y="5.5" width="8" height="8" rx="2"/><path d="M10.5 3.5v-.5a1.5 1.5 0 0 0-1.5-1.5H4A1.5 1.5 0 0 0 2.5 3v5A1.5 1.5 0 0 0 4 9.5h.5"/>';
const CLOSE_ICON = '<path d="M4 4l8 8M12 4l-8 8"/>';

async function copyId(id) {
  try {
    await navigator.clipboard.writeText(id);
    toast('ID copied');
  } catch {
    toast('Copying is not allowed here');
  }
}

const count = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;

function summary(model) {
  if (!model.data) return [note(model.error)];
  const byKind = new Map();
  for (const m of model.modules.values()) byKind.set(m.kind, (byKind.get(m.kind) ?? 0) + 1);
  const warnings = (app.check?.diagnostics ?? []).filter((d) => d.severity === 'warning').length;
  let status;
  if (app.checkError) status = ['bad', app.checkError];
  else if (!app.check) status = ['pending', 'Checking against the code'];
  else if (warnings) status = ['warn', `${count(warnings, 'warning')}, listed on the left`];
  else status = ['ok', 'No warnings. The model matches the code.'];
  const walkthroughs = model.data.walkthroughs ?? [];
  const goto = (label, onclick, extra) => h('li', {}, h('button', { class: 'link', onclick }, label), extra ? h('span', { class: 'n', text: extra }) : null);
  return [
    h('h2', { class: 'title', text: model.data.manifest.name }),
    h('p', { class: 'lede', text: `${count(model.modules.size, 'module')} in ${count(byKind.size, 'kind')}` }),
    h('p', { class: 'kinds-line', text: [...byKind].sort((a, b) => b[1] - a[1]).map(([k, n]) => `${n} ${kindName(k)}`).join(', ') }),
    h('h2', { text: 'Checks' }),
    h('p', { class: `status ${status[0]}` },
      status[0] === 'pending' ? h('span', { class: 'spinner', 'aria-hidden': 'true' }) : h('span', { class: 'status-dot', 'aria-hidden': 'true' }),
      status[1]),
    h('h2', { text: 'Flows' }),
    flows(model).length ? h('ul', { class: 'goto' }, flows(model).map((f) => goto(f.name, () => openFlow(f.id), f.steps ? count(f.steps.length, 'step') : null))) : note('No flows yet.'),
    h('h2', { text: 'Walkthroughs' }),
    walkthroughs.length ? h('ul', { class: 'goto' }, walkthroughs.map((w) => goto(w.name, () => playWalkthrough(w.id), count(w.beats, 'beat')))) : note('No walkthroughs yet.'),
  ];
}

function beatModule(model) {
  const focus = app.play?.walkthrough?.beats[app.play.index]?.reading?.focus ?? [];
  return focus.length === 1 ? model.modules.get(focus[0]) ?? null : null;
}

const section = (title, ...content) => [h('h2', { text: title }), ...content];
const dl = (facts) => h('dl', {}, facts.flatMap(([k, v]) => [h('dt', { text: k }), h('dd', {}, v)]));

function renderDetail() {
  const box = $('#detail');
  const typing = document.activeElement?.dataset.key === 'ask' ? document.activeElement : null;
  box.replaceChildren();
  renderModule(box);
  if (typing) {
    const field = box.querySelector('[data-key="ask"]');
    field?.focus();
    field?.setSelectionRange(typing.selectionStart, typing.selectionEnd);
  }
}

function renderModule(box) {
  const model = shown();
  const context = askContext();
  if (app.selection.length > 1) return renderSelection(box, model, context);
  const m = app.selection.length ? model.modules.get(app.selection[0]) : beatModule(model);
  if (!m) {
    if (app.selection.length) return box.append(note('The selected module is not part of this state.'));
    box.append(...summary(model));
    if (context && (context.flow || context.proposal)) box.append(askSection('summary', context));
    return;
  }
  const ask = (target) => askAbout(m.id, target);
  box.append(
    h('div', { class: 'subtitle' }, kindDot(m.kind), kindName(m.kind)),
    h('h2', { class: 'title', text: m.name }),
    h('div', { class: 'id' }, h('span', { text: m.id }),
      h('button', { class: 'icon', title: 'Copy ID', 'aria-label': 'Copy module ID', onclick: () => copyId(m.id) }, icon(COPY_ICON))),
    m.responsibility ? h('p', { class: 'responsibility', text: m.responsibility }) : '',
    m.description ? h('p', { text: m.description }) : '',
  );
  const facts = [];
  if (m.parent) facts.push(['Part of', link(m.parent)]);
  if (m.binding) {
    const where = `${m.binding.repo ? `${m.binding.repo}: ` : ''}${m.binding.path}`;
    facts.push(['Binding', m.binding.symbol ? `${where}, symbol ${m.binding.symbol}` : where]);
  }
  if (m.groups.length) facts.push(['Groups', m.groups.map((g) => h('span', { class: 'tag', text: g }))]);
  if (m.former_names.length) facts.push(['Formerly', m.former_names.join(', ')]);
  if (m.children.length) facts.push(['Parts', m.children.flatMap((c, i) => (i ? [', ', link(c)] : [link(c)]))]);
  if (facts.length) box.append(dl(facts));
  if (m.references?.length) box.append(...section('References', referenceList(m)));

  const relationships = model.data.relationships;
  const line = (r, other) => h('li', { class: 'rel' },
    h('span', { class: 'rel-main' }, kindDot(model.modules.get(other)?.kind), link(other)),
    h('span', { class: 'tag', text: relationshipKind(r.kind) }),
    context ? h('button', {
      class: 'link',
      title: 'Ask the agent about this relationship',
      onclick: () => ask({ ...context, relationship: r.id }),
    }, 'Ask') : null,
    r.label ? h('span', { class: 'rel-label', text: r.label }) : null);
  const shownRels = relationships.filter((r) => !app.hiddenRels.has(r.kind));
  const outgoing = shownRels.filter((r) => r.from === m.id).map((r) => line(r, r.to));
  const incoming = shownRels.filter((r) => r.to === m.id).map((r) => line(r, r.from));
  box.append(...section('Outgoing', outgoing.length ? h('ul', { class: 'rels' }, outgoing) : note('None.')));
  box.append(...section('Incoming', incoming.length ? h('ul', { class: 'rels' }, incoming) : note('None.')));
  const hidden = relationships.filter((r) => (r.from === m.id || r.to === m.id) && app.hiddenRels.has(r.kind)).length;
  if (hidden) box.append(note(`${count(hidden, 'relationship')} hidden by the relationship filter.`));

  const holders = new Set([m.id, ...ancestors(model, m.id), model.data.manifest.id]);
  const claims = (model.data.claims ?? []).filter((c) => c.subjects.some((s) => holders.has(s)));
  box.append(...section('Claims', claims.length
    ? h('ul', {}, claims.map((c) => h('li', {}, `${c.writer}: ${c.reason}`,
      h('div', { class: 'id', text: `doing ${c.doing}${c.revoked ? `, revoked by ${c.revoked.by}` : ''}` }))))
    : note(model.data.checkpoint ? 'Checkpoints hold no claims.' : 'No claim holds this module.')));

  const diagnostics = (app.check?.diagnostics ?? []).filter((d) => d.subjects.includes(m.id));
  box.append(...section('Diagnostics', diagnostics.length
    ? h('ul', {}, diagnostics.map((d) => h('li', {}, d.message, h('div', { class: 'id', text: `${d.severity}, ${d.code}` }))))
    : note(app.check ? 'None.' : (app.checkError ?? 'Checking'))));
  box.append(interfaceSection(m));
  if (context) box.append(askSection(m.id, { ...context, subjects: [m.id] }));
}

/** The combined panel of several selected modules: the members, the relationships among them
 * and the modules outside the selection they reach, each relationship kind the filter shows. */
function renderSelection(box, model, context) {
  const ids = app.selection.filter((id) => model.modules.has(id));
  const inside = new Set(ids.flatMap((id) => subtree(model, id)));
  const relationships = (model.data?.relationships ?? []).filter((r) => !app.hiddenRels.has(r.kind));
  const among = relationships.filter((r) => inside.has(r.from) && inside.has(r.to));
  const outside = new Map();
  for (const r of relationships) {
    if (inside.has(r.from) === inside.has(r.to)) continue;
    const other = inside.has(r.from) ? r.to : r.from;
    outside.set(other, (outside.get(other) ?? 0) + 1);
  }
  const missing = app.selection.length - ids.length;
  box.append(
    h('div', { class: 'subtitle', text: 'Selection' }),
    h('h2', { class: 'title', text: count(ids.length, 'module') }),
    finePointer.matches ? h('p', { class: 'lede', text: 'Shift-click adds or removes a module; Escape clears.' }) : '',
    missing ? note(`${count(missing, 'selected module')} not part of this state.`) : '',
    h('button', { class: 'clear', 'data-key': 'clear', onclick: () => select(null) }, 'Clear selection'),
  );
  box.append(...section('Members', h('ul', { class: 'rels' }, ids.map((id) => h('li', { class: 'rel' },
    h('span', { class: 'rel-main' }, kindDot(model.modules.get(id).kind), link(id)),
    h('span', { class: 'tag', text: kindName(model.modules.get(id).kind) }),
    h('button', { class: 'link remove', title: `Take ${nameOf(model, id)} out of the selection`, onclick: () => toggleSelect(id) }, 'Remove'))))));
  box.append(...section('Between them', among.length
    ? h('ul', { class: 'rels' }, among.map((r) => h('li', { class: 'rel' },
      h('span', { class: 'rel-main' }, link(r.from), h('span', { class: 'to', text: 'to' }), link(r.to)),
      h('span', { class: 'tag', text: relationshipKind(r.kind) }),
      r.label ? h('span', { class: 'rel-label', text: r.label }) : null)))
    : note('No relationship joins them.')));
  const neighbours = [...outside].sort((a, b) => b[1] - a[1] || nameOf(model, a[0]).localeCompare(nameOf(model, b[0])));
  box.append(...section('Outside neighbours', neighbours.length
    ? h('ul', { class: 'rels' }, neighbours.map(([id, n]) => h('li', { class: 'rel' },
      h('span', { class: 'rel-main' }, kindDot(model.modules.get(id)?.kind), link(id)),
      h('span', { class: 'n', text: count(n, 'relationship') }))))
    : note('Nothing outside the selection.')));
  if (context) box.append(askSection('selection', { ...context, subjects: ids }));
}

/** How a reference reads as authored: where, what and which state. */
function referenceLabel(r) {
  const state = r.pin ? `pinned ${r.pin.slice(0, 12)}`
    : r.state?.proposal ? `proposal ${r.state.proposal}`
      : r.state?.checkpoint ? `checkpoint ${r.state.checkpoint}` : '';
  return [r.model ?? 'this model', r.subject, state].filter(Boolean).join(', ');
}

/** The module's references, each followed once per state: a followed one opens read-only in
 * the reference pane, one that cannot be followed stays a plain label with the reason. */
function referenceList(m) {
  const state = activePane().state;
  const key = `${stateKey(state)}|${m.id}`;
  const list = h('ul', { class: 'refs' }, m.references.map((r) => h('li', { text: referenceLabel(r) })));
  if (!app.references.has(key)) app.references.set(key, attempt(api('references', { subject: m.id, ...stateParams(state) })));
  app.references.get(key).then(([data, error]) => {
    if (!data) return list.replaceChildren(note(error));
    list.replaceChildren(...data.references.map((entry) => {
      const label = referenceLabel(entry.reference);
      if (!entry.resolved) {
        return h('li', {}, h('span', { class: 'ref-external', text: label }),
          h('div', { class: 'id', text: `Not followed: ${entry.skipped.message}` }));
      }
      const { model, subject } = entry.resolved;
      const name = `${model.name}${subject ? `: ${subject.name}` : ''}`;
      return h('li', {}, h('button', { class: 'link', onclick: () => openReference(m.id, entry.index, name) }, name),
        h('div', { class: 'id', text: label }));
    }));
  });
  return list;
}

/** Reads reference `index` of module `id` and shows it read-only in the reference pane. */
async function openReference(id, index, label) {
  app.foreign = { label, data: null, error: null };
  renderForeign();
  const [data, error] = await attempt(api('reference', { subject: id, index, ...stateParams(activePane().state) }));
  if (app.foreign?.label !== label) return;
  Object.assign(app.foreign, { data, error });
  renderForeign();
}

function closeReference() {
  app.foreign = null;
  renderForeign();
}

/** The reference pane: the other model's reading as text, with nothing in it that edits or
 * leaves the page. */
function renderForeign() {
  const panel = $('#foreign');
  panel.hidden = !app.foreign;
  if (!app.foreign) return panel.replaceChildren();
  const { label, data, error } = app.foreign;
  const close = h('button', { class: 'icon', title: 'Close reference', 'aria-label': 'Close reference', onclick: closeReference }, icon(CLOSE_ICON));
  const state = data?.reference.state;
  const stateText = state?.proposal ? `proposal ${state.proposal}` : state?.checkpoint ? `checkpoint ${state.checkpoint}` : 'current state';
  const where = data ? `${data.reference.checkout}, ${stateText}` : '';
  const body = [];
  if (error) body.push(note(error));
  else if (!data) body.push(note('Reading the referenced model'));
  else body.push(...foreignBody(data));
  panel.replaceChildren(
    h('div', { class: 'bar' }, h('h2', { class: 'beat-title', text: `Reference: ${label}` }), close),
    where ? h('div', { class: 'id', text: where }) : '',
    h('div', { class: 'foreign-body' }, body),
  );
}

function foreignBody(data) {
  const modules = (list) => h('ul', {}, list.map((m) => h('li', {}, `${m.name} `, h('span', { class: 'tag', text: m.kind }))));
  const rels = (list) => h('ul', {}, list.map((r) => h('li', { text: `${r.from} to ${r.to}, ${r.kind}${r.label ? `, ${r.label}` : ''}` })));
  if (data.kind === 'read-module') {
    const m = data.module;
    return [
      h('h3', { text: `${m.name} (${m.kind})` }),
      m.responsibility ? h('p', { text: m.responsibility }) : '',
      ...(data.parts.length ? [h('p', { class: 'id', text: 'Parts' }), modules(data.parts)] : []),
      ...(data.outgoing.length ? [h('p', { class: 'id', text: 'Outgoing' }), rels(data.outgoing)] : []),
      ...(data.incoming.length ? [h('p', { class: 'id', text: 'Incoming' }), rels(data.incoming)] : []),
    ];
  }
  if (data.kind === 'read') return [h('h3', { text: data.manifest.name }), modules(data.modules)];
  const named = data.flow ?? data.collection ?? data.walkthrough ?? data.picture;
  return [h('h3', { text: named?.name ?? data.kind })];
}

/** The module's interface from `model inspect`, fetched once per state and module. */
function interfaceSection(m) {
  const state = activePane().state;
  const key = `${stateKey(state)}|${m.id}`;
  const body = h('div', {}, note('Scanning the code'));
  const details = h('details', {}, h('summary', { text: 'Interface' }), body);
  const show = async () => {
    if (state.kind === 'checkpoint') {
      return body.replaceChildren(note('The interface is measured on the checkout, so checkpoints have none.'));
    }
    if (!app.interfaces.has(key)) app.interfaces.set(key, attempt(api('inspect', { subject: m.id, ...stateParams(state) })));
    const [report, error] = await app.interfaces.get(key);
    body.replaceChildren(...(report ? interfaceBody(report, m.id) : [note(error)]));
  };
  details.addEventListener('toggle', () => {
    app.interfaceOpen = details.open ? m.id : null;
    if (details.open) show();
  });
  details.open = app.interfaceOpen === m.id;
  return details;
}

function interfaceBody(report, id) {
  const entry = report.modules.find((x) => x.module === id);
  if (!entry) {
    const skip = report.skips.find((s) => s.module === id);
    return [note(skip ? `Not measured: ${skip.reason}.` : 'No scanned file belongs to this module.')];
  }
  const facts = [['Files', entry.files], ['Entry points', entry.entry_points.join(', ') || 'none']];
  if (entry.symbols) {
    facts.push(['Exported', entry.symbols.exported], ['Used outside', entry.symbols.used]);
    facts.push(['Unused outside', entry.symbols.unused === null
      ? 'withheld, symbol coverage is incomplete'
      : entry.symbols.unused.map((u) => `${u.name} (${u.file})`).join(', ') || 'none']);
  } else if (entry.symbols_skipped) {
    facts.push(['Symbols', entry.symbols_skipped]);
  }
  const out = [dl(facts)];
  const neighbours = (list, arrow) => list.map((n) => h('li', {}, `${arrow} `, link(n.module),
    ` ${n.edges} import${n.edges === 1 ? '' : 's'}${n.relationships.length ? '' : ', undrawn'}`));
  const edges = [...neighbours(entry.outgoing, 'to'), ...neighbours(entry.incoming, 'from')];
  if (edges.length) out.push(h('ul', {}, edges));
  if (entry.deep_imports.length) {
    out.push(h('p', { class: 'id', text: 'Imports past the entry points:' }),
      h('ul', {}, entry.deep_imports.map((d) => h('li', { text: `${d.from} imports ${d.to}` }))));
  }
  return out;
}

// ---------------------------------------------------------------- asking the agent

/** What the active pane adds to a question, its open flow and its proposal; null for a
 * checkpoint, which `model explain` does not read. */
function askContext() {
  const pane = activePane();
  if (pane.state.kind === 'checkpoint') return null;
  return {
    subjects: [],
    flow: pane.view !== 'architecture' ? pane.flow : null,
    proposal: pane.state.kind === 'proposal' ? pane.state.id : null,
  };
}

function describe(target, model) {
  const parts = target.subjects.map((id) => nameOf(model, id));
  if (target.relationship) {
    const r = (model.data?.relationships ?? []).find((x) => x.id === target.relationship);
    parts.push(r ? `${nameOf(model, r.from)} to ${nameOf(model, r.to)}` : target.relationship);
  }
  if (target.flow) parts.push(`the flow ${flows(model).find((f) => f.id === target.flow)?.name ?? target.flow}`);
  if (target.proposal) parts.push('this proposal');
  return `About ${parts.join(', ')}`;
}

/** A prompt field for the open question of `owner`, else one about `fallback`. */
function askSection(owner, fallback) {
  if (app.ask && app.ask.owner !== owner) app.ask = null;
  const kept = app.ask;
  const target = kept?.target ?? fallback;
  const question = h('textarea', {
    rows: 3,
    'data-key': 'ask',
    'aria-label': 'Question for the agent',
    placeholder: 'What do you want to know?',
    oninput: () => { if (app.ask) app.ask.question = question.value; },
  });
  question.value = kept?.question ?? '';
  const details = h('details', { class: 'ask' },
    h('summary', { text: 'Ask the agent' }),
    h('p', { class: 'id', text: describe(target, shown()) }),
    question,
    h('button', { disabled: kept?.busy, onclick: () => send(question.value.trim()) }, 'Ask'),
    h('p', { class: 'answer', text: kept?.answer ?? '' }));
  details.open = Boolean(kept);
  details.addEventListener('toggle', () => {
    if (details.open && !app.ask) app.ask = { owner, target, question: question.value, answer: '' };
    if (!details.open && app.ask?.owner === owner && !app.ask.busy) app.ask = null;
  });
  return details;
}

/** Points the prompt in `owner`'s panel at `target` and opens it. */
function askAbout(owner, target) {
  app.ask = { owner, target, question: app.ask?.owner === owner ? app.ask.question : '', answer: '' };
  renderDetail();
  $('#detail [data-key="ask"]')?.focus();
}

const shellWord = (word) => (/^[\w@%+=:,./-]+$/.test(word) ? word : `'${word.replaceAll("'", "'\\''")}'`);

/** The `boundrail model explain` command line asking `question` about `target`. */
function explainCommand(target, question) {
  const words = ['boundrail', 'model', 'explain', ...target.subjects];
  for (const flag of ['relationship', 'flow', 'proposal']) {
    if (target[flag]) words.push(`--${flag}`, target[flag]);
  }
  return [...words, '--question', question].map(shellWord).join(' ');
}

/** Asks the open question: through the server when it was started with --allow-explain, else by
 * copying the command line for the user to run. The answer is shown as text. */
async function send(text) {
  const ask = app.ask;
  if (!ask || !text) return;
  ask.question = text;
  app.explainAllowed ??= api('explain').then((status) => status.allowed, () => false);
  if (!(await app.explainAllowed)) {
    const command = explainCommand(ask.target, text);
    ask.answer = `This view runs the agent only when started with --allow-explain. Run in a terminal:\n${command}`;
    try {
      await navigator.clipboard.writeText(command);
      toast('Command copied');
    } catch {
      toast('Copying is not allowed here');
    }
    return renderDetail();
  }
  ask.busy = true;
  ask.answer = 'Asking the agent';
  renderDetail();
  const [out, error] = await attempt(api('explain', { ...ask.target, question: text }, false, 'POST'));
  ask.busy = false;
  ask.answer = out ? out.answer : error;
  if (app.ask === ask) renderDetail();
}

// ---------------------------------------------------------------- canvas

/** A pane's own state, view and model; `box` is its zoom, `base` the drawing's bounds, `beat`
 * the walkthrough beat or picture it draws, null when the pane was set by hand. */
function newPane(from) {
  const { state = { kind: 'current' }, against = '', view = 'architecture', flow = null, scope = null, depth = 2 } = from ?? {};
  return { state: { ...state }, against, view, flow, scope, depth, beat: null, model: from?.model ?? null, loads: 0, ticket: 0 };
}

function buildPanes() {
  const host = $('#panes');
  app.panes.forEach((pane) => cancelAnimationFrame(pane.glide));
  host.replaceChildren();
  host.classList.toggle('split', app.split);
  app.panes.forEach((pane, i) => {
    pane.crumbs = h('div', { class: 'crumbs' });
    pane.scale = h('div', { class: 'scale', 'aria-hidden': 'true' });
    pane.sheet = h('div', {
      class: 'sheet',
      tabindex: '0',
      'aria-label': 'Diagram. Drag to pan, scroll to zoom, plus and minus keys zoom, 0 fits.',
    }, pane.scale);
    pane.el = h('div', { class: i === app.active ? 'pane active' : 'pane', onpointerdown: () => activate(i) },
      pane.crumbs, pane.sheet);
    Object.assign(pane, { svg: null, text: null, key: '', box: null, base: null });
    attachPanZoom(pane);
    host.append(pane.el);
  });
}

function renderCrumbs(pane) {
  const model = pane.model ?? EMPTY;
  const crumbs = [];
  const go = (scope, label) => h('button', {
    onclick: () => { pane.scope = scope; pane.view = 'architecture'; redraw(pane); },
  }, label);
  const name = model.data?.manifest?.name ?? 'Model';
  const atTop = pane.view === 'architecture' && !pane.scope;
  if (pane.view === 'architecture') {
    if (atTop) crumbs.push(h('span', { class: 'here', text: name }));
    else {
      crumbs.push(go(null, name));
      for (const a of ancestors(model, pane.scope).reverse()) crumbs.push(' / ', go(a, nameOf(model, a)));
      crumbs.push(' / ', h('span', { class: 'here', text: nameOf(model, pane.scope) }));
    }
  } else {
    const flow = flows(model).find((f) => f.id === pane.flow);
    crumbs.push(go(null, name), ' / ', h('span', { class: 'here', text: `${flow?.name ?? 'Flow'}, ${pane.view}` }));
  }
  const compared = baseOf(pane) ? `, compared with ${againstLabel(baseOf(pane))}` : '';
  crumbs.push(h('span', { class: 'caption', text: `${stateLabel(pane.state)}${compared}` }));
  pane.crumbs.replaceChildren(...crumbs);
  pane.crumbs.hidden = atTop && pane.state.kind === 'current' && !compared;
}

function clearSheet(pane) {
  cancelAnimationFrame(pane.glide);
  pane.sheet.querySelector('svg, .note')?.remove();
  Object.assign(pane, { svg: null, text: null, key: '' });
  pane.scale.textContent = '';
}

function showNote(pane, text, error = false) {
  clearSheet(pane);
  pane.sheet.prepend(h('div', { class: error ? 'note error' : 'note', text }));
}

/** The bounds of what `svg` draws, leaving out its background, with a little room around. */
function contentBox(svg) {
  let low = [Infinity, Infinity];
  let high = [-Infinity, -Infinity];
  for (const node of svg.children) {
    if (node === svg.firstElementChild && node.tagName === 'rect') continue;
    const b = node.getBBox();
    low = [Math.min(low[0], b.x), Math.min(low[1], b.y)];
    high = [Math.max(high[0], b.x + b.width), Math.max(high[1], b.y + b.height)];
  }
  if (low[0] === Infinity) return null;
  return { x: low[0] - FIT_PAD, y: low[1] - FIT_PAD, w: high[0] - low[0] + 2 * FIT_PAD, h: high[1] - low[1] + 2 * FIT_PAD };
}

/** Draws what the pane asks for, keeping its zoom when the picture is the same one redrawn. */
async function drawPane(pane) {
  renderCrumbs(pane);
  const flowView = FLOW_VIEWS.includes(pane.view);
  if (flowView && !pane.flow && !pane.beat) return showNote(pane, 'This state has no flows to draw.');
  // A beat is drawn as the server reads it, focus included.
  const params = pane.beat ? { ...pane.beat } : {
    ...stateParams(pane.state),
    view: pane.view,
    flow: flowView ? pane.flow : null,
    scope: flowView ? null : pane.scope,
    depth: flowView ? null : pane.depth,
    against: pane.against || null,
  };
  const ticket = ++pane.ticket;
  pane.sheet.classList.toggle('loading', JSON.stringify(params) !== pane.key);
  const [text, error] = await attempt(api('render', params, true));
  if (ticket !== pane.ticket) return;
  pane.sheet.classList.remove('loading');
  if (error) return showNote(pane, error, true);
  if (text === pane.text) return decorate(pane);
  const doc = new DOMParser().parseFromString(text, 'image/svg+xml');
  if (doc.documentElement.nodeName !== 'svg') return showNote(pane, 'The server sent no picture.', true);
  const svg = document.importNode(doc.documentElement, true);
  // The pane's header names what is drawn; the picture's own title only labels it.
  const title = svg.querySelector(':scope > text');
  svg.setAttribute('aria-label', title?.textContent ?? 'Model diagram');
  title?.remove();
  for (const attribute of ['width', 'height']) svg.removeAttribute(attribute);
  svg.setAttribute('preserveAspectRatio', 'xMidYMid meet');
  svg.setAttribute('role', 'img');
  const key = JSON.stringify(params);
  const kept = pane.key === key && pane.box;
  clearSheet(pane);
  pane.sheet.prepend(svg);
  dress(svg, pane.model ?? EMPTY);
  Object.assign(pane, { svg, text, key });
  const [x, y, w, height] = svg.getAttribute('viewBox').split(/\s+/).map(Number);
  pane.base = contentBox(svg) ?? { x, y, w, h: height };
  if (!kept) pane.box = fitBox(pane);
  applyBox(pane);
  decorate(pane);
}

const SVG_NS = 'http://www.w3.org/2000/svg';
function svgNode(tag, attributes) {
  const node = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attributes)) node.setAttribute(k, v);
  return node;
}

function fitText(text, size, room) {
  text.style.fontSize = `${size}px`;
  const length = text.getComputedTextLength();
  if (length > room) text.style.fontSize = `${Math.max(11, (size * room) / length).toFixed(1)}px`;
}

function ellipsize(text, size, room) {
  text.style.fontSize = `${size}px`;
  const full = text.textContent;
  if (text.getComputedTextLength() <= room) return;
  let kept = full.length;
  while (kept > 1 && text.getComputedTextLength() > room) text.textContent = `${full.slice(0, --kept).trimEnd()}\u2026`;
  text.append(svgNode('title', {}));
  text.lastChild.textContent = full;
}

function dress(svg, model) {
  for (const group of svg.querySelectorAll('g.module, g.lifeline')) {
    const kind = model.modules.get(group.dataset.id)?.kind;
    const box = group.querySelector(':scope > rect');
    const [title, kindLine] = group.querySelectorAll(':scope > text');
    if (!kind || !box || !title) continue;
    paint(group, kind, model);
    const [x, y, width] = ['x', 'y', 'width'].map((a) => Number(box.getAttribute(a)));
    const left = x + 14;
    const titleY = kindLine ? y + 22 : Number(title.getAttribute('y'));
    const room = x + width - (left + 12) - 8;
    for (const [text, at] of [[title, titleY], [kindLine, y + 39]]) {
      if (!text) continue;
      Object.entries({ x: left + 12, y: at, 'text-anchor': 'start' }).forEach(([k, v]) => text.setAttribute(k, v));
    }
    if (kindLine) kindLine.textContent = kindLine.textContent.replace(/^\[([^\]]*)\]/, '$1');
    if (group.classList.contains('lifeline')) ellipsize(title, 13, room);
    else fitText(title, 15, room);
    if (kindLine) fitText(kindLine, 12, room);
    box.after(svgNode('circle', { class: 'kind-dot', cx: left + 3.5, cy: titleY - 5, r: 3.5 }));
  }
  for (const box of svg.querySelectorAll('rect[fill="#fff9db"]')) {
    const [x, y, height] = ['x', 'y', 'height'].map((a) => Number(box.getAttribute(a)));
    box.classList.add('note-box');
    box.after(svgNode('rect', { class: 'note-rule', x: x + 1, y: y + 6, width: 2, height: height - 12, rx: 1 }));
  }
  for (const step of svg.querySelectorAll('g.step')) {
    const box = step.querySelector(':scope > rect');
    const text = step.querySelector(':scope > text');
    const parts = text?.textContent.match(/^(.+?) -> (.+?): (.*)$/);
    if (!box || !text) continue;
    if (parts) {
      text.textContent = '';
      text.append(svgNode('tspan', { class: 'actor' }), ' \u2192 ', svgNode('tspan', { class: 'actor' }), `: ${parts[3]}`);
      const [from, to] = text.querySelectorAll('tspan');
      from.textContent = parts[1];
      to.textContent = parts[2];
    }
    const [x, y, width, height] = ['x', 'y', 'width', 'height'].map((a) => Number(box.getAttribute(a)));
    text.style.fontSize = '15px';
    const need = text.getComputedTextLength() + 32;
    const grown = Math.max(width, need);
    box.setAttribute('x', x + width / 2 - grown / 2);
    box.setAttribute('width', grown);
    Object.entries({ x: x + width / 2 - grown / 2 + 16, y: y + height / 2 + 5, 'text-anchor': 'start' })
      .forEach(([k, v]) => text.setAttribute(k, v));
  }
}

function redraw(pane) {
  pane.beat = null;
  if (pane === activePane()) {
    renderControls();
    remember();
    renderFlows();
  }
  drawPane(pane);
}

function applyBox(pane) {
  pane.frame = 0;
  const { x, y, w, h: height } = pane.box;
  pane.svg.setAttribute('viewBox', `${x} ${y} ${w} ${height}`);
  pane.scale.textContent = `${Math.round(100 / mapping(pane, pane.sheet.getBoundingClientRect()).scale)}%`;
}

function fitBox(pane) {
  const rect = pane.sheet.getBoundingClientRect();
  const bar = $('#play');
  const below = pane === activePane() && !bar.hidden ? bar.offsetHeight + 24 : 0;
  const room = { w: Math.max(rect.width, 1), h: Math.max(rect.height - below, 1) };
  const { x, y, w, h: height } = pane.base;
  const sequence = Boolean(pane.svg.querySelector('g.lifeline'));
  const readsFromTop = phone.matches || sequence;
  let scale = readsFromTop ? Math.min(room.w / w, FIT_MAX) : Math.min(room.w / w, room.h / height, FIT_MAX);
  if (sequence && !phone.matches) scale = Math.max(scale, 1);
  const box = { w: room.w / scale, h: Math.max(rect.height, 1) / scale };
  const y0 = readsFromTop ? y - TOP_PAD / scale : y + height / 2 - room.h / (2 * scale);
  const x0 = w * scale > room.w ? x - TOP_PAD / scale : x + w / 2 - box.w / 2;
  return { ...box, x: x0, y: y0 };
}

/** Applies the pane's zoom on the next frame, once however many events changed it. */
function schedule(pane) {
  cancelAnimationFrame(pane.glide);
  if (pane.svg && !pane.frame) pane.frame = requestAnimationFrame(() => applyBox(pane));
}

/** Drawing units per screen pixel and the screen point of the box's top left, for `rect`. */
function mapping(pane, rect) {
  const { x, y, w, h: height } = pane.box;
  const scale = Math.max(w / rect.width, height / rect.height);
  return { scale, left: x - (rect.width * scale - w) / 2, top: y - (rect.height * scale - height) / 2 };
}

function zoomAt(pane, factor, clientX, clientY) {
  if (!pane.svg) return;
  const ratio = pane.base.w / (pane.box.w * factor);
  if (ratio < ZOOM_MIN || ratio > ZOOM_MAX) return;
  const rect = pane.sheet.getBoundingClientRect();
  const { scale, left, top } = mapping(pane, rect);
  const at = { x: left + (clientX - rect.left) * scale, y: top + (clientY - rect.top) * scale };
  const { x, y, w, h: height } = pane.box;
  pane.box = { x: at.x - (at.x - x) * factor, y: at.y - (at.y - y) * factor, w: w * factor, h: height * factor };
  schedule(pane);
}

function zoomCenter(pane, factor) {
  const r = pane.sheet.getBoundingClientRect();
  zoomAt(pane, factor, r.left + r.width / 2, r.top + r.height / 2);
}

function panBy(pane, dx, dy, from = pane.box, scale = mapping(pane, pane.sheet.getBoundingClientRect()).scale) {
  pane.box = { ...from, x: from.x - dx * scale, y: from.y - dy * scale };
  schedule(pane);
}

const slightlyCloser = (box) => ({ x: box.x + box.w * 0.02, y: box.y + box.h * 0.02, w: box.w * 0.96, h: box.h * 0.96 });

function fit(pane, glide = false) {
  if (!pane.svg) return;
  const to = fitBox(pane);
  if (!glide || matchMedia('(prefers-reduced-motion: reduce)').matches) {
    pane.box = to;
    return schedule(pane);
  }
  const fitted = ['x', 'y', 'w', 'h'].every((k) => Math.abs(pane.box[k] - to[k]) < 0.5);
  const from = fitted ? slightlyCloser(to) : { ...pane.box };
  const start = performance.now();
  const step = (now) => {
    if (!pane.svg) return;
    const t = Math.min(1, (now - start) / FIT_GLIDE);
    const e = 1 - (1 - t) ** 3;
    pane.box = Object.fromEntries(['x', 'y', 'w', 'h'].map((k) => [k, from[k] + (to[k] - from[k]) * e]));
    applyBox(pane);
    pane.glide = t < 1 ? requestAnimationFrame(step) : 0;
  };
  cancelAnimationFrame(pane.glide);
  pane.glide = requestAnimationFrame(step);
}

/** What the canvas keys do. */
const KEYS = {
  '+': (pane) => zoomCenter(pane, 1 / ZOOM_STEP),
  '=': (pane) => zoomCenter(pane, 1 / ZOOM_STEP),
  '-': (pane) => zoomCenter(pane, ZOOM_STEP),
  0: fit,
  ArrowLeft: (pane) => panBy(pane, KEY_STEP, 0),
  ArrowRight: (pane) => panBy(pane, -KEY_STEP, 0),
  ArrowUp: (pane) => panBy(pane, 0, KEY_STEP),
  ArrowDown: (pane) => panBy(pane, 0, -KEY_STEP),
};

function attachPanZoom(pane) {
  const sheet = pane.sheet;
  let drag = null;
  sheet.addEventListener('wheel', (event) => {
    if (!pane.svg) return;
    event.preventDefault();
    zoomAt(pane, Math.exp(event.deltaY * WHEEL_RATE), event.clientX, event.clientY);
  }, { passive: false });
  sheet.addEventListener('pointerdown', (event) => {
    if (!pane.svg || event.button !== 0) return;
    const scale = mapping(pane, sheet.getBoundingClientRect()).scale;
    drag = { x: event.clientX, y: event.clientY, box: { ...pane.box }, scale, moved: false };
    sheet.setPointerCapture(event.pointerId);
  });
  sheet.addEventListener('pointermove', (event) => {
    if (!drag) return;
    const dx = event.clientX - drag.x;
    const dy = event.clientY - drag.y;
    if (!drag.moved && Math.hypot(dx, dy) < DRAG_PX) return;
    drag.moved = true;
    sheet.classList.add('dragging');
    panBy(pane, dx, dy, drag.box, drag.scale);
  });
  const end = (event) => {
    const click = drag && !drag.moved && event.type === 'pointerup';
    drag = null;
    sheet.classList.remove('dragging');
    const id = click ? moduleAt(pane, event) : null;
    if (id) (adds(event) ? toggleSelect : select)(id);
    else if (click && phone.matches) setDrawer(false);
  };
  sheet.addEventListener('pointerup', end);
  sheet.addEventListener('pointercancel', end);
  sheet.addEventListener('dblclick', (event) => {
    const id = moduleAt(pane, event);
    if (id) drill(pane, id);
  });
  sheet.addEventListener('keydown', (event) => {
    // While a walkthrough plays, Left and Right step through its beats instead of panning.
    if (stepKey(event)) return;
    if (!pane.svg || !KEYS[event.key]) return;
    event.preventDefault();
    KEYS[event.key](pane);
  });
}

/** The module of the pane's model a pointer event is over: a box or a lifeline. */
function moduleAt(pane, event) {
  const group = document.elementFromPoint(event.clientX, event.clientY)?.closest('g[data-id]');
  const id = group?.dataset.id;
  return id && pane.model?.modules.has(id) ? id : null;
}

function drill(pane, id) {
  const m = pane.model?.modules.get(id);
  if (!m?.children.length && m?.references?.length) return select(id, true);
  if (!m?.children.length) return;
  pane.scope = id;
  pane.view = 'architecture';
  redraw(pane);
}

/** Marks the selection and warning subjects, and dims what focus mode leaves out. */
function decorate(pane) {
  if (!pane.svg) return;
  const model = pane.model ?? EMPTY;
  const chosen = new Set(app.selection.filter((id) => model.modules.has(id)));
  const near = app.focus && chosen.size ? neighbourhood(model, [...chosen]) : null;
  const beatFocus = Boolean(pane.beat && pane.svg.querySelector('g.dimmed'));
  const focus = new Set(beatFocus ? app.play?.walkthrough?.beats[app.play.index]?.reading?.focus ?? [] : []);
  const shownIds = new Set(focus);
  const ties = (model.data?.relationships ?? []).filter((r) => focus.has(r.from) || focus.has(r.to));
  ties.forEach((r) => { shownIds.add(r.from); shownIds.add(r.to); });
  const shownTies = new Set(ties.map((r) => r.id));
  for (const wrapper of pane.svg.querySelectorAll('g.dimmed')) {
    const id = wrapper.querySelector('[data-id]')?.dataset.id;
    wrapper.classList.toggle('undim', shownIds.has(id) || shownTies.has(id));
  }
  const filtering = app.filter.trim() !== '' || app.kinds.size > 0;
  const matched = filtering ? keptModules(model) : null;
  const rels = new Map((model.data?.relationships ?? []).map((r) => [r.id, r]));
  const linked = new Set((model.data?.relationships ?? [])
    .filter((r) => chosen.has(r.from) || chosen.has(r.to)).map((r) => r.id));
  for (const group of pane.svg.querySelectorAll('g[data-id]')) {
    const id = group.dataset.id;
    group.classList.toggle('is-selected', chosen.has(id));
    group.classList.toggle('is-hit', !chosen.has(id) && app.hits.includes(id));
    let kept = true;
    if (near && (group.classList.contains('module') || group.classList.contains('lifeline'))) kept = near.modules.has(id);
    if (near && group.classList.contains('relationship')) kept = near.relationships.has(id);
    group.classList.toggle('is-dim', !kept);
    group.classList.toggle('is-linked', linked.has(id));
    const rel = rels.get(id);
    group.classList.toggle('is-hidden', Boolean(rel) && app.hiddenRels.has(rel.kind));
    group.classList.toggle('is-filtered', Boolean(matched) && (rel ? !(matched.has(rel.from) && matched.has(rel.to))
      : group.classList.contains('module') && !matched.has(id)));
    group.classList.toggle('is-focus', beatFocus && group.classList.contains('module')
      && !group.closest('g.dimmed') && !group.querySelector(':scope > rect[fill-opacity]'));
  }
}

// ---------------------------------------------------------------- actions

/** Whether a click adds to the selection or takes from it rather than replacing it. */
const adds = (event) => event.shiftKey || event.metaKey || event.ctrlKey;

/** Selects module `id` alone, or nothing for null; `reveal` opens the list and the canvas
 * where it is. */
function select(id, reveal = false) {
  setSelection(id ? [id] : [], reveal ? id : null);
}

/** Adds module `id` to the selection, or takes it out when it is in. */
function toggleSelect(id) {
  const has = app.selection.includes(id);
  setSelection(has ? app.selection.filter((x) => x !== id) : [...app.selection, id], null);
}

/** Selects modules `ids`; `id` names the one to reveal, if any. */
function setSelection(ids, id) {
  app.selection = ids;
  app.hits = [];
  const model = shown();
  if (id) {
    ancestors(model, id).forEach((a) => app.collapsed.delete(a));
    const pane = activePane();
    const drawn = !pane.scope || (id !== pane.scope && subtree(model, pane.scope).includes(id));
    if (pane.view === 'architecture' && !drawn) {
      pane.scope = null;
      pane.beat = null;
      drawPane(pane);
    }
  }
  $('#announce').textContent = ids.length > 1 ? `${ids.length} modules selected`
    : ids.length ? `Selected: ${nameOf(model, ids[0])}` : 'Selection cleared';
  if (ids.length && phone.matches) setDrawer(true);
  renderTree();
  renderDetail();
  renderControls();
  remember();
  app.panes.forEach(decorate);
}

function focusSubjects(subjects) {
  const model = shown();
  const relationships = new Map((model.data?.relationships ?? []).map((r) => [r.id, r]));
  const modules = subjects.flatMap((s) => (relationships.has(s)
    ? [relationships.get(s).from, relationships.get(s).to]
    : [s])).filter((s) => model.modules.has(s));
  const flow = flows(model).find((f) => subjects.includes(f.id));
  if (!modules.length && flow) return openFlow(flow.id);
  if (!modules.length) return;
  select(modules[0], true);
  app.hits = modules;
  app.panes.forEach(decorate);
}

function openFlow(id) {
  const pane = activePane();
  pane.flow = id;
  if (pane.view === 'architecture') pane.view = 'sequence';
  redraw(pane);
}

function setView(view) {
  const pane = activePane();
  pane.view = view;
  if (FLOW_VIEWS.includes(view) && !pane.flow) pane.flow = flows(shown())[0]?.id ?? null;
  redraw(pane);
}

/** Shows `state` in the active pane; the other pane keeps its own. */
async function setState(state) {
  const pane = activePane();
  pane.state = state;
  pane.beat = null;
  if (pane.against === 'current' && state.kind === 'current') pane.against = '';
  if (state.kind === 'proposal' && pane.against === state.id) pane.against = '';
  await loadPane(pane);
  if (pane === activePane()) showActive();
}

function activate(i) {
  if (i === app.active) return;
  app.active = i;
  app.panes.forEach((p, k) => p.el.classList.toggle('active', k === i));
  showActive();
}

function toggleSplit() {
  app.split = !app.split;
  if (app.split) {
    const copy = newPane(activePane());
    // A second pane starts on the first draft, if any, so the two read as before and after.
    const draft = app.proposals.find((p) => p.status === 'draft');
    if (draft && copy.state.kind === 'current') copy.state = { kind: 'proposal', id: draft.id };
    app.panes = [activePane(), copy];
  } else {
    app.panes = [activePane()];
  }
  app.active = 0;
  buildPanes();
  Promise.all(app.panes.map(loadPane)).then(showActive);
}

// ---------------------------------------------------------------- walkthroughs

const STEP_KEYS = { ArrowLeft: -1, ArrowRight: 1 };
/** How far a key press steps through the walkthrough being played: -1, 1, or 0 for none. */
const stepKey = (event) => (app.play && !(event.altKey || event.metaKey || event.ctrlKey || event.shiftKey)
  ? STEP_KEYS[event.key] ?? 0 : 0);
const localId = (id) => id.split('#').pop();

/** The pane state a reading's `state` names. */
function readingState(state) {
  if (!state || state === 'current') return { kind: 'current' };
  if (state.proposal) return { kind: 'proposal', id: localId(state.proposal) };
  return { kind: 'checkpoint', id: state.checkpoint };
}

/** The pane's comparison a reading's `against` names: '' for none. */
function readingAgainst(against) {
  if (!against) return '';
  return against === 'current' ? 'current' : localId(against.proposal ?? '');
}

/** Starts playing walkthrough `id` at beat `index`, counted from 0. */
async function playWalkthrough(id, index = 0) {
  app.play = { id, walkthrough: null, index, error: null };
  await refreshPlay();
  applyBeat();
  await showBeat(true);
}

/** Reads the walkthrough being played again, keeping its beat where it still has one; a pane
 * changed by hand keeps what it shows. */
async function refreshPlay() {
  if (!app.play) return;
  const [data, error] = await attempt(api('walkthrough', { id: app.play.id }));
  app.play.error = error;
  if (!data) {
    // Gone or unreadable: the panel says why, and the pane keeps what it shows.
    app.play.walkthrough = null;
    activePane().beat = null;
    return;
  }
  const { walkthrough } = data;
  app.play.id = walkthrough.id;
  app.play.walkthrough = walkthrough;
  app.play.index = Math.max(0, Math.min(app.play.index, walkthrough.beats.length - 1));
  if (activePane().beat) applyBeat();
}

/** Sets the active pane to what the current beat reads; drawing it is the caller's. */
function applyBeat() {
  const { walkthrough, index } = app.play;
  const beat = walkthrough?.beats[index];
  const pane = activePane();
  if (!beat) {
    pane.beat = null;
    return;
  }
  applyReading(pane, beat.reading, { walkthrough: walkthrough.id, beat: index + 1 });
}

/** Sets `pane` to what `reading` shows; `drawn` names it to `/api/render`, which applies the focus. */
function applyReading(pane, reading, drawn) {
  Object.assign(pane, {
    state: readingState(reading.state),
    against: readingAgainst(reading.against),
    view: reading.view,
    flow: reading.flow ?? null,
    scope: reading.scope ?? null,
    depth: reading.depth ?? 2,
    beat: drawn,
  });
}

/** Draws the current beat in the active pane, fitted to it, and says which beat it is. The
 * sidebar and header follow only when `moved` says the pane's state or comparison changed. */
async function showBeat(moved) {
  const pane = activePane();
  renderPlay();
  if (!pane.beat) return renderWalkthroughs();
  await loadPane(pane);
  fit(pane);
  if (moved) {
    showActive();
  } else {
    renderControls();
    remember();
    renderFlows();
    renderDetail();
  }
  const { walkthrough, index } = app.play;
  $('#announce').textContent = `Beat ${index + 1} of ${walkthrough.beats.length}: ${walkthrough.beats[index].title}`;
}

/** Shows beat `k` of the walkthrough being played, counted from 0; does nothing past either end. */
function goBeat(k) {
  const beats = app.play?.walkthrough?.beats ?? [];
  if (k < 0 || k >= beats.length) return;
  const pane = activePane();
  const before = JSON.stringify([pane.state, pane.against]);
  app.play.index = k;
  applyBeat();
  showBeat(JSON.stringify([pane.state, pane.against]) !== before);
}

function stepBeat(by) {
  if (app.play) goBeat(app.play.index + by);
}

async function endWalkthrough() {
  const { id } = app.play;
  app.play = null;
  const pane = activePane();
  pane.beat = null;
  renderPlay();
  renderDetail();
  renderControls();
  remember();
  drawPane(pane);
  await renderWalkthroughs();
  $('#walkthroughs').querySelector(`[data-key="${CSS.escape(id)}"]`)?.focus();
}

/** The play panel: where the walkthrough is, the beat's title and narration, and the controls. */
function renderPlay() {
  const panel = $('#play');
  panel.hidden = !app.play;
  if (!app.play) return panel.replaceChildren();
  const { walkthrough, index, error } = app.play;
  const beats = walkthrough?.beats ?? [];
  const beat = beats[index];
  const end = h('button', { class: 'end icon', 'data-key': 'end', title: 'End walkthrough', 'aria-label': 'End walkthrough', onclick: endWalkthrough }, icon(CLOSE_ICON));
  if (!walkthrough || !beat) {
    const why = error ?? `${walkthrough?.name ?? 'This walkthrough'} has no beats yet.`;
    return keepFocus(panel, () => panel.replaceChildren(h('div', { class: 'bar' }, h('p', { class: 'narration', text: why }), end)));
  }
  const dots = beats.map((b, k) => h('button', {
    class: 'dot',
    'data-key': `dot:${k}`,
    'aria-label': `Beat ${k + 1}: ${b.title}`,
    'aria-current': k === index ? 'step' : null,
    title: b.title,
    onclick: () => goBeat(k),
  }));
  // The step buttons stay focusable at either end, so focus is not lost on reaching it.
  const step = (key, by, label, last) => h('button', {
    'data-key': key,
    class: key,
    'aria-disabled': last ? 'true' : null,
    'aria-keyshortcuts': by < 0 ? 'ArrowLeft' : 'ArrowRight',
    onclick: () => stepBeat(by),
  }, label);
  const narration = h('p', { class: 'narration', text: beat.narration });
  keepFocus(panel, () => panel.replaceChildren(
    h('div', { class: 'bar' },
      h('span', { class: 'where', title: walkthrough.name, 'aria-label': `${walkthrough.name}, beat ${index + 1} of ${beats.length}` },
        `${index + 1}/${beats.length}`),
      h('h2', { class: 'beat-title', text: beat.title }),
      h('div', { class: 'dots', role: 'group', 'aria-label': 'Beats' }, dots),
      h('div', { class: 'steps' },
        step('prev', -1, 'Previous', index === 0),
        step('next', 1, 'Next', index === beats.length - 1)),
      end),
    narration,
  ));
  // A narration longer than its box scrolls, so keyboard users can reach it.
  if (narration.scrollHeight > narration.clientHeight) {
    narration.tabIndex = 0;
    narration.setAttribute('role', 'region');
    narration.setAttribute('aria-label', `Narration of beat ${index + 1}`);
  }
}

/** Ends the walkthrough being played, if one is, leaving the pane as it is. */
function stopPlaying() {
  if (!app.play) return;
  app.play = null;
  renderPlay();
}

// ---------------------------------------------------------------- address

const SHOWN_KEYS = ['proposal', 'checkpoint', 'against', 'view', 'flow', 'scope', 'depth', 'saved', 'picture', 'walkthrough', 'beat'];

/** The hash of what `pane` shows; `chosen` adds the selection and focus, which belong to the
 * active pane. */
function hashOf(pane, chosen) {
  const values = {};
  const beat = chosen && app.play ? { walkthrough: app.play.id, beat: app.play.index + 1 } : pane.beat;
  if (beat?.walkthrough) Object.assign(values, { walkthrough: beat.walkthrough, beat: beat.beat });
  else if (beat?.saved) values.saved = beat.saved;
  else if (beat?.picture) values.picture = beat.picture;
  else {
    if (pane.state.kind !== 'current') values[pane.state.kind] = pane.state.id;
    values.against = pane.against;
    if (FLOW_VIEWS.includes(pane.view)) Object.assign(values, { view: pane.view, flow: pane.flow });
    else Object.assign(values, { scope: pane.scope, depth: pane.depth === 2 ? null : pane.depth });
  }
  const params = new URLSearchParams();
  for (const key of SHOWN_KEYS) {
    if (values[key] !== null && values[key] !== undefined && values[key] !== '') params.set(key, values[key]);
  }
  const ids = chosen ? app.selection : [];
  if (ids.length === 1 && !params.size && !app.focus) return encodeURIComponent(ids[0]);
  ids.forEach((id) => params.append('select', id));
  if (ids.length && app.focus) params.set('focus', '1');
  return params.toString();
}

/** What a hash shows, as `hashOf` writes it, `#walkthrough=<id>&beat=<n>` included; what does
 * not parse is left out. */
function readHash(hash) {
  const text = hash.replace(/^#/, '');
  const shown = { select: [], focus: false };
  if (!text.includes('=')) {
    try {
      if (text) shown.select.push(decodeURIComponent(text));
    } catch { /* a malformed hash selects nothing */ }
    return shown;
  }
  const params = new URLSearchParams(text);
  for (const key of SHOWN_KEYS) if (params.get(key)) shown[key] = params.get(key);
  for (const key of ['depth', 'beat']) shown[key] = /^\d+$/.test(shown[key] ?? '') && Number(shown[key]) > 0 ? Number(shown[key]) : undefined;
  if (!FLOW_VIEWS.includes(shown.view)) delete shown.view;
  shown.select = params.getAll('select').filter(Boolean);
  shown.focus = params.get('focus') === '1';
  return shown;
}

/** Sets `pane` to the state, view and comparison `shown` names; drawing it is the caller's. */
function setPane(pane, shown) {
  const state = (shown.proposal && { kind: 'proposal', id: shown.proposal })
    || (shown.checkpoint && { kind: 'checkpoint', id: shown.checkpoint }) || { kind: 'current' };
  Object.assign(pane, {
    state,
    against: shown.against ?? '',
    view: shown.view ?? 'architecture',
    flow: shown.flow ?? null,
    scope: shown.scope ?? null,
    depth: shown.depth ?? 2,
    beat: null,
  });
}

/** Shows what `shown` reads in the active pane: after Back or Forward, a pasted address, or a
 * reading `model view --show` sent. The address entry is replaced, not added to. */
async function restore(shown) {
  app.restoring = true;
  try {
    Object.assign(app, { selection: shown.select, focus: shown.focus, hits: [] });
    if (shown.walkthrough) return await playWalkthrough(shown.walkthrough, (shown.beat ?? 1) - 1);
    stopPlaying();
    if (shown.saved) return await showView(shown.saved);
    if (shown.picture) return await showPicture(shown.picture);
    const pane = activePane();
    setPane(pane, shown);
    await loadPane(pane);
    fit(pane);
    showActive();
  } finally {
    app.restoring = false;
    remember();
  }
}

/** Puts what the active pane shows in the address, as a new history entry so Back returns to
 * what it showed before, and tells the server what every pane shows. */
function remember() {
  report();
  const hash = hashOf(activePane(), true);
  if (hash === location.hash.replace(/^#/, '')) return;
  const url = `${location.pathname}${location.search}${hash ? `#${hash}` : ''}`;
  history[app.restoring ? 'replaceState' : 'pushState'](null, '', url);
}

/** Tells the server what this page's panes show, for `model view --panes` and `--show`, once
 * per change. */
function report(again = false) {
  if (!app.page) return;
  const panes = app.panes.map((pane, i) => `#${hashOf(pane, i === app.active)}`);
  const key = JSON.stringify([app.page, app.active, panes]);
  if (key === report.last && !again) return;
  report.last = key;
  api('pane-state', { page: app.page, active: app.active, pane: panes }, false, 'POST').catch(() => { report.last = null; });
}

// ---------------------------------------------------------------- live updates

/** Follows a `changed` event: everything when proposals or history changed, the revision differs
 * from the last event's or the last load of the current state failed, only claims and checks
 * when the Boundrail state alone changed. */
async function onChanged(event) {
  let news;
  try {
    news = JSON.parse(event.data);
  } catch {
    news = { revision: null, parts: ALL_PARTS };
  }
  const parts = new Set(news.parts ?? ALL_PARTS);
  const revision = news.revision ?? null;
  const moved = revision !== app.eventRevision || app.currentFailed;
  app.eventRevision = revision;
  if (parts.has('proposals') || parts.has('history') || moved) {
    await loadAll();
  } else if (parts.has('state')) {
    forget();
    const pane = activePane();
    pane.model = await loadModel(pane.state);
    showActive();
  } else {
    return;
  }
  if (shown().data) toast('Model updated');
}

let backoff = BACKOFF.first;
let offline = false;

/** Listens for changes. The browser reconnects a dropped stream itself; only a stream it gave
 * up on is opened again here, after a growing wait. */
function connect() {
  const source = new EventSource(`/api/events?t=${encodeURIComponent(TOKEN)}`);
  source.addEventListener('open', () => {
    backoff = BACKOFF.first;
    setLive(true);
    if (offline) {
      toast('Live updates are back');
      loadAll();
    }
    offline = false;
  });
  source.addEventListener('changed', onChanged);
  source.addEventListener('hello', (event) => {
    app.page = JSON.parse(event.data).page;
    report(true);
  });
  source.addEventListener('show', (event) => {
    const { reading } = JSON.parse(event.data);
    history.pushState(null, '', `${location.pathname}${reading ? `#${reading}` : ''}`);
    toast('Shown from the command line');
    restore(readHash(reading)).then(() => {
      const { el } = activePane();
      el.classList.remove('arrived');
      void el.offsetWidth;
      el.classList.add('arrived');
    });
  });
  source.addEventListener('error', () => {
    if (!offline) toast('Live updates offline, reconnecting');
    offline = true;
    setLive(false);
    if (source.readyState === EventSource.CLOSED) {
      setTimeout(connect, backoff);
      backoff = Math.min(backoff * 2, BACKOFF.most);
    }
  });
}

// ---------------------------------------------------------------- start

function setDrawer(open) {
  $('#drawer').classList.toggle('open', open);
  $('#drawer-handle').setAttribute('aria-expanded', String(open));
}

function closeSheets() {
  $('.topbar').classList.remove('menu-open');
  $('#menu').setAttribute('aria-expanded', 'false');
  setDrawer(false);
}

function bind() {
  $('#state').addEventListener('change', (event) => setState(parseState(event.target.value)));
  $('#against').addEventListener('change', (event) => {
    activePane().against = event.target.value;
    showActive();
    redraw(activePane());
  });
  for (const button of document.querySelectorAll('[data-view]')) {
    button.addEventListener('click', () => setView(button.dataset.view));
  }
  $('#depth').addEventListener('change', (event) => {
    const input = event.target;
    const [low, high] = [Number(input.min), Number(input.max)];
    activePane().depth = Math.max(low, Math.min(high, Number.parseInt(input.value, 10) || low));
    redraw(activePane());
  });
  for (const [id, by] of [['#depth-down', -1], ['#depth-up', 1]]) {
    $(id).addEventListener('click', () => {
      const input = $('#depth');
      input.value = Number(input.value) + by;
      input.dispatchEvent(new Event('change'));
    });
  }
  $('#menu').addEventListener('click', (event) => {
    const open = $('.topbar').classList.toggle('menu-open');
    event.currentTarget.setAttribute('aria-expanded', String(open));
  });
  $('#drawer-handle').addEventListener('click', () => setDrawer(!$('#drawer').classList.contains('open')));
  $('#drawer-close').addEventListener('click', () => setDrawer(false));
  const place = () => (phone.matches ? $('#state-controls') : $('.toolbar')).append($('.stepper'), $('.group'));
  phone.addEventListener('change', place);
  place();
  $('#fit').addEventListener('click', () => fit(activePane(), true));
  // Focus and Split stay focusable when they cannot act, so their tooltips can say why.
  const usable = (event) => event.currentTarget.getAttribute('aria-disabled') !== 'true';
  $('#split').addEventListener('click', (event) => usable(event) && toggleSplit());
  $('#focus').addEventListener('click', (event) => {
    if (!usable(event)) return;
    app.focus = !app.focus;
    renderControls();
    remember();
    app.panes.forEach(decorate);
  });
  $('#filter').addEventListener('input', (event) => {
    app.filter = event.target.value;
    renderTree();
    app.panes.forEach(decorate);
  });
  window.addEventListener('popstate', () => restore(readHash(location.hash)));
  window.addEventListener('focus', () => report(true));
  document.addEventListener('keydown', (event) => {
    const typing = /^(INPUT|SELECT|TEXTAREA)$/.test(document.activeElement?.tagName ?? '');
    if (stepKey(event) && !typing) {
      event.preventDefault();
      stepBeat(stepKey(event));
    } else if (event.key === '/' && !typing) {
      event.preventDefault();
      $('#filter').focus();
    } else if (event.key === 'Escape') {
      if (typing) document.activeElement.blur();
      const menu = $('.topbar').classList.contains('menu-open');
      const sheet = phone.matches && $('#drawer').classList.contains('open');
      if (menu || sheet) return closeSheets();
      select(null);
    }
  });
}

app.panes = [newPane()];
// The address opens what its hash shows; a reading drawn from the vault waits for the model.
const opened = readHash(location.hash);
const recipe = opened.walkthrough || opened.saved || opened.picture;
Object.assign(app, { selection: opened.select, focus: opened.focus });
if (!recipe) setPane(activePane(), opened);
bind();
buildPanes();
renderControls();
loadAll().then(() => {
  if (recipe) return restore(opened);
  app.restoring = false;
  return remember();
});
connect();
