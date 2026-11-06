// Settings view: extra app folders, protected list, deletion mode.

import { $, h, clear, showError } from './dom.js';
import { invoke, pickFolder } from './api.js';
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

export async function renderSettings() {
  const view = $('#view-settings');
  const s = state.settings;
  const warning = await invoke('rules_warning');
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
