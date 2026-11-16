import { describe, expect, it } from 'vitest';
import { EditorState } from '@codemirror/state';
import { dropSpans, fixesField, levelField, placeFixes } from './fixes.ts';
import { ghostsField } from './ghosts.ts';

describe('shrink cuts', () => {
  it('a cut that only drops words becomes the spans to drop; a rewording does not', () => {
    expect(dropSpans(' really very', '')).toEqual([[1, 12]]);
    expect(dropSpans('In which aspect? In the spirit, God', 'In the spirit, God')).toEqual([[0, 16]]);
    expect(dropSpans('a b c d', 'a d')).toEqual([[2, 5]]);
    expect(dropSpans('At the moment angels are created', 'At creation')).toBeNull();
  });

  it('cuts, least needed first, take the smallest level whose goal they fall within', () => {
    const state = EditorState.create({ doc: 'one two three four five six seven eight nine ten', extensions: [ghostsField, levelField, fixesField] });
    const cut = (find: string) => ({ find, replace: '', kind: 'cut' as const, why: '' });
    const placed = placeFixes(state, [cut(' two'), cut(' four'), cut(' six'), cut(' eight'), cut(' ten')], 10);
    expect(placed.map((f) => [state.sliceDoc(f.from, f.to), f.level])).toEqual([['two', 10], ['four', 20], ['six', 30], ['eight', 50], ['ten', 50]]);
  });
});
