// Chat attachments: a file sent from the page lands in `~/inbox/<YYYY-MM-DD>/`, where zenbot reads it
// with its own tools; the chat then names the path in the prompt.
//
// Safety: the name is cut to its last segment and to plain characters (no dot files, no `..`, no
// separators), the file is opened exclusively (never overwrites; a clash gets `-2`, `-3`…), the
// day folder must stay inside the inbox once symlinks are followed, and the body streams to disk
// under a size cap, the partial file removed when anything fails. The route is a write, so the
// server's `X-Zen-Web` and login checks apply before it is reached.

import { promises as fs } from 'node:fs';
import type { IncomingMessage } from 'node:http';
import path from 'node:path';
import { HttpError } from './files.ts';

export const MAX_UPLOAD = 1024 * 1024 * 1024; // 1 GiB: a long screen recording fits

/** A safe file name from what the browser sent, or null when nothing usable is left. */
export function cleanName(raw: unknown): string | null {
  if (typeof raw !== 'string') return null;
  let name = raw.split(/[\\/]/).pop() ?? '';
  name = name.normalize('NFC').replace(/[\u0000-\u001f\u007f]/g, '').replace(/[^\p{L}\p{N} ._()+-]/gu, '_').trim();
  name = name.replace(/^[.\s]+/, '').replace(/\s+/g, ' ');
  if (!name || name === '.' || name === '..') return null;
  if (name.length > 120) {
    const ext = path.extname(name).slice(0, 16);
    name = name.slice(0, 120 - ext.length) + ext;
  }
  return name;
}

const day = (d = new Date()) => d.toISOString().slice(0, 10);

/** Stream the request body into the inbox; returns the absolute path and size written. */
export async function save(inbox: string, rawName: unknown, req: IncomingMessage, max = MAX_UPLOAD): Promise<{ path: string; size: number }> {
  const name = cleanName(rawName);
  if (!name) throw new HttpError(400, 'bad file name');
  const declared = Number(req.headers['content-length'] ?? NaN);
  if (Number.isFinite(declared) && declared > max) throw new HttpError(413, 'file too large');
  await fs.mkdir(inbox, { recursive: true });
  const dir = path.join(inbox, day());
  await fs.mkdir(dir, { recursive: true });
  const realInbox = await fs.realpath(inbox);
  const realDir = await fs.realpath(dir);
  if (realDir !== path.join(realInbox, day())) throw new HttpError(400, 'inbox folder is not a plain folder');

  const ext = path.extname(name);
  const stem = name.slice(0, name.length - ext.length) || 'file';
  let handle: fs.FileHandle | null = null;
  let full = '';
  for (let i = 1; i < 1000 && !handle; i++) {
    full = path.join(realDir, i === 1 ? name : `${stem}-${i}${ext}`);
    handle = await fs.open(full, 'wx', 0o600).catch((e: NodeJS.ErrnoException) => {
      if (e.code === 'EEXIST') return null;
      throw e;
    });
  }
  if (!handle) throw new HttpError(409, 'too many files with that name today');

  let size = 0;
  try {
    for await (const chunk of req) {
      size += (chunk as Buffer).length;
      if (size > max) throw new HttpError(413, 'file too large');
      await handle.write(chunk as Buffer);
    }
    if (size === 0) throw new HttpError(400, 'empty file');
    await handle.close();
    handle = null;
    return { path: full, size };
  } catch (e) {
    await handle?.close().catch(() => {});
    await fs.rm(full, { force: true });
    throw e;
  }
}
