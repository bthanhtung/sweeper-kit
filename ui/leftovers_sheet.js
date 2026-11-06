// Leftovers removal flow: dry-run sheet, progress and report.

import { h, announce, showError } from './dom.js';
import { invoke, listen } from './api.js';
import { formatBytes, plural } from './format.js';
import {
  body, button, close, foot, setBusy, show, stepRow, title,
} from './sheet.js';
import { state } from './state.js';

/**
 * Entry point of the Move to Trash… button. `selected` holds paths only; the
 * plan shown here comes from Rust. `onDone` receives the scan made after the
 * removal (or null) once the report closes.
 */
export async function startRemove(selected, onDone) {
  try {
    const plan = await invoke('plan_leftovers', { selected });
    if (!plan.items.length) return showError('Nothing selected can be removed.');
    confirmSheet(selected, plan, onDone);
  } catch (e) {
    showError(String(e));
  }
}

function confirmSheet(selected, plan, onDone) {
  const permanent = state.settings.delete_immediately;
  show(
    body(
      title(permanent ? 'Delete leftovers permanently?' : 'Move leftovers to the Trash?'),
      h(
        'p',
        { class: permanent ? 'warn' : 'dim' },
        `${plural(plan.items.length, 'item')} · ${formatBytes(plan.total_size)}. ` +
          (permanent
            ? 'These items will be deleted permanently and cannot be restored.'
            : 'These items will be moved to the Trash and can be restored from History.'),
      ),
      h(
        'ul',
        { class: 'sheet-list', 'aria-label': 'Items to remove' },
        plan.items.map((i) => h('li', {}, `${i.path} — ${formatBytes(i.size)}`)),
      ),
      h('p', {}, h('strong', {}, `Total: ${formatBytes(plan.total_size)}`)),
    ),
    foot(
      button('Cancel', { onclick: close, autofocus: true }),
      button(permanent ? 'Delete permanently' : 'Move to Trash', {
        kind: permanent ? 'danger' : 'primary',
        onclick: () => run(selected, plan, onDone),
      }),
    ),
  );
}

async function run(selected, plan, onDone) {
  const total = plan.items.length;
  setBusy(true);
  const bar = h('progress', { max: total, value: 0, 'aria-label': 'Removal progress' });
  const label = h('p', { class: 'dim' }, `Removing 0 of ${total}…`);
  show(body(title('Removing leftovers'), bar, label), foot());
  const unlisten = await listen('leftovers-progress', ({ done, total: all, path }) => {
    bar.max = all;
    bar.value = done;
    label.textContent = `Removing ${done} of ${all}…`;
    announce(`${done} of ${all}: ${path}`);
  });
  try {
    const report = await invoke('remove_leftovers', { selected });
    label.textContent = 'Checking what is left…';
    const { scan, remaining } = await rescan(plan);
    setBusy(false);
    reportSheet(report, remaining, scan, onDone);
  } catch (e) {
    close();
    showError(String(e));
  } finally {
    unlisten();
  }
}

/** Scans again and counts how many of the planned paths still exist. */
async function rescan(plan) {
  try {
    const scan = await invoke('scan_leftovers');
    const planned = new Set(plan.items.map((i) => i.path));
    const remaining = scan.groups.flatMap((g) => g.items).filter((i) => planned.has(i.path));
    return { scan, remaining };
  } catch {
    return { scan: null, remaining: null };
  }
}

function reportSheet(report, remaining, scan, onDone) {
  const { removed, failed, warnings } = report;
  const size = removed.reduce((sum, r) => sum + r.size, 0);
  const trashed = removed.some((r) => r.trash_location);
  const done = () => {
    close();
    onDone(scan);
  };
  announce(`Removal finished. ${plural(removed.length, 'item')} removed.`);
  show(
    body(
      title('Leftovers removed'),
      h('p', {}, `${plural(removed.length, 'item')} removed · ${formatBytes(size)}`),
      (failed.length > 0 || warnings.length > 0) &&
        h(
          'ul',
          { class: 'sheet-list', 'aria-label': 'Problems' },
          failed.map(([path, why]) => stepRow(false, `${path} — ${why}`)),
          warnings.map((w) => stepRow(false, w)),
        ),
      remaining === null
        ? h('p', { class: 'dim' }, 'Scan again to check what is left.')
        : remaining.length === 0
          ? h('p', {}, '0 items left.')
          : [
              h('p', {}, `${plural(remaining.length, 'item')} left:`),
              h(
                'ul',
                { class: 'sheet-list', 'aria-label': 'Items left' },
                remaining.map((i) => h('li', {}, i.path)),
              ),
            ],
      trashed &&
        h('p', { class: 'dim' }, 'Items are in the Trash. You can restore them from History.'),
    ),
    foot(button('Done', { kind: 'primary', onclick: done, autofocus: true })),
  );
}
