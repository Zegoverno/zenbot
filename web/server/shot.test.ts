import { describe, expect, it } from 'vitest';
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { findChromium, readShot } from './shot.ts';

describe('preview shots', () => {
  it('shoot only /preview/ pages of this server, at a sane size', () => {
    expect(readShot({ url: '/preview/5180/a?b=1', selector: 'main > h1', width: 1056.4, height: 50 })).toEqual({ url: '/preview/5180/a?b=1', selector: 'main > h1', width: 1056, height: 200 });
    for (const url of ['/api/fs/read?path=x', 'http://evil.example/', '//evil.example/preview/5180/', '/preview/80/', '/preview/5180/a b', '/preview/5180/\nx']) {
      expect(readShot({ url, selector: '', width: 800, height: 600 }), url).toBeNull();
    }
    expect(readShot({ url: '/preview/5180/', selector: '', width: 'wide', height: 600 })).toBeNull();
    expect(readShot({ url: '/preview/5180/', selector: '', width: 99999, height: 600 })!.width).toBe(4000);
  });
  it("finds Playwright's newest headless shell first", async () => {
    const root = mkdtempSync(path.join(tmpdir(), 'pw-'));
    for (const [d, f] of [['chromium-1300/chrome-linux64', 'chrome'], ['chromium_headless_shell-1200/chrome-headless-shell-linux64', 'chrome-headless-shell'], ['chromium_headless_shell-1250/chrome-headless-shell-linux64', 'chrome-headless-shell']]) {
      mkdirSync(path.join(root, d), { recursive: true });
      writeFileSync(path.join(root, d, f), '');
    }
    expect(await findChromium({ PLAYWRIGHT_BROWSERS_PATH: root })).toBe(path.join(root, 'chromium_headless_shell-1250/chrome-headless-shell-linux64/chrome-headless-shell'));
    expect(await findChromium({ ZEN_WEB_CHROMIUM: '/x/chrome' })).toBe('/x/chrome');
    expect(await findChromium({ PLAYWRIGHT_BROWSERS_PATH: path.join(root, 'none') })).toBeNull();
  });
});
