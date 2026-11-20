// The Files view: the machine from `/` down, read-only, as the zen terminal app shows it, with a
// few safe writes inside the vault.
//
// - Paths are absolute or `~/…`, checked again once symlinks are followed. Reads reach the whole
//   file system except what holds the system's or other people's secrets (`/proc`, `/sys`, `/dev`,
//   `/run`, `/root`, `/tmp`, `/var`, other homes, `/etc`'s shadow, sudoers, ssh and private keys,
//   the VM's `/exe.dev`); the home folder's own secrets are denied as below.
// - Secrets are never listed or read: zen's token, settings and database, the engines' sign-ins,
//   ssh, gpg and cloud credentials (the terminal app's list, and more, since a web page is easier
//   to reach than a terminal). Anything under a denied folder is denied too.
// - Reads stop at 2 MB; a binary file says so instead of its bytes. Images (not SVG) are served as
//   they are, up to 25 MB, for the viewer to show.
// - Writes are restricted to the canonical vault and are only these: create an empty file, make a folder, rename. The new name is one plain
//   segment (no `/`, `\`, NUL, `.` or `..`, at most 255 bytes) and never a denied one (no new
//   `.env` or `id_rsa`); the parent must be a folder inside home. Nothing is overwritten (a taken
//   name is a 409), a rename stays in its folder, and home, `~/.zenbot` and what is within two
//   levels of it (`~/.zenbot/global`, `~/.zenbot/agents/zenbot`…) can't be renamed.
// - Find (the search field's name filter) walks a folder that resolves as above: no dot entries,
//   no symlinks, no secrets, no generated folders (`node_modules`, `target`…), a bounded number of
//   entries and levels, and the names it returns only.

import { promises as fs } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { HttpError } from './files.ts';

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

const under = (full: string, dir: string) => full === dir || full.startsWith(dir + path.sep);
const inside = (full: string) => under(full, HOME);

/** Outside home: what holds the system's secrets, live processes, or other people's files. */
const SYSTEM = ['/proc', '/sys', '/dev', '/run', '/root', '/tmp', '/var', '/lost+found', '/swapfile', '/exe.dev', '/boot',
  '/etc/shadow', '/etc/shadow-', '/etc/gshadow', '/etc/gshadow-', '/etc/sudoers', '/etc/sudoers.d', '/etc/ssh',
  '/etc/ssl/private', '/etc/security', '/etc/credstore', '/etc/credstore.encrypted'];
const HOMES = path.dirname(HOME);

/** Whether a path outside home may be read: not a system secret, not someone else's home. */
function systemOk(full: string): boolean {
  if (SYSTEM.some((d) => under(full, d))) return false;
  if (under(full, HOMES) && full !== HOMES && !inside(full)) return false;
  return !DENIED_NAMES.test(path.basename(full)) || path.basename(full).endsWith('.pub');
}

/** Readable: inside home and not a secret, or outside it and not a system secret. */
export const readable = (full: string) => (inside(full) ? !denied(full) : systemOk(full));

/** The real path of `p` (absolute or `~/…`), checked: readable before and after symlinks. */
export async function resolve(p: unknown): Promise<string> {
  if (typeof p !== 'string' || !p || p.includes('\0')) throw new HttpError(400, 'bad path');
  const full = path.resolve(HOME, p === '~' ? '.' : p.replace(/^~\//, ''));
  if (!readable(full)) throw new HttpError(403, 'not shown here');
  const real = await fs.realpath(full).catch(() => {
    throw new HttpError(404, 'not found');
  });
  if (!readable(real)) throw new HttpError(403, 'not shown here');
  return real;
}

/** Require an existing path to resolve inside the canonical vault. */
async function inVault(p: unknown): Promise<string> {
  const full = await resolve(p);
  const root = await resolve(VAULT);
  if (full !== root && !full.startsWith(root + path.sep)) throw new HttpError(403, 'outside vault');
  return full;
}

/** A new entry's name: one plain segment. */
function segment(name: unknown): string {
  if (typeof name !== 'string' || !name || name === '.' || name === '..' || /[/\\\0]/.test(name) || Buffer.byteLength(name) > 255) throw new HttpError(400, 'bad name');
  return name;
}

/** The folder `dir` and the new entry's path in it: the folder resolves, the name is plain and not a secret's. */
async function target(dir: unknown, name: unknown): Promise<string> {
  const seg = segment(name);
  const parent = await inVault(dir);
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
  const parent = await inVault(path.dirname(given));
  const from = path.join(parent, path.basename(given));
  const deep = from.startsWith(ZEN + path.sep) ? path.relative(ZEN, from).split(path.sep).length : 0;
  if (from === HOME || from === ZEN || (deep > 0 && deep <= 2)) throw new HttpError(403, 'protected');
  if (denied(from)) throw new HttpError(403, 'not shown here');
  await fs.lstat(from).catch(() => {
    throw new HttpError(404, 'not found');
  });
  // Even renaming a symlink must not operate on an escape or secret.
  await inVault(from);
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
    if (!readable(full)) continue;
    const st = await fs.stat(full).catch(() => null);
    if (!st) continue;
    entries.push({ name: e.name, path: full, dir: st.isDirectory(), hidden: e.name.startsWith('.'), size: st.size, mtime: st.mtimeMs });
  }
  entries.sort((a, b) => Number(b.dir) - Number(a.dir) || a.name.localeCompare(b.name));
  return { path: dir, entries };
}

export interface Found { name: string; path: string; rel: string; dir: boolean }

const FIND_SKIP = new Set(['node_modules', 'target', '__pycache__']);
const FIND_VISIT = 20000;
const FIND_DEPTH = 10;

/** Entries under `root` whose name contains `q` (case-insensitive), shallowest first, at most `limit`. */
export async function find(root: unknown, q: unknown, limit = 100): Promise<{ results: Found[]; truncated: boolean }> {
  const top = await resolve(root ?? HOME);
  const needle = typeof q === 'string' ? q.trim().toLowerCase() : '';
  if (!needle || needle.length > 200 || needle.includes('\0')) return { results: [], truncated: false };
  if (!(await fs.stat(top)).isDirectory()) throw new HttpError(400, 'not a folder');
  const results: Found[] = [];
  let visited = 0;
  let level = [top];
  for (let depth = 0; depth <= FIND_DEPTH && level.length && results.length < limit && visited < FIND_VISIT; depth++) {
    const next: string[] = [];
    for (const dir of level) {
      const items = await fs.readdir(dir, { withFileTypes: true }).catch(() => []);
      for (const e of items.sort((a, b) => a.name.localeCompare(b.name))) {
        if (++visited > FIND_VISIT) return { results, truncated: true };
        if (e.name.startsWith('.') || FIND_SKIP.has(e.name) || e.isSymbolicLink()) continue;
        const full = path.join(dir, e.name);
        if (!readable(full)) continue;
        if (e.isDirectory()) next.push(full);
        else if (!e.isFile()) continue;
        if (e.name.toLowerCase().includes(needle)) {
          if (results.length >= limit) return { results, truncated: true };
          results.push({ name: e.name, path: full, rel: path.relative(top, full).split(path.sep).join('/'), dir: e.isDirectory() });
        }
      }
    }
    level = next;
  }
  return { results, truncated: visited >= FIND_VISIT };
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

/** Image types the viewer shows as pictures (no SVG: it can carry scripts). */
export const IMAGES: Record<string, string> = { '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.gif': 'image/gif', '.webp': 'image/webp', '.avif': 'image/avif', '.bmp': 'image/bmp', '.ico': 'image/x-icon' };
const MAX_IMAGE = 25 * 1024 * 1024;

/** An image's bytes and type, under the same rules as read(); anything else is refused. */
export async function image(p: unknown): Promise<{ type: string; data: Buffer; mtime: number }> {
  const full = await resolve(p);
  const type = IMAGES[path.extname(full).toLowerCase()];
  if (!type) throw new HttpError(415, 'not an image');
  const st = await fs.stat(full);
  if (!st.isFile()) throw new HttpError(400, 'not a file');
  if (st.size > MAX_IMAGE) throw new HttpError(413, 'image too large');
  return { type, data: await fs.readFile(full), mtime: st.mtimeMs };
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

// A bounded, symlink-free scan; no hidden paths or secrets, and only small Markdown files.
async function scanVault(root: string): Promise<{ files: { path: string; text: string }[]; truncated: boolean }> {
  const files: { path: string; text: string }[] = [];
  const dirs = [root];
  let visited = 0, bytes = 0, truncated = false;
  while (dirs.length) {
    const dir = dirs.shift()!;
    const entries = await fs.readdir(dir, { withFileTypes: true }).catch(() => []);
    for (const e of entries.sort((a, b) => a.name.localeCompare(b.name))) {
      if (++visited > 10000) { truncated = true; break; }
      const full = path.join(dir, e.name);
      if (e.name.startsWith('.') || denied(full) || e.isSymbolicLink() || FIND_SKIP.has(e.name)) continue;
      if (e.isDirectory()) { dirs.push(full); continue; }
      if (!e.isFile() || !e.name.toLowerCase().endsWith('.md')) continue;
      const st = await fs.stat(full).catch(() => null);
      if (!st || st.size > 256 * 1024) continue;
      if (files.length >= 2000 || bytes + st.size > 16 * 1024 * 1024) { truncated = true; break; }
      const text = await fs.readFile(full, 'utf8').catch(() => null);
      if (text !== null) { files.push({ path: full, text }); bytes += st.size; }
    }
    if (truncated) break;
  }
  return { files, truncated };
}

/** Facets for Markdown notes beneath a vault directory. Supports scalar and simple YAML lists. */
export async function facets(p: unknown) {
  const root = await inVault(p ?? VAULT);
  if (!(await fs.stat(root)).isDirectory()) throw new HttpError(400, 'not a folder');
  const scan = await scanVault(root);
  const counts: Record<string, Map<string, number>> = { type: new Map(), status: new Map(), tags: new Map() };
  const notes: { path: string; type: string[]; status: string[]; tags: string[] }[] = [];
  for (const file of scan.files) {
    const note = { path: file.path, type: [] as string[], status: [] as string[], tags: [] as string[] };
    notes.push(note);
    const front = /^---\s*\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/.exec(file.text)?.[1];
    if (!front) continue;
    let key = '';
    const seen = new Set<string>();
    for (const line of front.split(/\r?\n/)) {
      const prop = /^(type|status|tags):\s*(.*)$/i.exec(line);
      const item = /^\s+-\s+(.+)$/.exec(line);
      if (prop) key = prop[1].toLowerCase();
      else if (!item) { key = ''; continue; }
      if (!key) continue;
      const raw = prop ? prop[2] : item![1];
      for (let value of raw.replace(/^\[|\]$/g, '').split(',')) {
        value = value.trim().replace(/^['"]|['"]$/g, '').trim();
        if (!value || seen.has(key + '\0' + value)) continue;
        seen.add(key + '\0' + value);
        note[key as 'type' | 'status' | 'tags'].push(value);
        counts[key].set(value, (counts[key].get(value) ?? 0) + 1);
      }
    }
  }
  const values = (key: string) => [...counts[key]].sort(([a], [b]) => a.localeCompare(b)).map(([value, count]) => ({ value, count }));
  return { type: values('type'), status: values('status'), tags: values('tags'), notes, truncated: scan.truncated };
}

/** Notes that contain a wiki link to the requested note, scanned across the vault. */
export async function backlinks(p: unknown) {
  const target = await inVault(p);
  if (!(await fs.stat(target)).isFile() || !target.toLowerCase().endsWith('.md')) throw new HttpError(400, 'not a markdown file');
  const root = await inVault(VAULT);
  const scan = await scanVault(root);
  const rel = path.relative(root, target).split(path.sep).join('/').replace(/\.md$/i, '');
  const name = path.basename(rel).toLowerCase();
  const references = scan.files.filter((file) => file.path !== target && [...file.text.matchAll(/\[\[([^\]|#]+)(?:#[^\]|]*)?(?:\|[^\]]*)?\]\]/g)].some((m) => {
    const link = m[1].trim().replace(/\.md$/i, '').replace(/^\//, '').toLowerCase();
    return link === rel.toLowerCase() || (!link.includes('/') && link === name);
  })).map((file) => ({ path: file.path, title: path.basename(file.path, path.extname(file.path)) }));
  return { references, truncated: scan.truncated };
}
