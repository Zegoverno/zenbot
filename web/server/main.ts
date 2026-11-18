// zen's web server: serves the page, reads and writes notes in one folder (the vault), shows the
// home folder (`home.ts`: reads, plus new files and folders and renames, never over anything),
// relays the zen kernel (`zen.ts`), runs zen's kernel commands (`commands.ts`) and takes chat
// attachments into `~/inbox` (`uploads.ts`).
//
// Security, in layers:
// - It listens on loopback only; a signing-in proxy in front is the way in.
// - Every request must carry the proxy's login header, which the proxy adds only for someone signed
//   in: `ZEN_WEB_AUTH_HEADER`, by default exe.dev's `X-ExeDev-UserID`. `ZEN_WEB_AUTH=off` turns
//   this off for local tests.
// - Every write needs the `X-Zen-Web: 1` header, which a page on another site can't send without
//   a CORS preflight this server never grants: no cross-site writes.
// - Paths stay inside the root: relative, Markdown only, no dot folders, no symlinks out.
// - The kernel token is read here for authenticated relays and never sent to the page.
//
// Run with Node's own type stripping: `node server/main.ts` (`--dev` serves the page from Vite;
// `ZEN_WEB_DEV_HOSTS`, comma-separated, are the host names Vite accepts besides localhost).

import { createServer, type IncomingMessage, type ServerResponse } from 'node:http';
import { promises as fs } from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { hash, HttpError, locked, readText, resolvePath as inRoot, writeAtomic } from './files.ts';
import * as zen from './zen.ts';
import * as home from './home.ts';
import * as commands from './commands.ts';
import * as uploads from './uploads.ts';

const HOME = os.homedir();
const ROOT = path.resolve(process.env.ZEN_WEB_ROOT ?? path.join(HOME, '.zenbot/global'));
const PORT = Number(process.env.ZEN_WEB_PORT ?? 4400);
const AUTH = process.env.ZEN_WEB_AUTH !== 'off';
const AUTH_HEADER = (process.env.ZEN_WEB_AUTH_HEADER || 'X-ExeDev-UserID').toLowerCase();
const DEV_HOSTS = (process.env.ZEN_WEB_DEV_HOSTS ?? '').split(',').map((h) => h.trim()).filter(Boolean);
const DEV = process.argv.includes('--dev');
const DIST = path.resolve(import.meta.dirname, '../dist');
const MAX_BODY = 5 * 1024 * 1024;
const resolvePath = (rel: unknown) => inRoot(ROOT, rel);

async function readBody(req: IncomingMessage): Promise<unknown> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const c of req) {
    size += (c as Buffer).length;
    if (size > MAX_BODY) throw new HttpError(413, 'too large');
    chunks.push(c as Buffer);
  }
  try {
    return JSON.parse(Buffer.concat(chunks).toString('utf8'));
  } catch {
    throw new HttpError(400, 'bad json');
  }
}

function send(res: ServerResponse, status: number, body: unknown) {
  const data = JSON.stringify(body);
  res.writeHead(status, { 'content-type': 'application/json; charset=utf-8', 'cache-control': 'no-store' });
  res.end(data);
}

async function api(req: IncomingMessage, res: ServerResponse, url: URL) {
  const method = req.method ?? 'GET';
  if (method !== 'GET' && req.headers['x-zen-web'] !== '1') throw new HttpError(403, 'missing X-Zen-Web header');
  if (url.pathname.startsWith('/api/zen/')) return zen.proxy(req, res, url);
  const q = (k: string) => url.searchParams.get(k);
  switch (`${method} ${url.pathname}`) {
    case 'GET /api/fs/list':
      return send(res, 200, await home.list(q('path')));
    case 'GET /api/fs/read':
      return send(res, 200, await home.read(q('path')));
    case 'GET /api/fs/stat':
      return send(res, 200, await home.stat(q('path')));
    case 'GET /api/fs/find':
      return send(res, 200, await home.find(q('root'), q('q')));
    case 'GET /api/fs/locate':
      return send(res, 200, { found: await home.locate(q('ref'), q('from')) });
    case 'GET /api/fs/home':
      return send(res, 200, { home: home.HOME, vault: home.VAULT });
    case 'POST /api/fs/create':
    case 'POST /api/fs/mkdir':
    case 'POST /api/fs/rename': {
      const b = (await readBody(req)) as { dir?: unknown; path?: unknown; name?: unknown } | null;
      if (!b || typeof b !== 'object') throw new HttpError(400, 'bad json');
      const act = url.pathname.slice('/api/fs/'.length);
      return send(res, 200, act === 'rename' ? await home.rename(b.path, b.name) : await home[act as 'create' | 'mkdir'](b.dir, b.name));
    }
    case 'POST /api/upload':
      return send(res, 200, await uploads.save(path.join(home.HOME, 'inbox'), q('name'), req));
    case 'GET /api/commands':
      return send(res, 200, { commands: await commands.list() });
    case 'POST /api/command': {
      const b = (await readBody(req)) as { name?: unknown; args?: unknown };
      return send(res, 200, await commands.run(b.name, b.args));
    }
    case 'GET /api/file': {
      const rel = url.searchParams.get('path');
      const text = await readText(await resolvePath(rel));
      if (text === null) return send(res, 404, { error: 'not found' });
      return send(res, 200, { path: rel, text, hash: hash(text) });
    }
    case 'GET /api/exists': {
      const text = await readText(await resolvePath(url.searchParams.get('path')));
      return send(res, 200, { exists: text !== null });
    }
    case 'PUT /api/file': {
      const b = (await readBody(req)) as { path?: unknown; text?: unknown; base?: unknown };
      const text = b.text;
      if (typeof text !== 'string') throw new HttpError(400, 'text missing');
      const full = await resolvePath(b.path);
      return locked(full, async () => {
        const current = await readText(full);
        // `base` is the hash the page last read or wrote; null creates. Anything else moved on.
        if ((current === null) !== (b.base === null) || (current !== null && hash(current) !== b.base)) {
          return send(res, 409, { error: 'changed on disk', text: current, hash: current === null ? null : hash(current) });
        }
        await writeAtomic(full, text);
        send(res, 200, { hash: hash(text) });
      });
    }
    default:
      throw new HttpError(404, 'no such route');
  }
}

const MIME: Record<string, string> = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.css': 'text/css',
  '.svg': 'image/svg+xml', '.woff2': 'font/woff2', '.png': 'image/png', '.ico': 'image/x-icon',
};

async function serveStatic(res: ServerResponse, pathname: string) {
  let file = path.join(DIST, 'index.html');
  try {
    const want = path.resolve(DIST, '.' + decodeURIComponent(pathname));
    if (want.startsWith(DIST + path.sep)) file = want;
  } catch {} // a malformed escape gets the page
  try {
    if ((await fs.stat(file)).isDirectory()) file = path.join(file, 'index.html');
  } catch {
    file = path.join(DIST, 'index.html');
  }
  const data = await fs.readFile(file);
  const immutable = file.includes(`${path.sep}assets${path.sep}`);
  res.writeHead(200, {
    'content-type': MIME[path.extname(file)] ?? 'application/octet-stream',
    'cache-control': immutable ? 'public, max-age=31536000, immutable' : 'no-cache',
  });
  res.end(data);
}

const SECURITY_HEADERS = {
  'x-content-type-options': 'nosniff',
  'referrer-policy': 'no-referrer',
  'x-frame-options': 'DENY',
};

async function main() {
  await fs.mkdir(ROOT, { recursive: true });
  let vite: { middlewares: (req: IncomingMessage, res: ServerResponse, next: () => void) => void } | null = null;
  const server = createServer(async (req, res) => {
    for (const [k, v] of Object.entries(SECURITY_HEADERS)) res.setHeader(k, v);
    try {
      if (AUTH && !req.headers[AUTH_HEADER]) throw new HttpError(401, 'sign in first');
      const url = new URL(req.url ?? '/', 'http://local');
      if (url.pathname.startsWith('/api/')) return await api(req, res, url);
      if (vite) return vite.middlewares(req, res, () => send(res, 404, { error: 'not found' }));
      return await serveStatic(res, url.pathname);
    } catch (e) {
      const tagged = (e as { status?: unknown }).status;
      const status = e instanceof HttpError ? e.status : typeof tagged === 'number' ? tagged : 500;
      if (status === 500) console.error(e);
      if (!res.headersSent) send(res, status, { error: e instanceof Error ? e.message : String(e) });
    }
  });
  // The session sockets, relayed to the kernel; Vite's hot reload handles its own path.
  server.on('upgrade', (req, socket, head) => {
    const authed = !AUTH || Boolean(req.headers[AUTH_HEADER]);
    if (!zen.relay(req, socket, head, authed) && !DEV) socket.destroy();
  });
  if (DEV) {
    // Hot reload rides on the same server and port, so it works through the proxy too.
    const { createServer: createVite } = await import('vite');
    vite = await createVite({ server: { middlewareMode: true, hmr: { server }, allowedHosts: [...DEV_HOSTS, 'localhost', '127.0.0.1'] }, appType: 'spa' });
  }
  server.listen(PORT, '127.0.0.1', () => console.log(`zen web on http://127.0.0.1:${PORT} (root ${ROOT}${DEV ? ', dev' : ''}${AUTH ? '' : ', auth OFF'})`));
}

main();
