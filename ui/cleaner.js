// Cleaner view: scan, collapsible categories with tri-state selection, footer.
// Only entry ids go back to Rust; it rebuilds the plan from its own scan.

import { $, h, clear, showError, announce } from './dom.js';
import { invoke, CANCELLED } from './api.js';
import { ACTION_COMMAND, formatBytes, plural } from './format.js';
import { startClean } from './clean_sheet.js';
import { state } from './state.js';

const clean = state.clean;

const entriesOf = (category) => category.entries;
const selectable = (category) => entriesOf(category).filter((e) => !e.refused);
const allEntries = () => (clean.scan ? clean.scan.categories.flatMap(entriesOf) : []);

/** Selected, non-refused entries and their total size. */
function totals() {
  const picked = allEntries().filter((e) => clean.selected.has(e.id) && !e.refused);
  return { count: picked.length, size: picked.reduce((sum, e) => sum + (e.size ?? 0), 0) };
}

const sizeText = (size) => (size == null ? 'Size unknown' : formatBytes(size));

/** What an entry is called to assistive technology: title, location and size. */
function entryLabel(entry) {
  const where = entry.path ?? ACTION_COMMAND[entry.action] ?? entry.action;
  return `${entry.title}, ${where}, ${sizeText(entry.size)}`;
}

export async function refreshCleanerWarning() {
  try {
    clean.warning = await invoke('clean_rules_warning');
  } catch {
    clean.warning = null;
  }
}

/** Runs a scan; a newer scan makes this one's result irrelevant. */
export async function startScan() {
  if (clean.scanning) return;
  const token = ++clean.token;
  clean.scanning = true;
  render();
  announce('Scanning for files that can be cleaned…');
  try {
    await refreshCleanerWarning();
    const scan = await invoke('scan_cleaner');
    if (token !== clean.token) return;
    clean.scan = scan;
    clean.selected = new Set(
      scan.categories.flatMap(entriesOf).filter((e) => e.checked && !e.refused).map((e) => e.id),
    );
    const found = allEntries().length;
    announce(`Scan finished: ${plural(found, 'item')}, ${formatBytes(scan.total_size)}.`);
  } catch (e) {
    if (token !== clean.token || String(e) === CANCELLED) return;
    clean.scan = null;
    showError(String(e));
  } finally {
    if (token === clean.token) {
      clean.scanning = false;
      render();
      $('#clean-scan')?.focus();
    }
  }
}

/** After a clean the stored scan is gone; ask for a new one. */
function afterClean() {
  clean.scan = null;
  clean.selected = new Set();
  clean.token += 1;
  clean.scanning = false;
  render();
  $('#clean-scan')?.focus();
}

function runClean() {
  const { count } = totals();
  if (!count) return;
  startClean([...clean.selected], afterClean);
}

async function reveal(path) {
  try {
    await invoke('reveal_in_finder', { path });
  } catch (e) {
    showError(String(e));
  }
}

/** Re-renders keeping scroll position and keyboard focus. */
function change() {
  const scroller = $('#clean-scroll');
  const top = scroller?.scrollTop ?? 0;
  const key = document.activeElement?.getAttribute?.('data-key');
  render();
  $('#clean-scroll').scrollTop = top;
  if (key) $(`[data-key="${CSS.escape(key)}"]`)?.focus();
}

function entryView(entry) {
  const refused = Boolean(entry.refused);
  const lead = refused
    ? h('span', { class: 'lead', title: 'Protected', 'aria-hidden': 'true' }, '🔒')
    : h('input', {
        type: 'checkbox',
        class: 'lead',
        checked: clean.selected.has(entry.id),
        'data-key': `entry:${entry.id}`,
        'aria-label': entryLabel(entry),
        onchange: (e) => {
          if (e.target.checked) clean.selected.add(entry.id);
          else clean.selected.delete(entry.id);
          change();
        },
      });
  return h(
    'li',
    { class: refused ? 'item refused' : 'item' },
    lead,
    h(
      'div',
      {},
      h('div', { class: 'name' }, entry.title),
      entry.path
        ? h('div', { class: 'path' }, entry.path)
        : h('code', { class: 'path command' }, ACTION_COMMAND[entry.action] ?? entry.action),
      entry.mode === 'delete_only' &&
        h('span', { class: 'badge danger' }, 'Deleted permanently'),
      entry.warning && h('span', { class: 'badge readonly' }, `⚠ ${entry.warning}`),
      refused && h('div', { class: 'reason' }, entry.refused),
    ),
    h('span', { class: 'size' }, sizeText(entry.size)),
    entry.path
      ? h(
          'button',
          {
            type: 'button',
            class: 'btn link',
            'aria-label': `Show ${entry.title} in Finder`,
            onclick: () => reveal(entry.path),
          },
          'Show in Finder',
        )
      : h('span'),
  );
}

function categoryView(category) {
  const open = clean.expanded.has(category.id);
  const pickable = selectable(category);
  const checkedCount = pickable.filter((e) => clean.selected.has(e.id)).length;
  const listId = `clean-list-${category.id}`;

  const box = h('input', {
    type: 'checkbox',
    checked: pickable.length > 0 && checkedCount === pickable.length,
    indeterminate: checkedCount > 0 && checkedCount < pickable.length,
    disabled: pickable.length === 0,
    'data-key': `category:${category.id}`,
    'aria-label': `Select all in ${category.title}`,
    onchange: (e) => {
      for (const entry of pickable) {
        if (e.target.checked) clean.selected.add(entry.id);
        else clean.selected.delete(entry.id);
      }
      change();
    },
  });
  const toggle = h(
    'button',
    {
      type: 'button',
      class: 'disclosure',
      'aria-expanded': String(open),
      'aria-controls': listId,
      'data-key': `toggle:${category.id}`,
      onclick: () => {
        if (open) clean.expanded.delete(category.id);
        else clean.expanded.add(category.id);
        change();
      },
    },
    h('span', { class: 'chevron', 'aria-hidden': 'true' }, open ? '▾' : '▸'),
    h('span', { class: 'cat-title' }, category.title),
  );
  return h(
    'section',
    { class: 'group', 'aria-label': category.title },
    h(
      'div',
      { class: 'group-head' },
      box,
      toggle,
      h('span', { class: 'dim' }, `${plural(category.entries.length, 'item')} · ${formatBytes(category.size)}`),
    ),
    h(
      'ul',
      { id: listId, class: 'clean-list', 'aria-label': `${category.title} items`, hidden: !open },
      open && category.entries.map(entryView),
    ),
  );
}

function body() {
  if (clean.scanning) {
    return h(
      'div',
      { class: 'detail-empty' },
      h('progress', { 'aria-label': 'Scanning' }),
      h('p', {}, 'Scanning…'),
    );
  }
  if (!clean.scan) {
    return h(
      'p',
      { class: 'detail-empty' },
      'Scan to find caches, logs and other files that can be regenerated. Nothing is removed until you confirm.',
    );
  }
  const { categories, inaccessible } = clean.scan;
  return [
    clean.warning && h('div', { class: 'notice', role: 'status' }, clean.warning),
    inaccessible.length > 0 &&
      h(
        'div',
        { class: 'notice', role: 'note' },
        h('strong', {}, 'Some locations could not be read'),
        h('ul', {}, inaccessible.map(([path, why]) => h('li', {}, `${path}: ${why}`))),
        h('div', { class: 'dim' }, 'Grant Full Disk Access in System Settings to include them.'),
      ),
    categories.length === 0 && h('p', { class: 'detail-empty' }, 'Nothing to clean.'),
    categories.map(categoryView),
  ];
}

export function render() {
  const view = $('#view-cleaner');
  const { count, size } = totals();
  clear(view);
  view.append(
    h(
      'div',
      { class: 'cleaner' },
      h(
        'div',
        { class: 'cleaner-head' },
        h('h2', {}, 'Cleaner'),
        clean.scan && h('span', { class: 'dim' }, `${formatBytes(clean.scan.total_size)} found`),
        h(
          'button',
          {
            type: 'button',
            class: 'btn',
            id: 'clean-scan',
            disabled: clean.scanning,
            'aria-keyshortcuts': 'Meta+R',
            onclick: startScan,
          },
          clean.scan ? 'Scan again' : 'Scan',
        ),
      ),
      h('div', { class: 'detail-scroll', id: 'clean-scroll' }, body()),
      h(
        'div',
        { class: 'actionbar' },
        h(
          'span',
          { class: 'summary', id: 'clean-summary' },
          clean.scan ? `${plural(count, 'item')} selected · ${formatBytes(size)}` : '',
        ),
        h(
          'button',
          {
            type: 'button',
            class: 'btn danger',
            id: 'clean-run',
            disabled: count === 0 || clean.scanning,
            'aria-describedby': 'clean-summary',
            onclick: runClean,
          },
          'Clean…',
        ),
      ),
    ),
  );
}
