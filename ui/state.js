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
  settings: {
    extra_app_folders: [],
    protected: [],
    delete_immediately: false,
    min_item_size_mb: 0,
    downloads_age_days: 30,
  },
  /** Cleaner view: the last scan and what is ticked/expanded. */
  clean: {
    scan: null,
    /** Entry ids to clean; the server rebuilds the plan from these alone. */
    selected: new Set(),
    expanded: new Set(),
    scanning: false,
    token: 0,
    warning: null,
  },
  /** Leftovers view: the last scan and what is ticked/expanded. */
  leftovers: {
    scan: null,
    /** Item paths to remove; the server only accepts paths from its own scan. */
    selected: new Set(),
    /** Group keys whose item list is open. */
    expanded: new Set(),
    scanning: false,
    token: 0,
    warning: null,
  },
  /** History view: Trash batches, newest first. */
  history: {
    batches: [],
    loaded: false,
    /** Entry ids ticked for restore. */
    selected: new Set(),
    expanded: new Set(),
    token: 0,
  },
};

export const sizeOf = (app) => state.sizes.get(app.path) ?? app.size;
