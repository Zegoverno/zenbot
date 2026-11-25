// The Preview tab's gateway: `/preview/<port>/…` on zen's own address shows the dev server on
// `localhost:<port>` (3000–9999, never zen's own ports), so it works wherever zen does: one port
// (443), no per-port sign-in, nothing a corporate firewall drops.
//
// Dev servers expect to live at `/`. An app started with the base `/preview/<port>/` (Vite:
// `--base /preview/5173/`) gets the path as it is; any other (a static page) gets it without the
// prefix. Which one a port wants is learnt from the first page and kept. HTML pages get
// `/zen-preview.js` first in <head> so they can be pointed at; frame-blocking headers are dropped;
// WebSocket upgrades (hot reload) pass through.
//
// Security, decided by the owner: the previewed app runs on zen's own origin, so its code (its
// packages included) can act as zen's page. The gateway still strips the proxy's login headers
// before forwarding, refuses zen's own ports, and bounds what it rewrites.

import { request, type IncomingMessage, type OutgoingHttpHeaders, type ServerResponse } from 'node:http';
import net from 'node:net';
import type { Duplex } from 'node:stream';
import { gunzipSync } from 'node:zlib';

export const MIN_PORT = 3000;
export const MAX_PORT = 9999;
export const PREFIX = '/preview/';
const MAX_HTML = 20 * 1024 * 1024;
const SCRIPT = '<script src="/zen-preview.js"></script>';

export type Options = { authHeader: string; blocked: number[] };

/** The dev server port for `raw`, or null when it is out of range or blocked. */
export function targetPort(raw: unknown, blocked: number[]): number | null {
  if (typeof raw !== 'string' || !/^\d{4}$/.test(raw)) return null;
  const n = Number(raw);
  return n >= MIN_PORT && n <= MAX_PORT && !blocked.includes(n) ? n : null;
}

/** `/preview/5173/app?x` → the port's text, its prefix and the rest (`/app?x`); null when it isn't one. */
export function previewPath(pathAndQuery: string): { port: string; prefix: string; rest: string } | null {
  const m = /^\/preview\/(\d{1,5})(\/[^]*)?$/.exec(pathAndQuery.split('?')[0]);
  if (!m) return null;
  const prefix = `${PREFIX}${m[1]}`;
  return { port: m[1], prefix, rest: pathAndQuery.slice(prefix.length) || '/' };
}

/** Adds the pointing script as the first thing in <head> (or at the very start). */
export function inject(html: string): string {
  const head = /<head(\s[^>]*)?>/i.exec(html);
  if (head) return html.slice(0, head.index + head[0].length) + SCRIPT + html.slice(head.index + head[0].length);
  const doctype = /^\s*<!doctype[^>]*>/i.exec(html);
  return doctype ? doctype[0] + SCRIPT + html.slice(doctype[0].length) : SCRIPT + html;
}

/** A response header set without what would stop zen from framing the page. */
export function framable(headers: IncomingMessage['headers']): OutgoingHttpHeaders {
  const out: OutgoingHttpHeaders = { ...headers };
  delete out['x-frame-options'];
  // A dev server that says nothing about caching (python's http.server, many static servers) lets the
  // browser keep its pages by heuristic for hours, so a reload shows the old page. Ask it to check
  // with the server every time instead; a server's own cache-control (hashed assets) is kept.
  if (!out['cache-control']) out['cache-control'] = 'no-cache';
  const csp = out['content-security-policy'];
  if (typeof csp === 'string') {
    const kept = csp.split(';').map((d) => d.trim()).filter((d) => d && !/^frame-ancestors\b/i.test(d)).join('; ');
    if (kept) out['content-security-policy'] = kept;
    else delete out['content-security-policy'];
  }
  return out;
}

/** Request headers for the dev server: localhost Host and Origin, no login headers. */
function upstreamHeaders(req: IncomingMessage, port: number, authHeader: string): OutgoingHttpHeaders {
  const h: OutgoingHttpHeaders = { ...req.headers, host: `localhost:${port}` };
  for (const k of Object.keys(h)) if (k === authHeader || k.startsWith('x-exedev-') || k.startsWith('x-forwarded-')) delete h[k];
  if (h.origin) h.origin = `http://localhost:${port}`;
  delete h.referer;
  // The gateway rewrites HTML, so ask for it uncompressed.
  delete h['accept-encoding'];
  return h;
}

/** Per port: whether the app wants the `/preview/<port>` prefix kept (learnt from its first page). */
const keepsPrefix = new Map<number, boolean>();

function page(res: ServerResponse, status: number, text: string) {
  if (res.headersSent) return void res.destroy();
  res.writeHead(status, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' });
  const esc = text.replace(/&/g, '&amp;').replace(/</g, '&lt;');
  res.end(`<!doctype html><html><head>${SCRIPT}<meta charset="utf-8"></head><body style="font:15px system-ui;padding:24px;color:#555">${esc}</body></html>`);
}

/** Serves a `/preview/<port>/…` request (main.ts has checked the login). */
export function handle(req: IncomingMessage, res: ServerResponse, o: Options) {
  // zen's page frames these; nothing else does.
  res.setHeader('x-frame-options', 'SAMEORIGIN');
  const p = previewPath(req.url ?? '/');
  const port = p && targetPort(p.port, o.blocked);
  if (!p || !port) return page(res, 400, `Preview shows dev servers on ports ${MIN_PORT}–${MAX_PORT}, not zen's own.`);
  if (!(req.url ?? '').startsWith(`${p.prefix}/`)) {
    res.writeHead(302, { location: `${p.prefix}/`, 'cache-control': 'no-store' });
    return res.end();
  }
  const known = keepsPrefix.get(port);
  // Unknown yet, and a plain read: try the path as it is, and without the prefix if that's a 404.
  const probe = known === undefined && (req.method === 'GET' || req.method === 'HEAD');
  forward(req, res, port, p.prefix, p.rest, known ?? true, probe, o);
}

function forward(req: IncomingMessage, res: ServerResponse, port: number, prefix: string, rest: string, keep: boolean, probe: boolean, o: Options) {
  const path = keep ? `${prefix}${rest}` : rest;
  const up = request({ host: '127.0.0.1', port, method: req.method, path, headers: upstreamHeaders(req, port, o.authHeader) }, (r) => {
    if (probe && keep && r.statusCode === 404) {
      r.resume();
      return forward(req, res, port, prefix, rest, false, true, o);
    }
    if (probe && (r.statusCode ?? 500) < 400) keepsPrefix.set(port, keep);
    const headers = framable(r.headers);
    // Redirects stay inside the preview.
    if (typeof headers.location === 'string') {
      let loc = headers.location.replace(new RegExp(`^https?://(localhost|127\\.0\\.0\\.1):${port}(?=/|$)`), '') || '/';
      if (loc.startsWith('/') && !loc.startsWith('//') && !loc.startsWith(prefix)) loc = `${prefix}${loc}`;
      headers.location = loc;
    }
    const type = String(r.headers['content-type'] ?? '');
    const encoding = String(r.headers['content-encoding'] ?? 'identity').toLowerCase();
    // Pages it can rewrite safely: HTML in UTF-8 (or no charset), plain or gzip; others pass as they are.
    const html = /^text\/html/i.test(type) && req.method !== 'HEAD' && ['identity', 'gzip'].includes(encoding) && (!/charset=/i.test(type) || /charset="?utf-?8/i.test(type));
    r.on('error', () => res.destroy());
    r.on('aborted', () => res.destroy());
    if (!html) {
      res.writeHead(r.statusCode ?? 502, headers);
      return void r.pipe(res);
    }
    const chunks: Buffer[] = [];
    let size = 0;
    r.on('data', (c: Buffer) => {
      size += c.length;
      if (size <= MAX_HTML) chunks.push(c);
    });
    r.on('end', () => {
      // Outside the request's try: a bad body must answer 502, never take the server down.
      try {
        if (size > MAX_HTML) return page(res, 502, 'The page is too large to preview.');
        let body = Buffer.concat(chunks);
        if (encoding === 'gzip') body = gunzipSync(body);
        const out = Buffer.from(inject(body.toString('utf8')));
        delete headers['content-length'];
        delete headers['content-encoding'];
        delete headers['transfer-encoding'];
        headers['content-length'] = out.length;
        res.writeHead(r.statusCode ?? 200, headers);
        res.end(out);
      } catch (e) {
        page(res, 502, `The dev server's page couldn't be read: ${(e as Error).message}`);
      }
    });
  });
  up.on('error', () => page(res, 502, `Nothing answers on localhost:${port}. Is the dev server running?`));
  // The browser gone: stop the dev server's answer too (long streams would leak).
  res.on('close', () => { if (!res.writableFinished) up.destroy(); });
  if (probe) up.end();
  else req.pipe(up);
}

/** Hot reload: a `/preview/<port>/…` WebSocket, passed through byte for byte. */
export function upgrade(req: IncomingMessage, socket: Duplex, head: Buffer, o: Options) {
  const p = previewPath(req.url ?? '/');
  const port = p && targetPort(p.port, o.blocked);
  if (!p || !port) return void socket.destroy();
  const path = keepsPrefix.get(port) === false ? p.rest : `${p.prefix}${p.rest}`;
  const up = net.connect(port, '127.0.0.1', () => {
    const h = upstreamHeaders(req, port, o.authHeader);
    const lines = [`${req.method} ${path} HTTP/1.1`, ...Object.entries(h).flatMap(([k, v]) => (Array.isArray(v) ? v.map((x) => `${k}: ${x}`) : v === undefined ? [] : [`${k}: ${v}`]))];
    up.write(lines.join('\r\n') + '\r\n\r\n');
    if (head.length) up.write(head);
    up.pipe(socket);
    socket.pipe(up);
  });
  up.on('error', () => socket.destroy());
  socket.on('error', () => up.destroy());
}
