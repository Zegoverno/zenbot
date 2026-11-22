import { describe, expect, it } from 'vitest';
import { cspFor, withCsp, zenNavigation, zenOpen, webLink, SANDBOX, validCommands, validKeyEvent, MAX_COMMANDS } from './frame.ts';

describe('plugin frame', () => {
  it('never gives a plugin UI the page origin', () => {
    expect(SANDBOX.split(' ')).not.toContain('allow-same-origin');
    expect(SANDBOX.split(' ')).not.toContain('allow-top-navigation');
  });

  it('allows no network by default', () => {
    const p = cspFor(undefined);
    expect(p).toContain("default-src 'none'");
    expect(p).toContain("connect-src 'none'");
    expect(p).toContain("frame-src 'none'");
    expect(p).toContain("script-src 'unsafe-inline'; ");
  });

  it('names only the declared domains, and only well-formed ones', () => {
    const p = cspFor({ connectDomains: ['https://api.example.com', "'unsafe-eval'", 'https://x.com; script-src *'], resourceDomains: ['https://*.cdn.example'] });
    expect(p).toContain('connect-src https://api.example.com;');
    expect(p).toContain("script-src 'unsafe-inline' https://*.cdn.example;");
    expect(p).not.toContain('unsafe-eval');
    expect(p).not.toContain('x.com');
  });

  it('puts the policy before every untrusted byte, including misleading heads', () => {
    for (const html of [
      '<!doctype html><html><head><title>t</title></head><body></body></html>',
      '<script src="https://evil.example/a.js"></script><head></head>',
      '<!-- <head> --><img src="https://evil.example/a"><head></head>',
      '<p>x</p>',
    ]) {
      const rendered = withCsp(html, undefined);
      expect(rendered).toMatch(/^<!doctype html><meta http-equiv="Content-Security-Policy"/);
      expect(rendered.endsWith(html)).toBe(true);
      expect(rendered.indexOf('default-src')).toBeLessThan(rendered.indexOf(html));
    }
  });

  it('reads zen://open links to vault-relative paths', () => {
    expect(zenOpen('zen://open?path=wiki%2FGrace.md')).toBe('wiki/Grace.md');
    expect(zenOpen('zen://open?path=../token')).toBeNull();
    expect(zenOpen('zen://open?path=/etc/passwd')).toBeNull();
    expect(zenOpen('zen://open?path=a//b')).toBeNull();
    expect(zenOpen('zen://other?path=a.md')).toBeNull();
    expect(zenOpen('zen://other/open?path=a.md')).toBeNull();
    expect(zenOpen('zen://open/other?path=a.md')).toBeNull();
    expect(zenOpen('zen://open?path=a%00.md')).toBeNull();
    expect(zenOpen('zen://open?path=a%5C..%5Ctoken')).toBeNull();
    expect(zenOpen('https://example.com')).toBeNull();
    expect(webLink('https://example.com')).toBe(true);
    expect(webLink('javascript:alert(1)')).toBe(false);
  });
});

describe('pane navigation', () => {
  it('validates mode and close without accepting ambiguous URLs', () => {
    expect(zenNavigation('zen://open?path=wiki/a.md&mode=edit')).toEqual({ action: 'open', path: 'wiki/a.md', mode: 'edit' });
    expect(zenNavigation('zen://task?id=L%3ATasks.md%3A7&done=1')).toEqual({ action: 'task', id: 'L:Tasks.md:7', done: true });
    for (const u of ['zen://task?id=x&done=2', 'zen://task?id=x&done=1&done=0', 'zen://task?id=x&done=1&extra=y', 'zen://task?id=%00&done=1']) expect(zenNavigation(u)).toBeNull();
    expect(zenNavigation('zen://close')).toEqual({ action: 'close' });
    for (const u of ['zen://close?path=a', 'zen://close#x', 'zen://user@close', 'zen://open?path=a&mode=write',
      'zen://open?path=a&mode=read&mode=edit', 'zen://open?path=a&path=b', 'zen://open?path=a&unknown=x',
      'zen://open?path=%2E%2E/token', 'zen://open?path=a%5Cb', 'zen://close/other']) expect(zenNavigation(u)).toBeNull();
  });
});

describe('plugin commands (zen/commands)', () => {
  const ok = { id: 'new-game', title: 'New game', key: 'n', ring: true, icon: 'plus' };

  it('keeps a valid command, in order, as plain data', () => {
    expect(validCommands({ commands: [ok, { id: 'undo', title: 'Undo', key: 'Mod+z' }] })).toEqual([
      { id: 'new-game', title: 'New game', key: 'n', ring: true, icon: 'plus' },
      { id: 'undo', title: 'Undo', key: 'Mod+z', ring: false },
    ]);
  });
  it('drops what is not a list of commands', () => {
    for (const p of [null, undefined, 'x', 5, [], {}, { commands: 'x' }, { commands: { id: 'a' } }]) expect(validCommands(p)).toEqual([]);
  });
  it('drops a command with a bad id, title, key, ring or icon type', () => {
    const bad = [
      { ...ok, id: 'New' }, { ...ok, id: '' }, { ...ok, id: 'a'.repeat(33) }, { ...ok, id: 'a b' }, { ...ok, id: 5 },
      { ...ok, title: '' }, { ...ok, title: '   ' }, { ...ok, title: 'x'.repeat(61) }, { ...ok, title: 5 }, { ...ok, title: 'a\nb' },
      { ...ok, key: 'N' }, { ...ok, key: 'Ctrl+n' }, { ...ok, key: 5 }, { ...ok, ring: 'yes' }, { ...ok, icon: 5 }, null, 'x', [],
    ];
    for (const c of bad) expect(validCommands({ commands: [c] }), JSON.stringify(c)).toEqual([]);
  });
  it('takes a 60 character title and a 32 character id', () => {
    expect(validCommands({ commands: [{ id: 'a'.repeat(32), title: 'x'.repeat(60) }] })).toHaveLength(1);
  });
  it('falls back to the plugin\'s icon for an unknown icon name', () => {
    expect(validCommands({ commands: [{ ...ok, icon: 'rocket' }] })[0].icon).toBeUndefined();
    expect(validCommands({ commands: [{ ...ok, icon: '<svg onload=x>' }] })[0].icon).toBeUndefined();
  });
  it('keeps at most 30, and each id once', () => {
    const many = Array.from({ length: 40 }, (_, i) => ({ id: `c${i}`, title: `C${i}` }));
    expect(validCommands({ commands: many })).toHaveLength(MAX_COMMANDS);
    expect(validCommands({ commands: [ok, ok] })).toHaveLength(1);
  });
  it('keeps markup as text: the title is returned as it came, never parsed', () => {
    expect(validCommands({ commands: [{ id: 'x', title: '<img src=x onerror=alert(1)>' }] })[0].title).toBe('<img src=x onerror=alert(1)>');
    // Bidi controls would reorder what is drawn around the title: they are dropped.
    expect(validCommands({ commands: [{ id: 'x', title: 'Save\u202Egpj.exe\u2066\u200f' }] })[0].title).toBe('Savegpj.exe');
    expect(validCommands({ commands: [{ id: 'x', title: '\u202e\u2069' }] })).toEqual([]);
  });
});

describe('plugin keys (zen/key)', () => {
  it('reads the KeyboardEvent fields', () => {
    expect(validKeyEvent({ key: 'k', code: 'KeyK', ctrl: true, meta: false, alt: false, shift: false, typing: true })).toEqual({
      key: 'k', code: 'KeyK', ctrlKey: true, metaKey: false, altKey: false, shiftKey: false, typing: true,
    });
    expect(validKeyEvent({ key: 'j' })).toEqual({ key: 'j', code: '', ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, typing: false });
  });
  it('refuses a missing key, a wrong type, or an absurd length', () => {
    for (const p of [null, 'k', 5, {}, { key: '' }, { key: 5 }, { key: 'x'.repeat(21) }, { key: 'k', ctrl: 'yes' }, { key: 'k', typing: 1 }, { key: 'k', code: 5 }, { key: 'k\n' }]) {
      expect(validKeyEvent(p), JSON.stringify(p)).toBeNull();
    }
  });
});
