import { describe, expect, it } from 'vitest';
import { parseArchive } from './model.ts';

// The same cases as zen-client's commands.rs: the web reads /archive as the other clients do.
describe('parseArchive', () => {
  it('reads [outcome] [rating] [note]', () => {
    expect(parseArchive('')).toEqual({ outcome: null, rating: null, note: '' });
    expect(parseArchive('Partial tests still red')).toEqual({ outcome: 'partial', rating: null, note: 'tests still red' });
    expect(parseArchive('done 8')).toEqual({ outcome: 'done', rating: 8, note: '' });
    expect(parseArchive('done 10 shipped it')).toEqual({ outcome: 'done', rating: 10, note: 'shipped it' });
    expect(parseArchive('not-done 1')).toEqual({ outcome: 'not-done', rating: 1, note: '' });
    expect(parseArchive('7')).toEqual({ outcome: null, rating: 7, note: '' });
    expect(parseArchive('done 5 files changed')).toEqual({ outcome: 'done', rating: 5, note: 'files changed' });
  });
  it('takes a number later, or a decimal, as the note', () => {
    expect(parseArchive('done tests 7 pass')).toEqual({ outcome: 'done', rating: null, note: 'tests 7 pass' });
    expect(parseArchive('done 7.5')).toEqual({ outcome: 'done', rating: null, note: '7.5' });
  });
  it('refuses the wrong shapes', () => {
    for (const bad of ['maybe', 'done 0', 'done 11', '99 note', '0', 'done 300', 'a note without outcome', '-']) expect(parseArchive(bad), bad).toBeNull();
  });
});
