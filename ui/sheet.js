// Uninstall flow: quit prompts, dry-run sheet, progress and report.

import { $, h, clear, announce, showError } from './dom.js';
import { invoke, listen, confirm } from './api.js';
import { formatBytes, plural } from './format.js';
import { state } from './state.js';

const sheet = $('#sheet');
/** While true the sheet cannot be dismissed (work in progress). */
let busy = false;

/** Marks work in progress: the sheet then ignores Esc. */
export function setBusy(value) {
  busy = value;
}

sheet.addEventListener('cancel', (e) => {
  if (busy) e.preventDefault();
});

export function show(...children) {
  clear(sheet);
  sheet.append(...children);
  if (!sheet.open) sheet.showModal();
}

export function close() {
  busy = false;
  if (sheet.open) sheet.close();
}

export const body = (...children) => h('div', { class: 'sheet-body' }, ...children);
export const foot = (...buttons) => h('div', { class: 'sheet-foot' }, ...buttons);
export const title = (text) => h('h2', { id: 'sheet-title' }, text);

export function button(label, { kind = '', onclick, autofocus = false } = {}) {
  return h('button', { type: 'button', class: `btn ${kind}`.trim(), onclick, autofocus }, label);
}

/** Asks to quit running instances; resolves to false if the user declines. */
async function ensureStopped(scan) {
  const running = await invoke('running_instances', { scan });
  if (!running.length) return true;
  const name = scan.app.name;
  const ok = await confirm(`${name} is running and has to be closed before it can be removed.`, {
    title: `Quit ${name}?`,
    okLabel: 'Quit',
  });
  if (!ok) return false;
  let outcome = await invoke('quit_apps', { scan, allowForce: false });
  if (!outcome.still_running.length) return true;
  const force = await confirm(
    `${name} did not quit within 5 seconds. Forcing it to quit may lose unsaved work.`,
    { title: 'Force quit?', okLabel: 'Force Quit' },
  );
  if (!force) return false;
  outcome = await invoke('quit_apps', { scan, allowForce: true });
  if (outcome.still_running.length) throw new Error(`${name} could not be stopped.`);
  return true;
}

/** Entry point of the Uninstall button. `onDone` runs after the report closes. */
export async function startUninstall(scan, onDone) {
  try {
    if (!(await ensureStopped(scan))) return;
    const plan = await invoke('plan_removal', { scan });
    if (!plan.items.length) return;
    confirmSheet(scan, plan, onDone);
  } catch (e) {
    showError(String(e));
  }
}

function confirmSheet(scan, plan, onDone) {
  const permanent = state.settings.delete_immediately;
  const action = permanent ? 'Delete permanently' : 'Move to Trash';
  show(
    body(
      title(`Uninstall ${scan.app.name}?`),
      h(
        'p',
        { class: 'dim' },
        `${plural(plan.items.length, 'item')} · ${formatBytes(plan.total_size)}. ` +
          (permanent
            ? 'These items will be deleted permanently and cannot be restored.'
            : 'These items will be moved to the Trash and can be restored.'),
      ),
      h(
        'ul',
        { class: 'sheet-list', 'aria-label': 'Items to remove' },
        plan.items.map((i) => h('li', {}, `${i.path} — ${formatBytes(i.size)}`)),
      ),
    ),
    foot(
      button('Cancel', { onclick: close, autofocus: true }),
      button(action, { kind: permanent ? 'danger' : 'primary', onclick: () => run(scan, plan, onDone) }),
    ),
  );
}

async function run(scan, plan, onDone) {
  busy = true;
  const bar = h('progress', { max: plan.items.length, value: 0, 'aria-label': 'Uninstall progress' });
  const label = h('p', { class: 'dim' }, `Removing 0 of ${plan.items.length}…`);
  show(body(title(`Uninstalling ${scan.app.name}`), bar, label), foot());
  const unlisten = await listen('uninstall-progress', ({ done, total, path }) => {
    bar.max = total;
    bar.value = done;
    label.textContent = `Removing ${done} of ${total}…`;
    announce(`${done} of ${total}: ${path}`);
  });
  try {
    const report = await invoke('uninstall', { scan });
    busy = false;
    reportSheet(scan, report, onDone);
  } catch (e) {
    close();
    showError(String(e));
  } finally {
    unlisten();
  }
}

export function stepRow(ok, text) {
  return h(
    'li',
    { class: 'step' },
    h('span', { class: ok ? 'ok' : 'bad', 'aria-label': ok ? 'Succeeded' : 'Warning' }, ok ? '✓' : '⚠'),
    h('span', {}, text),
  );
}

function reportSheet(scan, report, onDone) {
  const { removed, failed, warnings } = report.removal;
  const size = removed.reduce((sum, r) => sum + r.size, 0);
  const trashed = removed.some((r) => r.trash_location);
  const done = () => {
    close();
    onDone();
  };
  announce(`Uninstall finished. ${plural(removed.length, 'item')} removed.`);
  show(
    body(
      title(`${scan.app.name} uninstalled`),
      h('p', {}, `${plural(removed.length, 'item')} removed · ${formatBytes(size)}`),
      h(
        'ul',
        { class: 'sheet-list', 'aria-label': 'Steps' },
        report.steps.map((s) => stepRow(s.ok, s.detail ? `${s.name} — ${s.detail}` : s.name)),
        failed.map(([path, why]) => stepRow(false, `${path} — ${why}`)),
        warnings.map((w) => stepRow(false, w)),
      ),
      report.remaining.length === 0
        ? h('p', {}, '0 items left.')
        : [
            h('p', {}, `${plural(report.remaining.length, 'item')} left:`),
            h(
              'ul',
              { class: 'sheet-list', 'aria-label': 'Items left' },
              report.remaining.map(([path, why]) =>
                h('li', {}, path, h('div', { class: 'why' }, why))),
            ),
          ],
      trashed &&
        h(
          'p',
          { class: 'dim' },
          'Items are in the Trash. ',
          h(
            'button',
            {
              type: 'button',
              class: 'btn link',
              onclick: () => {
                done();
                document.dispatchEvent(new CustomEvent('show-view', { detail: 'history' }));
              },
            },
            'View in History',
          ),
        ),
    ),
    foot(button('Done', { kind: 'primary', onclick: done, autofocus: true })),
  );
}

/** Runs `brew uninstall --cask --zap` after confirmation. */
export async function startBrewZap(app, onDone) {
  try {
    const command = await invoke('brew_zap_command', { app });
    if (!command) return showError('This app was not installed by a Homebrew cask.');
    const ok = await confirm(`Run “${command.join(' ')}”? Homebrew will remove the app and its files.`, {
      title: `Remove ${app.name} with Homebrew?`,
      okLabel: 'Run',
    });
    if (!ok) return;
    busy = true;
    show(
      body(title(`Removing ${app.name}`), h('progress', { 'aria-label': 'Running Homebrew' }), h('p', { class: 'dim' }, command.join(' '))),
      foot(),
    );
    const step = await invoke('brew_zap', { app });
    busy = false;
    announce(step.ok ? 'Homebrew finished.' : 'Homebrew reported a problem.');
    show(
      body(
        title(step.ok ? `${app.name} removed` : 'Homebrew could not remove the app'),
        h('ul', { class: 'sheet-list' }, stepRow(step.ok, step.detail || step.name)),
      ),
      foot(button('Done', { kind: 'primary', autofocus: true, onclick: () => { close(); onDone(); } })),
    );
  } catch (e) {
    close();
    showError(String(e));
  }
}
