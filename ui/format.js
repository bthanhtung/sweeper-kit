// Formatting helpers.

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB'];

/** Decimal units, as Finder shows them. */
export function formatBytes(bytes) {
  if (bytes == null) return '…';
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < UNITS.length - 1) {
    value /= 1000;
    unit += 1;
  }
  const digits = unit === 0 || value >= 100 ? 0 : 1;
  return `${value.toFixed(digits)} ${UNITS[unit]}`;
}

const dateFormat = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium' });

/** `secs` is a Unix timestamp or null. */
export function formatDate(secs) {
  return secs == null ? 'Never opened' : dateFormat.format(new Date(secs * 1000));
}

const dateTimeFormat = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });

/** `secs` is a Unix timestamp. */
export function formatDateTime(secs) {
  return dateTimeFormat.format(new Date(secs * 1000));
}

export function basename(path) {
  const trimmed = path.replace(/\/+$/, '');
  return trimmed.slice(trimmed.lastIndexOf('/') + 1) || trimmed;
}

export function plural(count, one, many = `${one}s`) {
  return `${count} ${count === 1 ? one : many}`;
}

/** Stable colour for the generic app icon. */
export function hueOf(text) {
  let hash = 0;
  for (const ch of text) hash = (hash * 31 + ch.codePointAt(0)) >>> 0;
  return hash % 360;
}

/** Command line each cleaner action runs (the tool is looked up at fixed system paths). */
export const ACTION_COMMAND = {
  simctl_delete_unavailable: 'xcrun simctl delete unavailable',
  brew_cleanup: 'brew cleanup -s',
  docker_prune: 'docker system prune -f',
};

export const SOURCE_LABEL = { app_store: 'App Store', homebrew: 'Homebrew', other: 'Other' };

export const CATEGORY_LABEL = {
  app_bundle: 'App bundle',
  app_data: 'Application data',
  caches: 'Caches',
  preferences: 'Preferences',
  cookies_state: 'Cookies & saved state',
  logs: 'Logs',
  background_items: 'Background items',
};
