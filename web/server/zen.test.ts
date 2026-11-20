import { describe, expect, it } from 'vitest';
import { promises as fs, readFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import type { IncomingMessage } from 'node:http';
import { allowed, sameOrigin } from './zen.ts';
import { list, run } from './commands.ts';

describe('kernel proxy', () => {
  it('exposes redesign routes without granting arbitrary kernel access', () => {
    for (const [m, route] of [['GET','prefs'],['PATCH','prefs'],['GET','ideas'],['GET','tasks/meta'],['POST','tasks'],['PATCH','tasks/L%3AProjects%2Fself%2Ftasks.md%3A1'],['DELETE','tasks/abc123'],['PUT','prompt-files/USER.md'],['POST','sessions/abc/stop']]) expect(allowed(m,route)).toBe(true);
    for (const route of ['prompt-files/SOUL.md','prompt-files/../token','tasks/../token']) expect(allowed('PUT',route)).toBe(false);
  });

  it('lets through only what the terminal app uses', () => {
    expect(allowed('GET', 'board')).toBe(true);
    expect(allowed('GET', 'sessions/abc-123')).toBe(true);
    expect(allowed('PATCH', 'sessions/abc-123')).toBe(true);
    expect(allowed('POST', 'tasks/x1/agent')).toBe(true);
    expect(allowed('DELETE', 'jobs/daily-plan')).toBe(true);
    expect(allowed('POST', 'complete')).toBe(false); // one-off completions stay server-side
    expect(allowed('POST', 'brain')).toBe(false);
    expect(allowed('POST', 's1/replay')).toBe(false);
    expect(allowed('DELETE', 'sessions/abc')).toBe(false);
    expect(allowed('GET', 'sessions/../token')).toBe(false);
    expect(allowed('GET', 'sessions/a/b')).toBe(false);
  });

  it('reaches agents by their kernel names only', () => {
    expect(allowed('GET', 'agents')).toBe(true);
    expect(allowed('POST', 'agents')).toBe(true);
    for (const m of ['GET', 'PUT', 'DELETE']) {
      expect(allowed(m, 'agents/rex')).toBe(true);
      expect(allowed(m, 'agents/a-1')).toBe(true);
      expect(allowed(m, `agents/a${'b'.repeat(31)}`)).toBe(true);
    }
    expect(allowed('PATCH', 'agents/rex')).toBe(false);
    expect(allowed('POST', 'agents/rex')).toBe(false);
    expect(allowed('PUT', 'agents')).toBe(false);
    expect(allowed('DELETE', 'agents')).toBe(false);
    for (const bad of ['Rex', '1rex', '-rex', 're_x', 're.x', '..', 'rex/', 'rex/soul', '../token', `a${'b'.repeat(32)}`, ''])
      for (const m of ['GET', 'PUT', 'DELETE']) expect(allowed(m, `agents/${bad}`), `${m} agents/${bad}`).toBe(false);
  });

  it('reaches plugins by their names, and only the host routes', () => {
    expect(allowed('GET', 'plugins')).toBe(true);
    expect(allowed('GET', 'plugins/marketplace')).toBe(true);
    expect(allowed('PUT', 'plugins/defaults')).toBe(true);
    expect(allowed('POST', 'plugins/install')).toBe(true);
    expect(allowed('PATCH', 'plugins/zenread')).toBe(true);
    expect(allowed('DELETE', 'plugins/zen-sudoku')).toBe(true);
    expect(allowed('GET', 'plugins/zenread/resource')).toBe(true);
    expect(allowed('POST', 'plugins/zenread/call')).toBe(true);
    expect(allowed('GET', 'plugins/zenread/call')).toBe(false);
    expect(allowed('POST', 'plugins/zenread/resource')).toBe(false);
    expect(allowed('POST', 'plugins/zenread')).toBe(false);
    expect(allowed('DELETE', 'plugins')).toBe(false);
    for (const bad of ['Zen', 'z', '..', 'zen_read', 'zen.read', `a${'b'.repeat(40)}`, 'zenread/../x'])
      expect(allowed('POST', `plugins/${bad}/call`), bad).toBe(false);
  });

  it('opens a session socket only for pages of this site', () => {
    const req = (h: Record<string, string>) => ({ headers: h }) as unknown as IncomingMessage;
    expect(sameOrigin(req({ origin: 'https://zen.example.com', host: 'zen.example.com' }))).toBe(true);
    expect(sameOrigin(req({ origin: 'https://evil.example', host: 'zen.example.com' }))).toBe(false);
    expect(sameOrigin(req({ host: 'zen.example.com' }))).toBe(false);
    expect(sameOrigin(req({ origin: 'null', host: 'zen.example.com' }))).toBe(false);
  });
});

describe('home folder', async () => {
  const home = await fs.mkdtemp(path.join(os.tmpdir(), 'zw-home-'));
  process.env.ZEN_WEB_HOME = home;
  process.env.ZEN_WEB_ROOT = path.join(home, '.zenbot/global');
  const h = await import('./home.ts');
  await fs.mkdir(path.join(home, '.zenbot/global/wiki'), { recursive: true });
  await fs.mkdir(path.join(home, '.ssh'));
  await fs.writeFile(path.join(home, '.zenbot/token'), 'secret');
  await fs.writeFile(path.join(home, '.ssh/id_ed25519'), 'key');
  await fs.writeFile(path.join(home, '.zenbot/global/wiki/God.md'), '# God\n');
  await fs.writeFile(path.join(home, 'notes.txt'), 'hi');
  await fs.writeFile(path.join(home, '.env'), 'X=1');
  await fs.symlink(path.join(home, '.zenbot/token'), path.join(home, 'innocent.txt'));
  await fs.symlink('/etc', path.join(home, 'etc'));

  it('never shows a secret, by name, folder or symlink', async () => {
    for (const p of ['~/.zenbot/token', '~/.ssh/id_ed25519', '~/.ssh', '~/.env', '~/innocent.txt', '/etc/shadow', '~/../x'])
      await expect(h.read(p), p).rejects.toThrow(/not shown|not found/);
    // Files reads from / down: a symlink out of home reads like the path it points to.
    await expect(h.read('~/etc/hostname')).resolves.toBeTruthy();
    const names = (await h.list('~')).entries.map((e) => e.name);
    expect(names).toContain('notes.txt');
    expect(names).not.toContain('.ssh');
    expect(names).not.toContain('.env');
    expect((await h.list('~/.zenbot')).entries.map((e) => e.name)).not.toContain('token');
  });

  it('reads text, finds wiki pages and gives the vault path', async () => {
    expect((await h.read('~/notes.txt')).text).toBe('hi');
    const god = await h.locate('[[god]]');
    expect(god?.path).toBe(path.join(home, '.zenbot/global/wiki/God.md'));
    expect((await h.read(god!.path)).vault).toBe('wiki/God.md');
    expect((await h.locate('wiki/God'))?.path).toBe(god!.path);
    expect((await h.locate('~/notes.txt:12'))?.path).toBe(path.join(home, 'notes.txt'));
    expect(await h.locate('https://example.com')).toBeNull();
  });

});

describe('commands', () => {
  it('runs only listed kernel commands with plain arguments', async () => {
    await expect(run('ask', ['hi'])).rejects.toThrow(/not a command/);
    await expect(run('jobs', ['rm', '; rm -rf ~'])).rejects.toThrow(/not a command/);
    await expect(run('memory', ['sleep', 'now'])).rejects.toThrow(/not a command/);
    await expect(run('status', ['--url', 'http://evil'])).rejects.toThrow(/not a command/);
  });

  // The table comes from `zen --commands` (crates/zen/src/main.rs tests what it prints).
  it('takes the table from zen --commands', async () => {
    const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'zen-bin-'));
    const bin = path.join(dir, 'zen');
    const table = [{ name: 'new', args: '[@agent] [title]', help: 'start a new session', scope: 'session' }];
    await fs.writeFile(bin, `#!/bin/sh\n[ "$1" = --commands ] && echo '${JSON.stringify({ commands: table })}'\n`, { mode: 0o755 });
    expect(await list(bin)).toEqual(table);
    await expect(list(path.join(dir, 'missing'))).rejects.toThrow(/zen --commands/);
  });
});
