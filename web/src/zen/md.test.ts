import { describe, expect, it } from 'vitest';
import type { SyntaxNode } from '@lezer/common';
import { parser, rowCells } from './md.ts';

/** Each row's cells as text, '' for an empty one. */
function cells(src: string): string[][] {
  const rows: string[][] = [];
  parser.parse(src).iterate({
    enter(n) {
      if (n.name !== 'TableHeader' && n.name !== 'TableRow') return;
      rows.push(rowCells(n.node as SyntaxNode, src).map((c) => (c ? src.slice(c.from, c.to) : '')));
      return false;
    },
  });
  return rows;
}

describe('table rows', () => {
  it('keep empty cells in their column', () => {
    expect(cells('| | A | B |\n|---|---|---|\n| x | | z |\n|  |  | w |\n')).toEqual([
      ['', 'A', 'B'],
      ['x', '', 'z'],
      ['', '', 'w'],
    ]);
  });
  it('read rows without outer pipes (a leading pipe only opens the row)', () => {
    expect(cells('A | B\n--|--\nx | y\n | z\n')).toEqual([
      ['A', 'B'],
      ['x', 'y'],
      ['z'],
    ]);
  });
});
