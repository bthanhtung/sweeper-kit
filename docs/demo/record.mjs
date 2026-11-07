// Records docs/demo.gif from the real UI: node --experimental-websocket docs/demo/record.mjs
// Node 20, no dependencies. Needs Google Chrome (override with CHROME) and ffmpeg on PATH (override with FFMPEG).
// Chrome virtual time makes the run deterministic: every frame is one fixed time slice of director.js.

import { spawn, spawnSync } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, existsSync, rmSync, renameSync, statSync, createReadStream } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, normalize, resolve, extname, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const FPS = 15;
const WIDTH = 1280;
const HEIGHT = 800;
const SCALE = 2;
const OUT_WIDTH = 960;
const MAX_FRAMES = FPS * 90; // safety stop: the scene is ~25 s
const TARGET_BYTES = 4 * 1024 * 1024;

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const output = join(root, 'docs', 'demo.gif');
const chromePath = process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const ffmpegPath = process.env.FFMPEG || 'ffmpeg';

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.json': 'application/json',
};

const sleepReal = (ms) => new Promise((done) => setTimeout(done, ms));

/** Static file server for the repo root, bound to loopback only. */
function startServer() {
  const server = createServer((req, res) => {
    const path = normalize(decodeURIComponent(new URL(req.url, 'http://x').pathname));
    const file = join(root, path);
    if (file !== root && !file.startsWith(root + sep)) {
      res.writeHead(403).end();
      return;
    }
    if (!existsSync(file) || !statSync(file).isFile()) {
      res.writeHead(404).end();
      return;
    }
    res.writeHead(200, { 'content-type': MIME[extname(file)] ?? 'application/octet-stream', 'cache-control': 'no-store' });
    createReadStream(file).pipe(res);
  });
  return new Promise((done, fail) => {
    server.once('error', fail);
    server.listen(0, '127.0.0.1', () => done(server));
  });
}

/** Minimal CDP client over the global WebSocket. */
function connect(url) {
  const socket = new WebSocket(url);
  let next = 0;
  const pending = new Map();
  const waiters = new Map();
  socket.addEventListener('message', ({ data }) => {
    const message = JSON.parse(String(data));
    if (message.id !== undefined) {
      const call = pending.get(message.id);
      pending.delete(message.id);
      if (!call) return;
      if (message.error) call.fail(new Error(`${call.method}: ${message.error.message}`));
      else call.done(message.result);
    } else if (waiters.has(message.method)) {
      const list = waiters.get(message.method);
      waiters.delete(message.method);
      for (const done of list) done(message.params);
    }
  });
  const opened = new Promise((done, fail) => {
    socket.addEventListener('open', done, { once: true });
    socket.addEventListener('error', () => fail(new Error('DevTools socket failed')), { once: true });
  });
  return {
    opened,
    send: (method, params = {}) =>
      new Promise((done, fail) => {
        next += 1;
        pending.set(next, { method, done, fail });
        socket.send(JSON.stringify({ id: next, method, params }));
      }),
    /** Resolves on the next occurrence of `method`; call before triggering it. */
    once: (method) =>
      new Promise((done) => {
        waiters.set(method, [...(waiters.get(method) ?? []), done]);
      }),
    close: () => socket.close(),
  };
}

async function waitForFile(path, ms) {
  for (let waited = 0; waited < ms; waited += 100) {
    if (existsSync(path)) {
      const text = readFileSync(path, 'utf8');
      if (text.includes('\n')) return text;
    }
    await sleepReal(100);
  }
  throw new Error(`Chrome did not write ${path}; is it installed at ${chromePath}?`);
}

async function main() {
  const work = mkdtempSync(join(tmpdir(), 'sweeper-demo-'));
  const framesDir = join(work, 'frames');
  mkdirSync(framesDir);
  let server;
  let chrome;
  let cdp;
  try {
    server = await startServer();
    const origin = `http://127.0.0.1:${server.address().port}`;

    chrome = spawn(chromePath, [
      '--headless=new',
      '--remote-debugging-port=0',
      `--user-data-dir=${join(work, 'profile')}`,
      '--hide-scrollbars',
      '--no-first-run',
      '--no-default-browser-check',
      '--mute-audio',
      '--force-color-profile=srgb',
      '--run-all-compositor-stages-before-draw',
      'about:blank',
    ], { stdio: 'ignore' });
    chrome.once('error', (e) => console.error(`Chrome failed to start: ${e.message}`));

    const active = await waitForFile(join(work, 'profile', 'DevToolsActivePort'), 20000);
    const port = Number(active.split('\n')[0]);
    let targets = [];
    for (let attempt = 0; attempt < 50 && !targets.length; attempt += 1) {
      try {
        targets = (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).filter((t) => t.type === 'page');
      } catch { /* Chrome is still starting */ }
      if (!targets.length) await sleepReal(100);
    }
    if (!targets.length) throw new Error('No page target found');

    cdp = connect(targets[0].webSocketDebuggerUrl);
    await cdp.opened;
    const evaluate = async (expression) =>
      (await cdp.send('Runtime.evaluate', { expression, returnByValue: true })).result.value;

    await cdp.send('Page.enable');
    await cdp.send('Runtime.enable');
    await cdp.send('Emulation.setDeviceMetricsOverride', {
      width: WIDTH, height: HEIGHT, deviceScaleFactor: SCALE, mobile: false,
    });
    await cdp.send('Emulation.setEmulatedMedia', {
      features: [{ name: 'prefers-color-scheme', value: 'light' }, { name: 'prefers-reduced-motion', value: 'no-preference' }],
    });
    // The director waits for this flag, so nothing runs before virtual time is paused.
    await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: 'window.__demoHold = true;' });

    const loaded = cdp.once('Page.loadEventFired');
    await cdp.send('Page.navigate', { url: `${origin}/docs/demo/stage.html` });
    await loaded;
    await cdp.send('Emulation.setVirtualTimePolicy', { policy: 'pause' });
    await evaluate('window.__demoHold = false');

    const slice = 1000 / FPS;
    let frames = 0;
    const capture = async () => {
      const { data } = await cdp.send('Page.captureScreenshot', { format: 'png', fromSurface: true });
      writeFileSync(join(framesDir, `f${String(frames).padStart(5, '0')}.png`), Buffer.from(data, 'base64'));
      frames += 1;
    };

    await capture();
    while (frames < MAX_FRAMES) {
      const expired = cdp.once('Emulation.virtualTimeBudgetExpired');
      await cdp.send('Emulation.setVirtualTimePolicy', { policy: 'advance', budget: slice });
      await expired;
      await capture();
      if (await evaluate('window.__demoDone === true')) break;
      if (frames % FPS === 0) process.stdout.write(`\r${frames / FPS} s recorded`);
    }
    process.stdout.write('\n');

    const error = await evaluate('window.__demoError');
    if (error) throw new Error(error);
    if (!(await evaluate('window.__demoDone === true'))) throw new Error(`Scene did not finish within ${MAX_FRAMES} frames`);
    const length = await evaluate('window.__demoLength');

    cdp.close();
    await stop(chrome);
    chrome = null;

    const temp = join(work, 'demo.gif');
    const filter = `[0:v]scale=${OUT_WIDTH}:-1:flags=lanczos,split[a][b];`
      + '[a]palettegen=stats_mode=diff:max_colors=192[p];'
      + '[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle';
    const result = spawnSync(ffmpegPath, [
      '-y', '-loglevel', 'error',
      '-framerate', String(FPS), '-i', join(framesDir, 'f%05d.png'),
      '-filter_complex', filter,
      '-r', String(FPS), '-loop', '0', temp,
    ], { stdio: ['ignore', 'inherit', 'inherit'] });
    if (result.error) throw new Error(`ffmpeg could not start: ${result.error.message}`);
    if (result.status !== 0) throw new Error(`ffmpeg exited with ${result.status}`);

    renameOrCopy(temp, output);
    const bytes = statSync(output).size;
    console.log(`frames: ${frames} @ ${FPS} fps`);
    console.log(`duration: ${(frames / FPS).toFixed(1)} s (scene timeline ${(length / 1000).toFixed(1)} s)`);
    console.log(`size: ${(bytes / 1024 / 1024).toFixed(2)} MB -> ${output}`);
    if (bytes > TARGET_BYTES) console.warn('warning: GIF is larger than 4 MB; consider fewer colors, a smaller width or a lower fps.');
  } finally {
    try { cdp?.close(); } catch { /* already closed */ }
    if (chrome) await stop(chrome);
    server?.closeAllConnections();
    await new Promise((done) => (server ? server.close(done) : done()));
    rmSync(work, { recursive: true, force: true });
  }
}

/** Moves across devices when rename is not possible (tmp on another volume). */
function renameOrCopy(from, to) {
  try {
    renameSync(from, to);
  } catch {
    writeFileSync(to, readFileSync(from));
  }
}

async function stop(child) {
  if (child.exitCode !== null) return;
  const exited = new Promise((done) => child.once('exit', done));
  child.kill('SIGTERM');
  const timeout = sleepReal(3000).then(() => child.kill('SIGKILL'));
  await Promise.race([exited, timeout]);
}

main().then(
  () => process.exit(0),
  (error) => {
    console.error(`record failed: ${error.message}`);
    process.exit(1);
  },
);
