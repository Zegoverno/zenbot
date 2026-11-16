// The Files view: the owner's home folder, as the zen terminal app shows it, with a few safe writes.
//
// - Paths are absolute or `~/…`, and must stay inside the home folder once symlinks are followed.
// - Secrets are never listed or read: zen's token, settings and database, the engines' sign-ins,
//   ssh, gpg and cloud credentials (the terminal app's list, and more, since a web page is easier
//   to reach than a terminal). Anything under a denied folder is denied too.
// - Reads stop at 2 MB; a binary file says so instead of its bytes.
// - `reader.json` (the file viewer's settings, shared with the terminal app) is read and written
//   here, key by key, so other keys in it survive.
// - Writes are only these: create an empty file, make a folder, rename. The new name is one plain
//   segment (no `/`, `\`, NUL, `.` or `..`, at most 255 bytes) and never a denied one (no new
//   `.env` or `id_rsa`); the parent must be a folder inside home. Nothing is overwritten (a taken
//   name is a 409), a rename stays in its folder, and home, `~/.zenbot` and what is within two
//   levels of it (`~/.zenbot/global`, `~/.zenbot/agents/zenbot`…) can't be renamed.

import { promises as fs } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { HttpError, writeAtomic } from './files.ts';

export const HOME = path.resolve(process.env.ZEN_WEB_HOME ?? os.homedir());
const ZEN = path.join(HOME, '.zenbot');
export const VAULT = path.resolve(process.env.ZEN_WEB_ROOT ?? path.join(ZEN, 'global'));
const MAX = 2 * 1024 * 1024;

const DENIED = [
  '.zenbot/token', '.zenbot/db-password', '.zenbot/env', '.zenbot/env.prev', '.zenbot/mcp.json',
  '.zenbot/matrix.env', '.zenbot/matrix', '.zenbot/mcp-auth', '.zenbot/zen.sqlite3',
  '.zenbot/zen.sqlite3-wal', '.zenbot/zen.sqlite3-shm', '.codex/auth.json', '.claude/.credentials.json',
  '.ssh', '.gnupg', '.aws', '.azure', '.kube', '.docker/config.json', '.config/gh', '.config/gcloud',
  '.netrc', '.git-credentials', '.npmrc', '.pypirc',
].map((p) => path.join(HOME, p));
const DENIED_NAMES = /^(\.env(\..*)?|id_(rsa|ecdsa|ed25519|dsa)(\.pub)?|.*\.(pem|key|p12|pfx))$/i;

export function denied(full: string): boolean {
  if (DENIED_NAMES.test(path.basename(full)) && !path.basename(full).endsWith('.pub')) return true;
  return DENIED.some((d) => full === d || full.startsWith(d + path.sep));
}

const inside = (full: string) => full === HOME || full.startsWith(HOME + path.sep);

/** The real path of `p` (absolute or `~/…`), checked: inside home, not a secret. */
export async function resolve(p: unknown): Promise<string> {
  if (typeof p !== 'string' || !p || p.includes('\0')) throw new HttpError(400, 'bad path');
  const full = path.resolve(HOME, p === '~' ? '.' : p.replace(/^~\//, ''));
  if (!inside(full) || denied(full)) throw new HttpError(403, 'not shown here');
  const real = await fs.realpath(full).catch(() => {
    throw new HttpError(404, 'not found');
  });
  if (!inside(real) || denied(real)) throw new HttpError(403, 'not shown here');
  return real;
}

/** A new entry's name: one plain segment. */
function segment(name: unknown): string {
  if (typeof name !== 'string' || !name || name === '.' || name === '..' || /[/\\\0]/.test(name) || Buffer.byteLength(name) > 255) throw new HttpError(400, 'bad name');
  return name;
}

/** The folder `dir` and the new entry's path in it: the folder resolves, the name is plain and not a secret's. */
async function target(dir: unknown, name: unknown): Promise<string> {
  const seg = segment(name);
  const parent = await resolve(dir);
  const st = await fs.stat(parent);
  if (!st.isDirectory()) throw new HttpError(400, 'not a folder');
  const full = path.join(parent, seg);
  if (denied(full)) throw new HttpError(403, 'not allowed here');
  return full;
}

const taken = (e: unknown) => {
  const code = (e as NodeJS.ErrnoException)?.code;
  if (code === 'EEXIST') return new HttpError(409, 'already exists');
  if (code === 'EACCES' || code === 'EPERM') return new HttpError(403, 'not allowed');
  return e;
};

/** A new empty file `name` in folder `dir`; never over an existing one. */
export async function create(dir: unknown, name: unknown): Promise<{ path: string }> {
  const full = await target(dir, name);
  try {
    await (await fs.open(full, 'wx')).close();
  } catch (e) {
    throw taken(e);
  }
  return { path: full };
}

/** A new folder `name` in folder `dir`. */
export async function mkdir(dir: unknown, name: unknown): Promise<{ path: string }> {
  const full = await target(dir, name);
  try {
    await fs.mkdir(full);
  } catch (e) {
    throw taken(e);
  }
  return { path: full };
}

/** Rename `p` (its last segment only) to `name`; home, `~/.zenbot` and what is two levels in stay put. */
export async function rename(p: unknown, name: unknown): Promise<{ path: string }> {
  const seg = segment(name);
  if (typeof p !== 'string' || !p || p.includes('\0')) throw new HttpError(400, 'bad path');
  const given = path.resolve(HOME, p === '~' ? '.' : p.replace(/^~\//, ''));
  // The entry itself (a symlink is renamed, not what it points at): its folder resolves, the name stays.
  const parent = await resolve(path.dirname(given));
  const from = path.join(parent, path.basename(given));
  const deep = from.startsWith(ZEN + path.sep) ? path.relative(ZEN, from).split(path.sep).length : 0;
  if (from === HOME || from === ZEN || (deep > 0 && deep <= 2)) throw new HttpError(403, 'protected');
  if (denied(from)) throw new HttpError(403, 'not shown here');
  await fs.lstat(from).catch(() => {
    throw new HttpError(404, 'not found');
  });
  const to = path.join(parent, seg);
  if (denied(to)) throw new HttpError(403, 'not allowed here');
  if (to === from) return { path: from };
  if (await fs.lstat(to).then(() => true, () => false)) throw new HttpError(409, 'already exists');
  try {
    await fs.rename(from, to);
  } catch (e) {
    throw taken(e);
  }
  return { path: to };
}

export interface Entry { name: string; path: string; dir: boolean; hidden: boolean; size: number; mtime: number }

/** A folder's entries, folders first, then by name; secrets left out. */
export async function list(p: unknown): Promise<{ path: string; entries: Entry[] }> {
  const dir = await resolve(p ?? HOME);
  const items = await fs.readdir(dir, { withFileTypes: true }).catch(() => {
    throw new HttpError(400, 'not a folder');
  });
  const entries: Entry[] = [];
  for (const e of items.slice(0, 5000)) {
    const full = path.join(dir, e.name);
    if (denied(full)) continue;
    const st = await fs.stat(full).catch(() => null);
    if (!st) continue;
    entries.push({ name: e.name, path: full, dir: st.isDirectory(), hidden: e.name.startsWith('.'), size: st.size, mtime: st.mtimeMs });
  }
  entries.sort((a, b) => Number(b.dir) - Number(a.dir) || a.name.localeCompare(b.name));
  return { path: dir, entries };
}

export interface FileRead { path: string; mtime: number; size: number; text?: string; binary?: boolean; tooBig?: boolean; dir?: boolean; vault?: string | null }

/** A file's text (or that it is binary or too big), with its modification time. */
export async function read(p: unknown): Promise<FileRead> {
  const full = await resolve(p);
  const st = await fs.stat(full);
  if (st.isDirectory()) return { path: full, dir: true, mtime: st.mtimeMs, size: st.size };
  if (st.size > MAX) return { path: full, tooBig: true, mtime: st.mtimeMs, size: st.size };
  const buf = await fs.readFile(full);
  const binary = buf.subarray(0, 8192).includes(0);
  return { path: full, mtime: st.mtimeMs, size: st.size, ...(binary ? { binary: true } : { text: buf.toString('utf8') }), vault: vaultPath(full) };
}

export async function stat(p: unknown) {
  const st = await fs.stat(await resolve(p));
  return { mtime: st.mtimeMs, size: st.size };
}

/** The vault-relative path when `full` is a Markdown note the writer can edit, else null. */
export function vaultPath(full: string): string | null {
  if (!full.startsWith(VAULT + path.sep) || !full.toLowerCase().endsWith('.md')) return null;
  const rel = path.relative(VAULT, full);
  return rel.split(path.sep).some((s) => s.startsWith('.')) ? null : rel.split(path.sep).join('/');
}

/**
 * What a link or a word in the chat points at, as the terminal app finds it: `[[Page]]` by name
 * in the vault; a path tried against the vault, `from`'s folder, ~/.zenbot and home, with `~/`,
 * a `:line` suffix and a missing `.md` understood. Null when nothing is there.
 */
export async function locate(ref: unknown, from?: unknown): Promise<{ path: string; dir: boolean } | null> {
  if (typeof ref !== 'string' || !ref.trim()) return null;
  let r = ref.trim().replace(/^<|>$/g, '');
  const wiki = /^\[\[([^\]|#]+)/.exec(r);
  if (wiki) return findByName(wiki[1].trim());
  try {
    r = decodeURIComponent(r);
  } catch {}
  r = r.replace(/#.*$/, '').replace(/:\d+(:\d+)?$/, '');
  if (!r || /^[a-z]+:\/\//i.test(r)) return null;
  const bases = [typeof from === 'string' && from ? path.dirname(from) : null, VAULT, HOME, ZEN].filter(Boolean) as string[];
  const tries = r.startsWith('~/') || r.startsWith('/') ? [r] : bases.map((b) => path.join(b, r));
  for (const t of tries.flatMap((t) => (path.extname(t) ? [t] : [t, `${t}.md`]))) {
    try {
      const full = await resolve(t);
      const st = await fs.stat(full);
      return { path: full, dir: st.isDirectory() };
    } catch {}
  }
  return null;
}

/** The vault note named `name` (no `.md`, any case), the shallowest first. */
async function findByName(name: string): Promise<{ path: string; dir: boolean } | null> {
  const want = `${name.replace(/\.md$/i, '').toLowerCase()}.md`;
  let level = [VAULT];
  for (let depth = 0; depth < 8 && level.length; depth++) {
    const next: string[] = [];
    for (const dir of level) {
      const items = await fs.readdir(dir, { withFileTypes: true }).catch(() => []);
      for (const e of items) {
        if (e.name.startsWith('.') || e.name === 'node_modules') continue;
        const full = path.join(dir, e.name);
        if (e.isDirectory()) next.push(full);
        else if (e.name.toLowerCase() === want && !denied(full)) return { path: full, dir: false };
      }
    }
    level = next;
  }
  return null;
}

// The file viewer's settings, shared with the terminal app (which ignores keys it doesn't know).
const READER = path.join(ZEN, 'reader.json');
export const READER_DEFAULTS = { wpm: 200, pauses: false, width: 72, spaced: false, bionic: false, focus: 'off', dim: 300 };
const CHECK: Record<string, (v: unknown) => boolean> = {
  wpm: (v) => Number.isInteger(v) && (v as number) >= 100 && (v as number) <= 1000,
  pauses: (v) => typeof v === 'boolean',
  width: (v) => [0, 60, 72, 90].includes(v as number),
  spaced: (v) => typeof v === 'boolean',
  bionic: (v) => typeof v === 'boolean',
  focus: (v) => ['off', 'paragraph', 'line'].includes(v as string),
  dim: (v) => [100, 300, 500, 700, 900].includes(v as number),
};

async function readerFile(): Promise<Record<string, unknown>> {
  try {
    const v = JSON.parse(await fs.readFile(READER, 'utf8'));
    return v && typeof v === 'object' ? v : {};
  } catch {
    return {};
  }
}

export async function reader() {
  const file = await readerFile();
  const out: Record<string, unknown> = { ...READER_DEFAULTS };
  for (const k of Object.keys(CHECK)) if (k in file && CHECK[k](file[k])) out[k] = file[k];
  return out;
}

export async function setReader(change: unknown) {
  if (!change || typeof change !== 'object') throw new HttpError(400, 'bad settings');
  const file = await readerFile();
  for (const [k, v] of Object.entries(change)) {
    if (!(k in CHECK) || !CHECK[k](v)) throw new HttpError(400, `bad setting ${k}`);
    file[k] = v;
  }
  await writeAtomic(READER, JSON.stringify(file, null, 2) + '\n');
  return reader();
}
