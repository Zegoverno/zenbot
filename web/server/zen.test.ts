import { describe, expect, it } from 'vitest';
import { promises as fs, readFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import type { IncomingMessage } from 'node:http';
import { allowed, sameOrigin } from './zen.ts';
import { COMMANDS, run } from './commands.ts';

describe('kernel proxy', () => {
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
    for (const p of ['~/.zenbot/token', '~/.ssh/id_ed25519', '~/.ssh', '~/.env', '~/innocent.txt', '~/etc/hostname', '/etc/passwd', '~/../x'])
      await expect(h.read(p), p).rejects.toThrow(/not shown|not found/);
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

  it('keeps reader settings valid and other keys in the file', async () => {
    await fs.writeFile(path.join(home, '.zenbot/reader.json'), '{"wpm": 260, "mine": 1}');
    expect(await h.setReader({ dim: 700, focus: 'line' })).toMatchObject({ wpm: 260, dim: 700, focus: 'line' });
    await expect(h.setReader({ dim: 650 })).rejects.toThrow(/bad setting/);
    await expect(h.setReader({ evil: true })).rejects.toThrow(/bad setting/);
    expect(JSON.parse(readFileSync(path.join(home, '.zenbot/reader.json'), 'utf8')).mine).toBe(1);
  });
});

describe('commands', () => {
  it('runs only listed kernel commands with plain arguments', async () => {
    await expect(run('ask', ['hi'])).rejects.toThrow(/not a command/);
    await expect(run('jobs', ['rm', '; rm -rf ~'])).rejects.toThrow(/not a command/);
    await expect(run('memory', ['sleep', 'now'])).rejects.toThrow(/not a command/);
    await expect(run('status', ['--url', 'http://evil'])).rejects.toThrow(/not a command/);
  });

  // The page's table mirrors zenbot's (crates/zen-client/src/commands.rs).
  const table = path.resolve(import.meta.dirname, '../../crates/zen-client/src/commands.rs');
  it('is the zenbot table without its command-line-only commands', () => {
    const rs = readFileSync(table, 'utf8');
    const theirs = [...rs.matchAll(/cmd\("([\w-]+)", "([^"]*)", "((?:[^"\\]|\\.)*)", (\w+)\)/g)]
      .filter((m) => m[4] !== 'Line')
      .map((m) => ({ name: m[1], args: m[2], help: m[3].replace(/\\"/g, '"'), scope: m[4].toLowerCase() }));
    expect(COMMANDS).toEqual(theirs);
  });
});
