// Settings view: extra app folders, protected list, deletion mode.

import { $, h, clear, showError, announce } from './dom.js';
import { invoke, pickFolder } from './api.js';
import { formatDate } from './format.js';
import { state } from './state.js';

let onFoldersChanged = () => {};

export async function initSettings(foldersChanged) {
  onFoldersChanged = foldersChanged;
  state.settings = await invoke('get_settings');
  await renderSettings();
}

/** Persists `next`; Rust validates and returns the stored form. */
async function save(next) {
  try {
    const foldersBefore = state.settings.extra_app_folders.join('\n');
    state.settings = await invoke('save_settings', { settings: next });
    if (foldersBefore !== state.settings.extra_app_folders.join('\n')) onFoldersChanged();
  } catch (e) {
    showError(String(e));
  }
  await renderSettings();
}

async function forgetRemovedApp(app) {
  try {
    await invoke('forget_removed_app', { bundleId: app.bundle_id });
    announce(`${app.name} forgotten.`);
  } catch (e) {
    showError(String(e));
  }
  await renderSettings();
}

function list(label, values, remove) {
  return h(
    'ul',
    { 'aria-label': label },
    values.length === 0
      ? h('li', {}, h('span', { class: 'dim' }, 'None'))
      : values.map((value) =>
          h(
            'li',
            {},
            h('span', {}, value),
            h('button', { type: 'button', class: 'btn', 'aria-label': `Remove ${value}`, onclick: () => remove(value) }, 'Remove'),
          )),
  );
}

const MAX_SIZE_MB = 1_000_000;
const AGE_RANGE = [1, 3650];

/** A labelled whole-number field that saves on change and explains bad input. */
function numberField({ id, label, hint, value, min, max, store }) {
  const message = h('p', { id: `${id}-message`, class: 'warn', role: 'alert' });
  const input = h('input', {
    type: 'number',
    id,
    value: String(value),
    min,
    max,
    step: 1,
    inputmode: 'numeric',
    'aria-describedby': `${id}-hint ${id}-message`,
    onchange: () => {
      const text = input.value.trim();
      const number = Number(text);
      if (text === '' || !Number.isInteger(number) || number < min || number > max) {
        message.textContent = `Enter a whole number from ${min} to ${max}.`;
        input.setAttribute('aria-invalid', 'true');
        return;
      }
      message.textContent = '';
      input.removeAttribute('aria-invalid');
      store(number);
    },
  });
  return h(
    'div',
    { class: 'field' },
    h('label', { for: id }, label),
    input,
    h('p', { id: `${id}-hint` }, hint),
    message,
  );
}

export async function renderSettings() {
  const view = $('#view-settings');
  const s = state.settings;
  const warning = await invoke('rules_warning');
  const removedApps = await invoke('removed_apps').catch((e) => {
    showError(String(e));
    return [];
  });
  const entry = h('input', {
    type: 'text',
    id: 'protect-input',
    placeholder: '/path/to/keep or com.example.app',
    'aria-label': 'Path or bundle identifier to protect',
    autocomplete: 'off',
    spellcheck: 'false',
  });
  const addProtected = () => {
    const value = entry.value.trim();
    if (value) save({ ...s, protected: [...s.protected, value] });
  };
  entry.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') addProtected();
  });

  clear(view);
  view.append(
    h(
      'div',
      { class: 'settings' },
      h('h2', {}, 'Settings'),
      warning && h('div', { class: 'notice', role: 'status' }, warning),

      h(
        'section',
        {},
        h('h3', {}, 'Extra app folders'),
        h('p', {}, 'Apps in these folders are listed in addition to /Applications and ~/Applications.'),
        list('Extra app folders', s.extra_app_folders, (folder) =>
          save({ ...s, extra_app_folders: s.extra_app_folders.filter((f) => f !== folder) })),
        h(
          'button',
          {
            type: 'button',
            class: 'btn',
            onclick: async () => {
              const folder = await pickFolder();
              if (folder) save({ ...s, extra_app_folders: [...s.extra_app_folders, folder] });
            },
          },
          'Add folder…',
        ),
      ),

      h(
        'section',
        {},
        h('h3', {}, 'Protected items'),
        h('p', {}, 'Paths and bundle identifiers listed here are never removed.'),
        list('Protected items', s.protected, (value) =>
          save({ ...s, protected: s.protected.filter((p) => p !== value) })),
        h('div', { class: 'row' }, entry, h('button', { type: 'button', class: 'btn', onclick: addProtected }, 'Add')),
      ),

      h(
        'section',
        {},
        h('h3', {}, 'Cleaner'),
        numberField({
          id: 'min-item-size',
          label: 'Minimum item size (MB)',
          hint: 'The cleaner hides items smaller than this. 0 shows everything.',
          value: s.min_item_size_mb,
          min: 0,
          max: MAX_SIZE_MB,
          store: (value) => save({ ...s, min_item_size_mb: value }),
        }),
        numberField({
          id: 'downloads-age',
          label: 'Old downloads: older than N days',
          hint: 'Files in Downloads not modified for this many days are offered by the cleaner, never pre-selected.',
          value: s.downloads_age_days,
          min: AGE_RANGE[0],
          max: AGE_RANGE[1],
          store: (value) => save({ ...s, downloads_age_days: value }),
        }),
      ),

      h(
        'section',
        {},
        h('h3', {}, 'Removed apps history'),
        h(
          'p',
          {},
          'Apps you uninstalled with Sweeper Kit. Their leftovers are listed as high confidence. Forgetting an app lowers the confidence of its future leftovers.',
        ),
        h(
          'ul',
          { 'aria-label': 'Removed apps' },
          removedApps.length === 0
            ? h('li', {}, h('span', { class: 'dim' }, 'None'))
            : [...removedApps].reverse().map((app) =>
                h(
                  'li',
                  {},
                  h('span', {}, `${app.name} (${app.bundle_id}) — ${formatDate(app.removed_at)}`),
                  h(
                    'button',
                    {
                      type: 'button',
                      class: 'btn',
                      'aria-label': `Forget ${app.name}`,
                      onclick: () => forgetRemovedApp(app),
                    },
                    'Forget',
                  ),
                )),
        ),
      ),

      h(
        'section',
        {},
        h('h3', {}, 'Removal'),
        h(
          'div',
          { class: 'switch-row' },
          h('input', {
            type: 'checkbox',
            id: 'delete-immediately',
            checked: s.delete_immediately,
            'aria-describedby': 'delete-warning',
            onchange: (e) => save({ ...s, delete_immediately: e.target.checked }),
          }),
          h(
            'div',
            {},
            h('label', { for: 'delete-immediately' }, 'Delete immediately instead of moving to Trash'),
            h(
              'p',
              { id: 'delete-warning', class: s.delete_immediately ? 'warn' : '' },
              'Deleted items cannot be restored. Leave this off to keep every removal undoable from the Trash.',
            ),
          ),
        ),
      ),
    ),
  );
}
