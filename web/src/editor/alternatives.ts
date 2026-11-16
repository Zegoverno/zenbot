// Alternatives in the page: groups of versions kept behind the text that shows.
//
// A StateField holds the groups with their positions, mapped through every edit. Typing inside
// a group's text edits the version that shows (an AI version you edit becomes yours). Flipping
// replaces the text with another version and is kept out of undo history, as Write_On does:
// undo takes back writing, not browsing. Creating and removing groups are undoable.
//
// Drawn as: words and sentences underlined, with dots after them (one per version, the big one
// showing) and 🤖 when an AI version shows; paragraphs with a line in the left margin and the
// dots running down it.

import { EditorSelection, StateEffect, StateField, Transaction, type EditorState, type Range } from '@codemirror/state';
import { Decoration, EditorView, WidgetType, type DecorationSet } from '@codemirror/view';
import { invertedEffects } from '@codemirror/commands';
import { newId, scopeOf, type Group, type Variant } from '../doc/note.ts';

export const addGroup = StateEffect.define<Group>();
const removeGroup = StateEffect.define<Group>();
/** Replaces a group's versions and which one shows, without touching the text. */
export const setGroup = StateEffect.define<Group>();
/** Marks a flip: the group's text was replaced by the version now active. */
const flipped = StateEffect.define<{ id: string; from: number; length: number }>();

function mapGroup(g: Group, tr: Transaction): Group | null {
  const para = g.scope === 'paragraph';
  const from = tr.changes.mapPos(g.from, para ? -1 : 1);
  const to = tr.changes.mapPos(g.to, para ? 1 : -1);
  if (to <= from) return null;
  if (from === g.from && to === g.to && !tr.changes.touchesRange(g.from, g.to)) return g;
  const next = { ...g, from, to };
  const text = tr.newDoc.sliceString(from, to);
  if (text !== g.variants[g.active].text) {
    // The writer edited the showing version: keep it in step, and it is theirs now.
    next.variants = g.variants.map((v, i): Variant => (i === g.active ? { text, by: 'owner' } : v));
  }
  return next;
}

export const groupsField = StateField.define<Group[]>({
  create: () => [],
  update(groups, tr) {
    const flips = new Map<string, { from: number; length: number }>();
    for (const e of tr.effects) if (e.is(flipped)) flips.set(e.value.id, e.value);
    let out: Group[] = [];
    for (const g of groups) {
      const f = flips.get(g.id);
      if (f) {
        out.push({ ...g, from: f.from, to: f.from + f.length });
        continue;
      }
      if (!tr.docChanged) {
        out.push(g);
        continue;
      }
      const m = mapGroup(g, tr);
      if (m) out.push(m);
    }
    for (const e of tr.effects) {
      if (e.is(addGroup)) {
        // Groups don't nest: a new group takes the place of any it overlaps.
        out = out.filter((g) => g.to <= e.value.from || g.from >= e.value.to);
        out.push(e.value);
      } else if (e.is(removeGroup)) {
        out = out.filter((g) => g.id !== e.value.id);
      } else if (e.is(setGroup)) {
        out = out.map((g) => (g.id === e.value.id ? { ...e.value, from: g.from, to: g.to } : g));
      }
    }
    return out.sort((a, b) => a.from - b.from);
  },
  provide: (f) => EditorView.decorations.from(f, (groups) => (groups.length ? build(groups) : Decoration.none)),
});

/** Undo of creating a group removes it, and the other way round. */
const undoGroups = invertedEffects.of((tr) => {
  const out: StateEffect<unknown>[] = [];
  const before = tr.startState.field(groupsField);
  for (const e of tr.effects) {
    if (e.is(addGroup)) {
      out.push(removeGroup.of(e.value));
      for (const g of before) if (g.to > e.value.from && g.from < e.value.to) out.push(addGroup.of(g));
    } else if (e.is(removeGroup)) {
      const g = before.find((x) => x.id === e.value.id);
      if (g) out.push(addGroup.of(g));
    }
  }
  return out;
});

class Dots extends WidgetType {
  group: Group;
  constructor(group: Group) {
    super();
    this.group = group;
  }
  eq(o: Dots) {
    return key(o.group) === key(this.group);
  }
  toDOM(view: EditorView) {
    const g = this.group;
    const el = document.createElement('span');
    el.className = g.scope === 'paragraph' ? 'zw-dots zw-dots-para' : 'zw-dots';
    el.setAttribute('aria-label', `version ${g.active + 1} of ${g.variants.length}`);
    g.variants.forEach((v, i) => {
      const d = document.createElement('span');
      // Big: the version showing. Blue: the default. The rest small and grey.
      const def = g.def ?? g.active;
      d.className = 'zw-dot' + (i === g.active ? ' zw-dot-on' : '') + (i === def ? ' zw-dot-def' : '') + (v.by === 'zenbot' ? ' zw-dot-zenbot' : '');
      d.title = (v.by === 'ai' ? '🤖 ' : v.by === 'zenbot' ? 'zenbot: ' : '✍️ ') + v.text + (i === def ? '  (default)' : '');
      d.addEventListener('mousedown', (ev) => {
        ev.preventDefault();
        const current = groupById(view.state, g.id);
        if (current) flipTo(view, current, i);
      });
      el.appendChild(d);
    });
    if (g.variants[g.active].by === 'zenbot') {
      const r = document.createElement('span');
      r.className = 'zw-robot zw-by-mark';
      r.textContent = 'zenbot';
      el.appendChild(r);
    } else if (g.variants[g.active].by === 'ai') {
      const r = document.createElement('span');
      r.className = 'zw-robot';
      r.textContent = '🤖';
      el.appendChild(r);
    }
    return el;
  }
  ignoreEvent() {
    return true;
  }
}

function key(g: Group): string {
  return `${g.scope}|${g.active}|${g.def ?? ''}|${g.variants.length}|${g.variants[g.active].by}`;
}

function build(groups: Group[]): DecorationSet {
  const ranges: Range<Decoration>[] = [];
  for (const g of groups) {
    if (g.variants.length < 2 && g.scope !== 'paragraph') {
      ranges.push(Decoration.mark({ class: 'zw-alt zw-alt-new' }).range(g.from, g.to));
      continue;
    }
    const ai = g.variants[g.active].by === 'ai' ? ' zw-alt-ai' : '';
    if (g.scope === 'paragraph') {
      ranges.push(Decoration.line({ class: `zw-alt-para${ai}` }).range(g.from));
      ranges.push(Decoration.widget({ widget: new Dots(g), side: -1 }).range(g.from));
    } else {
      ranges.push(Decoration.mark({ class: `zw-alt${ai}` }).range(g.from, g.to));
      ranges.push(Decoration.widget({ widget: new Dots(g), side: 1 }).range(g.to));
    }
  }
  return Decoration.set(ranges, true);
}

/** The group with this id, as it is now. */
export const groupById = (state: EditorState, id: string) => state.field(groupsField).find((x) => x.id === id);

/** Sets what a group remembers showing, outside undo. */
const remember = (view: EditorView, id: string, resume: number | undefined) => {
  const g = groupById(view.state, id);
  if (g) view.dispatch({ effects: setGroup.of({ ...g, resume }), annotations: Transaction.addToHistory.of(false) });
};

/** Reading: each group shows its default, remembering what was showing for when writing resumes. */
export function showDefaults(view: EditorView) {
  for (const g of view.state.field(groupsField)) {
    if (g.def === undefined || g.def === g.active) continue;
    const cur = groupById(view.state, g.id);
    if (cur) flipTo(view, cur, g.def);
    remember(view, g.id, g.active);
  }
}

/** Writing: brings back the versions left showing (the file and reading show defaults). */
export function resumeShowing(view: EditorView) {
  for (const g of view.state.field(groupsField)) {
    if (g.resume === undefined) continue;
    const cur = groupById(view.state, g.id);
    if (cur && g.resume < cur.variants.length) flipTo(view, cur, g.resume);
    remember(view, g.id, undefined);
  }
}

export function groupAt(state: EditorState, pos = state.selection.main.head): Group | undefined {
  return state.field(groupsField).find((g) => pos >= g.from && pos <= g.to);
}

/** Shows version `index` of a group; the caret moves into it unless `caret` is false. */
export function flipTo(view: EditorView, g: Group, index: number, caret = true) {
  const i = ((index % g.variants.length) + g.variants.length) % g.variants.length;
  if (i === g.active) return;
  const text = g.variants[i].text;
  const head = view.state.selection.main.head;
  const offset = Math.min(Math.max(head - g.from, 0), text.length);
  view.dispatch({
    changes: { from: g.from, to: g.to, insert: text },
    // Flipping only changes what shows: the default stays where it was.
    effects: [setGroup.of({ ...g, active: i, def: g.def ?? g.active }), flipped.of({ id: g.id, from: g.from, length: text.length })],
    selection: caret ? EditorSelection.cursor(g.from + offset) : undefined,
    annotations: Transaction.addToHistory.of(false),
    userEvent: 'writer.flip',
  });
}

export function flip(view: EditorView, step: number): boolean {
  const g = groupAt(view.state);
  if (!g || g.variants.length < 2) return false;
  flipTo(view, g, g.active + step);
  return true;
}

/** The range a new group takes: the selection, trimmed of spaces at its ends. */
function targetRange(state: EditorState): { from: number; to: number } | null {
  const sel = state.selection.main;
  if (sel.empty) return null;
  // Trim spaces off the ends, so "the word " groups "the word".
  const text = state.sliceDoc(sel.from, sel.to);
  const lead = text.length - text.trimStart().length;
  const trail = text.length - text.trimEnd().length;
  const from = sel.from + lead;
  const to = sel.to - trail;
  if (to <= from || state.sliceDoc(from, to).includes('\n')) return null;
  return { from, to };
}

/**
 * The group at the cursor, or a new one over the selection. A selection that overlaps groups
 * already there never replaces them: you get the one it touches ('overlap' if it touches
 * several), and removing alternatives is always a choice of yours (Alt+Delete in the panel).
 */
export function ensureGroup(view: EditorView): Group | 'overlap' | null {
  const existing = groupAt(view.state);
  if (existing && view.state.selection.main.empty) return existing;
  const r = targetRange(view.state);
  if (!r) return existing ?? null;
  const touching = view.state.field(groupsField).filter((g) => g.from < r.to && g.to > r.from);
  if (touching.length === 1) return touching[0];
  if (touching.length > 1) return 'overlap';
  const doc = view.state.doc.toString();
  const g: Group = {
    id: newId(),
    scope: scopeOf(doc, r.from, r.to),
    from: r.from,
    to: r.to,
    variants: [{ text: doc.slice(r.from, r.to), by: 'owner' }],
    active: 0,
  };
  view.dispatch({ effects: addGroup.of(g), userEvent: 'writer.group' });
  return g;
}

export function addVariants(view: EditorView, id: string, variants: Variant[]): Group | null {
  const g = groupById(view.state, id);
  if (!g) return null;
  const have = new Set(g.variants.map((v) => v.text));
  const fresh = variants.filter((v) => v.text.trim() && !have.has(v.text) && (have.add(v.text), true));
  if (!fresh.length) return g;
  const next = { ...g, variants: [...g.variants, ...fresh] };
  view.dispatch({ effects: setGroup.of(next) });
  return next;
}

/** Removes one version; the group's default after it (a deleted default hands over to the next). */
export function removeVariant(view: EditorView, id: string, index: number): number | undefined {
  const g = groupById(view.state, id);
  if (!g || g.variants.length < 2) return g?.def;
  if (index === g.active) {
    // Show the next one first, then drop the old one.
    flipTo(view, g, index + 1 < g.variants.length ? index + 1 : index - 1);
  }
  const cur = groupById(view.state, id)!;
  const variants = cur.variants.filter((_, i) => i !== index);
  const active = index < cur.active ? cur.active - 1 : cur.active;
  const d = g.def ?? g.active;
  const def = index < d ? d - 1 : index === d ? Math.min(index, variants.length - 1) : d;
  view.dispatch({ effects: setGroup.of({ ...cur, variants, active, def }) });
  return def;
}

/** Removes all of a group's alternatives, keeping its default in the text. Undo brings them back. */
export function removeAll(view: EditorView, id: string, def: number) {
  let g = groupById(view.state, id);
  if (!g) return;
  flipTo(view, g, def);
  g = groupById(view.state, id)!;
  view.dispatch({ effects: removeGroup.of(g), userEvent: 'writer.removeAll' });
}

/** Drops groups that never got a second version (made by opening the panel, then left). */
export function dropLonely(view: EditorView, exceptId?: string) {
  const lonely = view.state.field(groupsField).filter((g) => g.variants.length < 2 && g.id !== exceptId);
  if (lonely.length) view.dispatch({ effects: lonely.map((g) => removeGroup.of(g)), annotations: Transaction.addToHistory.of(false) });
}

export function alternatives() {
  return [groupsField, undoGroups];
}
