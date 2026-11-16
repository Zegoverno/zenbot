// The AI's proposals on the page, in three families. Check's fixes: underlined by kind, each
// accepted (the text changes), kept as an alternative (the fix shows, your words stay one key
// away) or dismissed. Marks: underlined with a reason, never changing anything. Shrink's cuts:
// struck through and faded, shown up to the level chosen (10, 20, 30, 50%); a cut taken is
// ghosted, or for a reworded phrase made an alternative, so no word is ever lost. They live only
// in the page: asking again is cheap, and nothing of it goes into the file.

import { StateEffect, StateField, type EditorState, type Range } from '@codemirror/state';
import { Decoration, EditorView, WidgetType, type DecorationSet } from '@codemirror/view';
import { newId, scopeOf } from '../doc/note.ts';
import type { FixKind, FixProposal } from '../lib/api.ts';
import { addGroup, groupsField } from './alternatives.ts';
import { ghostRange, ghostsField, type Refusal } from './ghosts.ts';

export interface Fix {
  id: string;
  from: number;
  to: number;
  replace: string;
  kind: FixKind;
  why: string;
  /** A cut's level: the smallest share (10, 20, 30, 50) that takes it. */
  level?: number;
}

export type Family = 'check' | 'marks' | 'shrink';
export const LEVELS = [10, 20, 30, 50];
const MARKS: FixKind[] = ['weak', 'long', 'filler'];
export const familyOf = (k: FixKind): Family => (k === 'cut' ? 'shrink' : MARKS.includes(k) ? 'marks' : 'check');
export const isMark = (k: FixKind) => MARKS.includes(k);

/** A cut's shorter words, shown in place right after the words they replace. */
class Replacement extends WidgetType {
  readonly text: string;
  constructor(text: string) {
    super();
    this.text = text;
  }
  eq(o: Replacement) {
    return o.text === this.text;
  }
  toDOM() {
    const el = document.createElement('span');
    el.className = 'zw-fix-new';
    el.textContent = this.text;
    return el;
  }
}

export const setFixes = StateEffect.define<Fix[]>();
/** The Shrink level showing (0: none). */
export const setLevel = StateEffect.define<number>();
export const levelField = StateField.define<number>({
  create: () => 0,
  update(v, tr) {
    for (const e of tr.effects) if (e.is(setLevel)) v = e.value;
    return v;
  },
});
/** The proposals showing: every fix and mark, and the cuts up to the level. */
export const shownFixes = (state: EditorState, family?: Family): Fix[] => {
  const level = state.field(levelField);
  return state.field(fixesField).filter((f) => (!f.level || f.level <= level) && (!family || familyOf(f.kind) === family));
};
export const dropFix = StateEffect.define<string>();

export const fixesField = StateField.define<Fix[]>({
  create: () => [],
  update(fixes, tr) {
    let out = fixes;
    if (tr.docChanged) {
      // A fix whose words were edited, or next to an edit, is gone: it no longer says what it
      // was about (two fixes side by side often mend the same thing, a comma say).
      out = [];
      for (const f of fixes) {
        let touched = false;
        tr.changes.iterChangedRanges((a, b) => {
          if (a <= f.to && b >= f.from) touched = true;
        });
        if (!touched) out.push({ ...f, from: tr.changes.mapPos(f.from, 1), to: tr.changes.mapPos(f.to, -1) });
      }
    }
    for (const e of tr.effects) {
      if (e.is(setFixes)) out = e.value;
      else if (e.is(dropFix)) out = out.filter((f) => f.id !== e.value);
    }
    return out;
  },
  provide: (f) => EditorView.decorations.compute([f, levelField], (state): DecorationSet =>
    Decoration.set(shownFixes(state).filter((x) => x.to > x.from).flatMap((x): Range<Decoration>[] => [
      Decoration.mark({ class: `zw-fix zw-fix-${x.kind}`, attributes: { title: `${x.kind}: ${x.why}` } }).range(x.from, x.to),
      ...(x.kind === 'cut' && x.replace ? [Decoration.widget({ widget: new Replacement(x.replace), side: 1 }).range(x.to)] : []),
    ]), true)),
});

const wordsIn = (t: string) => t.split(/\s+/).filter((w) => /[\p{L}\p{N}]/u.test(w)).length;

/** Where a cut only drops words (what is left reads as `replace`): the spans of `find` to drop. */
export function dropSpans(find: string, replace: string): Array<[number, number]> | null {
  const tokens = [...find.matchAll(/\S+/g)].map((m) => [m.index, m.index + m[0].length, m[0]] as const);
  const keep = replace.split(/\s+/).filter(Boolean);
  const kept = new Set<number>();
  // Matched from the end, so a dropped opening ("In which aspect? In the spirit") goes whole.
  let k = keep.length - 1;
  for (let i = tokens.length - 1; i >= 0 && k >= 0; i--) if (tokens[i][2] === keep[k]) (kept.add(i), k--);
  if (k >= 0) return null;
  const spans: Array<[number, number]> = [];
  tokens.forEach(([a, b], i) => {
    if (kept.has(i)) return;
    const last = spans.at(-1);
    if (last && !kept.has(i - 1) && i > 0) last[1] = b;
    else spans.push([a, b]);
  });
  return spans;
}

/** Places the proposals in the text: the first free occurrence of each, never in a ghost. Cuts,
 *  least needed first, get their level from the words they save out of `total`; a cut that only
 *  drops words becomes one drop per run of words. */
export function placeFixes(state: EditorState, proposals: FixProposal[], total = 0): Fix[] {
  const doc = state.doc.toString();
  const ghosts = state.field(ghostsField);
  const taken: Array<[number, number]> = ghosts.map((g) => [g.from, g.to]);
  const out: Fix[] = [];
  let saved = 0;
  for (const p of proposals) {
    for (let at = doc.indexOf(p.find); at >= 0; at = doc.indexOf(p.find, at + 1)) {
      const to = at + p.find.length;
      if (taken.some(([a, b]) => at < b && to > a)) continue;
      taken.push([at, to]);
      if (p.kind !== 'cut') {
        out.push({ id: newId(), from: at, to, replace: p.replace, kind: p.kind, why: p.why });
        break;
      }
      const level = LEVELS.find((l) => saved < Math.max(1, Math.round((total * l) / 100)));
      saved += wordsIn(p.find) - wordsIn(p.replace);
      if (!level) break;
      const spans = dropSpans(p.find, p.replace);
      if (!spans) out.push({ id: newId(), from: at, to, replace: p.replace, kind: 'cut', why: p.why, level });
      else for (const [a, b] of spans) out.push({ id: newId(), from: at + a, to: at + b, replace: '', kind: 'cut', why: p.why, level });
      break;
    }
  }
  return out.sort((a, b) => a.from - b.from);
}

/** The proposal selected on the page, or the one at the cursor (of one family). */
export function fixAt(state: EditorState, family?: Family): Fix | undefined {
  const { from, to, empty, head } = state.selection.main;
  const fixes = shownFixes(state, family);
  if (!empty) return fixes.find((f) => f.from === from && f.to === to) ?? fixes.find((f) => f.from <= from && to <= f.to);
  return fixes.find((f) => f.from <= head && head <= f.to);
}

export function acceptFix(view: EditorView, f: Fix) {
  if (isMark(f.kind)) return view.dispatch({ effects: dropFix.of(f.id) });
  let { from } = f;
  // A cut that drops words leaves no double space, nor a space before punctuation.
  const doc = view.state.doc;
  if (!f.replace && from > 0 && doc.sliceString(from - 1, from) === ' ' && /^[ .,;:!?)]/.test(doc.sliceString(f.to, f.to + 1))) from -= 1;
  view.dispatch({ changes: { from, to: f.to, insert: f.replace }, effects: dropFix.of(f.id), userEvent: 'input.fix' });
}

/** The fix shows, as the default; your words stay as the other version (Alt+↑/↓). */
export function acceptAsAlternative(view: EditorView, f: Fix): boolean {
  if (view.state.field(groupsField).some((g) => g.from < f.to && g.to > f.from)) {
    acceptFix(view, f);
    return false;
  }
  const original = view.state.sliceDoc(f.from, f.to);
  const doc = view.state.doc.toString();
  const after = doc.slice(0, f.from) + f.replace + doc.slice(f.to);
  view.dispatch({
    changes: { from: f.from, to: f.to, insert: f.replace },
    effects: [
      dropFix.of(f.id),
      addGroup.of({
        id: newId(), scope: scopeOf(after, f.from, f.from + f.replace.length), from: f.from, to: f.from + f.replace.length,
        variants: [{ text: original, by: 'owner' }, { text: f.replace, by: 'ai' }], active: 1,
      }),
    ],
    userEvent: 'input.fix',
  });
  return true;
}

/** Takes a cut: its words are ghosted (out of the piece, one key from coming back); a reworded
 *  phrase becomes an alternative, the shorter one showing. */
export function takeCut(view: EditorView, f: Fix): Refusal {
  if (f.replace) return (acceptAsAlternative(view, f), null);
  const why = ghostRange(view, f.from, f.to);
  if (!why) view.dispatch({ effects: dropFix.of(f.id) });
  return why;
}

/** Selects a fix on the page. */
export function selectFix(view: EditorView, f: Fix) {
  view.dispatch({ selection: { anchor: f.from, head: f.to }, effects: EditorView.scrollIntoView(f.from, { y: 'center' }) });
  view.focus();
}

/** Selects the next fix in the text (after the selected one, wrapping), or with `back` the one
 *  before; `first` starts at the top. */
export function nextFix(view: EditorView, family: Family, first = false, back = false): boolean {
  const fixes = shownFixes(view.state, family);
  if (!fixes.length) return false;
  const here = fixAt(view.state, family);
  const pos = here ? here.from : view.state.selection.main.from - 1;
  const before = [...fixes].reverse().find((x) => x.from < (here ? here.from : pos + 1));
  const f = first ? fixes[0] : back ? (before ?? fixes[fixes.length - 1]) : (fixes.find((x) => x.from > pos) ?? fixes[0]);
  selectFix(view, f);
  return true;
}
