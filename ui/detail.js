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

const ADMIN_BADGE = 'Needs administrator password';
const ADMIN_UNAVAILABLE =
  'Administrator tools are not available in this build, so this item cannot be removed.';

/**
 * Groups shown in the pane: scan groups, a "System" group for leftovers that
 * need administrator access (a bundle that does stays in its own group), then
 * other copies.
 */
function groupsOf(scan) {
  const groups = [];
  const system = [];
  for (const g of scan.groups) {
    const isSystem = (i) => i.requires_admin && g.category !== 'app_bundle';
    system.push(...g.items.filter(isSystem));
    const rest = g.items.filter((i) => !isSystem(i));
    if (rest.length || !g.items.length) groups.push({ title: CATEGORY_LABEL[g.category], items: rest });
  }
  if (system.length) groups.push({ title: 'System', items: system });
  if (scan.other_copies.length) groups.push({ title: 'Other copies', items: scan.other_copies });
  return groups;
}

/** Without the helper, administrator items cannot be removed, so none stays checked. */
function enforceAdminAvailability(scan) {
  if (state.adminAvailable) return;
  for (const item of allItems(scan)) {
    if (item.requires_admin) item.checked = false;
  }
}

/** Re-renders keeping the scroll position and focus (receipts arrived or were ticked). */
export function refreshDetail() {
  const top = $('.detail-scroll')?.scrollTop ?? 0;
  const key = document.activeElement?.getAttribute?.('data-key');
  renderDetail();
  const scroll = $('.detail-scroll');
  if (scroll) scroll.scrollTop = top;
  if (key) $(`[data-key="${CSS.escape(key)}"]`)?.focus();
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
  enforceAdminAvailability(scan);
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
  if (!app.is_apple && state.receipts.length) scroll.append(receiptsView());

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

/** Installer receipts; forgetting one is opt-in and needs the administrator password. */
function receiptsView() {
  const unavailable = !state.adminAvailable;
  return h(
    'section',
    { class: 'group', 'aria-label': 'Installer receipts' },
    h(
      'div',
      { class: 'group-head' },
      h('h3', {}, 'Installer receipts'),
      h('span', { class: 'dim' }, plural(state.receipts.length, 'receipt')),
    ),
    h(
      'p',
      { class: 'dim receipt-note' },
      'Removes the installer record. Files listed by the receipt are not deleted by this step.',
    ),
    state.receipts.map((r) => {
      const details = [
        r.version && `Version ${r.version}`,
        r.install_location && `Installs to ${r.install_location}`,
        r.files_count != null && plural(r.files_count, 'file'),
      ].filter(Boolean).join(' · ');
      return h(
        'div',
        { class: 'item' },
        h('input', {
          type: 'checkbox',
          class: 'lead',
          checked: state.forgetReceipts.has(r.pkg_id),
          disabled: unavailable,
          title: unavailable ? ADMIN_UNAVAILABLE : null,
          id: `receipt-${r.pkg_id}`,
          'data-key': `receipt:${r.pkg_id}`,
          'aria-label': `Forget receipt ${r.pkg_id}`,
          onchange: (e) => {
            if (e.target.checked) state.forgetReceipts.add(r.pkg_id);
            else state.forgetReceipts.delete(r.pkg_id);
            refreshDetail();
          },
        }),
        h(
          'div',
          {},
          h('div', { class: 'name' }, r.pkg_id),
          details && h('div', { class: 'reason' }, details),
          h('span', { class: 'badge conf-medium' }, ADMIN_BADGE),
        ),
        h('label', { class: 'dim receipt-label', for: `receipt-${r.pkg_id}` }, 'Forget receipt'),
      );
    }),
  );
}

function groupView(group) {
  const selectable = group.items.filter((i) => !i.refused && !(i.requires_admin && !state.adminAvailable));
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
        disabled: item.requires_admin && !state.adminAvailable,
        title: item.requires_admin && !state.adminAvailable ? ADMIN_UNAVAILABLE : null,
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
      item.requires_admin && !item.refused && h(
        'span',
        {
          class: 'badge conf-medium',
          title: state.adminAvailable ? null : ADMIN_UNAVAILABLE,
        },
        h('span', { 'aria-hidden': 'true' }, '🔒 '),
        ADMIN_BADGE,
      ),
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
