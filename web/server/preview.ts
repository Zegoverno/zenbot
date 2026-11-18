// The preview gateway: one port that shows any dev server on this machine in zen's Preview tab.
//
// Why: exe.dev's proxy signs the owner in per port, and a frame can't do that sign-in. Every dev
// server's port would need a visit in a tab first. Behind one gateway port there is one sign-in,
// and the Preview tab can do it with a quick top-level bounce (`/__zen/back`).
//
// What it does, on loopback only (`ZEN_WEB_PREVIEW_PORT`, default 4490):
// - `/__zen/open?port=N&path=/p` picks the dev server (`localhost:N`, 3000–9999, never zen's own
//   ports) in a cookie and redirects to the path; every other request goes to that server,
//   WebSocket upgrades (hot reload) included, with Host and Origin rewritten to localhost:N so dev
//   servers accept them.
// - HTML pages get `/__zen/preview.js` (public/zen-preview.js) as the first script in <head>, so
//   any app can be pointed at; frame-blocking headers are dropped from responses.
// - `/__zen/ping` answers zen's page (CORS, with credentials) so it knows the owner is signed in
//   here; `/__zen/back?to=/path` sends a top-level visit back to zen after that sign-in.
//
// Security: it is another origin than zen's page (another port), so a framed app can't reach
// zen's page or storage. It requires the proxy's login header like main.ts, and it strips that
// header (and exe.dev's email) before forwarding: zen's own web server behind it, on any port,
// sees an anonymous request and refuses it. Its own port, zen web's and the kernel's are refused
// as targets.

import { createServer, request, type IncomingMessage, type OutgoingHttpHeaders, type ServerResponse } from 'node:http';
import net from 'node:net';
import { readFile } from 'node:fs/promises';
import { gunzipSync } from 'node:zlib';

export const MIN_PORT = 3000;
export const MAX_PORT = 9999;
const COOKIE = 'zen_preview_target';
const MAX_HTML = 20 * 1024 * 1024;
const SCRIPT = '<script src="/__zen/preview.js"></script>';

export type Options = {
  port: number;
  auth: boolean;
  authHeader: string;
  /** Ports never shown: zen web's, the kernel's (the gateway's own is added). */
  blocked: number[];
  /** Where zen-preview.js is read from. */
  script: string;
};

/** The dev server port for `raw`, or null when it is out of range or blocked. */
export function targetPort(raw: unknown, blocked: number[]): number | null {
  if (typeof raw !== 'string' || !/^\d{4}$/.test(raw)) return null;
  const n = Number(raw);
  return n >= MIN_PORT && n <= MAX_PORT && !blocked.includes(n) ? n : null;
}

/** A same-site path (`/x?y`), never another host (`//evil`, `/\evil`), or `/`. */
export function localPath(raw: unknown): string {
  return typeof raw === 'string' && raw.startsWith('/') && !/^\/[/\\]/.test(raw) && !/[\u0000-\u001f]/.test(raw) ? raw : '/';
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
  const csp = out['content-security-policy'];
  if (typeof csp === 'string') {
    const kept = csp.split(';').map((d) => d.trim()).filter((d) => d && !/^frame-ancestors\b/i.test(d)).join('; ');
    if (kept) out['content-security-policy'] = kept;
    else delete out['content-security-policy'];
  }
  return out;
}

const cookieOf = (req: IncomingMessage, name: string) =>
  (req.headers.cookie ?? '').split(';').map((c) => c.trim().split('=')).find(([k]) => k === name)?.[1];

/** Request headers for the dev server: localhost Host and Origin, no login headers. */
function upstreamHeaders(req: IncomingMessage, port: number, authHeader: string): OutgoingHttpHeaders {
  const h: OutgoingHttpHeaders = { ...req.headers, host: `localhost:${port}` };
  for (const k of Object.keys(h)) if (k === authHeader || k.startsWith('x-exedev-') || k.startsWith('x-forwarded-')) delete h[k];
  if (h.origin) h.origin = `http://localhost:${port}`;
  if (typeof h.referer === 'string') {
    try {
      const r = new URL(h.referer);
      h.referer = `http://localhost:${port}${r.pathname}${r.search}`;
    } catch {
      delete h.referer;
    }
  }
  // The gateway rewrites HTML, so ask for it uncompressed.
  delete h['accept-encoding'];
  return h;
}

/** zen's page on this host (another port, or none) may ask whether the owner is signed in here. */
function zenOrigin(req: IncomingMessage, port: number): string | null {
  const o = req.headers.origin;
  if (!o) return null;
  try {
    const u = new URL(o);
    const host = (req.headers['x-forwarded-host'] as string | undefined) ?? req.headers.host ?? '';
    const hostname = host.replace(/:\d+$/, '').replace(/^\[|\]$/g, '');
    return u.hostname === hostname && u.port !== String(port) ? u.origin : null;
  } catch {
    return null;
  }
}

function page(res: ServerResponse, status: number, text: string) {
  res.writeHead(status, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' });
  const esc = text.replace(/&/g, '&amp;').replace(/</g, '&lt;');
  res.end(`<!doctype html><html><head>${SCRIPT}<meta charset="utf-8"></head><body style="font:15px system-ui;padding:24px;color:#555">${esc}</body></html>`);
}

export function start(o: Options) {
  const blocked = [...o.blocked, o.port];
  let script: Promise<Buffer> | null = null;

  const server = createServer(async (req, res) => {
    res.setHeader('x-content-type-options', 'nosniff');
    try {
      if (o.auth && !req.headers[o.authHeader]) return page(res, 401, 'Sign in first.');
      const url = new URL(req.url ?? '/', 'http://local');
      if (url.pathname === '/__zen/ping') {
        const zen = zenOrigin(req, o.port);
        if (zen) {
          res.setHeader('access-control-allow-origin', zen);
          res.setHeader('access-control-allow-credentials', 'true');
          res.setHeader('vary', 'origin');
        }
        res.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' });
        return res.end('{"ok":true}');
      }
      if (url.pathname === '/__zen/back') {
        // After the proxy's sign-in on this port: back to zen's page on this host.
        const proto = (req.headers['x-forwarded-proto'] as string | undefined) === 'https' ? 'https' : 'http';
        const host = ((req.headers['x-forwarded-host'] as string | undefined) ?? req.headers.host ?? '').replace(/:\d+$/, '');
        const zenPort = targetPort(url.searchParams.get('port') ?? '', []) ?? null;
        res.writeHead(302, { location: `${proto}://${host}${zenPort ? `:${zenPort}` : ''}${localPath(url.searchParams.get('to'))}`, 'cache-control': 'no-store' });
        return res.end();
      }
      if (url.pathname === '/__zen/preview.js') {
        script ??= readFile(o.script);
        res.writeHead(200, { 'content-type': 'text/javascript; charset=utf-8', 'cache-control': 'no-cache' });
        return res.end(await script);
      }
      if (url.pathname === '/__zen/open') {
        const port = targetPort(url.searchParams.get('port'), blocked);
        if (!port) return page(res, 400, `Preview shows dev servers on ports ${MIN_PORT}–${MAX_PORT}, not zen's own.`);
        const secure = req.headers['x-forwarded-proto'] === 'https' ? '; Secure' : '';
        res.writeHead(302, { location: localPath(url.searchParams.get('path')), 'set-cookie': `${COOKIE}=${port}; Path=/; HttpOnly; SameSite=Lax${secure}`, 'cache-control': 'no-store' });
        return res.end();
      }
      const port = targetPort(cookieOf(req, COOKIE), blocked);
      if (!port) return page(res, 400, 'No dev server chosen: pick a port in zen\'s Preview tab.');
      forward(req, res, port);
    } catch (e) {
      if (!res.headersSent) page(res, 500, (e as Error).message);
    }
  });

  function forward(req: IncomingMessage, res: ServerResponse, port: number) {
    const up = request({ host: '127.0.0.1', port, method: req.method, path: req.url, headers: upstreamHeaders(req, port, o.authHeader) }, (r) => {
      const headers = framable(r.headers);
      // Redirects to the dev server's own address stay on the gateway.
      if (typeof headers.location === 'string') headers.location = headers.location.replace(new RegExp(`^https?://(localhost|127\\.0\\.0\\.1):${port}(?=/|$)`), '') || '/';
      const html = /^text\/html/i.test(String(r.headers['content-type'] ?? '')) && req.method !== 'HEAD';
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
        if (size > MAX_HTML) return page(res, 502, 'The page is too large to preview.');
        let body = Buffer.concat(chunks);
        if (/gzip/i.test(String(r.headers['content-encoding'] ?? ''))) body = gunzipSync(body);
        const out = Buffer.from(inject(body.toString('utf8')));
        delete headers['content-length'];
        delete headers['content-encoding'];
        delete headers['transfer-encoding'];
        headers['content-length'] = out.length;
        res.writeHead(r.statusCode ?? 200, headers);
        res.end(out);
      });
      r.on('error', () => res.destroy());
    });
    up.on('error', () => page(res, 502, `Nothing answers on localhost:${port}. Is the dev server running?`));
    req.pipe(up);
  }

  // Hot reload: the dev server's WebSocket, passed through byte for byte.
  server.on('upgrade', (req, socket, head) => {
    const port = targetPort(cookieOf(req, COOKIE), blocked);
    if ((o.auth && !req.headers[o.authHeader]) || !port) return socket.destroy();
    const up = net.connect(port, '127.0.0.1', () => {
      const h = upstreamHeaders(req, port, o.authHeader);
      delete h['accept-encoding'];
      const lines = [`${req.method} ${req.url} HTTP/1.1`, ...Object.entries(h).flatMap(([k, v]) => (Array.isArray(v) ? v.map((x) => `${k}: ${x}`) : v === undefined ? [] : [`${k}: ${v}`]))];
      up.write(lines.join('\r\n') + '\r\n\r\n');
      if (head.length) up.write(head);
      up.pipe(socket);
      socket.pipe(up);
    });
    up.on('error', () => socket.destroy());
    socket.on('error', () => up.destroy());
  });

  server.listen(o.port, '127.0.0.1', () => console.log(`zen preview gateway on http://127.0.0.1:${o.port}`));
  server.on('error', (e) => console.error(`preview gateway: ${(e as Error).message}`));
  return server;
}
