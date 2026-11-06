// App list column: search, sort, keyboard navigation and size updates.

import { $, h, clear, announce } from './dom.js';
import { SOURCE_LABEL, formatBytes, formatDate, hueOf } from './format.js';
import { sizeOf, state } from './state.js';

let onScan = () => {};
/** path -> {row, size} for in-place size updates. */
const rows = new Map();
let resortTimer;
/** Option highlighted by the keyboard (aria-activedescendant). */
let active = null;

export function initList(scanHandler) {
  onScan = scanHandler;
  const list = $('#app-list');
  $('#search').addEventListener('input', (e) => {
    state.query = e.target.value.trim().toLowerCase();
    renderList();
  });
  $('#sort').addEventListener('change', (e) => {
    state.sort = e.target.value;
    renderList();
  });
  list.addEventListener('keydown', onKeyDown);
  list.addEventListener('focus', () => {
    if (!active) setActive(state.selected ?? visible()[0]?.path ?? null);
  });
}

function visible() {
  const q = state.query;
  const apps = state.apps.filter(
    (a) => !q || a.name.toLowerCase().includes(q) || a.bundle_id.toLowerCase().includes(q),
  );
  const by = {
    name: (a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base' }),
    size: (a, b) => (sizeOf(b) ?? -1) - (sizeOf(a) ?? -1),
    used: (a, b) => (b.last_opened ?? -1) - (a.last_opened ?? -1),
  }[state.sort];
  return apps.sort(by);
}

export function renderList() {
  const list = $('#app-list');
  const apps = visible();
  clear(list);
  rows.clear();
  const fragment = document.createDocumentFragment();
  for (const app of apps) fragment.append(buildRow(app));
  list.append(fragment);

  const empty = $('#list-empty');
  empty.hidden = apps.length > 0;
  if (!apps.length) {
    empty.textContent = state.apps.length ? 'No apps match your search.' : 'No apps found.';
  }
  if (active && !rows.has(active)) active = null;
  syncMarks();
}

function buildRow(app) {
  const size = h('span', { class: 'app-size' }, formatBytes(sizeOf(app)));
  const row = h(
    'li',
    {
      class: 'app-row',
      role: 'option',
      id: rowId(app.path),
      'aria-selected': 'false',
      'aria-label': `${app.name}, version ${app.version ?? 'unknown'}`,
      onclick: () => choose(app.path),
    },
    appIcon(app),
    h('span', { class: 'app-name' }, app.name),
    size,
    h(
      'span',
      { class: 'app-meta dim' },
      app.version ? `${app.version} · ` : '',
      app.bundle_id,
    ),
    h(
      'span',
      { class: 'app-meta' },
      h('span', { class: 'badge' }, SOURCE_LABEL[app.source.kind]),
      app.is_apple && h('span', { class: 'badge readonly' }, 'Read-only'),
      h('span', { class: 'dim' }, formatDate(app.last_opened)),
    ),
  );
  rows.set(app.path, { row, size });
  return row;
}

export function appIcon(app, large = false) {
  const icon = h(
    'span',
    { class: large ? 'app-icon large' : 'app-icon', 'aria-hidden': 'true' },
    [...app.name][0]?.toUpperCase() ?? '?',
  );
  icon.style.setProperty('--hue', hueOf(app.bundle_id));
  return icon;
}

/** Stable DOM id for aria-activedescendant (djb2 hash of the path). */
const rowId = (path) => `app-${[...path].reduce((a, c) => (a * 33 + c.codePointAt(0)) >>> 0, 5381)}`;

function syncMarks() {
  for (const [path, { row }] of rows) {
    row.setAttribute('aria-selected', String(path === state.selected));
    row.classList.toggle('active', path === active);
  }
  const list = $('#app-list');
  const entry = rows.get(active);
  if (entry) {
    list.setAttribute('aria-activedescendant', entry.row.id);
    entry.row.scrollIntoView({ block: 'nearest' });
  } else {
    list.removeAttribute('aria-activedescendant');
  }
}

function setActive(path) {
  active = path;
  syncMarks();
}

/** Selects and scans. */
function choose(path) {
  state.selected = path;
  setActive(path);
  onScan(path);
}

/** Highlights `path` in the list without scanning (used for dropped apps). */
export function highlight(path) {
  state.selected = path;
  active = path;
  syncMarks();
}

function onKeyDown(event) {
  const apps = visible();
  if (!apps.length) return;
  const index = apps.findIndex((a) => a.path === active);
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault();
    const next = event.key === 'ArrowDown' ? Math.min(index + 1, apps.length - 1) : Math.max(index - 1, 0);
    const path = apps[index < 0 ? 0 : next].path;
    state.selected = path;
    setActive(path);
    announce(apps.find((a) => a.path === path).name);
  } else if (event.key === 'Enter' && index >= 0) {
    event.preventDefault();
    choose(active);
  }
}

/** Applies an `app-sizes` event without rebuilding the list. */
export function updateSize({ path, size }) {
  state.sizes.set(path, size);
  const entry = rows.get(path);
  if (entry) entry.size.textContent = formatBytes(size);
  if (state.sort === 'size') {
    clearTimeout(resortTimer);
    resortTimer = setTimeout(renderList, 300);
  }
}
