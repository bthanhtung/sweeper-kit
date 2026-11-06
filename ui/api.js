// Bridge to the Rust command layer (Tauri global API, no bundler).

const tauri = window.__TAURI__;

export const invoke = (command, args) => tauri.core.invoke(command, args);
export const listen = (event, handler) => tauri.event.listen(event, (e) => handler(e.payload));

/** Native confirmation dialog; resolves to true when the user accepts. */
export const confirm = (message, { title, okLabel, cancelLabel = 'Cancel' }) =>
  tauri.dialog.ask(message, { title, kind: 'warning', okLabel, cancelLabel });

/** Native folder picker; resolves to a path or null. */
export const pickFolder = () => tauri.dialog.open({ directory: true, multiple: false });

/** The message the Rust side uses for a superseded scan. */
export const CANCELLED = 'cancelled';
