import { describe, expect, it } from 'vitest';
import { cspFor, withCsp, zenOpen, webLink, SANDBOX } from './frame.ts';

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
