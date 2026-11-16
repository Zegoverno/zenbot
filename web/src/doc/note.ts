// A note on disk and in memory. No DOM, no editor: pure functions, shared by the page and the
// server and covered by unit tests.
//
// A file is plain Markdown that Obsidian reads as it is:
//
//   ---                      optional YAML properties (kept byte for byte; edited by a form)
//   type: idea
//   ---
//   # Title                  the H1: every note has one
//
//   Body, one paragraph per line.
//
//   <!-- zen:alternatives    only when the note has alternatives: the versions behind the
//   {"version":1,"groups":[…]}   text that shows, each anchored to the text it covers
//   -->
//
// The body always holds the version that is showing, so every other tool sees a normal note.
// A group is found again on open by its anchor: the showing text and which occurrence of it
// in the body it was. A group whose text was changed elsewhere (in Obsidian, say) can't be
// placed; it is kept, as an orphan, and written back, so nothing is ever lost silently.

export type Scope = 'word' | 'sentence' | 'paragraph';
export type Author = 'owner' | 'ai' | 'zenbot';

export interface Variant {
  text: string;
  by: Author;
  /** The model that wrote it, for AI versions. */
  model?: string;
}

/** A group as it lives in memory: placed at [from, to) of the body. */
export interface Group {
  id: string;
  scope: Scope;
  from: number;
  to: number;
  variants: Variant[];
  active: number;
  /** The default, the version you chose, when another one is showing; unset means the one showing. */
  def?: number;
  /** While reading (which shows defaults): the version to show again when the editor comes on. */
  resume?: number;
}

/** A group as it is saved: anchored by text, not by offset. */
export interface SavedGroup {
  id: string;
  scope: Scope;
  anchor: string;
  occurrence: number;
  /** The version showing in the body. */
  active: number;
  /** The version left showing in the editor, when it isn't the default (the body holds the default). */
  showing?: number;
  /** Older files only: the default when another version was showing. Now the file always shows it. */
  default?: number;
  variants: Variant[];
}

export interface Note {
  /** The properties block with its `---` fences and trailing newline, or ''. */
  frontmatter: string;
  /** What the page edits: the H1 and everything after it. */
  body: string;
  groups: Group[];
  /** Groups that could not be placed in the body (their text was changed elsewhere). */
  orphans: SavedGroup[];
  /** The page's changelog (`## Changelog` to the end), kept out of what the page edits; or ''. */
  changelog?: string;
  /** The page's `## Notes` section (heading included), when it is the last before the changelog. */
  notes?: string;
  /** Ghosted text: in the body the page edits, faded; left out of the file's text. */
  ghosts?: Ghost[];
  /** Ghosts that could not be placed again (the text around them changed elsewhere). */
  lostGhosts?: SavedGhost[];
}

/** Ghosted text as it lives in memory: [from, to) of the body, which holds it. */
export interface Ghost {
  id: string;
  from: number;
  to: number;
  /** 'zenbot' for text zenbot suggests while a version is open: shown outlined, kept out of the
   *  file like a ghost until the writer keeps it. */
  by?: 'zenbot';
}

/** Ghosted text as it is saved: its words, and where they go in the file's text. */
export interface SavedGhost {
  id: string;
  text: string;
  /** Offset in the file's text (without ghosts), and the text just before it, to find it again. */
  at: number;
  before: string;
  by?: 'zenbot';
}

const GHOST_CONTEXT = 32;

function isSavedGhost(g: unknown): g is SavedGhost {
  if (!g || typeof g !== 'object') return false;
  const o = g as Record<string, unknown>;
  return typeof o.id === 'string' && typeof o.text === 'string' && o.text.length > 0 &&
    Number.isInteger(o.at) && (o.at as number) >= 0 && typeof o.before === 'string' &&
    (o.by === undefined || o.by === 'zenbot');
}

/** Where a saved ghost goes in the clean text: after its `before` text, nearest to `at`. */
function placeGhost(clean: string, g: SavedGhost): number {
  if (g.before === '') return g.at === 0 ? 0 : -1;
  let best = -1;
  for (let i = clean.indexOf(g.before); i >= 0; i = clean.indexOf(g.before, i + 1)) {
    const p = i + g.before.length;
    if (best < 0 || Math.abs(p - g.at) < Math.abs(best - g.at)) best = p;
  }
  return best;
}

/** The text a reader sees: the body without its ghosts. */
export function withoutGhosts(body: string, ghosts: Ghost[] = []): string {
  let out = '';
  let at = 0;
  for (const g of [...ghosts].sort((a, b) => a.from - b.from)) {
    out += body.slice(at, g.from);
    at = Math.max(at, g.to);
  }
  return out + body.slice(at);
}

const META_OPEN = '<!-- zen:alternatives';
const META_CLOSE = '-->';

function splitFrontmatter(text: string): { frontmatter: string; rest: string } {
  const m = /^---\r?\n[\s\S]*?\r?\n---[ \t]*(\r?\n|$)/.exec(text);
  if (!m) return { frontmatter: '', rest: text };
  return { frontmatter: m[0], rest: text.slice(m[0].length) };
}

function nthIndex(hay: string, needle: string, n: number): number {
  let at = -1;
  for (let i = 0; i <= n; i++) {
    at = hay.indexOf(needle, at + 1);
    if (at < 0) return -1;
  }
  return at;
}

function occurrenceOf(hay: string, needle: string, from: number): number {
  let n = 0;
  for (let at = hay.indexOf(needle); at >= 0 && at < from; at = hay.indexOf(needle, at + 1)) n++;
  return n;
}

function isVariant(v: unknown): v is Variant {
  if (!v || typeof v !== 'object') return false;
  const o = v as Record<string, unknown>;
  if (o.by === 'ze') o.by = 'owner'; // notes tagged before the owner's tag got its name
  return typeof o.text === 'string' && (o.by === 'owner' || o.by === 'ai' || o.by === 'zenbot') && (o.model === undefined || typeof o.model === 'string');
}

function isSavedGroup(g: unknown): g is SavedGroup {
  if (!g || typeof g !== 'object') return false;
  const o = g as Record<string, unknown>;
  return (
    typeof o.id === 'string' &&
    (o.scope === 'word' || o.scope === 'sentence' || o.scope === 'paragraph') &&
    typeof o.anchor === 'string' && o.anchor.length > 0 &&
    Number.isInteger(o.occurrence) && (o.occurrence as number) >= 0 &&
    Array.isArray(o.variants) && o.variants.length > 0 && o.variants.every(isVariant) &&
    Number.isInteger(o.active) && (o.active as number) >= 0 && (o.active as number) < o.variants.length
  );
}

/** A line that opens or closes a code block, where headings are not headings. */
const fence = (line: string) => /^\s*(```|~~~)/.test(line);

/**
 * Splits off the changelog: from a `Changelog` heading (any level, outside code) to the end.
 * zenbot writes it, one line per change; the page shows it apart and never edits it.
 */
function splitChangelog(body: string): { text: string; changelog: string } {
  let code = false;
  let at = 0;
  for (const line of body.split('\n')) {
    if (fence(line)) code = !code;
    else if (!code && /^#{1,6}[ \t]+changelog[ \t#]*$/i.test(line.trim())) return { text: body.slice(0, at), changelog: body.slice(at) };
    at += line.length + 1;
  }
  return { text: body, changelog: '' };
}

/**
 * Splits off the notes: a `Notes` heading with no heading of its level or higher after it, so
 * it is the last section (before the changelog). Edited in the side panel, not on the page.
 */
function splitNotes(text: string): { text: string; notes: string } {
  const lines = text.split('\n');
  let code = false;
  let at = 0;
  let found = -1;
  let level = 0;
  for (const line of lines) {
    if (fence(line)) code = !code;
    else if (!code) {
      const m = /^(#{1,6})[ \t]+(.*?)[ \t#]*$/.exec(line.trim());
      if (m && /^notes$/i.test(m[2])) {
        found = at;
        level = m[1].length;
      } else if (m && found >= 0 && m[1].length <= level) found = -1;
    }
    at += line.length + 1;
  }
  return found < 0 ? { text, notes: '' } : { text: text.slice(0, found), notes: text.slice(found) };
}

/** What the notes section says, without its heading. */
export function notesText(notes: string): string {
  return notes.replace(/^[^\n]*\n?/, '').replace(/^\n+/, '').replace(/\s+$/, '');
}

/** A notes section with new words: the same heading, or `## Notes`; '' when there are none. */
export function withNotesText(notes: string, words: string): string {
  if (!words.trim()) return '';
  const head = notes ? notes.split('\n')[0] : '## Notes';
  return `${head}\n\n${words.replace(/\s+$/, '')}\n`;
}

/** The changelog's entries, newest first as written: the text of each `- ` line. */
export function changelogEntries(changelog: string): string[] {
  return changelog.split('\n').slice(1).map((l) => l.trim()).filter(Boolean).map((l) => l.replace(/^[-*]\s+/, ''));
}

/** Reads a file into a note. Never throws: a broken alternatives block is left in the body. */
export function parseNote(text: string): Note {
  const { frontmatter, rest } = splitFrontmatter(text);
  let body = rest;
  let saved: SavedGroup[] = [];
  let savedGhosts: SavedGhost[] = [];
  // Entries that don't read as ours are kept as they are, written back untouched, never dropped.
  let badGroups: SavedGroup[] = [];
  let badGhosts: SavedGhost[] = [];
  const start = rest.lastIndexOf(META_OPEN);
  if (start >= 0) {
    const end = rest.indexOf(META_CLOSE, start + META_OPEN.length);
    const after = end >= 0 ? rest.slice(end + META_CLOSE.length) : 'x';
    if (end >= 0 && after.trim() === '') {
      try {
        const meta = JSON.parse(rest.slice(start + META_OPEN.length, end));
        if (meta && (Array.isArray(meta.groups) || Array.isArray(meta.ghosts))) {
          const groups: unknown[] = Array.isArray(meta.groups) ? meta.groups : [];
          const ghosts: unknown[] = Array.isArray(meta.ghosts) ? meta.ghosts : [];
          saved = groups.filter(isSavedGroup);
          badGroups = groups.filter((g) => !isSavedGroup(g)) as SavedGroup[];
          savedGhosts = ghosts.filter(isSavedGhost);
          badGhosts = ghosts.filter((g) => !isSavedGhost(g)) as SavedGhost[];
          body = rest.slice(0, start).replace(/\n+$/, '\n');
        }
      } catch {
        // Not ours, or broken: leave it in the body where the writer can see it.
      }
    }
  }
  const split = splitChangelog(body);
  const parts = splitNotes(split.text);
  body = parts.text;
  const changelog = split.changelog;
  const notes = parts.notes;
  const groups: Group[] = [];
  const orphans: SavedGroup[] = [...badGroups];
  const taken: Array<[number, number]> = [];
  for (const s of saved) {
    const shown = s.variants[s.active].text;
    const at = shown === s.anchor ? nthIndex(body, s.anchor, s.occurrence) : -1;
    const to = at + s.anchor.length;
    if (at < 0 || taken.some(([a, b]) => at < b && to > a)) {
      orphans.push(s);
      continue;
    }
    taken.push([at, to]);
    const g: Group = { id: s.id, scope: s.scope, from: at, to, variants: s.variants.map((v) => ({ ...v })), active: s.active };
    const d = s.default;
    if (Number.isInteger(d) && d! >= 0 && d! < s.variants.length && d !== s.active) g.def = d;
    const sh = s.showing;
    if (Number.isInteger(sh) && sh! >= 0 && sh! < s.variants.length && sh !== s.active) g.resume = sh;
    groups.push(g);
  }
  groups.sort((a, b) => a.from - b.from);
  const placed = placeGhosts({ frontmatter, body, groups, orphans }, savedGhosts);
  return { ...placed, lostGhosts: [...(placed.lostGhosts ?? []), ...badGhosts], changelog, notes };
}

/** Puts saved ghosts back into the text (never inside a group), moving the groups after them. */
function placeGhosts(note: Note, saved: SavedGhost[]): Note {
  const lost: SavedGhost[] = [];
  const placed: Array<{ g: SavedGhost; p: number }> = [];
  for (const g of saved) {
    const p = placeGhost(note.body, g);
    if (p < 0 || note.groups.some((x) => p > x.from && p < x.to)) lost.push(g);
    else placed.push({ g, p });
  }
  placed.sort((a, b) => a.p - b.p);
  let body = '';
  let at = 0;
  let shift = 0;
  const ghosts: Ghost[] = [];
  for (const { g, p } of placed) {
    body += note.body.slice(at, p) + g.text;
    at = p;
    ghosts.push({ id: g.id, from: p + shift, to: p + shift + g.text.length, ...(g.by ? { by: g.by } : {}) });
    shift += g.text.length;
  }
  body += note.body.slice(at);
  // A ghost at a group's start goes before it; none go inside one.
  const before = (pos: number) => placed.filter((x) => x.p <= pos).reduce((n, x) => n + x.g.text.length, 0);
  const groups = note.groups.map((x) => ({ ...x, from: x.from + before(x.from), to: x.to + before(x.from) }));
  return { ...note, body, groups, ghosts, lostGhosts: lost };
}

/**
 * The note as it is kept: every group showing its default. What the editor shows while you
 * compare versions is a preview; the file, and so the viewer and Obsidian, holds your choice.
 */
function withDefaults(note: Note): Note {
  let body = note.body;
  let shift = 0;
  const groups: Group[] = [];
  for (const g of [...note.groups].sort((a, b) => a.from - b.from)) {
    const from = g.from + shift;
    const to = g.to + shift;
    // The showing version is what the body holds (it may have been edited in place).
    const variants = g.variants.map((v, i) => (i === g.active ? { ...v, text: body.slice(from, to) } : v));
    const d = g.def !== undefined && g.def < variants.length ? g.def : g.active;
    const text = variants[d].text;
    body = body.slice(0, from) + text + body.slice(to);
    shift += text.length - (to - from);
    // Remember the version left showing in the editor, to bring it back there.
    const shown = g.active !== d ? g.active : (g.resume ?? d);
    const kept: Group = { id: g.id, scope: g.scope, from, to: from + text.length, variants, active: d };
    if (shown !== d && shown < variants.length) kept.resume = shown;
    groups.push(kept);
  }
  // Ghosts never sit inside a group: each moves by how much the groups before it grew or shrank.
  const grew = (pos: number) => note.groups.reduce((n, g) => {
    if (g.to > pos) return n;
    const k = groups.find((x) => x.id === g.id)!;
    return n + (k.to - k.from) - (g.to - g.from);
  }, 0);
  const ghosts = (note.ghosts ?? []).map((gh) => ({ ...gh, from: gh.from + grew(gh.from), to: gh.to + grew(gh.from) }));
  return { ...note, body, groups, ghosts };
}

/** Writes a note back, each group at its default. With no groups and no orphans, plain Markdown. */
export function serializeNote(input: Note): string {
  const full = withDefaults(input);
  // The file's text leaves the ghosts out; they are kept in the comment, with where they go.
  const ghosts = [...(full.ghosts ?? [])].filter((g) => g.to > g.from).sort((a, b) => a.from - b.from);
  let body = withoutGhosts(full.body, ghosts);
  const cut = (pos: number) => pos - ghosts.filter((g) => g.to <= pos).reduce((n, g) => n + g.to - g.from, 0);
  const savedGhosts: SavedGhost[] = ghosts.map((g) => {
    const at = cut(g.from);
    return { id: g.id, text: full.body.slice(g.from, g.to), at, before: body.slice(Math.max(0, at - GHOST_CONTEXT), at), ...(g.by ? { by: g.by } : {}) };
  });
  savedGhosts.push(...(full.lostGhosts ?? []));
  const note = { ...full, groups: full.groups.map((g) => ({ ...g, from: cut(g.from), to: cut(g.from) + (g.to - g.from) })) };
  const saved: SavedGroup[] = note.groups
    .filter((g) => g.variants.length > 1 && g.to > g.from)
    .map((g) => ({
      id: g.id,
      scope: g.scope,
      anchor: body.slice(g.from, g.to),
      occurrence: occurrenceOf(body, body.slice(g.from, g.to), g.from),
      active: g.active,
      ...(g.resume !== undefined ? { showing: g.resume } : {}),
      variants: g.variants.map((v, i) => (i === g.active ? { ...v, text: body.slice(g.from, g.to) } : v)),
    }));
  saved.push(...note.orphans);
  for (const section of [note.notes, note.changelog]) {
    if (!section) continue;
    if (body && !body.endsWith('\n')) body += '\n';
    if (body && !body.endsWith('\n\n')) body += '\n';
    body += section;
  }
  if (saved.length === 0 && savedGhosts.length === 0) return note.frontmatter + body;
  if (!body.endsWith('\n')) body += '\n';
  const json = JSON.stringify({ version: 1, groups: saved, ...(savedGhosts.length ? { ghosts: savedGhosts } : {}) });
  return `${note.frontmatter}${body}\n${META_OPEN}\n${json}\n${META_CLOSE}\n`;
}

/** The H1's text, or '' when the body doesn't start with one. */
export function titleOf(body: string): string {
  const m = /^\s*#[ \t]+(.+?)[ \t#]*$/m.exec(body);
  return m && body.trimStart().startsWith('#') && !body.trimStart().startsWith('##') ? m[1].trim() : '';
}

/** Words in the body as a reader sees them: no Markdown marks. */
export function wordCount(body: string): number {
  const text = body.replace(/^#+\s/gm, '').replace(/[*_`>#\[\]()-]/g, ' ');
  const words = text.match(/[\p{L}\p{N}][\p{L}\p{N}'’.-]*/gu);
  return words ? words.length : 0;
}

/** A file name from what the writer typed: no folders, no characters Obsidian or Git trip on. */
export function cleanFileName(input: string): string {
  let name = input.trim().replace(/\.md$/i, '');
  name = name.replace(/[\\/:*?"<>|#^\[\]]/g, ' ').replace(/\s+/g, ' ').trim();
  name = name.replace(/^[.\s]+/, '');
  return name ? `${name}.md` : '';
}

/** The scope a selection makes: a whole paragraph, whole sentences, or anything less (a word). */
export function scopeOf(body: string, from: number, to: number): Scope {
  const lineStart = body.lastIndexOf('\n', from - 1) + 1;
  let lineEnd = body.indexOf('\n', to);
  if (lineEnd < 0) lineEnd = body.length;
  const para = body.slice(lineStart, lineEnd);
  const sel = body.slice(from, to).trim();
  if (sel && sel === para.trim()) return /[.!?…]\s+\S/.test(sel) ? 'paragraph' : 'sentence';
  const before = body.slice(lineStart, from);
  const startsSentence = before.trim() === '' || /[.!?…]["'”’)]*\s+$/.test(before);
  const endsSentence = /[.!?…]["'”’)]*$/.test(sel);
  return startsSentence && endsSentence ? 'sentence' : 'word';
}

let counter = 0;
export function newId(): string {
  counter = (counter + 1) % 1296;
  return Date.now().toString(36).slice(-5) + counter.toString(36).padStart(2, '0');
}
