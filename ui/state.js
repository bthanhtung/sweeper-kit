// Shared UI state. Rust owns the data; this holds what the views display.

export const state = {
  apps: [],
  /** path -> bytes, filled in by `app-sizes` events. */
  sizes: new Map(),
  query: '',
  sort: 'name',
  /** Path of the highlighted app. */
  selected: null,
  /** The app shown in the detail pane (a one-off dropped app is not in `apps`). */
  scan: null,
  preset: 'Everything',
  scanToken: 0,
  settings: { extra_app_folders: [], protected: [], delete_immediately: false },
};

export const sizeOf = (app) => state.sizes.get(app.path) ?? app.size;
