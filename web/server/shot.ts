// Screenshots for Preview pins, taken on this machine: the owner's browser sends only the page,
// the element's selector and the screen size; a headless Chromium here opens the same
// `/preview/<port>/…` page through this server, at that size, and shoots the element. Nothing is
// uploaded from the browser (some networks refuse uploads), and the render is a real browser's.
// It is a fresh load, so state the owner made (an open menu, typed text) isn't in it.
//
// Chromium is the one Playwright installs (`~/.cache/ms-playwright`, the headless shell first), or
// `ZEN_WEB_CHROMIUM`. It's driven over the DevTools protocol with Node's own WebSocket: no packages.
// One shot at a time, each bounded in time; the browser is closed after each.

import { spawn } from 'node:child_process';
import { promises as fs } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const TIMEOUT = 25_000;
const MAX_SIDE = 4000;

/** The Chromium to run: `ZEN_WEB_CHROMIUM`, else Playwright's newest headless shell or Chromium. */
export async function findChromium(env = process.env, home = os.homedir()): Promise<string | null> {
  if (env.ZEN_WEB_CHROMIUM) return env.ZEN_WEB_CHROMIUM;
  const root = env.PLAYWRIGHT_BROWSERS_PATH || path.join(home, '.cache/ms-playwright');
  let names: string[] = [];
  try {
    names = await fs.readdir(root);
  } catch {
    return null;
  }
  const rev = (n: string) => Number(n.split('-').pop()) || 0;
  const pick = async (prefix: string, rel: string[]) => {
    for (const n of names.filter((x) => x.startsWith(prefix)).sort((a, b) => rev(b) - rev(a))) {
      for (const r of rel) {
        const p = path.join(root, n, r);
        try {
          await fs.access(p);
          return p;
        } catch {}
      }
    }
    return null;
  };
  return (
    (await pick('chromium_headless_shell-', ['chrome-headless-shell-linux64/chrome-headless-shell', 'chrome-linux/headless_shell'])) ??
    (await pick('chromium-', ['chrome-linux64/chrome', 'chrome-linux/chrome']))
  );
}

export type ShotRequest = { url: string; selector: string; width: number; height: number };

/** A shot request from the page, checked: a `/preview/` path on this server, a sane size. */
export function readShot(body: unknown): ShotRequest | null {
  const b = body as Record<string, unknown> | null;
  if (!b || typeof b !== 'object') return null;
  const url = typeof b.url === 'string' ? b.url : '';
  if (!/^\/preview\/\d{4}\//.test(url) || url.length > 1000 || /[\u0000-\u001f\s]/.test(url)) return null;
  const size = (v: unknown) => (typeof v === 'number' && Number.isFinite(v) ? Math.min(MAX_SIDE, Math.max(200, Math.round(v))) : 0);
  const width = size(b.width), height = size(b.height);
  if (!width || !height) return null;
  const selector = typeof b.selector === 'string' ? b.selector.slice(0, 400) : '';
  return { url, selector, width, height };
}

let queue: Promise<unknown> = Promise.resolve();

/** Shoots `req` from this server (`origin`, with the login header it requires) into a PNG. */
export function shoot(req: ShotRequest, origin: string, authHeader: string): Promise<Buffer> {
  const run = queue.then(() => shootNow(req, origin, authHeader));
  queue = run.catch(() => {});
  return run;
}

async function shootNow(req: ShotRequest, origin: string, authHeader: string): Promise<Buffer> {
  const chrome = await findChromium();
  if (!chrome) throw new Error('no Chromium on this machine (npx playwright install chromium)');
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'zen-shot-'));
  const proc = spawn(chrome, ['--headless', '--remote-debugging-port=0', `--user-data-dir=${dir}`, '--no-first-run', '--no-default-browser-check', '--disable-gpu', '--hide-scrollbars', '--mute-audio', 'about:blank'], { stdio: ['ignore', 'ignore', 'pipe'] });
  let ws: WebSocket | null = null;
  const timer = setTimeout(() => proc.kill('SIGKILL'), TIMEOUT);
  try {
    const endpoint = await new Promise<string>((ok, fail) => {
      let err = '';
      proc.stderr!.on('data', (d) => {
        err += String(d);
        const m = /DevTools listening on (ws:\/\/\S+)/.exec(err);
        if (m) ok(m[1]);
      });
      proc.on('exit', (code) => fail(new Error(`Chromium exited (${code}): ${err.slice(-300)}`)));
      proc.on('error', fail);
    });
    ws = new WebSocket(endpoint);
    await new Promise((ok, fail) => {
      ws!.onopen = ok;
      ws!.onerror = () => fail(new Error('could not reach Chromium'));
    });
    let id = 0;
    const waiting = new Map<number, { ok: (v: any) => void; fail: (e: Error) => void }>();
    const events: { method: string; sessionId?: string }[] = [];
    let onEvent: (() => void) | null = null;
    ws.onmessage = (m) => {
      const msg = JSON.parse(String(m.data));
      if (msg.id && waiting.has(msg.id)) {
        const w = waiting.get(msg.id)!;
        waiting.delete(msg.id);
        if (msg.error) w.fail(new Error(msg.error.message));
        else w.ok(msg.result);
      } else if (msg.method) {
        events.push(msg);
        onEvent?.();
      }
    };
    const send = (method: string, params: object = {}, sessionId?: string): Promise<any> =>
      new Promise((ok, fail) => {
        const n = ++id;
        waiting.set(n, { ok, fail });
        ws!.send(JSON.stringify({ id: n, method, params, ...(sessionId ? { sessionId } : {}) }));
      });
    const until = (method: string, sessionId: string, ms: number) =>
      new Promise<void>((ok) => {
        const t = setTimeout(ok, ms);
        const check = () => {
          if (events.some((e) => e.method === method && e.sessionId === sessionId)) {
            clearTimeout(t);
            ok();
          }
        };
        onEvent = check;
        check();
      });

    const { targetId } = await send('Target.createTarget', { url: 'about:blank' });
    const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });
    await send('Emulation.setDeviceMetricsOverride', { width: req.width, height: req.height, deviceScaleFactor: 2, mobile: false }, sessionId);
    await send('Network.enable', {}, sessionId);
    // This server requires the proxy's login header; the shot comes from this machine.
    await send('Network.setExtraHTTPHeaders', { headers: { [authHeader]: 'zen-preview-shot' } }, sessionId);
    await send('Page.enable', {}, sessionId);
    await send('Page.navigate', { url: origin + req.url }, sessionId);
    await until('Page.loadEventFired', sessionId, 15_000);
    await new Promise((r) => setTimeout(r, 600));
    const expr = `(() => { let e = null; try { e = ${JSON.stringify(req.selector)} ? document.querySelector(${JSON.stringify(req.selector)}) : null; } catch {} if (!e) return null; e.scrollIntoView({ block: 'center' }); const r = e.getBoundingClientRect(); return r.width && r.height ? { x: r.left + scrollX, y: r.top + scrollY, w: r.width, h: r.height } : null; })()`;
    const { result } = await send('Runtime.evaluate', { expression: expr, returnByValue: true }, sessionId);
    const box = result?.value as { x: number; y: number; w: number; h: number } | null;
    const clip = box ? { x: Math.max(0, box.x), y: Math.max(0, box.y), width: Math.min(box.w, MAX_SIDE), height: Math.min(box.h, MAX_SIDE), scale: 1 } : undefined;
    // The element when it's found, else what the screen shows.
    const shot = await send('Page.captureScreenshot', { format: 'png', ...(clip ? { clip, captureBeyondViewport: true } : {}) }, sessionId);
    return Buffer.from(shot.data, 'base64');
  } finally {
    clearTimeout(timer);
    try {
      ws?.close();
    } catch {}
    proc.kill('SIGKILL');
    await fs.rm(dir, { recursive: true, force: true }).catch(() => {});
  }
}
