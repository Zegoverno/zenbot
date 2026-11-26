// The page's way to the zen kernel: a narrow proxy that adds the owner's token, so the token never
// reaches the browser.
//
// - HTTP: `/api/zen/<route>` goes to the kernel's `/api/<route>` when the method and route are on
//   the list below (what the terminal app and the plugin host use, and nothing else);
//   `/api/zen/health` to `/health`.
// - WebSocket: `/api/zen/sessions/<id>/ws` is relayed byte for byte to the kernel's session
//   socket, with `Authorization` added to the upgrade request (a browser can't set that header on
//   a WebSocket). The upgrade must come from this site (`Origin` matches `Host`): without that
//   check any page could open the socket with the owner's login cookie and drive a session.
// - Answers of 1 KB or more go gzipped when the browser accepts it: a plugin's UI resource is
//   0.5–2 MB of HTML, a phone feels every byte. A UI resource asked with `v` (the plugin's version
//   and change time, set by the page) may stay in the browser's cache: a new install changes `v`.
// - Events: `kernelEvents` opens the kernel's change stream for `events.ts`, which merges it with
//   file changes into the page's `/api/events`.

import net from 'node:net';
import { promises as fs } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import type { IncomingMessage, ServerResponse } from 'node:http';
import type { Duplex } from 'node:stream';
import { promisify } from 'node:util';
import zlib from 'node:zlib';
import { HttpError } from './files.ts';

const ZEN_URL = new URL(process.env.ZEN_URL ?? 'http://127.0.0.1:8100');
/** The kernel's port, which the preview gateway never shows. */
export const KERNEL_PORT = Number(ZEN_URL.port || (ZEN_URL.protocol === 'https:' ? 443 : 80));
const TOKEN_FILE = process.env.ZEN_TOKEN_FILE ?? path.join(os.homedir(), '.zenbot/token');
const token = async () => (await fs.readFile(TOKEN_FILE, 'utf8')).trim();

const ID = '[A-Za-z0-9_-]{1,80}';
const NAME = '[A-Za-z0-9._-]{1,80}';
/** An agent's name, as the kernel takes it (agents.rs): a lowercase letter, then letters, digits, hyphens. */
const AGENT = '[a-z][a-z0-9-]{0,31}';
/** A plugin's name, as the kernel takes it (plugins.rs). */
const PLUGIN = '[a-z0-9-]{2,40}';
/** Method and route (after `/api/`) the page may reach. */
const ROUTES: [string, RegExp][] = [
  ['GET', /^(prefs|ideas|tasks\/meta|prompt-files\/USER\.md)$/],
  ['PATCH', /^prefs$/],
  ['PUT', /^prompt-files\/USER\.md$/],
  ['POST', /^tasks$/],
  ['PATCH', /^tasks\/[^/]{1,2048}$/],
  ['DELETE', /^tasks\/[^/]{1,2048}$/],
  ['POST', /^tasks\/[^/]{1,2048}\/agent$/],
  ['POST', new RegExp(`^sessions/${ID}/stop$`)],
  ['GET', /^engines$/],
  ['POST', /^engines\/(codex|claude|openrouter)\/(login|logout)$/],
  ['PUT', /^engines\/openrouter\/key$/],
  ['GET', /^health$/],
  ['GET', /^(board|sessions|models|version|upgrade|tasks|jobs|jobs\/runs|memory|skills|suggestions|mcp|mcp\/pending)$/],
  ['GET', /^agents$/],
  ['POST', /^(sessions|upgrade|memory\/sleep|search|mcp\/login|agents)$/],
  ['GET', new RegExp(`^sessions/${ID}$`)],
  ['PATCH', new RegExp(`^sessions/${ID}$`)],
  ['POST', new RegExp(`^tasks/${ID}/agent$`)],
  ['POST', new RegExp(`^mcp/login/${ID}$`)],
  ['PATCH', new RegExp(`^jobs/${NAME}$`)],
  ['DELETE', new RegExp(`^jobs/${NAME}$`)],
  ['POST', new RegExp(`^jobs/${NAME}/run$`)],
  ['GET', new RegExp(`^agents/${AGENT}$`)],
  ['PUT', new RegExp(`^agents/${AGENT}$`)],
  ['DELETE', new RegExp(`^agents/${AGENT}$`)],
  // Plugins (ADR 0001, Host API): the list, the marketplace, installs, and each plugin's UI
  // resources and tool calls (the kernel checks the URI and the tool belong to that plugin).
  ['GET', /^plugins(\/marketplace)?$/],
  ['PUT', /^plugins\/defaults$/],
  ['POST', /^plugins\/install$/],
  ['PATCH', new RegExp(`^plugins/${PLUGIN}$`)],
  ['DELETE', new RegExp(`^plugins/${PLUGIN}$`)],
  ['GET', new RegExp(`^plugins/${PLUGIN}/resource$`)],
  ['POST', new RegExp(`^plugins/${PLUGIN}/call$`)],
];

export const allowed = (method: string, route: string) => ROUTES.some(([m, re]) => m === method && re.test(route));

/** Sends `/api/zen/<route>?query` on to the kernel and its answer back. */
export async function proxy(req: IncomingMessage, res: ServerResponse, url: URL) {
  const route = url.pathname.slice('/api/zen/'.length);
  const method = req.method ?? 'GET';
  if (!allowed(method, route)) throw new HttpError(404, 'no such kernel route');
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const c of req) {
    size += (c as Buffer).length;
    if (size > 1024 * 1024) throw new HttpError(413, 'too large');
    chunks.push(c as Buffer);
  }
  const target = new URL(route === 'health' ? '/health' : `/api/${route}`, ZEN_URL);
  target.search = url.search;
  let answer: Response;
  try {
    answer = await fetch(target, {
      method,
      headers: { authorization: `Bearer ${await token()}`, ...(chunks.length ? { 'content-type': 'application/json' } : {}) },
      body: chunks.length ? Buffer.concat(chunks) : undefined,
      signal: AbortSignal.timeout(120_000),
    });
  } catch (e) {
    throw new HttpError(502, `zen kernel unreachable: ${e instanceof Error ? e.message : e}`);
  }
  const versioned = answer.ok && method === 'GET' && RESOURCE.test(route) && url.searchParams.has('v');
  await send(req, res, answer.status, {
    'content-type': answer.headers.get('content-type') ?? 'application/json',
    'cache-control': versioned ? 'private, max-age=604800' : 'no-store',
  }, Buffer.from(await answer.arrayBuffer()));
}

const RESOURCE = new RegExp(`^plugins/${PLUGIN}/resource$`);
const gzip = promisify(zlib.gzip);

/** Writes an answer, gzipped when it is worth it and the browser accepts it. */
export async function send(req: IncomingMessage, res: ServerResponse, status: number, headers: Record<string, string>, body: Buffer) {
  if (body.length >= 1024 && /\bgzip\b/.test(String(req.headers['accept-encoding'] ?? ''))) {
    const z = await gzip(body, { level: 6 });
    res.writeHead(status, { ...headers, 'content-encoding': 'gzip', vary: 'accept-encoding', 'content-length': String(z.length) });
    res.end(z);
    return;
  }
  res.writeHead(status, { ...headers, 'content-length': String(body.length) });
  res.end(body);
}

/** Opens the kernel's change stream (`/api/events`, live.rs), token added; events.ts relays it. */
export async function kernelEvents(signal: AbortSignal): Promise<Response> {
  return fetch(new URL('/api/events', ZEN_URL), { headers: { authorization: `Bearer ${await token()}`, accept: 'text/event-stream' }, signal });
}

const WS_PATH = new RegExp(`^/api/zen/sessions/(${ID})/ws$`);

/** Whether the page asking is this site: its `Origin` names the same host the request came to. */
export function sameOrigin(req: IncomingMessage): boolean {
  const origin = req.headers.origin;
  const host = req.headers['x-forwarded-host'] ?? req.headers.host;
  if (!origin || !host) return false;
  try {
    return new URL(origin).host === String(host).split(',')[0].trim();
  } catch {
    return false;
  }
}

/**
 * Handles an upgrade on the session socket path; returns false for any other path (Vite's hot
 * reload has its own). `authed` says whether the proxy's login header was checked.
 */
export function relay(req: IncomingMessage, socket: Duplex, head: Buffer, authed: boolean): boolean {
  const m = WS_PATH.exec(new URL(req.url ?? '/', 'http://local').pathname);
  if (!m) return false;
  const refuse = (status: string) => {
    socket.end(`HTTP/1.1 ${status}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`);
  };
  if (!authed) return refuse('401 Unauthorized'), true;
  if (!sameOrigin(req)) return refuse('403 Forbidden'), true;
  const key = req.headers['sec-websocket-key'];
  if (typeof key !== 'string' || !/^[A-Za-z0-9+/=]{16,40}$/.test(key)) return refuse('400 Bad Request'), true;
  token().then(
    (t) => {
      const up = net.connect(Number(ZEN_URL.port || 80), ZEN_URL.hostname);
      let connected = false;
      up.on('connect', () => {
        connected = true;
        up.write(
          `GET /api/sessions/${m[1]}/ws HTTP/1.1\r\nHost: ${ZEN_URL.host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n` +
            `Sec-WebSocket-Key: ${key}\r\nSec-WebSocket-Version: 13\r\nAuthorization: Bearer ${t}\r\n\r\n`,
        );
        if (head.length) up.write(head);
        up.pipe(socket);
        socket.pipe(up);
      });
      const close = () => {
        up.destroy();
        socket.destroy();
      };
      up.on('error', () => (connected ? close() : refuse('502 Bad Gateway')));
      socket.on('error', close);
      up.on('close', () => socket.destroy());
      socket.on('close', () => up.destroy());
    },
    () => refuse('502 Bad Gateway'),
  );
  return true;
}
