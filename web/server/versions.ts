// A note's versions. One version is open at a time: the note file itself, saved as you type.
// Closing it (with a changelog line) freezes a copy; editing again opens the next one.
//
//   <root>/.versions/<note path>/v1.md, v2.md, ...   each closed version, the whole file
//   <root>/.versions/<note path>/index.json          [{ n, date, by, log }]
//
// The folder is hidden, so Obsidian and the note list leave it out; Git syncs it with the vault.
// A note that had no versions yet gets its text as it was frozen as v1 before its first change.
// The open version is the file when it differs from the last closed copy (ignoring which
// alternative the editor left showing, which is not a change to the text).

import { promises as fs } from 'node:fs';
import path from 'node:path';
import { parseNote, serializeNote } from '../src/doc/note.ts';
import { readText, writeAtomic } from './files.ts';

export interface Version {
  n: number;
  date: string;
  by: 'owner' | 'zenbot';
  log: string;
}

export interface VersionState {
  versions: Version[];
  /** The newest closed version, 0 when there is none. */
  current: number;
  /** True when the file has changes since that version: the open version is current + 1. */
  open: boolean;
}

const dirOf = (root: string, rel: string) => path.join(root, '.versions', rel);
const fileOf = (root: string, rel: string, n: number) => path.join(dirOf(root, rel), `v${n}.md`);

/** The text as far as versions care: without which alternative the editor left showing. */
const same = (a: string, b: string) => a.replace(/"showing":\d+,/g, '') === b.replace(/"showing":\d+,/g, '');

/** The closed versions; null when the note has never been versioned (no index yet). */
async function index(root: string, rel: string): Promise<Version[] | null> {
  const raw = await readText(path.join(dirOf(root, rel), 'index.json'));
  if (raw === null) return null;
  // A broken index stops here rather than be overwritten: it is the note's history.
  const v = JSON.parse(raw);
  if (!Array.isArray(v)) throw new Error(`broken version index for ${rel}`);
  return v;
}

const list = async (root: string, rel: string): Promise<Version[]> => (await index(root, rel)) ?? [];

export async function read(root: string, rel: string, n: number): Promise<string | null> {
  if (!Number.isInteger(n) || n < 1) return null;
  return readText(fileOf(root, rel, n));
}

export async function state(root: string, rel: string, text: string | null): Promise<VersionState> {
  const known = await index(root, rel);
  // Never versioned: the file as it is counts as v1, closed (its first change freezes it).
  if (known === null) return { versions: [], current: text === null ? 0 : 1, open: false };
  const versions = known;
  const current = versions.length ? versions[versions.length - 1].n : 0;
  // A note made here starts as v1, open, until it is first closed.
  if (!current) return { versions, current, open: text !== null };
  const last = await read(root, rel, current);
  return { versions, current, open: text !== null && (last === null || !same(last, text)) };
}

async function record(root: string, rel: string, text: string, v: Version) {
  await writeAtomic(fileOf(root, rel, v.n), text);
  const versions = await list(root, rel);
  versions.push(v);
  await writeAtomic(path.join(dirOf(root, rel), 'index.json'), JSON.stringify(versions, null, 1) + '\n');
}

const today = () => new Date().toISOString().slice(0, 10);

/** Before a note's first change: freeze it as it is, as v1. A new note starts versioned, v1 open. */
export async function baseline(root: string, rel: string, text: string | null) {
  if ((await index(root, rel)) !== null) return;
  if (text === null) {
    await writeAtomic(path.join(dirOf(root, rel), 'index.json'), '[]\n');
    return;
  }
  await record(root, rel, text, { n: 1, date: today(), by: 'owner', log: 'As it was before versions' });
}

/** The file with a changelog line added at the end of its changelog (made when missing). */
function withLogLine(text: string, line: string): string {
  const note = parseNote(text);
  const log = note.changelog ? note.changelog.replace(/\s+$/, '') + '\n' : '## Changelog\n\n';
  return serializeNote({ ...note, changelog: `${log}- ${line}\n` });
}

/** Closes the open version: adds its changelog line to the file and freezes it. */
export async function close(root: string, rel: string, text: string, log: string, by: 'owner' | 'zenbot'): Promise<string> {
  const s = await state(root, rel, text);
  if (!s.open) throw Object.assign(new Error('nothing to close: no changes since the last version'), { status: 400 });
  const words = log.trim().replace(/\s+/g, ' ');
  if (!words) throw Object.assign(new Error('a version needs a changelog line'), { status: 400 });
  const n = s.current + 1;
  const date = today();
  const out = withLogLine(text, `${date} — v${n}: ${words}${by === 'zenbot' ? ' (zenbot)' : ''}`);
  await record(root, rel, out, { n, date, by, log: words });
  return out;
}

/** A new open version that starts from version n, keeping today's changelog. */
export async function branch(root: string, rel: string, text: string, n: number): Promise<string> {
  const s = await state(root, rel, text);
  if (s.open) throw Object.assign(new Error(`close v${s.current + 1} first`), { status: 400 });
  const old = await read(root, rel, n);
  if (old === null) throw Object.assign(new Error(`no version ${n}`), { status: 404 });
  const from = parseNote(old);
  const now = parseNote(text);
  return serializeNote({ ...from, changelog: now.changelog });
}
