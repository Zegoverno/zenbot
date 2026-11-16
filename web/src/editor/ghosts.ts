// Ghosted text: cut, but kept in place, faded, so it can come back. The file and reading leave
// it out (it is kept in the note's comment); the editor shows it.
//
//   Alt+/    ghost the selection, or bring back the ghost at the cursor
//   Alt+K    keep the selection: ghost the rest of its paragraph
//   Alt+Del  delete the ghost at the cursor for good (undo still brings it back)
//
// Ghosts never overlap alternatives: those keys refuse, with a hint, rather than mix the two.

import { StateEffect, StateField, type EditorState, type Range } from '@codemirror/state';
import { Decoration, EditorView, WidgetType, type DecorationSet } from '@codemirror/view';
import { invertedEffects } from '@codemirror/commands';
import { newId, type Ghost } from '../doc/note.ts';
import { groupsField } from './alternatives.ts';

const mapGhost = (g: Ghost, m: { mapPos: (pos: number, assoc?: number) => number }) => ({ ...g, from: m.mapPos(g.from, 1), to: m.mapPos(g.to, -1) });
export const addGhost = StateEffect.define<Ghost>({ map: mapGhost });
const removeGhost = StateEffect.define<Ghost>({ map: mapGhost });

export const ghostsField = StateField.define<Ghost[]>({
  create: () => [],
  update(ghosts, tr) {
    // Typing at a ghost's edge stays outside it; typing inside it is ghosted too.
    let out = tr.docChanged ? ghosts.map((g) => mapGhost(g, tr.changes)).filter((g) => g.to > g.from) : ghosts;
    for (const e of tr.effects) {
      if (e.is(addGhost)) out = [...out.filter((g) => g.id !== e.value.id), e.value];
      else if (e.is(removeGhost)) out = out.filter((g) => g.id !== e.value.id);
    }
    return out === ghosts ? ghosts : [...out].sort((a, b) => a.from - b.from);
  },
  provide: (f) => EditorView.decorations.from(f, (ghosts): DecorationSet => {
    const ranges: Range<Decoration>[] = [];
    for (const g of ghosts) {
      if (g.by !== 'zenbot') {
        ranges.push(Decoration.mark({ class: 'zw-ghost' }).range(g.from, g.to));
        continue;
      }
      ranges.push(Decoration.mark({ class: 'zw-suggest' }).range(g.from, g.to));
      ranges.push(Decoration.widget({ widget: new ByZenbot(g.id), side: 1 }).range(g.to));
    }
    return Decoration.set(ranges, true);
  }),
});

/** The mark at the end of zenbot's text: who wrote it, and keep or drop it. */
class ByZenbot extends WidgetType {
  id: string;
  constructor(id: string) {
    super();
    this.id = id;
  }
  eq(other: ByZenbot) {
    return other.id === this.id;
  }
  toDOM(view: EditorView) {
    const el = document.createElement('span');
    el.className = 'zw-by';
    el.innerHTML = '<span class="who">zenbot</span><button class="keep" title="Keep it (Alt+/): it becomes your text">✓</button><button class="drop" title="Drop it (Alt+Del)">✕</button>';
    el.querySelector('.keep')!.addEventListener('mousedown', (e) => (e.preventDefault(), settle(view, this.id, true)));
    el.querySelector('.drop')!.addEventListener('mousedown', (e) => (e.preventDefault(), settle(view, this.id, false)));
    return el;
  }
  ignoreEvent() {
    return true;
  }
}

/** Keeps zenbot's text (it becomes yours) or drops it. */
export function settle(view: EditorView, id: string, keep: boolean) {
  const g = view.state.field(ghostsField).find((x) => x.id === id);
  if (!g) return;
  view.dispatch(keep
    ? { effects: removeGhost.of(g), userEvent: 'writer.keep' }
    : { changes: { from: g.from, to: g.to }, effects: removeGhost.of(g), userEvent: 'delete.suggestion' });
}

const undoGhosts = invertedEffects.of((tr) => {
  const out: StateEffect<unknown>[] = [];
  for (const e of tr.effects) {
    if (e.is(addGhost)) out.push(removeGhost.of(e.value));
    else if (e.is(removeGhost)) out.push(addGhost.of(e.value));
  }
  return out;
});

export function ghosts() {
  return [ghostsField, undoGhosts];
}

/** Why a key could not do its job, for the hint; null when it did. */
export type Refusal = string | null;

const touchesGroup = (state: EditorState, from: number, to: number) =>
  state.field(groupsField).some((g) => g.from < to && g.to > from);

/** The ghosts at the cursor (edges included), or overlapping the selection. */
function ghostsAt(state: EditorState): Ghost[] {
  const { from, to, empty } = state.selection.main;
  return state.field(ghostsField).filter((g) => (empty ? g.from <= from && from <= g.to : g.from < to && g.to > from));
}

/** Ghosts [from, to), taking in any ghosts it touches, so ghosts never overlap. */
function ghostRanges(view: EditorView, ranges: Array<{ from: number; to: number }>, revive: Ghost[] = []) {
  const state = view.state;
  const effects: StateEffect<unknown>[] = revive.map((g) => removeGhost.of(g));
  const existing = state.field(ghostsField).filter((g) => !revive.includes(g));
  for (let { from, to } of ranges) {
    for (const g of existing) {
      if (g.from <= to && g.to >= from) {
        from = Math.min(from, g.from);
        to = Math.max(to, g.to);
        effects.push(removeGhost.of(g));
      }
    }
    effects.push(addGhost.of({ id: newId(), from, to }));
  }
  // Let go of the selection, so the fade shows.
  const end = view.state.selection.main.to;
  view.dispatch({ effects, selection: { anchor: end }, userEvent: 'writer.ghost' });
}

/** Alt+/: ghost the selection, or bring back the ghost at the cursor. */
export function toggleGhost(view: EditorView): Refusal {
  const state = view.state;
  const sel = state.selection.main;
  const here = ghostsAt(state);
  if (sel.empty) {
    if (!here.length) return 'Select the text to ghost.';
    view.dispatch({ effects: here.map((g) => removeGhost.of(g)), userEvent: 'writer.revive' });
    return null;
  }
  // A selection inside one ghost brings that ghost back.
  if (here.length === 1 && here[0].from <= sel.from && sel.to <= here[0].to) {
    view.dispatch({ effects: removeGhost.of(here[0]), userEvent: 'writer.revive' });
    return null;
  }
  const { from, to } = withSpace(state, sel.from, sel.to);
  if (touchesGroup(state, from, to)) return 'That text has alternatives: remove them first (Alt+A, then Alt+Del).';
  ghostRanges(view, [{ from, to }]);
  return null;
}

/** Takes one space along (after, or before at a paragraph's end or punctuation), so the text
 *  left has no gap. */
function withSpace(state: EditorState, from: number, to: number) {
  const prev = from === 0 ? '\n' : state.sliceDoc(from - 1, from);
  const next = to === state.doc.length ? '\n' : state.sliceDoc(to, to + 1);
  if (/\s/.test(prev) && next === ' ' && !/\s/.test(state.sliceDoc(to - 1, to))) to += 1;
  else if (prev === ' ' && /[\n.,;:!?)]/.test(next)) from -= 1;
  return { from, to };
}

/** Alt+K: keep the selection, ghost the rest of its paragraph. */
export function keepOnly(view: EditorView): Refusal {
  const state = view.state;
  const sel = state.selection.main;
  if (sel.empty) return 'Select what to keep; the rest of the paragraph fades.';
  const line = state.doc.lineAt(sel.from);
  if (sel.to > line.to) return 'Select within one paragraph.';
  const parts = [{ from: line.from, to: sel.from }, { from: sel.to, to: line.to }].filter((r) => state.sliceDoc(r.from, r.to).trim());
  if (!parts.length) return 'That is the whole paragraph already.';
  if (parts.some((r) => touchesGroup(state, r.from, r.to))) return 'The rest of that paragraph has alternatives: remove them first (Alt+A, then Alt+Del).';
  // Whatever was ghosted inside the selection comes back: you are keeping it.
  const revive = state.field(ghostsField).filter((g) => g.from < sel.to && g.to > sel.from);
  ghostRanges(view, parts, revive);
  return null;
}

/** Alt+Del: delete the ghost at the cursor for good. False when there is none (normal Alt+Del). */
export function deleteGhost(view: EditorView): boolean {
  const here = ghostsAt(view.state);
  if (!here.length) return false;
  view.dispatch({
    changes: here.map((g) => ({ from: g.from, to: g.to })),
    effects: here.map((g) => removeGhost.of(g)),
    userEvent: 'delete.ghost',
  });
  return true;
}

/** Brings back the ghost at the cursor (keeps a zenbot suggestion). False when there is none. */
export function reviveGhost(view: EditorView): boolean {
  const here = ghostsAt(view.state);
  if (!here.length) return false;
  view.dispatch({ effects: here.map((g) => removeGhost.of(g)), userEvent: 'writer.revive' });
  return true;
}

/** Selects the next ghost in the text (or the one before), wrapping. False when there are none. */
export function stepGhost(view: EditorView, d: 1 | -1): boolean {
  const all = [...view.state.field(ghostsField)].sort((a, b) => a.from - b.from);
  if (!all.length) return false;
  const pos = view.state.selection.main.from;
  const g = d > 0 ? (all.find((x) => x.from > pos) ?? all[0]) : ([...all].reverse().find((x) => x.from < pos) ?? all[all.length - 1]);
  selectGhost(view, g);
  return true;
}

export function selectGhost(view: EditorView, g: { from: number; to: number }) {
  view.dispatch({ selection: { anchor: g.from, head: g.to }, effects: EditorView.scrollIntoView(g.from, { y: 'center' }) });
  view.focus();
}

/** The ghost at the cursor, if any. */
export const ghostHere = (state: EditorState): Ghost | undefined => ghostsAt(state)[0];

/** Ghosts [from, to) and a space beside it (a cut from Shrink, say), unless it has alternatives. */
export function ghostRange(view: EditorView, from: number, to: number): Refusal {
  if (touchesGroup(view.state, from, to)) return 'That text has alternatives: remove them first.';
  ghostRanges(view, [withSpace(view.state, from, to)]);
  return null;
}
