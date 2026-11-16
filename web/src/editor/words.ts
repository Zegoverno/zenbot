// A selection made with the mouse snaps to whole words: starting or ending halfway through a word
// takes in the whole word, as in a word processor. Keyboard selection stays exact.

import { EditorSelection, EditorState, type TransactionSpec } from '@codemirror/state';

/** The position moved out to the edge of the word it falls inside, if it does. */
function edge(state: EditorState, pos: number, toEnd: boolean): number {
  const w = state.wordAt(pos);
  return w && w.from < pos && pos < w.to ? (toEnd ? w.to : w.from) : pos;
}

export const wholeWords = EditorState.transactionFilter.of((tr) => {
  if (!tr.selection || !tr.isUserEvent('select.pointer')) return tr;
  const r = tr.selection.main;
  if (r.empty) return tr;
  const state = tr.state;
  const from = edge(state, r.from, false);
  const to = edge(state, r.to, true);
  if (from === r.from && to === r.to) return tr;
  const sel = r.anchor < r.head ? EditorSelection.range(from, to) : EditorSelection.range(to, from);
  return [tr, { selection: EditorSelection.create([sel]), sequential: true } as TransactionSpec];
});
