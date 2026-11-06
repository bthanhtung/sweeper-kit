// Leftovers view: scan, confidence-ranked groups with tri-state selection, footer.
// Only item paths go back to Rust; it plans from its own stored scan.

import { $, h, clear, showError, announce } from './dom.js';
import { invoke, CANCELLED } from './api.js';
import { CATEGORY_LABEL, basename, formatBytes, plural } from './format.js';
import { startRemove } from './leftovers_sheet.js';
import { state } from './state.js';

const left = state.leftovers;

/** Badge text and style per confidence level. */
const CONFIDENCE = {
  high: { label: 'Removed app', style: 'conf-high' },
  medium: { label: 'Unclaimed bundle ID', style: 'conf-medium' },
  low: { label: 'Name match only', style: 'conf-low' },
};

const groups = () => left.scan?.groups ?? [];
const selectable = (group) => group.items.filter((i) => !i.refused);
const allItems = () => groups().flatMap((g) => g.items);

/** Selected, non-refused items and their total size. */
function totals() {
  const picked = allItems().filter((i) => left.selected.has(i.path) && !i.refused);
  return { count: picked.length, size: picked.reduce((sum, i) => sum + i.size, 0) };
}

function groupLabel(group) {
  return `${group.display_name}, ${CONFIDENCE[group.confidence].label}, ${formatBytes(group.size)}`;
}

function itemLabel(item) {
  return `${item.path}, ${formatBytes(item.size)}`;
}

export async function refreshLeftoversWarning() {
  try {
    left.warning = await invoke('orphan_rules_warning');
  } catch {
    left.warning = null;
  }
}

/** Ticks what the server pre-checked (high confidence only). */
export function applyScan(scan) {
  left.scan = scan;
  left.selected = new Set(
    scan.groups.flatMap((g) => g.items).filter((i) => i.checked && !i.refused).map((i) => i.path),
  );
}

/** Runs a scan; a newer scan makes this one's result irrelevant. */
export async function startScan() {
  if (left.scanning) return;
  const token = ++left.token;
  left.scanning = true;
  render();
  announce('Scanning for leftovers…');
  try {
    await refreshLeftoversWarning();
    const scan = await invoke('scan_leftovers');
    if (token !== left.token) return;
    applyScan(scan);
    announce(`Scan finished: ${plural(scan.groups.length, 'group')}, ${formatBytes(scan.total_size)}.`);
  } catch (e) {
    if (token !== left.token || String(e) === CANCELLED) return;
    left.scan = null;
    showError(String(e));
  } finally {
    if (token === left.token) {
      left.scanning = false;
      render();
      $('#leftovers-scan')?.focus();
    }
  }
}

/** After a removal the sheet hands over the fresh scan it made. */
function afterRemove(scan) {
  left.token += 1;
  left.scanning = false;
  if (scan) applyScan(scan);
  else {
    left.scan = null;
    left.selected = new Set();
  }
  render();
  $('#leftovers-scan')?.focus();
}

function runRemove() {
  if (!totals().count) return;
  startRemove([...left.selected], afterRemove);
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
  const top = $('#leftovers-scroll')?.scrollTop ?? 0;
  const key = document.activeElement?.getAttribute?.('data-key');
  render();
  $('#leftovers-scroll').scrollTop = top;
  if (key) $(`[data-key="${CSS.escape(key)}"]`)?.focus();
}

function setPicked(items, on) {
  for (const item of items) {
    if (on) left.selected.add(item.path);
    else left.selected.delete(item.path);
  }
}

function selectHighConfidence() {
  setPicked(groups().filter((g) => g.confidence === 'high').flatMap(selectable), true);
  change();
  announce(`${plural(totals().count, 'item')} selected.`);
}

function clearSelection() {
  left.selected = new Set();
  change();
  announce('Selection cleared.');
}

function itemView(item) {
  const refused = Boolean(item.refused);
  const lead = refused
    ? h('span', { class: 'lead', title: 'Protected', 'aria-hidden': 'true' }, '🔒')
    : h('input', {
        type: 'checkbox',
        class: 'lead',
        checked: left.selected.has(item.path),
        'data-key': `item:${item.path}`,
        'aria-label': itemLabel(item),
        onchange: (e) => {
          setPicked([item], e.target.checked);
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
      h('div', { class: 'name' }, basename(item.path)),
      h('div', { class: 'path' }, item.path),
      h('div', { class: 'reason' }, `${CATEGORY_LABEL[item.category] ?? item.category} · ${item.reason}`),
      refused && h('div', { class: 'reason refusal' }, item.refused),
    ),
    h('span', { class: 'size' }, formatBytes(item.size)),
    h(
      'button',
      {
        type: 'button',
        class: 'btn link',
        'aria-label': `Show ${basename(item.path)} in Finder`,
        onclick: () => reveal(item.path),
      },
      'Show in Finder',
    ),
  );
}

function groupView(group) {
  const open = left.expanded.has(group.key);
  const pickable = selectable(group);
  const checkedCount = pickable.filter((i) => left.selected.has(i.path)).length;
  const listId = `leftovers-list-${group.key.replace(/[^A-Za-z0-9_-]/g, '_')}`;
  const conf = CONFIDENCE[group.confidence];

  const box = h('input', {
    type: 'checkbox',
    checked: pickable.length > 0 && checkedCount === pickable.length,
    indeterminate: checkedCount > 0 && checkedCount < pickable.length,
    disabled: pickable.length === 0,
    'data-key': `group:${group.key}`,
    'aria-label': `Select ${groupLabel(group)}`,
    onchange: (e) => {
      setPicked(pickable, e.target.checked);
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
      'data-key': `toggle:${group.key}`,
      onclick: () => {
        if (open) left.expanded.delete(group.key);
        else left.expanded.add(group.key);
        change();
      },
    },
    h('span', { class: 'chevron', 'aria-hidden': 'true' }, open ? '▾' : '▸'),
    h('span', { class: 'cat-title' }, group.display_name),
    group.display_name !== group.key && h('span', { class: 'dim key' }, group.key),
  );
  return h(
    'section',
    { class: 'group', 'aria-label': group.display_name },
    h(
      'div',
      { class: 'group-head' },
      box,
      toggle,
      h('span', { class: `badge ${conf.style}` }, conf.label),
      h('span', { class: 'dim' }, `${plural(group.items.length, 'item')} · ${formatBytes(group.size)}`),
    ),
    h(
      'ul',
      { id: listId, class: 'clean-list', 'aria-label': `${group.display_name} items`, hidden: !open },
      open && group.items.map(itemView),
    ),
  );
}

function body() {
  if (left.scanning) {
    return h(
      'div',
      { class: 'detail-empty' },
      h('progress', { 'aria-label': 'Scanning' }),
      h('p', {}, 'Scanning…'),
    );
  }
  if (!left.scan) {
    return h(
      'p',
      { class: 'detail-empty' },
      'Scan to find data in your Library that no installed app claims. Nothing is removed until you confirm.',
    );
  }
  const { inaccessible } = left.scan;
  return [
    left.warning && h('div', { class: 'notice', role: 'status' }, left.warning),
    inaccessible.length > 0 &&
      h(
        'div',
        { class: 'notice', role: 'note' },
        h('strong', {}, 'Some locations could not be read'),
        h('ul', {}, inaccessible.map(([path, why]) => h('li', {}, `${path}: ${why}`))),
        h('div', { class: 'dim' }, 'Grant Full Disk Access in System Settings to include them.'),
      ),
    groups().length === 0 && h('p', { class: 'detail-empty' }, 'No leftovers found.'),
    groups().map(groupView),
  ];
}

export function render() {
  const view = $('#view-leftovers');
  const { count, size } = totals();
  const hasHigh = groups().some((g) => g.confidence === 'high' && selectable(g).length > 0);
  clear(view);
  view.append(
    h(
      'div',
      { class: 'cleaner' },
      h(
        'div',
        { class: 'cleaner-head' },
        h('h2', {}, 'Leftovers'),
        left.scan && h('span', { class: 'dim' }, `${formatBytes(left.scan.total_size)} found`),
        left.scan && h(
          'button',
          {
            type: 'button',
            class: 'btn',
            disabled: !hasHigh || left.scanning,
            onclick: selectHighConfidence,
          },
          'Select all high confidence',
        ),
        left.scan && h(
          'button',
          {
            type: 'button',
            class: 'btn',
            disabled: count === 0 || left.scanning,
            onclick: clearSelection,
          },
          'Clear selection',
        ),
        h(
          'button',
          {
            type: 'button',
            class: 'btn',
            id: 'leftovers-scan',
            disabled: left.scanning,
            'aria-keyshortcuts': 'Meta+L',
            onclick: startScan,
          },
          left.scan ? 'Scan again' : 'Scan',
        ),
      ),
      h(
        'p',
        { class: 'dim view-note' },
        'Data in your Library that no installed app claims. Only high-confidence items are pre-selected.',
      ),
      h('div', { class: 'detail-scroll', id: 'leftovers-scroll' }, body()),
      h(
        'div',
        { class: 'actionbar' },
        h(
          'span',
          { class: 'summary', id: 'leftovers-summary' },
          left.scan ? `${plural(count, 'item')} selected · ${formatBytes(size)}` : '',
        ),
        h(
          'button',
          {
            type: 'button',
            class: 'btn danger',
            id: 'leftovers-run',
            disabled: count === 0 || left.scanning,
            'aria-describedby': 'leftovers-summary',
            onclick: runRemove,
          },
          state.settings.delete_immediately ? 'Delete permanently…' : 'Move to Trash…',
        ),
      ),
    ),
  );
}
