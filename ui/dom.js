// Tiny DOM builder. Text always goes through text nodes, so data is never parsed as markup.

const PROPERTIES = new Set(['checked', 'indeterminate', 'value', 'disabled', 'hidden']);

export function h(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value == null || value === false) continue;
    if (key.startsWith('on')) node.addEventListener(key.slice(2), value);
    else if (key === 'class') node.className = value;
    else if (PROPERTIES.has(key)) node[key] = value;
    else node.setAttribute(key, value === true ? '' : String(value));
  }
  append(node, children);
  return node;
}

function append(node, children) {
  for (const child of children) {
    if (child == null || child === false) continue;
    if (Array.isArray(child)) append(node, child);
    else node.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
}

export function clear(node) {
  node.replaceChildren();
}

export const $ = (selector) => document.querySelector(selector);

/** Announces `message` to screen readers. */
export function announce(message) {
  $('#live').textContent = message;
}

let errorTimer;
/** Shows a dismissing error toast. */
export function showError(message) {
  const toast = $('#error');
  toast.textContent = message;
  toast.hidden = false;
  clearTimeout(errorTimer);
  errorTimer = setTimeout(() => { toast.hidden = true; }, 8000);
}
