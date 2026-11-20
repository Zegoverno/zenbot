import { describe, expect, it } from 'vitest';
import { cspFor, withCsp, zenNavigation, zenOpen, webLink, SANDBOX } from './frame.ts';

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
