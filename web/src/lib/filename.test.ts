import { describe, expect, it } from 'vitest';
import { cleanFileName } from './filename.ts';

describe('Markdown filename', () => {
  it('keeps a plain filename and strips path punctuation', () => {
    expect(cleanFileName('  My idea.md ')).toBe('My idea.md');
    expect(cleanFileName('../a/b\\c')).toBe('a b c.md');
    expect(cleanFileName('...')).toBe('');
    expect(cleanFileName('')).toBe('');
  });
});
