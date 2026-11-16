import { describe, expect, it } from 'vitest';
import { readProps, removeProp, setProp } from './props.ts';

describe('properties', () => {
  const fm = '---\ntype: idea # what kind\naliases: [a, b]\ndone: true\n---\n';

  it('reads rows, lists joined', () => {
    expect(readProps(fm)).toEqual([
      { key: 'type', value: 'idea', list: false, fixed: false },
      { key: 'aliases', value: 'a, b', list: true, fixed: false },
      { key: 'done', value: 'true', list: false, fixed: false },
    ]);
  });

  it('changes one property, keeping comments, list style and types', () => {
    const out = setProp(fm, 'aliases', 'a, b, c', true);
    expect(out).toContain('type: idea # what kind');
    expect(out).toContain('aliases: [a, b, c]');
    expect(out).toContain('done: true');
    expect(setProp(fm, 'n', '3', false)).toContain('n: 3\n');
  });

  it('leaves broken YAML alone', () => {
    const bad = '---\ntype: [idea\n---\n';
    expect(readProps(bad)).toBeNull();
    expect(setProp(bad, 'type', 'x', false)).toBe(bad);
  });

  it('removing the last property removes the front matter', () => {
    expect(removeProp('---\ntype: idea\n---\n', 'type')).toBe('');
  });
});
