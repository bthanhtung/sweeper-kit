// Scripted demo timeline. Drives the real UI inside the same-origin iframe of stage.html:
// an eased cursor moves to an element, shows a press, then dispatches a real click().
// Time passes only through setTimeout, so Chrome virtual time (see record.mjs) controls it.
// Exposes window.__demoLength (ms, total virtual length) and window.__demoDone.
(() => {
  'use strict';

  const FRAME = 1000 / 60; // animation step; the recorder samples at a lower rate
  const frame = document.getElementById('app');
  const win = document.getElementById('window');
  const cursor = document.getElementById('cursor');
  const arrow = cursor.querySelector('svg');
  const ripple = document.getElementById('ripple');

  /** Virtual milliseconds consumed by the main track. */
  let elapsed = 0;
  const tick = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const sleep = async (ms) => {
    elapsed += ms;
    await tick(ms);
  };

  const easeInOut = (t) => (t < 0.5 ? 4 * t * t * t : 1 - ((-2 * t + 2) ** 3) / 2);
  const easeOut = (t) => 1 - (1 - t) ** 3;

  /** Calls step(progress 0..1) every FRAME for `ms`, on the main track. */
  async function animate(ms, step, ease = easeInOut) {
    const steps = Math.max(1, Math.round(ms / FRAME));
    for (let i = 1; i <= steps; i += 1) {
      await sleep(ms / steps);
      step(ease(i / steps));
    }
  }

  // ---- cursor ------------------------------------------------------------------

  const pos = { x: 1010, y: 640 };
  const paint = () => {
    cursor.style.transform = `translate(${pos.x - 3}px, ${pos.y - 2}px)`;
  };

  /** Eases the cursor along a gentle arc to (x, y). */
  async function moveTo(x, y, ms = 900) {
    const from = { ...pos };
    const dx = x - from.x;
    const dy = y - from.y;
    const dist = Math.hypot(dx, dy) || 1;
    const bend = Math.min(60, dist * 0.12);
    const nx = (-dy / dist) * bend;
    const ny = (dx / dist) * bend;
    const duration = Math.max(350, Math.min(ms, dist * 1.6));
    await animate(duration, (t) => {
      const arc = Math.sin(Math.PI * t);
      pos.x = from.x + dx * t + nx * arc;
      pos.y = from.y + dy * t + ny * arc;
      paint();
    });
  }

  /** Press effect: the arrow shrinks briefly and a ring fades out under it. Does not block. */
  async function pressEffect() {
    arrow.style.transform = 'scale(0.86)';
    ripple.style.transform = `translate(${pos.x}px, ${pos.y}px) scale(0.4)`;
    ripple.style.opacity = '1';
    const x = pos.x;
    const y = pos.y;
    await tick(90);
    arrow.style.transform = 'scale(1)';
    const steps = 18;
    for (let i = 1; i <= steps; i += 1) {
      await tick(20);
      const t = easeOut(i / steps);
      ripple.style.transform = `translate(${x}px, ${y}px) scale(${0.4 + t * 0.9})`;
      ripple.style.opacity = String(1 - t);
    }
    ripple.style.opacity = '0';
  }

  // ---- DOM access ----------------------------------------------------------------

  const doc = () => frame.contentDocument;
  const q = (selector) => doc().querySelector(selector);
  const byText = (selector, text) =>
    [...doc().querySelectorAll(selector)].find((node) => node.textContent.trim() === text) ?? null;

  /** Polls (virtual time) until `find()` returns something; fails loudly after `limit` ms. */
  async function waitFor(find, label, limit = 8000) {
    for (let waited = 0; waited <= limit; waited += 50) {
      const found = find();
      if (found) return found;
      await sleep(50);
    }
    throw new Error(`director: timed out waiting for ${label}`);
  }

  const scrollParent = (node) => {
    for (let el = node.parentElement; el; el = el.parentElement) {
      const overflow = frame.contentWindow.getComputedStyle(el).overflowY;
      if ((overflow === 'auto' || overflow === 'scroll') && el.scrollHeight > el.clientHeight) return el;
    }
    return null;
  };

  /** Smoothly scrolls the nearest scroller so `node` sits comfortably inside it. */
  async function reveal(node) {
    const box = scrollParent(node);
    if (!box) return;
    const outer = box.getBoundingClientRect();
    const inner = node.getBoundingClientRect();
    const margin = 70;
    if (inner.top >= outer.top + margin && inner.bottom <= outer.bottom - margin) return;
    const start = box.scrollTop;
    const target = start + (inner.top - outer.top) - (outer.height - inner.height) / 2;
    await animate(600, (t) => { box.scrollTop = start + (target - start) * t; });
  }

  /** On-screen centre of an element, in stage coordinates. */
  function centre(node) {
    const outer = frame.getBoundingClientRect();
    const r = node.getBoundingClientRect();
    return { x: outer.left + r.left + r.width / 2, y: outer.top + r.top + r.height / 2 };
  }

  /** Waits for the element, moves the cursor onto it, presses and clicks it. */
  async function click(find, label, { hold = 350 } = {}) {
    const node = await waitFor(find, label);
    await reveal(node);
    const target = centre(node);
    await moveTo(target.x + 2, target.y + 1);
    pressEffect();
    await sleep(90);
    node.click();
    await sleep(hold);
  }

  /** Smoothly scrolls an element by `distance` px. */
  async function scrollBy(node, distance, ms = 1200) {
    const start = node.scrollTop;
    await animate(ms, (t) => { node.scrollTop = start + distance * t; });
  }

  /** Types into an input, firing `input` events. Kept for scenes that need it. */
  async function type(find, text, label) {
    const input = await waitFor(find, label);
    const target = centre(input);
    await moveTo(target.x, target.y);
    pressEffect();
    input.focus();
    for (let i = 1; i <= text.length; i += 1) {
      await sleep(70);
      input.value = text.slice(0, i);
      input.dispatchEvent(new Event('input', { bubbles: true }));
    }
  }

  const fade = (ms, from, to) => animate(ms, (t) => {
    win.style.opacity = String(from + (to - from) * t);
  }, easeOut);

  // ---- scene -----------------------------------------------------------------------

  async function scene() {
    // The recorder holds the timeline until virtual time is paused (uncounted polling).
    while (window.__demoHold) await tick(20);
    paint();
    await waitFor(() => doc() && doc().readyState === 'complete' && q('.app-row'), 'app list');
    await sleep(500); // sizes arrive 300 ms after the list
    await fade(500, 0, 1);
    cursor.style.opacity = '1';
    await sleep(600);

    // 1. Pick Pixel Forge, let the scan finish, scroll through the categories.
    await click(() => byText('.app-row .app-name', 'Pixel Forge'), 'Pixel Forge row', { hold: 200 });
    await waitFor(() => byText('.detail-scroll h2', 'Pixel Forge'), 'Pixel Forge scan');
    await sleep(1100);
    await scrollBy(q('.detail-scroll'), 300, 1400);
    await sleep(700);

    // 2. Uninstall → dry-run sheet → Move to Trash → result → Done.
    await click(() => q('#uninstall'), 'Uninstall button', { hold: 200 });
    await waitFor(() => byText('#sheet button', 'Move to Trash'), 'confirmation sheet');
    await sleep(1900);
    await click(() => byText('#sheet button', 'Move to Trash'), 'Move to Trash', { hold: 200 });
    await waitFor(() => byText('#sheet button', 'Done'), 'result sheet', 12000);
    await sleep(1900);
    await click(() => byText('#sheet button', 'Done'), 'Done button', { hold: 900 });

    // 3. Leftovers: scan, open groups, tick a couple of items.
    await click(() => q('#nav-leftovers'), 'Leftovers nav', { hold: 500 });
    await click(() => q('#leftovers-scan'), 'Scan button', { hold: 200 });
    await waitFor(() => q('#view-leftovers .group'), 'leftovers result');
    await sleep(1000);
    await click(() => q('[data-key="toggle:com.waypoint.maps"]'), 'Waypoint group', { hold: 600 });
    await click(() => q('[data-key="toggle:org.example.legacytool"]'), 'legacytool group', { hold: 500 });
    await click(
      () => q('section[aria-label="org.example.legacytool"] input.lead'),
      'legacytool item',
      { hold: 900 },
    );
    await click(() => q('[data-key="toggle:Old Sketches"]'), 'Old Sketches group', { hold: 500 });
    await click(
      () => q('section[aria-label="Old Sketches"] input.lead'),
      'Old Sketches item',
      { hold: 1500 },
    );

    // 4. Cleaner: scan and show cache categories with sizes.
    await click(() => q('#nav-cleaner'), 'Cleaner nav', { hold: 500 });
    await click(() => q('#clean-scan'), 'Scan button', { hold: 200 });
    await waitFor(() => q('#view-cleaner .group'), 'cleaner result');
    await sleep(1100);
    await click(() => q('[data-key="toggle:user-caches"]'), 'User caches group', { hold: 700 });
    await click(() => q('[data-key="toggle:developer"]'), 'Developer group', { hold: 1800 });

    // 5. History shows the uninstall; back to the Uninstaller for a clean loop end.
    await click(() => q('#nav-history'), 'History nav', { hold: 400 });
    await waitFor(() => q('#view-history .disclosure'), 'history entries');
    await sleep(700);
    await click(() => q('#view-history .disclosure'), 'newest history batch', { hold: 1800 });
    await click(() => q('#nav-uninstaller'), 'Uninstaller nav', { hold: 1100 });
    cursor.style.opacity = '0';
    await fade(500, 1, 0);
    await sleep(300);
  }

  window.__demoDone = false;
  window.__demoError = null;
  window.__demoLength = 0;
  scene().then(
    () => {
      window.__demoLength = elapsed;
      window.__demoDone = true;
    },
    (error) => {
      window.__demoError = String(error && error.message ? error.message : error);
      window.__demoDone = true;
    },
  );
  void type; // available for scenes that fill in a field
})();
