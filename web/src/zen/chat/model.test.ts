import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { parseArchive } from './model.ts';
import * as M from './model.ts';

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

describe('drafts', () => {
  const store = new Map<string, string>();
  beforeEach(() => {
    store.clear();
    vi.stubGlobal('localStorage', { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => void store.set(k, v) });
  });
  afterEach(() => vi.unstubAllGlobals());

  it('keeps unsent text per session and forgets a blank one', () => {
    M.saveDraft('s1', 'half a thought');
    M.saveDraft('new', 'a new chat');
    expect(M.loadDraft('s1')).toBe('half a thought');
    expect(M.loadDraft('new')).toBe('a new chat');
    expect(M.loadDraft('s2')).toBe('');
    M.saveDraft('s1', '  ');
    expect(M.loadDraft('s1')).toBe('');
    expect(JSON.parse(store.get('zen.drafts')!)).toEqual({ new: 'a new chat' });
  });

  it('keeps the 50 latest', () => {
    for (let i = 0; i < 55; i++) M.saveDraft(`s${i}`, `t${i}`);
    M.saveDraft('s5', 'touched');
    const kept = Object.keys(JSON.parse(store.get('zen.drafts')!));
    expect(kept.length).toBe(50);
    expect(kept.at(-1)).toBe('s5');
    expect(M.loadDraft('s4')).toBe('');
    expect(M.loadDraft('s54')).toBe('t54');
  });

  it('survives a broken store', () => {
    store.set('zen.drafts', '[not json');
    expect(M.loadDraft('s1')).toBe('');
    M.saveDraft('s1', 'x');
    expect(M.loadDraft('s1')).toBe('x');
  });
});
