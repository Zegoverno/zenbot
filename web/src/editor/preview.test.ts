import { describe, expect, it } from 'vitest';
import { accented, stacked } from './preview.ts';

describe('display math', () => {
  it('sets one formula per row: line breaks and \\qquad outside braces', () => {
    expect(stacked('a=1,\\qquad\nb=2,\\qquad c=3.')).toBe('\\begin{gathered}a=1,\\\\b=2,\\\\c=3.\\end{gathered}');
    expect(stacked('x^{a\\qquad b}')).toBe('x^{a\\qquad b}');
    expect(stacked('a\\\\b')).toBe('a\\\\b');
    expect(stacked('\\frac{a}{b}')).toBe('\\frac{a}{b}');
  });
  it('sets accented words as text, leaving \\text{} alone', () => {
    expect(accented('IP=\\frac{Orçamento}{Receitas}')).toBe('IP=\\frac{\\text{Orçamento}}{Receitas}');
    expect(accented('\\text{Cotação} + x')).toBe('\\text{Cotação} + x');
  });
});
