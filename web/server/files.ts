// Reading and writing notes safely: paths kept inside the root, atomic writes, one write at a
// time per note.

import { createHash, randomBytes } from 'node:crypto';
import { promises as fs } from 'node:fs';
import path from 'node:path';

export const hash = (text: string) => createHash('sha256').update(text).digest('hex').slice(0, 16);

export class HttpError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

/** A Markdown path inside the root, or an error. Symlinks that lead out are refused too. */
export async function resolvePath(root: string, rel: unknown): Promise<string> {
  if (typeof rel !== 'string' || !rel || rel.includes('\0') || path.isAbsolute(rel)) throw new HttpError(400, 'bad path');
  const parts = rel.split(/[\\/]/);
  if (parts.some((p) => p === '' || p === '..' || p.startsWith('.'))) throw new HttpError(400, 'bad path');
  if (!rel.toLowerCase().endsWith('.md')) throw new HttpError(400, 'only Markdown files');
  const full = path.resolve(root, rel);
  if (!full.startsWith(root + path.sep)) throw new HttpError(400, 'bad path');
  const realRoot = await fs.realpath(root);
  const inside = (real: string) => real === realRoot || real.startsWith(realRoot + path.sep);
  // The nearest folder that exists must be inside the root once symlinks are followed.
  for (let dir = path.dirname(full); ; dir = path.dirname(dir)) {
    const real = await fs.realpath(dir).catch(() => null);
    if (real === null) continue;
    if (!inside(real)) throw new HttpError(400, 'bad path');
    break;
  }
  const real = await fs.realpath(full).catch(() => null);
  if (real !== null && !inside(real)) throw new HttpError(400, 'bad path');
  return full;
}

export async function readText(full: string): Promise<string | null> {
  try {
    return await fs.readFile(full, 'utf8');
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw e;
  }
}

/** Writes through a temporary file and a rename, so a crash never leaves half a note; keeps its mode. */
export async function writeAtomic(full: string, text: string) {
  await fs.mkdir(path.dirname(full), { recursive: true });
  const tmp = path.join(path.dirname(full), `.${path.basename(full)}.${randomBytes(4).toString('hex')}.tmp`);
  const mode = await fs.stat(full).then((s) => s.mode & 0o777, () => 0o644);
  await fs.writeFile(tmp, text, { mode });
  await fs.rename(tmp, full);
}

const queues = new Map<string, Promise<unknown>>();

/** Runs `f` once every earlier call for the same key is done: one read-check-write at a time per note. */
export function locked<T>(key: string, f: () => Promise<T>): Promise<T> {
  const run = (queues.get(key) ?? Promise.resolve()).then(f, f);
  const done = run.catch(() => {});
  queues.set(key, done);
  done.then(() => queues.get(key) === done && queues.delete(key));
  return run;
}

