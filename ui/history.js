// History view: Trash batches with restore. Only entry ids go back to Rust.

import { $, h, clear, showError, announce } from './dom.js';
import { invoke } from './api.js';
import { basename, formatBytes, formatDateTime, plural } from './format.js';
import {
  body, button, close, foot, setBusy, show, stepRow, title,
} from './sheet.js';
import { state } from './state.js';

const hist = state.history;

const restorable = (batch) => batch.entries.filter((s) => s.in_trash);
const allStatuses = () => hist.batches.flatMap((b) => b.entries);
const statusLabel = (s) => (s.in_trash ? 'In Trash' : 'No longer in Trash');
const entryLabel = (s) => `${s.entry.original}, ${formatBytes(s.entry.size)}, ${statusLabel(s)}`;

/** Ticked entries that can still be restored. */
const picked = () => allStatuses().filter((s) => s.in_trash && hist.selected.has(s.entry.id));

/** Loads the batches again; drops ticks for entries that are gone. */
export async function loadHistory() {
  const token = ++hist.token;
  try {
    const batches = await invoke('history');
    if (token !== hist.token) return;
    hist.batches = batches;
    hist.loaded = true;
    const valid = new Set(allStatuses().filter((s) => s.in_trash).map((s) => s.entry.id));
    hist.selected = new Set([...hist.selected].filter((id) => valid.has(id)));
  } catch (e) {
    if (token !== hist.token) return;
    hist.batches = [];
    hist.loaded = true;
    showError(String(e));
  }
  render();
}

/** Re-renders keeping scroll position and keyboard focus. */
function change() {
  const top = $('#history-scroll')?.scrollTop ?? 0;
  const key = document.activeElement?.getAttribute?.('data-key');
  render();
  $('#history-scroll').scrollTop = top;
  if (key) $(`[data-key="${CSS.escape(key)}"]`)?.focus();
}

function setPicked(statuses, on) {
  for (const s of statuses) {
    if (on) hist.selected.add(s.entry.id);
    else hist.selected.delete(s.entry.id);
  }
}

async function forget(status) {
  try {
    await invoke('forget_history', { ids: [status.entry.id] });
    announce(`${basename(status.entry.original)} removed from history.`);
  } catch (e) {
    showError(String(e));
  }
  await loadHistory();
}

function entryView(status) {
  const { entry } = status;
  const lead = status.in_trash
    ? h('input', {
        type: 'checkbox',
        class: 'lead',
        checked: hist.selected.has(entry.id),
        'data-key': `entry:${entry.id}`,
        'aria-label': entryLabel(status),
        onchange: (e) => {
          setPicked([status], e.target.checked);
          change();
        },
      })
    : h('span', { class: 'lead' });
  return h(
    'li',
    { class: status.in_trash ? 'item' : 'item gone' },
    lead,
    h(
      'div',
      {},
      h('div', { class: 'name' }, basename(entry.original)),
      h('div', { class: 'path' }, entry.original),
      h(
        'span',
        { class: status.in_trash ? 'badge conf-high' : 'badge conf-low' },
        statusLabel(status),
      ),
    ),
    h('span', { class: 'size' }, formatBytes(entry.size)),
    status.in_trash
      ? h('span')
      : h(
          'button',
          {
            type: 'button',
            class: 'btn link',
            'aria-label': `Remove ${basename(entry.original)} from history`,
            onclick: () => forget(status),
          },
          'Remove from history',
        ),
  );
}

function batchView(batch) {
  const open = hist.expanded.has(batch.batch_id);
  const pickable = restorable(batch);
  const checkedCount = pickable.filter((s) => hist.selected.has(s.entry.id)).length;
  const listId = `history-list-${batch.batch_id}`;
  const when = formatDateTime(batch.timestamp);
  const summary = `${plural(batch.entries.length, 'item')} · ${formatBytes(batch.size)}`;

  const box = h('input', {
    type: 'checkbox',
    checked: pickable.length > 0 && checkedCount === pickable.length,
    indeterminate: checkedCount > 0 && checkedCount < pickable.length,
    disabled: pickable.length === 0,
    'data-key': `batch:${batch.batch_id}`,
    'aria-label': `Select all restorable items from ${when}`,
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
      'data-key': `toggle:${batch.batch_id}`,
      onclick: () => {
        if (open) hist.expanded.delete(batch.batch_id);
        else hist.expanded.add(batch.batch_id);
        change();
      },
    },
    h('span', { class: 'chevron', 'aria-hidden': 'true' }, open ? '▾' : '▸'),
    h('span', { class: 'cat-title' }, when),
  );
  return h(
    'section',
    { class: 'group', 'aria-label': `Removed ${when}` },
    h('div', { class: 'group-head' }, box, toggle, h('span', { class: 'dim' }, summary)),
    h(
      'ul',
      { id: listId, class: 'clean-list', 'aria-label': `Items removed ${when}`, hidden: !open },
      open && batch.entries.map(entryView),
    ),
  );
}

function body_() {
  if (!hist.loaded) {
    return h('div', { class: 'detail-empty' }, h('progress', { 'aria-label': 'Loading' }));
  }
  if (hist.batches.length === 0) {
    return h(
      'p',
      { class: 'detail-empty' },
      'Nothing has been moved to the Trash yet. Items you remove with Sweeper Kit appear here so you can put them back.',
    );
  }
  return hist.batches.map(batchView);
}

function confirmRestore() {
  const items = picked();
  if (!items.length) return;
  show(
    body(
      title('Restore selected items?'),
      h(
        'p',
        { class: 'dim' },
        `${plural(items.length, 'item')} will move out of the Trash back to where they were. Existing items are never overwritten.`,
      ),
      h(
        'ul',
        { class: 'sheet-list', 'aria-label': 'Items to restore' },
        items.map((s) => h('li', {}, `${basename(s.entry.trash_location)} → ${s.entry.original}`)),
      ),
    ),
    foot(
      button('Cancel', { onclick: close, autofocus: true }),
      button('Restore', { kind: 'primary', onclick: () => runRestore(items) }),
    ),
  );
}

async function runRestore(items) {
  setBusy(true);
  show(
    body(title('Restoring'), h('progress', { 'aria-label': 'Restoring' })),
    foot(),
  );
  try {
    const outcomes = await invoke('restore', { ids: items.map((s) => s.entry.id) });
    setBusy(false);
    resultSheet(items, outcomes);
  } catch (e) {
    close();
    showError(String(e));
    await loadHistory();
  }
}

function resultSheet(items, outcomes) {
  const byId = new Map(items.map((s) => [s.entry.id, s]));
  const okCount = outcomes.filter((o) => o.ok).length;
  const done = async () => {
    close();
    await loadHistory();
    $('#history-restore')?.focus();
  };
  announce(`Restore finished. ${okCount} of ${outcomes.length} restored.`);
  show(
    body(
      title('Restore finished'),
      h('p', {}, `${okCount} of ${plural(outcomes.length, 'item')} restored.`),
      h(
        'ul',
        { class: 'sheet-list', 'aria-label': 'Results' },
        outcomes.map((o) => {
          const original = byId.get(o.id)?.entry.original ?? o.id;
          return stepRow(o.ok, o.ok ? `Restored ${o.path_or_error}` : `${original} — ${o.path_or_error}`);
        }),
      ),
    ),
    foot(button('Done', { kind: 'primary', onclick: done, autofocus: true })),
  );
}

export function render() {
  const view = $('#view-history');
  const count = picked().length;
  const size = picked().reduce((sum, s) => sum + s.entry.size, 0);
  clear(view);
  view.append(
    h(
      'div',
      { class: 'cleaner' },
      h(
        'div',
        { class: 'cleaner-head' },
        h('h2', {}, 'History'),
        hist.batches.length > 0 &&
          h('span', { class: 'dim' }, plural(hist.batches.length, 'removal')),
      ),
      h(
        'p',
        { class: 'dim view-note' },
        'Items Sweeper Kit moved to the Trash, newest first. Restore puts them back where they were.',
      ),
      h('div', { class: 'detail-scroll', id: 'history-scroll' }, body_()),
      h(
        'div',
        { class: 'actionbar' },
        h(
          'span',
          { class: 'summary', id: 'history-summary' },
          count ? `${plural(count, 'item')} selected · ${formatBytes(size)}` : '',
        ),
        h(
          'button',
          {
            type: 'button',
            class: 'btn primary',
            id: 'history-restore',
            disabled: count === 0,
            'aria-describedby': 'history-summary',
            onclick: confirmRestore,
          },
          'Restore…',
        ),
      ),
    ),
  );
}
