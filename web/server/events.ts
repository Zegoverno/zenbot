// The page's one change stream, `GET /api/events` (Server-Sent Events): the kernel's `/api/events`
// (live.rs: `{topic, id?}` when sessions, agents, jobs, tasks, plugins, memory… change), relayed with
// the owner's token added here, merged with `files` events from this server when notes under the
// served root change (`{topic: "files", paths}`, paths relative to the root, debounced).
//
// - The token never reaches the page; the page sees only topics.
// - Cross-site pages can't read it: a request another site started (`Sec-Fetch-Site` other than
//   same-origin or none, or an `Origin` that isn't this host) is refused.
// - When the kernel is down the stream stays open (files events, keep-alives) and reconnects to it
//   every few seconds, then sends `resync` so the page refetches what it shows.
// - One recursive watcher on the root for all pages, started with the first and closed with the
//   last; dot folders (`.git`, `.obsidian`) are ignored.

import { watch, type FSWatcher } from 'node:fs';
import type { IncomingMessage, ServerResponse } from 'node:http';
import { HttpError } from './files.ts';
import { sameOrigin } from './zen.ts';

type Send = (ev: Record<string, unknown>) => void;

const KEEPALIVE_MS = 15_000;
const RETRY_MS = 5_000;
const FILES_DEBOUNCE_MS = 300;

// ---- the files watcher, shared ----

const listeners = new Set<Send>();
let watcher: FSWatcher | null = null;
let pending = new Set<string>();
let timer: ReturnType<typeof setTimeout> | null = null;

function startWatch(root: string) {
  if (watcher) return;
  try {
    watcher = watch(root, { recursive: true }, (_kind, name) => {
      const rel = typeof name === 'string' ? name : '';
      if (rel.split('/').some((s) => s.startsWith('.'))) return;
      pending.add(rel);
      timer ??= setTimeout(() => {
        const paths = [...pending].filter(Boolean).slice(0, 50);
        pending = new Set();
        timer = null;
        for (const l of listeners) l({ topic: 'files', paths });
      }, FILES_DEBOUNCE_MS);
    });
    watcher.on('error', () => stopWatch());
  } catch {
    watcher = null; // no watcher: the page's slow poll still catches changes
  }
}

function stopWatch() {
  watcher?.close();
  watcher = null;
  if (timer) clearTimeout(timer);
  timer = null;
  pending = new Set();
}

/** Whether a request was started by another site (it may not read the stream). */
export function crossSite(req: IncomingMessage): boolean {
  const site = req.headers['sec-fetch-site'];
  if (typeof site === 'string' && site !== 'same-origin' && site !== 'none') return true;
  return Boolean(req.headers.origin) && !sameOrigin(req);
}

/**
 * Serve the merged stream. `kernel(signal)` opens the kernel's `/api/events` (token added by the
 * caller, zen.ts); `root` is the folder whose changes are `files` events.
 */
export function handle(req: IncomingMessage, res: ServerResponse, root: string, kernel: (signal: AbortSignal) => Promise<Response>) {
  if (crossSite(req)) throw new HttpError(403, 'cross-site');
  res.writeHead(200, { 'content-type': 'text/event-stream; charset=utf-8', 'cache-control': 'no-store', 'x-accel-buffering': 'no', connection: 'keep-alive' });
  res.write('retry: 3000\n\n');
  let closed = false;
  const ac = new AbortController();
  const send: Send = (ev) => { if (!closed) res.write(`data: ${JSON.stringify(ev)}\n\n`); };
  const ping = setInterval(() => !closed && res.write(': ping\n\n'), KEEPALIVE_MS);
  listeners.add(send);
  startWatch(root);
  const close = () => {
    if (closed) return;
    closed = true;
    clearInterval(ping);
    ac.abort();
    listeners.delete(send);
    if (!listeners.size) stopWatch();
  };
  req.on('close', close);
  res.on('close', close);
  res.on('error', close);
  send({ topic: 'hello' });

  void (async () => {
    let down = false;
    while (!closed) {
      try {
        const r = await kernel(ac.signal);
        if (!r.ok || !r.body) throw new Error(`HTTP ${r.status}`);
        if (down) send({ topic: 'resync' }); // events were missed while it was away
        down = false;
        const dec = new TextDecoder();
        let buf = '';
        for await (const chunk of r.body as unknown as AsyncIterable<Uint8Array>) {
          buf += dec.decode(chunk, { stream: true });
          let i: number;
          while ((i = buf.indexOf('\n\n')) >= 0) {
            const block = buf.slice(0, i);
            buf = buf.slice(i + 2);
            // Only `data:` lines, re-checked as JSON, go on: nothing else from upstream reaches the page.
            for (const line of block.split('\n')) {
              if (!line.startsWith('data:')) continue;
              try {
                const ev = JSON.parse(line.slice(5).trim());
                if (ev && typeof ev.topic === 'string' && ev.topic !== 'hello') send({ topic: ev.topic, ...(typeof ev.id === 'string' ? { id: ev.id } : {}) });
              } catch {}
            }
          }
        }
      } catch {}
      if (closed) break;
      down = true;
      await new Promise((r) => setTimeout(r, RETRY_MS));
    }
  })();
}
