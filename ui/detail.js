// Detail pane: scan result, presets, per-item selection and actions.

import { $, h, clear, showError } from './dom.js';
import { invoke } from './api.js';
import {
  CATEGORY_LABEL, SOURCE_LABEL, basename, formatBytes, formatDate, plural,
} from './format.js';
import { appIcon } from './list.js';
import { state } from './state.js';

const PRESETS = [
  ['AppOnly', 'App only'],
  ['AppAndCaches', 'App + caches'],
  ['Everything', 'Everything'],
];

let handlers = { onUninstall: () => {}, onBrewZap: () => {} };

export function initDetail(actions) {
  handlers = actions;
  showPlaceholder('Select an app to see what it leaves behind.');
}

export function showPlaceholder(message) {
  clear($('#detail'));
  $('#detail').append(h('p', { class: 'detail-empty' }, message));
}

export function showLoading(name) {
  showPlaceholder(`Scanning ${name}…`);
}

/** Groups shown in the pane: scan groups, then other copies. */
function groupsOf(scan) {
  const groups = scan.groups.map((g) => ({ title: CATEGORY_LABEL[g.category], items: g.items }));
  if (scan.other_copies.length) groups.push({ title: 'Other copies', items: scan.other_copies });
  return groups;
}

const allItems = (scan) => groupsOf(scan).flatMap((g) => g.items);

export function selectedTotals() {
  const picked = allItems(state.scan).filter((i) => i.checked && !i.refused);
  return { count: picked.length, size: picked.reduce((sum, i) => sum + i.size, 0) };
}

export function renderDetail() {
  const scan = state.scan;
  const detail = $('#detail');
  clear(detail);
  if (!scan) return showPlaceholder('Select an app to see what it leaves behind.');
  const { app } = scan;

  const scroll = h('div', { class: 'detail-scroll' });
  scroll.append(
    h(
      'div',
      { class: 'detail-head' },
      appIcon(app, true),
      h(
        'div',
        {},
        h('h2', {}, app.name),
        h('div', { class: 'sub' }, `${app.version ? `Version ${app.version} · ` : ''}${app.bundle_id}`),
        h('div', { class: 'sub' }, `${app.path} · Last opened: ${formatDate(app.last_opened)}`),
        h(
          'div',
          { class: 'badges' },
          h('span', { class: 'badge' }, SOURCE_LABEL[app.source.kind]),
          app.is_apple && h('span', { class: 'badge readonly' }, 'Read-only'),
        ),
      ),
    ),
  );

  if (app.source.kind === 'homebrew') scroll.append(brewBox(app));
  if (scan.inaccessible.length) scroll.append(inaccessibleNotice(scan.inaccessible));
  if (!app.is_apple) scroll.append(presetBar());
  for (const group of groupsOf(scan)) scroll.append(groupView(group));

  detail.append(scroll, actionBar());
}

function brewBox(app) {
  const box = h('div', { class: 'brew' }, h('strong', {}, 'Installed with Homebrew'));
  const command = ['brew', 'uninstall', '--cask', '--zap', app.source.cask].join(' ');
  box.append(
    h('div', { class: 'dim' }, 'The cask can remove this app and its files in one step:'),
    h('code', {}, command),
    h(
      'button',
      {
        type: 'button',
        class: 'btn',
        disabled: app.is_apple,
        onclick: () => handlers.onBrewZap(app),
      },
      'Run with Homebrew…',
    ),
  );
  return box;
}

function inaccessibleNotice(list) {
  return h(
    'div',
    { class: 'notice', role: 'note' },
    h('strong', {}, 'Some locations could not be read'),
    h(
      'ul',
      {},
      list.map(([path, why]) => h('li', {}, `${path}: ${why}`)),
    ),
    h('div', { class: 'dim' }, 'Grant Full Disk Access in System Settings to include them.'),
  );
}

function presetBar() {
  const bar = h('div', { class: 'presets' }, h('span', { class: 'dim', id: 'preset-label' }, 'Select'));
  const group = h('div', { class: 'segmented', role: 'radiogroup', 'aria-labelledby': 'preset-label' });
  for (const [value, label] of PRESETS) {
    group.append(
      h(
        'button',
        {
          type: 'button',
          role: 'radio',
          'aria-checked': String(state.preset === value),
          onclick: () => applyPreset(value),
        },
        label,
      ),
    );
  }
  bar.append(group);
  return bar;
}

async function applyPreset(preset) {
  try {
    state.scan = await invoke('apply_preset', { scan: state.scan, preset });
    state.preset = preset;
    renderDetail();
  } catch (e) {
    showError(String(e));
  }
}

function groupView(group) {
  const selectable = group.items.filter((i) => !i.refused);
  const checkedCount = selectable.filter((i) => i.checked).length;
  const total = group.items.reduce((sum, i) => sum + i.size, 0);

  const box = h('input', {
    type: 'checkbox',
    checked: selectable.length > 0 && checkedCount === selectable.length,
    indeterminate: checkedCount > 0 && checkedCount < selectable.length,
    disabled: selectable.length === 0,
    'data-key': `group:${group.title}`,
    'aria-label': `Select all in ${group.title}`,
    onchange: (e) => {
      for (const item of selectable) item.checked = e.target.checked;
      selectionChanged();
    },
  });
  return h(
    'section',
    { class: 'group', 'aria-label': group.title },
    h(
      'div',
      { class: 'group-head' },
      box,
      h('h3', {}, group.title),
      h('span', { class: 'dim' }, `${plural(group.items.length, 'item')} · ${formatBytes(total)}`),
    ),
    group.items.map(itemView),
  );
}

function itemView(item) {
  const name = basename(item.path);
  const lead = item.refused
    ? h('span', { class: 'lead', title: 'Protected', 'aria-hidden': 'true' }, '🔒')
    : h('input', {
        type: 'checkbox',
        class: 'lead',
        checked: item.checked,
        'data-key': item.path,
        'aria-label': `${name}, ${item.path}, ${formatBytes(item.size)}`,
        onchange: (e) => {
          item.checked = e.target.checked;
          selectionChanged();
        },
      });
  return h(
    'div',
    { class: item.refused ? 'item refused' : 'item' },
    lead,
    h(
      'div',
      {},
      h('div', { class: 'name' }, name),
      h('div', { class: 'path' }, item.path),
      h('div', { class: 'reason' }, item.refused ?? item.reason),
    ),
    h('span', { class: 'size' }, formatBytes(item.size)),
    h(
      'button',
      {
        type: 'button',
        class: 'btn link',
        'aria-label': `Show ${name} in Finder`,
        onclick: () => revealInFinder(item.path),
      },
      'Show in Finder',
    ),
  );
}

async function revealInFinder(path) {
  try {
    await invoke('reveal_in_finder', { path });
  } catch (e) {
    showError(String(e));
  }
}

/** Manual changes leave the presets, so none stays highlighted. */
function selectionChanged() {
  state.preset = null;
  const top = $('.detail-scroll')?.scrollTop ?? 0;
  const key = document.activeElement?.getAttribute?.('data-key');
  renderDetail();
  $('.detail-scroll').scrollTop = top;
  if (key) $(`[data-key="${CSS.escape(key)}"]`)?.focus();
}

function actionBar() {
  const { count, size } = selectedTotals();
  const app = state.scan.app;
  const button = h(
    'button',
    {
      type: 'button',
      class: 'btn danger',
      id: 'uninstall',
      disabled: count === 0 || app.is_apple,
      onclick: () => handlers.onUninstall(),
    },
    'Uninstall…',
  );
  const summary = app.is_apple
    ? 'Apple apps are read-only.'
    : `${plural(count, 'item')} selected · ${formatBytes(size)}`;
  return h('div', { class: 'actionbar' }, h('span', { class: 'summary' }, summary), button);
}
