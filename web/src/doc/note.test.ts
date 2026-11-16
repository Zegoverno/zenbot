import { describe, expect, it } from 'vitest';
import { cleanFileName, parseNote, scopeOf, serializeNote, titleOf, wordCount, type Note } from './note.ts';

const FM = '---\ntype: idea\n---\n';

describe('parse and serialize', () => {
  it('keeps a plain note byte for byte', () => {
    const text = `${FM}# Title\n\nOne line.\n`;
    const n = parseNote(text);
    expect(n.frontmatter).toBe(FM);
    expect(n.body).toBe('# Title\n\nOne line.\n');
    expect(serializeNote(n)).toBe(text);
  });

  it("reads the owner's old author tag", () => {
    const note: Note = { frontmatter: '', body: '# T\n\nHello there.\n', orphans: [],
      groups: [{ id: 'g', scope: 'word', from: 5, to: 10, active: 0, variants: [{ text: 'Hello', by: 'owner' }, { text: 'Hi', by: 'ai' }] }] };
    const old = serializeNote(note).replace('"by":"owner"', '"by":"ze"');
    expect(old).toContain('"by":"ze"');
    expect(parseNote(old).groups[0].variants[0].by).toBe('owner');
  });

  it('round-trips alternatives and places them by anchor', () => {
    const body = '# T\n\nThe page is the product. The page is the product.\n';
    const second = body.lastIndexOf('The page');
    const note: Note = {
      frontmatter: '',
      body,
      orphans: [],
      groups: [{ id: 'g1', scope: 'sentence', from: second, to: second + 24, active: 0,
        variants: [{ text: 'The page is the product.', by: 'owner' }, { text: 'Ship the words.', by: 'ai', model: 'm' }] }],
    };
    const text = serializeNote(note);
    expect(text).toContain('<!-- zen:alternatives');
    const back = parseNote(text);
    expect(back.body).toBe(body);
    expect(back.groups[0].from).toBe(second);
    expect(back.groups[0].variants[1]).toEqual({ text: 'Ship the words.', by: 'ai', model: 'm' });
    expect(back.groups[0].def).toBeUndefined();
    // Previewing version 0 with version 1 as the default: the file shows the default.
    note.groups[0].def = 1;
    const kept = serializeNote(note);
    expect(kept.startsWith('# T\n\nThe page is the product. Ship the words.\n')).toBe(true);
    const again = parseNote(kept).groups[0];
    expect(again.active).toBe(1);
    expect(again.def).toBeUndefined();
    // …and remembers the version left showing in the editor.
    expect(again.variants[0].text).toBe('The page is the product.');
    expect(again.resume).toBe(0);
    expect(kept).toContain('"active":1,"showing":0,');
    expect(serializeNote(back)).toBe(text);
  });

  it('keeps a group whose text was changed elsewhere as an orphan', () => {
    const note: Note = { frontmatter: '', body: '# T\n\nHello world.\n', orphans: [],
      groups: [{ id: 'g', scope: 'word', from: 11, to: 16, active: 0, variants: [{ text: 'world', by: 'owner' }, { text: 'earth', by: 'owner' }] }] };
    const edited = serializeNote(note).replace('Hello world.', 'Hello there.');
    const back = parseNote(edited);
    expect(back.groups).toHaveLength(0);
    expect(back.orphans).toHaveLength(1);
    expect(serializeNote(back)).toContain('"earth"');
  });

  it('leaves a broken block in the body', () => {
    const text = '# T\n\n<!-- zen:alternatives\n{oops\n-->\n';
    const n = parseNote(text);
    expect(n.body).toBe(text);
    expect(serializeNote(n)).toBe(text);
  });

  it('writes no block when a group has one version left', () => {
    const note: Note = { frontmatter: '', body: '# T\n\nHi.\n', orphans: [],
      groups: [{ id: 'g', scope: 'word', from: 5, to: 7, active: 0, variants: [{ text: 'Hi', by: 'owner' }] }] };
    expect(serializeNote(note)).toBe('# T\n\nHi.\n');
  });
});

describe('helpers', () => {
  it('reads the title', () => {
    expect(titleOf('# Hello world\n\nx')).toBe('Hello world');
    expect(titleOf('## Not h1\n')).toBe('');
    expect(titleOf('text first\n# Late\n')).toBe('');
  });
  it('counts words', () => {
    expect(wordCount("# Title\n\nIt's a **test**, ok.")).toBe(5);
  });
  it('cleans file names', () => {
    expect(cleanFileName(' My idea: v2 ')).toBe('My idea v2.md');
    expect(cleanFileName('../etc/passwd')).toBe('etc passwd.md');
    expect(cleanFileName('Note.md')).toBe('Note.md');
    expect(cleanFileName('   ')).toBe('');
  });
  it('picks the scope from the selection', () => {
    const body = '# T\n\nFirst one. Second one.\nAlone.\n';
    const p = body.indexOf('First');
    expect(scopeOf(body, p, p + 5)).toBe('word');
    expect(scopeOf(body, p, p + 10)).toBe('sentence');
    expect(scopeOf(body, p, p + 22)).toBe('paragraph');
    const a = body.indexOf('Alone');
    expect(scopeOf(body, a, a + 6)).toBe('sentence');
  });
});

describe('ghosts', () => {
  const body = '# T\n\nKeep this. Cut this. Keep that.\n';
  const cutFrom = body.indexOf('Cut this. ');
  const ghost = { id: 'h1', from: cutFrom, to: cutFrom + 'Cut this. '.length };

  it('leaves ghosts out of the file text and brings them back in place', () => {
    const text = serializeNote({ frontmatter: '', body, groups: [], orphans: [], ghosts: [ghost] });
    expect(text.startsWith('# T\n\nKeep this. Keep that.\n')).toBe(true);
    expect(text).toContain('"ghosts":[{"id":"h1","text":"Cut this. "');
    const back = parseNote(text);
    expect(back.body).toBe(body);
    expect(back.ghosts).toEqual([ghost]);
    expect(serializeNote(back)).toBe(text);
  });

  it('keeps groups placed around ghosts, and ghosts after a group at its default', () => {
    const b = '# T\n\nHello world. Cut this. Bye.\n';
    const w = b.indexOf('world');
    const c = b.indexOf('Cut this. ');
    const note = {
      frontmatter: '', body: b, orphans: [],
      ghosts: [{ id: 'h', from: c, to: c + 10 }],
      // 'world' shows, the default is 'earth!' (one longer): the ghost moves with it.
      groups: [{ id: 'g', scope: 'word' as const, from: w, to: w + 5, active: 0, def: 1,
        variants: [{ text: 'world', by: 'owner' as const }, { text: 'earth!', by: 'owner' as const }] }],
    };
    const text = serializeNote(note);
    expect(text.startsWith('# T\n\nHello earth!. Bye.\n')).toBe(true);
    const back = parseNote(text);
    expect(back.body).toBe('# T\n\nHello earth!. Cut this. Bye.\n');
    expect(back.body.slice(back.groups[0].from, back.groups[0].to)).toBe('earth!');
    expect(back.body.slice(back.ghosts![0].from, back.ghosts![0].to)).toBe('Cut this. ');
  });

  it('keeps a ghost it cannot place, and never loses it', () => {
    const text = serializeNote({ frontmatter: '', body, groups: [], orphans: [], ghosts: [ghost] });
    const edited = text.replace('Keep this. Keep that.', 'Something else entirely.');
    const back = parseNote(edited);
    expect(back.ghosts).toEqual([]);
    expect(back.lostGhosts![0].text).toBe('Cut this. ');
    expect(serializeNote(back)).toContain('"text":"Cut this. "');
  });
});

describe('what a round trip must never change', () => {
  it('leaves headings inside ~~~ code blocks alone', () => {
    const text = '# T\n\n~~~\n## Notes\nnot a section\n~~~\n\nEnd.\n';
    const n = parseNote(text);
    expect(n.notes).toBe('');
    expect(serializeNote(n)).toBe(text);
  });

  it('keeps alternatives entries it cannot read, instead of dropping the block', () => {
    const meta = '{"version":1,"groups":[{"id":"x","scope":"word"}],"ghosts":[{"id":"y"}]}';
    const text = `# T\n\nOne line.\n\n<!-- zen:alternatives\n${meta}\n-->\n`;
    const out = serializeNote(parseNote(text));
    expect(out).toContain('"id":"x"');
    expect(out).toContain('"id":"y"');
  });

  it('is stable: writing twice gives the same text', () => {
    const text = '---\ntype: idea\n---\n# T\n\nBody.\n## Notes\nA note.\n## Changelog\n- 2026-10-29 — v2: x\n';
    const once = serializeNote(parseNote(text));
    expect(serializeNote(parseNote(once))).toBe(once);
  });
});
