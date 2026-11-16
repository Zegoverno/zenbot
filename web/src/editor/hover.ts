// Flipping under the pointer (from Write_On): rest the pointer on text with alternatives and the
// arrow keys flip its versions where it stands (→ ↓ next, ← ↑ back), the text tinted while they
// do. Typing gives the arrows back to the caret until the pointer moves again. The caret stays
// where it was.

import { Prec, StateEffect, StateField } from '@codemirror/state';
import { Decoration, EditorView } from '@codemirror/view';
import { flipTo, groupById, groupsField } from './alternatives.ts';

const setHover = StateEffect.define<string | null>();

const hoverField = StateField.define<string | null>({
  create: () => null,
  update(id, tr) {
    for (const e of tr.effects) if (e.is(setHover)) id = e.value;
    return id;
  },
  provide: (f) => EditorView.decorations.compute([f, groupsField], (state) => {
    const id = state.field(f);
    const g = id ? groupById(state, id) : undefined;
    return g ? Decoration.set([Decoration.mark({ class: 'zw-hover' }).range(g.from, g.to)]) : Decoration.none;
  }),
});

const STEP: Record<string, number> = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 };

function hover(view: EditorView, id: string | null) {
  if (view.state.field(hoverField) !== id) view.dispatch({ effects: setHover.of(id) });
}

export const hoverFlip = [
  hoverField,
  Prec.highest(EditorView.domEventHandlers({
    mousemove(e, view) {
      if (view.state.readOnly) return hover(view, null);
      const pos = view.posAtCoords({ x: e.clientX, y: e.clientY });
      const g = pos === null ? undefined : view.state.field(groupsField).find((x) => x.variants.length > 1 && pos >= x.from && pos < x.to);
      hover(view, g?.id ?? null);
    },
    mouseleave: (_e, view) => hover(view, null),
    keydown(e, view) {
      const id = view.state.field(hoverField);
      if (!id) return false;
      const step = STEP[e.key];
      if (!step || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) {
        if (!['Alt', 'Control', 'Meta', 'Shift'].includes(e.key)) hover(view, null);
        return false;
      }
      const g = groupById(view.state, id);
      if (!g) return hover(view, null), false;
      flipTo(view, g, g.active + step, false);
      e.preventDefault();
      return true;
    },
  })),
];
