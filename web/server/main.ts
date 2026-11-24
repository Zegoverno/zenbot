// zen's web server: serves the page, reads and writes notes in one folder (the vault), shows the
// file system (`home.ts`: reads from / down without secrets, plus new files and folders and renames
// in the vault, never over anything),
// relays the zen kernel (`zen.ts`) and its change stream, merged with the vault's file changes
// (`events.ts`, `/api/events`), runs zen's kernel commands (`commands.ts`) and takes chat
// attachments into `~/inbox` (`uploads.ts`), and shows dev servers under `/preview/<port>/` for the
// Preview tab (`preview.ts`), with its pins' screenshots taken here (`shot.ts`) and its list of
// dev servers, which can be stopped and restarted (`servers.ts`).
//
// Security, in layers:
// - It listens on loopback only; a signing-in proxy in front is the way in.
// - Every request must carry the proxy's login header, which the proxy adds only for someone signed
//   in: `ZEN_WEB_AUTH_HEADER`, by default exe.dev's `X-ExeDev-UserID`. `ZEN_WEB_AUTH=off` turns
//   this off for local tests.
// - Every write needs the `X-Zen-Web: 1` header, which a page on another site can't send without
//   a CORS preflight this server never grants: no cross-site writes.
// - Paths stay inside the root: relative, Markdown only, no dot folders, no symlinks out.
// - Files (`/api/fs`, downloads included) reads from `/` down but never secrets (zen's token, env, matrix and
//   database, sign-ins, keys, `/proc`, `/etc/shadow`, other homes: `home.ts`), and writes only
//   inside the vault.
// - Preview's server list (`servers.ts`) lets any signed-in user stop or restart the owner's own dev
//   servers (processes of this server's user on ports 3000–9999). System services (cgroup
//   `/system.slice/`), zen's own ports and this server's own unit are never listed or touched:
//   a port that isn't listed answers 404, and a pid never comes from the client.
// - The kernel token is read here for authenticated relays and never sent to the page.
// - The change stream (`events.ts`) carries only topics and vault-relative paths, and refuses
//   requests another site started (`Sec-Fetch-Site`, `Origin`).
//
// Run with Node's own type stripping: `node server/main.ts` (`--dev` serves the page from Vite;
// `ZEN_WEB_DEV_HOSTS`, comma-separated, are the host names Vite accepts besides localhost).

import { createServer, type IncomingMessage, type ServerResponse } from 'node:http';
import { createReadStream, promises as fs } from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { hash, HttpError, locked, readText, resolvePath as inRoot, writeAtomic } from './files.ts';
import * as zen from './zen.ts';
import * as events from './events.ts';
import * as home from './home.ts';
import * as commands from './commands.ts';
import * as uploads from './uploads.ts';
import * as preview from './preview.ts';
import * as shot from './shot.ts';
import * as servers from './servers.ts';

const HOME = os.homedir();
const ROOT = path.resolve(process.env.ZEN_WEB_ROOT ?? path.join(HOME, '.zenbot/global'));
const PORT = Number(process.env.ZEN_WEB_PORT ?? 4400);
const AUTH = process.env.ZEN_WEB_AUTH !== 'off';
const AUTH_HEADER = (process.env.ZEN_WEB_AUTH_HEADER || 'X-ExeDev-UserID').toLowerCase();
const DEV_HOSTS = (process.env.ZEN_WEB_DEV_HOSTS ?? '').split(',').map((h) => h.trim()).filter(Boolean);
const DEV = process.argv.includes('--dev');
const DIST = path.resolve(import.meta.dirname, '../dist');
const MAX_BODY = 5 * 1024 * 1024;
/** Ports Preview never shows or touches: zen's own. `ZEN_WEB_PREVIEW_SYSTEM=1` (tests) also lists system.slice processes. */
const BLOCKED = [PORT, zen.KERNEL_PORT];
const SERVERS = { blocked: BLOCKED, includeSystem: process.env.ZEN_WEB_PREVIEW_SYSTEM === '1' };
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
  if (method === 'GET' && url.pathname === '/api/events') return events.handle(req, res, ROOT, zen.kernelEvents);
  const q = (k: string) => url.searchParams.get(k);
  switch (`${method} ${url.pathname}`) {
    case 'GET /api/fs/list':
      return send(res, 200, await home.list(q('path')));
    case 'GET /api/fs/read':
      return send(res, 200, await home.read(q('path')));
    case 'GET /api/fs/image': {
      const im = await home.image(q('path'));
      res.writeHead(200, { 'content-type': im.type, 'content-length': im.data.length, 'cache-control': 'private, no-cache', 'content-security-policy': "default-src 'none'; sandbox" });
      return void res.end(im.data);
    }
    case 'GET /api/fs/download': {
      // Any readable file, as an attachment: never rendered here (octet-stream, nosniff, sandboxed),
      // streamed so big files don't sit in memory. The name goes as RFC 5987 UTF-8 plus an ASCII fallback.
      const f = await home.download(q('path'));
      const ascii = f.name.replace(/[^\x20-\x7e]|["\\]/g, '_');
      res.writeHead(200, {
        'content-type': 'application/octet-stream', 'content-length': f.size, 'cache-control': 'private, no-store',
        'content-disposition': `attachment; filename="${ascii}"; filename*=UTF-8''${encodeURIComponent(f.name)}`,
        'x-content-type-options': 'nosniff', 'content-security-policy': "default-src 'none'; sandbox",
      });
      const s = createReadStream(f.path);
      s.on('error', () => res.destroy());
      return void s.pipe(res);
    }
    case 'GET /api/fs/stat':
      return send(res, 200, await home.stat(q('path')));
    case 'GET /api/fs/facets':
      return send(res, 200, await home.facets(q('path')));
    case 'GET /api/fs/backlinks':
      return send(res, 200, await home.backlinks(q('path')));
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
    case 'POST /api/preview/shot': {
      // A Preview pin's screenshot, taken here (shot.ts) and saved to ~/inbox like an attachment.
      const r = shot.readShot(await readBody(req));
      if (!r) throw new HttpError(400, 'bad shot request');
      const png = await shot.shoot(r, `http://127.0.0.1:${PORT}`, AUTH_HEADER).catch((e) => {
        throw new HttpError(502, `screenshot failed: ${(e as Error).message}`);
      });
      const dir = path.join(home.HOME, 'inbox', new Date().toISOString().slice(0, 10));
      await fs.mkdir(dir, { recursive: true });
      const file = path.join(dir, `preview-${Date.now()}-${Math.random().toString(36).slice(2, 6)}.png`);
      await fs.writeFile(file, png, { flag: 'wx' });
      return send(res, 200, { path: file, data: png.toString('base64') });
    }
    case 'GET /api/preview/servers':
      return send(res, 200, { servers: await servers.list(SERVERS) });
    case 'POST /api/preview/servers/stop': {
      const b = (await readBody(req)) as { port?: unknown } | null;
      await servers.stop(b?.port, SERVERS);
      return send(res, 200, { ok: true });
    }
    case 'POST /api/preview/servers/restart': {
      const b = (await readBody(req)) as { port?: unknown } | null;
      return send(res, 200, { server: await servers.restart(b?.port, SERVERS) });
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
  const previewOptions = { authHeader: AUTH_HEADER, blocked: BLOCKED };
  const server = createServer(async (req, res) => {
    for (const [k, v] of Object.entries(SECURITY_HEADERS)) res.setHeader(k, v);
    try {
      if (AUTH && !req.headers[AUTH_HEADER]) throw new HttpError(401, 'sign in first');
      const url = new URL(req.url ?? '/', 'http://local');
      if (url.pathname.startsWith('/api/')) return await api(req, res, url);
      if (url.pathname.startsWith(preview.PREFIX) || url.pathname === preview.PREFIX.slice(0, -1)) return preview.handle(req, res, previewOptions);
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
    if ((req.url ?? '').startsWith(preview.PREFIX)) return authed ? preview.upgrade(req, socket, head, previewOptions) : socket.destroy();
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
