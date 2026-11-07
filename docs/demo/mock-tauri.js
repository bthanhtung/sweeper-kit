// Demo harness: stands in for the Tauri globals so the real UI runs in a plain
// browser. Fictional sample data only; nothing touches the system. State lives
// in memory and resets on reload. Shapes follow the serde types in
// crates/sweeper-core and crates/sweeper-app.
(() => {
  'use strict';

  const MB = 1_000_000;
  const HOME = '/Users/demo';
  const PIXEL = 'dev.pixelforge.app';

  const ts = (iso) => Math.floor(Date.parse(iso) / 1000);
  const clone = (value) => structuredClone(value);
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const sum = (values) => values.reduce((total, v) => total + (v ?? 0), 0);
  const basename = (path) => path.slice(path.lastIndexOf('/') + 1);

  // ---- events ---------------------------------------------------------------

  const listeners = new Map();

  function emit(event, payload) {
    for (const handler of listeners.get(event) ?? []) handler({ event, payload });
  }

  function listen(event, handler) {
    if (!listeners.has(event)) listeners.set(event, new Set());
    listeners.get(event).add(handler);
    return Promise.resolve(() => listeners.get(event)?.delete(handler));
  }

  // ---- applications -----------------------------------------------------------

  const SIZE = {
    [PIXEL]: 412 * MB,
    'io.notely.mac': 88 * MB,
    'com.tunebox.player': 150 * MB,
    'com.waypoint.navigator': 64 * MB,
    'com.clipstack.app': 21 * MB,
    'com.hexcalc.mac': 9 * MB,
    'app.gridline.desktop': 37 * MB,
    'com.apple.Safari': 486 * MB,
  };

  let apps = [
    app('Pixel Forge', `${HOME}/Applications/Pixel Forge.app`, '4.2.1', PIXEL, '2026-11-05T18:40:00Z', { kind: 'other' }),
    app('Notely', '/Applications/Notely.app', '2.8.0', 'io.notely.mac', '2026-11-06T08:15:00Z', { kind: 'app_store' }),
    app('Tunebox', '/Applications/Tunebox.app', '1.14.3', 'com.tunebox.player', '2026-11-03T21:02:00Z', { kind: 'homebrew', cask: 'tunebox' }),
    app('Waypoint', '/Applications/Waypoint.app', '3.0.2', 'com.waypoint.navigator', '2026-10-30T12:30:00Z', { kind: 'other' }),
    app('Clipstack', '/Applications/Clipstack.app', '1.6.0', 'com.clipstack.app', '2026-11-01T16:45:00Z', { kind: 'other' }),
    app('Hexcalc', '/Applications/Hexcalc.app', '5.1', 'com.hexcalc.mac', '2026-09-18T10:05:00Z', { kind: 'app_store' }),
    app('Gridline', `${HOME}/Applications/Gridline.app`, '0.9.4', 'app.gridline.desktop', null, { kind: 'other' }),
    app('Safari', '/Applications/Safari.app', '18.6', 'com.apple.Safari', '2026-11-06T22:10:00Z', { kind: 'other' }, true),
  ];

  function app(name, path, version, bundleId, lastOpened, source, isApple = false) {
    return {
      path,
      name,
      version,
      bundle_id: bundleId,
      team_id: null,
      size: null,
      last_opened: lastOpened ? ts(lastOpened) : null,
      source,
      is_apple: isApple,
    };
  }

  const findApp = (path) => {
    const found = apps.find((a) => a.path === path);
    if (!found) throw new Error(`demo: no installed app at ${path}`);
    return found;
  };

  // ---- app scans --------------------------------------------------------------

  const item = (path, size, category, reason, extra = {}) => ({
    path,
    size,
    checked: true,
    category,
    reason,
    refused: null,
    shared: false,
    requires_admin: false,
    ...extra,
  });

  function pixelForgeGroups() {
    return [
      { category: 'app_bundle', items: [item(`${HOME}/Applications/Pixel Forge.app`, SIZE[PIXEL], 'app_bundle', 'Application bundle')] },
      { category: 'app_data', items: [item(`${HOME}/Library/Application Support/Pixel Forge`, 180 * MB, 'app_data', 'Named after the app')] },
      { category: 'preferences', items: [item(`${HOME}/Library/Preferences/${PIXEL}.plist`, 24_000, 'preferences', 'Bundle identifier match')] },
      { category: 'caches', items: [item(`${HOME}/Library/Caches/${PIXEL}`, 96 * MB, 'caches', 'Bundle identifier match')] },
      { category: 'cookies_state', items: [item(`${HOME}/Library/Saved Application State/${PIXEL}.savedState`, 1_400_000, 'cookies_state', 'Bundle identifier match')] },
      { category: 'logs', items: [item(`${HOME}/Library/Logs/Pixel Forge`, 6 * MB, 'logs', 'Named after the app')] },
      {
        category: 'background_items',
        items: [
          item(`${HOME}/Library/LaunchAgents/${PIXEL}.updater.plist`, 4_000, 'background_items', 'Launch agent of the app'),
          item(`/Library/PrivilegedHelperTools/${PIXEL}.helper`, 9 * MB, 'background_items', 'Privileged helper installed by the app', { requires_admin: true }),
        ],
      },
    ];
  }

  function genericGroups(a) {
    const size = SIZE[a.bundle_id];
    if (a.is_apple) {
      return [
        {
          category: 'app_bundle',
          items: [item(a.path, size, 'app_bundle', 'Apple app — read only', { checked: false, refused: 'Apple app — read only' })],
        },
      ];
    }
    return [
      { category: 'app_bundle', items: [item(a.path, size, 'app_bundle', 'Application bundle')] },
      { category: 'caches', items: [item(`${HOME}/Library/Caches/${a.bundle_id}`, Math.round(size * 0.2), 'caches', 'Bundle identifier match')] },
      { category: 'preferences', items: [item(`${HOME}/Library/Preferences/${a.bundle_id}.plist`, 16_000, 'preferences', 'Bundle identifier match')] },
    ];
  }

  function scanOf(a) {
    const groups = a.bundle_id === PIXEL ? pixelForgeGroups() : genericGroups(a);
    const otherCopies = a.bundle_id === PIXEL
      ? [item(`${HOME}/Developer/build/Pixel Forge.app`, 402 * MB, 'app_bundle', 'Other copy with the same bundle identifier', { checked: false })]
      : [];
    const everything = [...groups.flatMap((g) => g.items), ...otherCopies];
    return {
      app: { ...clone(a), size: SIZE[a.bundle_id] },
      groups,
      other_copies: otherCopies,
      inaccessible: [],
      total_size: sum(everything.map((i) => i.size)),
    };
  }

  const WANTED = {
    AppOnly: (category) => category === 'app_bundle',
    AppAndCaches: (category) => category === 'app_bundle' || category === 'caches',
    Everything: () => true,
  };

  /** Mirrors `apply_preset` in sweeper-core: refused and shared items are never checked. */
  function applyPreset(scan, preset) {
    const wanted = WANTED[preset];
    if (!wanted) throw new Error(`demo: unknown preset ${preset}`);
    const next = clone(scan);
    for (const group of next.groups) {
      for (const i of group.items) i.checked = wanted(group.category) && !i.refused && !i.shared;
    }
    for (const i of next.other_copies) i.checked = false;
    return next;
  }

  const allItems = (scan) => [...scan.groups.flatMap((g) => g.items), ...scan.other_copies];

  function planRemoval(scan) {
    const items = allItems(scan)
      .filter((i) => i.checked && !i.refused)
      .map((i) => ({ path: i.path, size: i.size }));
    return { items, total_size: sum(items.map((i) => i.size)) };
  }

  // ---- running apps, receipts, settings ---------------------------------------

  /** Pixel Forge is running until the first quit request succeeds. */
  let pixelRunning = true;
  const PIXEL_PROCESS = { pid: 4123, path: `${HOME}/Applications/Pixel Forge.app`, name: 'Pixel Forge' };

  const receiptStore = new Map([
    [PIXEL, [{ pkg_id: `${PIXEL}.pkg`, version: '4.2.1', install_location: '/Applications', files_count: 318 }]],
  ]);

  let settings = {
    extra_app_folders: [],
    protected: [],
    delete_immediately: false,
    min_item_size_mb: 0,
    downloads_age_days: 30,
  };

  let removedApps = [
    { bundle_id: 'com.waypoint.maps', name: 'Waypoint', removed_at: ts('2026-10-20T09:00:00Z'), app_path: '/Applications/Waypoint.app' },
  ];

  /** Drops an app from the list after it was removed, and records it in the removed-apps history. */
  function forgetInstalled(a) {
    apps = apps.filter((x) => x.path !== a.path);
    removedApps.push({ bundle_id: a.bundle_id, name: a.name, removed_at: Math.floor(Date.now() / 1000), app_path: a.path });
  }

  // ---- cleaner ----------------------------------------------------------------

  const entry = (id, title, path, size, extra = {}) => ({
    id,
    title,
    path,
    action: null,
    size,
    checked: true,
    warning: null,
    refused: null,
    mode: 'trash',
    ...extra,
  });

  const ACTION_LABEL = {
    simctl_delete_unavailable: 'Delete unavailable simulators',
    brew_cleanup: 'Homebrew cleanup',
    docker_prune: 'Docker prune',
  };

  let cleanCategories = [
    {
      id: 'user-caches',
      title: 'User caches',
      entries: [
        entry('user-caches/pixel-forge', 'Pixel Forge cache', `${HOME}/Library/Caches/${PIXEL}`, 96 * MB),
        entry('user-caches/notely', 'Notely cache', `${HOME}/Library/Caches/io.notely.mac`, 41 * MB),
        entry('user-caches/tunebox', 'Tunebox cache', `${HOME}/Library/Caches/com.tunebox.player`, 18 * MB),
      ],
    },
    {
      id: 'logs',
      title: 'Logs',
      entries: [
        entry('logs/diagnostic-reports', 'Diagnostic reports', `${HOME}/Library/Logs/DiagnosticReports`, 12 * MB),
        entry('logs/clipstack', 'Clipstack logs', `${HOME}/Library/Logs/Clipstack`, 3 * MB),
      ],
    },
    {
      id: 'browser-caches',
      title: 'Browser caches',
      entries: [
        entry('browser-caches/fernfox', 'Fernfox browser cache', `${HOME}/Library/Caches/com.fernfox.browser`, 230 * MB),
      ],
    },
    {
      id: 'developer',
      title: 'Developer',
      entries: [
        entry('developer/derived-data', 'Xcode DerivedData', `${HOME}/Library/Developer/Xcode/DerivedData`, 2_100 * MB),
        entry('developer/npm', 'npm cache', `${HOME}/.npm/_cacache`, 640 * MB),
        entry('developer/brew-cleanup', 'Homebrew old downloads and versions', null, null, { action: 'brew_cleanup' }),
      ],
    },
    {
      id: 'trash',
      title: 'Trash',
      entries: [
        entry('trash/items', 'Trash', `${HOME}/.Trash`, 380 * MB, { checked: false, mode: 'delete_only' }),
      ],
    },
    {
      id: 'old-downloads',
      title: 'Old downloads',
      entries: [
        entry('old-downloads/pixel-forge-installer', 'PixelForge-3.9.dmg', `${HOME}/Downloads/PixelForge-3.9.dmg`, 310 * MB, {
          checked: false,
          warning: 'Older than 30 days',
        }),
        entry('old-downloads/archive-2025', 'archive-2025.zip', `${HOME}/Downloads/archive-2025.zip`, 88 * MB, {
          checked: false,
          warning: 'Older than 30 days',
        }),
      ],
    },
  ];

  const cleanEntries = (selected) => {
    const wanted = new Set(selected);
    return cleanCategories.flatMap((c) => c.entries).filter((e) => wanted.has(e.id) && !e.refused);
  };

  /** Mirrors `CleanPlan::from_scan`: path entries go to Trash or delete lists, actions are deduplicated. */
  function planClean(selected) {
    const plan = { trash_items: [], delete_items: [], actions: [], total_size: 0 };
    for (const e of cleanEntries(selected)) {
      const size = e.size ?? 0;
      if (e.path) (e.mode === 'delete_only' ? plan.delete_items : plan.trash_items).push({ path: e.path, size });
      else if (e.action && !plan.actions.includes(e.action)) plan.actions.push(e.action);
      else continue;
      plan.total_size += size;
    }
    return plan;
  }

  // ---- leftovers --------------------------------------------------------------

  let leftoverGroups = [
    {
      key: 'com.waypoint.maps',
      display_name: 'Waypoint (removed)',
      confidence: 'high',
      items: [
        item(`${HOME}/Library/Application Support/com.waypoint.maps`, 180 * MB, 'app_data', 'Bundle identifier of an app you removed'),
        item(`${HOME}/Library/Caches/com.waypoint.maps`, 50 * MB, 'caches', 'Bundle identifier of an app you removed'),
      ],
    },
    {
      key: 'org.example.legacytool',
      display_name: 'org.example.legacytool',
      confidence: 'medium',
      items: [
        item(`${HOME}/Library/Application Support/org.example.legacytool`, 36 * MB, 'app_data', 'Identifier-shaped name, no installed app uses it', { checked: false }),
      ],
    },
    {
      key: 'Old Sketches',
      display_name: 'Old Sketches',
      confidence: 'low',
      items: [
        item(`${HOME}/Library/Application Support/Old Sketches`, 52 * MB, 'app_data', 'Folder name matches no installed app', { checked: false }),
      ],
    },
  ];

  const leftoverItems = () => leftoverGroups.flatMap((g) => g.items);

  function withGroupSizes(groups) {
    return groups.map((g) => ({ ...g, size: sum(g.items.map((i) => i.size)) }));
  }

  // ---- history ----------------------------------------------------------------

  const historyEntry = (id, batchId, timestamp, original, trashLocation, size, { admin = false, inTrash = true } = {}) => ({
    entry: { id, timestamp, original, trash_location: trashLocation, size, batch_id: batchId, admin },
    in_trash: inTrash,
  });

  const batch = (batchId, timestamp, entries) => ({
    batch_id: batchId,
    timestamp,
    entries,
    size: sum(entries.map((s) => s.entry.size)),
  });

  let batches = [
    batch('batch-0002', ts('2026-11-04T14:20:00Z'), [
      historyEntry('e-0001', 'batch-0002', ts('2026-11-04T14:20:00Z'), `${HOME}/Library/Caches/io.notely.mac-old`, `${HOME}/.Trash/io.notely.mac-old`, 22 * MB),
      historyEntry('e-0002', 'batch-0002', ts('2026-11-04T14:20:00Z'), `${HOME}/Library/Logs/Clipstack`, `${HOME}/.Trash/Clipstack`, 3 * MB, { inTrash: false }),
    ]),
    batch('batch-0001', ts('2026-10-28T10:05:00Z'), [
      historyEntry('e-0003', 'batch-0001', ts('2026-10-28T10:05:00Z'), `${HOME}/Library/Application Support/Hexcalc Legacy`, `${HOME}/.Trash/Hexcalc Legacy`, 14 * MB),
      historyEntry('e-0004', 'batch-0001', ts('2026-10-28T10:05:00Z'), '/Library/LaunchDaemons/com.example.oldsync.plist', '/Library/Application Support/SweeperKit/Staging/com.example.oldsync.plist', 1 * MB, { admin: true }),
    ]),
  ];

  // ---- command handlers -------------------------------------------------------

  function listApps() {
    const current = apps.slice();
    // Sizes arrive a moment after the list, as they do in the app.
    setTimeout(() => {
      for (const a of current) emit('app-sizes', { path: a.path, size: SIZE[a.bundle_id] });
    }, 300);
    return apps.map((a) => ({ ...clone(a), size: null }));
  }

  async function uninstall({ scan, forgetReceipts }) {
    const plan = planRemoval(scan);
    const adminPaths = new Set(allItems(scan).filter((i) => i.requires_admin).map((i) => i.path));
    const total = plan.items.length;
    const step = Math.round(1500 / Math.max(total, 1));
    for (const [index, planned] of plan.items.entries()) {
      await sleep(step);
      emit('uninstall-progress', { done: index + 1, total, path: planned.path });
    }

    const removed = plan.items.map((i) => ({
      original: i.path,
      trash_location: adminPaths.has(i.path)
        ? `/Library/Application Support/SweeperKit/Staging/${basename(i.path)}`
        : `${HOME}/.Trash/${basename(i.path)}`,
      size: i.size,
    }));
    const adminCount = plan.items.filter((i) => adminPaths.has(i.path)).length;
    const userCount = plan.items.length - adminCount;
    const forgotten = forgetReceipts ?? [];
    for (const pkgId of forgotten) {
      for (const [bundleId, list] of receiptStore) {
        receiptStore.set(bundleId, list.filter((r) => r.pkg_id !== pkgId));
      }
    }

    const steps = [
      { name: 'Unloaded background items', ok: true, detail: `${PIXEL}.updater` },
      { name: 'Moved app files to the Trash', ok: true, detail: `${userCount} items` },
      ...forgotten.map((pkgId) => ({ name: 'Forgot installer receipt', ok: true, detail: pkgId })),
    ];
    const admin = adminCount > 0
      ? { name: 'Removed system items', ok: true, detail: `${adminCount} items moved to the staging folder` }
      : null;

    // The removal also lands in History, as the real app records it.
    const now = Math.floor(Date.now() / 1000);
    const batchId = `batch-${String(batches.length + 1).padStart(4, '0')}`;
    batches.unshift(batch(batchId, now, removed.map((r, index) => historyEntry(
      `e-${batchId}-${index}`, batchId, now, r.original, r.trash_location, r.size, { admin: adminPaths.has(r.original) },
    ))));

    forgetInstalled(scan.app);
    return { steps, removal: { removed, failed: [], warnings: [] }, admin, remaining: [] };
  }

  async function cleanRun({ selected }) {
    const plan = planClean(selected);
    const steps = [
      ...plan.trash_items.map((i) => ({ kind: 'path', ...i, mode: 'trash' })),
      ...plan.delete_items.map((i) => ({ kind: 'path', ...i, mode: 'delete_only' })),
      ...plan.actions.map((action) => ({ kind: 'action', action })),
    ];
    const pause = Math.round(1200 / Math.max(steps.length, 1));
    const removed = [];
    const actions = [];
    let freed = 0;
    for (const [index, step] of steps.entries()) {
      await sleep(pause);
      if (step.kind === 'path') {
        removed.push({
          original: step.path,
          trash_location: step.mode === 'trash' ? `${HOME}/.Trash/${basename(step.path)}` : null,
          size: step.size,
        });
        freed += step.size;
        emit('clean-progress', { done: index + 1, total: steps.length, path: step.path });
      } else {
        actions.push({ name: ACTION_LABEL[step.action], ok: true, detail: 'Completed' });
        emit('clean-progress', { done: index + 1, total: steps.length, path: ACTION_LABEL[step.action] });
      }
    }
    const done = new Set(cleanEntries(selected).map((e) => e.id));
    cleanCategories = cleanCategories
      .map((c) => ({ ...c, entries: c.entries.filter((e) => !done.has(e.id)) }))
      .filter((c) => c.entries.length > 0);
    return { removed, actions, failed: [], freed, warnings: [] };
  }

  async function removeLeftovers({ selected }) {
    const wanted = new Set(selected);
    const planned = leftoverItems().filter((i) => wanted.has(i.path) && !i.refused);
    const pause = Math.round(900 / Math.max(planned.length, 1));
    for (const [index, i] of planned.entries()) {
      await sleep(pause);
      emit('leftovers-progress', { done: index + 1, total: planned.length, path: i.path });
    }
    const removedPaths = new Set(planned.map((i) => i.path));
    leftoverGroups = leftoverGroups
      .map((g) => ({ ...g, items: g.items.filter((i) => !removedPaths.has(i.path)) }))
      .filter((g) => g.items.length > 0);
    return {
      removed: planned.map((i) => ({
        original: i.path,
        trash_location: `${HOME}/.Trash/${basename(i.path)}`,
        size: i.size,
      })),
      failed: [],
      warnings: [],
    };
  }

  const commands = {
    list_apps: () => listApps(),
    scan_app: async ({ path }) => {
      const found = findApp(path);
      await sleep(600);
      return scanOf(found);
    },
    apply_preset: ({ scan, preset }) => applyPreset(scan, preset),
    plan_removal: ({ scan }) => planRemoval(scan),
    receipts: ({ path }) => {
      const found = apps.find((a) => a.path === path);
      return found ? clone(receiptStore.get(found.bundle_id) ?? []) : [];
    },
    admin_available: () => true,
    fda_status: () => 'Granted',
    open_fda_settings: () => null,
    reveal_in_finder: () => null,
    take_pending_open: () => null,
    rules_warning: () => null,
    clean_rules_warning: () => null,
    orphan_rules_warning: () => null,
    running_instances: ({ scan }) => (scan.app.bundle_id === PIXEL && pixelRunning ? [clone(PIXEL_PROCESS)] : []),
    quit_apps: ({ scan }) => {
      const exited = scan.app.bundle_id === PIXEL && pixelRunning ? [clone(PIXEL_PROCESS)] : [];
      if (exited.length) pixelRunning = false;
      return { exited, forced: [], still_running: [] };
    },
    uninstall: (args) => uninstall(args),
    brew_zap_command: ({ app }) =>
      app.source.kind === 'homebrew' ? ['brew', 'uninstall', '--cask', '--zap', app.source.cask] : null,
    brew_zap: ({ app }) => {
      forgetInstalled(app);
      return { name: 'brew uninstall --cask --zap', ok: true, detail: `${app.name} removed by Homebrew` };
    },
    get_settings: () => clone(settings),
    save_settings: ({ settings: next }) => {
      settings = clone(next);
      return clone(settings);
    },
    scan_cleaner: () => {
      const categories = cleanCategories.map((c) => ({ ...clone(c), size: sum(c.entries.map((e) => e.size ?? 0)) }));
      return {
        categories,
        inaccessible: [],
        total_size: sum(categories.map((c) => c.size)),
      };
    },
    plan_clean: ({ selected }) => planClean(selected),
    clean: (args) => cleanRun(args),
    scan_leftovers: () => {
      const groups = withGroupSizes(clone(leftoverGroups));
      return { groups, inaccessible: [], total_size: sum(groups.map((g) => g.size)) };
    },
    plan_leftovers: ({ selected }) => {
      const wanted = new Set(selected);
      const items = leftoverItems()
        .filter((i) => wanted.has(i.path) && !i.refused)
        .map((i) => ({ path: i.path, size: i.size }));
      return { items, total_size: sum(items.map((i) => i.size)) };
    },
    remove_leftovers: (args) => removeLeftovers(args),
    history: () => clone(batches),
    restore: ({ ids }) =>
      ids.map((id) => {
        const status = batches.flatMap((b) => b.entries).find((s) => s.entry.id === id);
        if (!status) return { id, ok: false, path_or_error: 'No longer in history' };
        if (!status.in_trash) return { id, ok: false, path_or_error: 'No longer in Trash' };
        status.in_trash = false;
        return { id, ok: true, path_or_error: status.entry.original };
      }),
    forget_history: ({ ids }) => {
      const wanted = new Set(ids);
      let count = 0;
      batches = batches
        .map((b) => ({
          ...b,
          entries: b.entries.filter((s) => {
            const drop = wanted.has(s.entry.id) && !s.in_trash;
            if (drop) count += 1;
            return !drop;
          }),
        }))
        .filter((b) => b.entries.length > 0);
      return count;
    },
    removed_apps: () => clone(removedApps),
    forget_removed_app: ({ bundleId }) => {
      removedApps = removedApps.filter((a) => a.bundle_id !== bundleId);
      return null;
    },
  };

  function invoke(command, args = {}) {
    const handler = commands[command];
    if (!handler) {
      console.warn(`demo: unknown command ${command}`);
      return Promise.reject(new Error(`demo: unknown command ${command}`));
    }
    return Promise.resolve().then(() => handler(args));
  }

  window.__TAURI__ = {
    core: { invoke },
    event: { listen },
    dialog: {
      // Every confirmation is accepted; the sheets are the real UI.
      ask: () => sleep(150).then(() => true),
      open: () => Promise.resolve('/Users/demo/Applications'),
    },
  };
})();
