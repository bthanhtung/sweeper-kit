// Clean flow: dry-run sheet, extra confirmations, progress and report.

import { h, announce, showError } from './dom.js';
import { invoke, listen, confirm } from './api.js';
import { ACTION_COMMAND, formatBytes, plural } from './format.js';
import {
  body, button, close, foot, setBusy, show, stepRow, title,
} from './sheet.js';

const commandOf = (action) => ACTION_COMMAND[action] ?? action;

const itemList = (label, items) =>
  h(
    'ul',
    { class: 'sheet-list', 'aria-label': label },
    items.map((i) => h('li', {}, `${i.path} — ${formatBytes(i.size)}`)),
  );

/**
 * Entry point of the Clean… button. `selected` holds entry ids only; the plan
 * shown here comes from Rust. `onDone` runs after the report closes.
 */
export async function startClean(selected, onDone) {
  try {
    const plan = await invoke('plan_clean', { selected });
    const count = plan.trash_items.length + plan.delete_items.length + plan.actions.length;
    if (!count) return showError('Nothing selected can be cleaned.');
    confirmSheet(selected, plan, onDone);
  } catch (e) {
    showError(String(e));
  }
}

function confirmSheet(selected, plan, onDone) {
  const permanent = plan.delete_items.length > 0;
  show(
    body(
      title('Clean selected items?'),
      h(
        'p',
        { class: 'dim' },
        `${plural(plan.trash_items.length + plan.delete_items.length, 'item')}` +
          `${plan.actions.length ? ` and ${plural(plan.actions.length, 'command')}` : ''}` +
          ` · ${formatBytes(plan.total_size)}`,
      ),
      plan.trash_items.length > 0 && [
        h('h3', { class: 'sheet-section' }, `Move to Trash (${plan.trash_items.length})`),
        itemList('Items to move to the Trash', plan.trash_items),
      ],
      permanent && [
        h(
          'h3',
          { class: 'sheet-section warn' },
          `Delete permanently (${plan.delete_items.length})`,
        ),
        h('p', { class: 'warn' }, 'These items cannot be restored.'),
        itemList('Items to delete permanently', plan.delete_items),
      ],
      plan.actions.length > 0 && [
        h('h3', { class: 'sheet-section' }, `Run commands (${plan.actions.length})`),
        h(
          'ul',
          { class: 'sheet-list', 'aria-label': 'Commands to run' },
          plan.actions.map((a) => h('li', {}, commandOf(a))),
        ),
      ],
      h('p', {}, h('strong', {}, `Total: ${formatBytes(plan.total_size)}`)),
    ),
    foot(
      button('Cancel', { onclick: close, autofocus: true }),
      button('Clean', { kind: permanent ? 'danger' : 'primary', onclick: () => confirmed(selected, plan, onDone) }),
    ),
  );
}

/** Second confirmations for the irreversible parts, then the run. */
async function confirmed(selected, plan, onDone) {
  try {
    if (plan.delete_items.length > 0) {
      const ok = await confirm(
        `${plural(plan.delete_items.length, 'item')} in the Trash will be deleted and cannot be restored.`,
        { title: 'Empty Trash permanently?', okLabel: 'Empty Trash' },
      );
      if (!ok) return;
    }
    if (plan.actions.includes('docker_prune')) {
      const ok = await confirm(
        'This removes stopped containers, unused networks, dangling images and build cache.',
        { title: 'Run docker system prune -f?', okLabel: 'Run' },
      );
      if (!ok) return;
    }
  } catch (e) {
    return showError(String(e));
  }
  run(selected, plan, onDone);
}

async function run(selected, plan, onDone) {
  const total = plan.trash_items.length + plan.delete_items.length + plan.actions.length;
  setBusy(true);
  const bar = h('progress', { max: total, value: 0, 'aria-label': 'Cleaning progress' });
  const label = h('p', { class: 'dim' }, `Cleaning 0 of ${total}…`);
  show(body(title('Cleaning'), bar, label), foot());
  const unlisten = await listen('clean-progress', ({ done, total: all, path }) => {
    bar.max = all;
    bar.value = done;
    label.textContent = `Cleaning ${done} of ${all}…`;
    announce(`${done} of ${all}: ${path}`);
  });
  try {
    const report = await invoke('clean', { selected });
    setBusy(false);
    reportSheet(report, onDone);
  } catch (e) {
    close();
    showError(String(e));
  } finally {
    unlisten();
  }
}

function reportSheet(report, onDone) {
  const { removed, actions, failed, warnings, freed } = report;
  const trashed = removed.some((r) => r.trash_location);
  const done = () => {
    close();
    onDone();
  };
  announce(`Cleaning finished. ${formatBytes(freed)} freed, ${plural(removed.length, 'item')} removed.`);
  show(
    body(
      title('Cleaning finished'),
      h('p', {}, `${formatBytes(freed)} freed · ${plural(removed.length, 'item')} removed`),
      (actions.length > 0 || failed.length > 0 || warnings.length > 0) &&
        h(
          'ul',
          { class: 'sheet-list', 'aria-label': 'Details' },
          actions.map((a) => stepRow(a.ok, a.detail ? `${a.name} — ${a.detail}` : a.name)),
          failed.map(([path, why]) => stepRow(false, `${path} — ${why}`)),
          warnings.map((w) => stepRow(false, w)),
        ),
      trashed &&
        h('p', { class: 'dim' }, 'Items moved to the Trash can be restored from the Trash.'),
    ),
    foot(button('Done', { kind: 'primary', onclick: done, autofocus: true })),
  );
}
