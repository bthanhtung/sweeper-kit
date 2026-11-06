// Entry point: wires the views to the Rust command layer and its events.

import { $, h, clear, showError, announce } from './dom.js';
import { invoke, listen, CANCELLED } from './api.js';
import { highlight, initList, renderList, updateSize } from './list.js';
import {
  initDetail, refreshDetail, renderDetail, showLoading, showPlaceholder,
} from './detail.js';
import { render as renderCleaner, startScan } from './cleaner.js';
import { render as renderLeftovers, startScan as startLeftoversScan } from './leftovers.js';
import { render as renderHistory, loadHistory } from './history.js';
import { initSettings, renderSettings } from './settings.js';
import { startBrewZap, startUninstall } from './sheet.js';
import { state } from './state.js';

async function loadApps() {
  try {
    state.apps = await invoke('list_apps');
    renderList();
    announce(`${state.apps.length} apps`);
  } catch (e) {
    showError(String(e));
  }
}

/** Scans `path`; a newer request makes older results irrelevant. */
async function scanApp(path) {
  const token = ++state.scanToken;
  const known = state.apps.find((a) => a.path === path);
  showLoading(known ? known.name : path);
  try {
    const scan = await invoke('scan_app', { path });
    if (token !== state.scanToken) return;
    state.scan = scan;
    state.preset = 'Everything';
    state.receipts = [];
    state.forgetReceipts = new Set();
    renderDetail();
    announce(`${scan.app.name}: ${scan.groups.length} groups of leftovers`);
    loadReceipts(scan.app.path, token);
  } catch (e) {
    if (token !== state.scanToken || String(e) === CANCELLED) return;
    state.scan = null;
    showPlaceholder(String(e));
  }
}

/** Receipts come after the scan (they ask `pkgutil`); a newer scan makes them irrelevant. */
async function loadReceipts(path, token) {
  try {
    const receipts = await invoke('receipts', { path });
    if (token !== state.scanToken) return;
    state.receipts = receipts;
    if (receipts.length) refreshDetail();
  } catch {
    // Receipts are optional; the scan result stays usable without them.
  }
}

/** After an uninstall: refresh the list and clear the detail pane. */
async function afterRemoval() {
  state.scan = null;
  state.selected = null;
  state.scanToken += 1;
  showPlaceholder('Select an app to see what it leaves behind.');
  await loadApps();
  refreshFda();
}

async function refreshFda() {
  const banner = $('#fda-banner');
  let status = 'Unknown';
  try {
    status = await invoke('fda_status');
  } catch {
    // Unknown means no banner.
  }
  banner.hidden = status !== 'Denied';
  if (status !== 'Denied') return;
  clear(banner);
  banner.append(
    h('strong', {}, 'Full Disk Access is off'),
    h('p', {}, 'Without it some app data (containers, mail, Safari) cannot be found. Turn on Sweeper Kit in Privacy & Security › Full Disk Access, then reopen the app.'),
    h('button', {
      type: 'button',
      class: 'btn',
      onclick: () => invoke('open_fda_settings').catch((e) => showError(String(e))),
    }, 'Open System Settings'),
  );
}

/** An app dropped on the window or the Dock icon. */
function openDropped(path) {
  const known = state.apps.find((a) => a.path === path);
  if (known) highlight(path);
  showView('uninstaller');
  scanApp(path);
}

const VIEWS = ['uninstaller', 'leftovers', 'cleaner', 'history', 'settings'];

function showView(name) {
  for (const view of VIEWS) {
    $(`#view-${view}`).hidden = view !== name;
    const nav = $(`#nav-${view}`);
    if (view === name) nav.setAttribute('aria-current', 'page');
    else nav.removeAttribute('aria-current');
  }
  if (name === 'cleaner') renderCleaner();
  if (name === 'leftovers') renderLeftovers();
  if (name === 'history') {
    renderHistory();
    loadHistory();
  }
  if (name === 'settings') renderSettings();
}

function bindShortcuts() {
  document.addEventListener('keydown', (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'f') {
      e.preventDefault();
      showView('uninstaller');
      $('#search').focus();
      $('#search').select();
    }
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'r') {
      // Also stops the webview from reloading itself.
      e.preventDefault();
      if ($('#sheet').open) return;
      showView('cleaner');
      startScan();
    }
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'l') {
      e.preventDefault();
      if ($('#sheet').open) return;
      showView('leftovers');
      startLeftoversScan();
    }
  });
  document.addEventListener('show-view', (e) => showView(e.detail));
}

async function main() {
  for (const nav of document.querySelectorAll('.nav-item')) {
    nav.addEventListener('click', () => showView(nav.dataset.view));
  }
  initList(scanApp);
  initDetail({
    onUninstall: () => startUninstall(state.scan, afterRemoval),
    onBrewZap: (app) => startBrewZap(app, afterRemoval),
  });
  bindShortcuts();

  await listen('app-sizes', updateSize);
  await listen('open-app', openDropped);
  window.addEventListener('focus', refreshFda);

  state.adminAvailable = await invoke('admin_available');
  await initSettings(loadApps);
  await loadApps();
  refreshFda();

  const pending = await invoke('take_pending_open');
  if (pending) openDropped(pending);
}

main().catch((e) => showError(String(e)));
