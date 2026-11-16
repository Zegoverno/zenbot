// zenbot's side of versions and suggestions, from the shell:
//
//   node scripts/zen.ts status  <note>                      v3 · open | v2 · closed
//   node scripts/zen.ts begin   <note>                      before editing: fails if a version is open
//   node scripts/zen.ts close   <note> --log "what changed" closes the open version as zenbot
//   node scripts/zen.ts suggest <note> --after "text" --text " new words"
//   node scripts/zen.ts alt     <note> --target "text" --text "another way to say it"
//   (suggest and alt need an open version; --open starts one holding only the suggestion)
//
// The rule: with no open version zenbot edits the note itself (begin, edit, close). With one
// open, the note is the owner's: zenbot only suggests (outlined text, or an alternative), never edits.
// <note> is relative to the vault (ZEN_WEB_ROOT, default ~/.zenbot/global).

import { promises as fs } from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { newId, parseNote, scopeOf, serializeNote } from '../src/doc/note.ts';
import * as versions from '../server/versions.ts';
import { writeAtomic } from '../server/files.ts';

const ROOT = path.resolve(process.env.ZEN_WEB_ROOT ?? path.join(os.homedir(), '.zenbot/global'));
const [cmd, rel, ...rest] = process.argv.slice(2);
const opt = (name: string) => {
  const i = rest.indexOf(`--${name}`);
  return i >= 0 ? rest[i + 1] : undefined;
};
const die = (msg: string): never => {
  console.error(msg);
  process.exit(1);
};

if (!cmd || !rel || rel.includes('..') || rel.split('/').some((p) => p.startsWith('.')) || !rel.endsWith('.md')) {
  die('usage: zen.ts status|begin|close|suggest|alt <note.md> [--log|--after|--target|--text ...]');
}
const full = path.join(ROOT, rel);
const text = await fs.readFile(full, 'utf8').catch(() => die(`no such note: ${rel}`));
const s = await versions.state(ROOT, rel, text);
const label = s.open ? `v${s.current + 1} · open` : `v${s.current} · closed`;

const write = (out: string) => writeAtomic(full, out);

switch (cmd) {
  case 'status':
    console.log(label);
    break;
  case 'begin':
    if (s.open) die(`${label}: the owner has a version open; suggest instead of editing`);
    await versions.baseline(ROOT, rel, text);
    console.log(`ok: edit, then close (now v${s.current} · closed)`);
    break;
  case 'close': {
    const out = await versions.close(ROOT, rel, text, opt('log') ?? '', 'zenbot').catch((e) => die(e.message));
    await write(out);
    console.log(`closed v${s.current + 1}`);
    break;
  }
  case 'suggest':
  case 'alt':
    // --open: start the next version with only these suggestions in it (for the owner's idea pages,
    // whose words zenbot never changes itself).
    if (!s.open && !rest.includes('--open')) die(`${label}: no open version, so edit the note yourself (begin, edit, close), or pass --open`);
    if (!s.open) await versions.baseline(ROOT, rel, text);
}

switch (cmd) {
  case 'status':
  case 'begin':
  case 'close':
    break;
  case 'suggest': {
    const after = opt('after');
    const words = opt('text');
    if (!after || !words) die('suggest needs --after and --text');
    const note = parseNote(text);
    const at = note.body.indexOf(after!);
    if (at < 0) die('--after text not found in the note');
    const pos = at + after!.length;
    if (note.groups.some((g) => g.from < pos && pos < g.to)) die('that spot is inside text with alternatives');
    const n = words!.length;
    const move = <T extends { from: number; to: number }>(x: T): T => (x.from >= pos ? { ...x, from: x.from + n, to: x.to + n } : x);
    const out = serializeNote({
      ...note,
      body: note.body.slice(0, pos) + words + note.body.slice(pos),
      groups: note.groups.map(move),
      ghosts: [...(note.ghosts ?? []).map(move), { id: newId(), from: pos, to: pos + n, by: 'zenbot' }],
    });
    await write(out);
    console.log(`suggested (${label})`);
    break;
  }
  case 'alt': {
    const target = opt('target');
    const words = opt('text');
    if (!target || !words) die('alt needs --target and --text');
    const note = parseNote(text);
    const from = note.body.indexOf(target!);
    if (from < 0) die('--target text not found in the note');
    const to = from + target!.length;
    const same = note.groups.find((g) => g.from === from && g.to === to);
    if (!same && note.groups.some((g) => g.from < to && g.to > from)) die('that text overlaps other alternatives');
    const groups = same
      ? note.groups.map((g) => (g === same ? { ...g, variants: [...g.variants, { text: words!, by: 'zenbot' as const }] } : g))
      : [...note.groups, { id: newId(), scope: scopeOf(note.body, from, to), from, to, active: 0, variants: [{ text: target!, by: 'owner' as const }, { text: words!, by: 'zenbot' as const }] }];
    await write(serializeNote({ ...note, groups: groups.sort((a, b) => a.from - b.from) }));
    console.log(`alternative added (${label})`);
    break;
  }
  default:
    die(`unknown command ${cmd}`);
}
