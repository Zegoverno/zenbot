import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { promises as fs } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const base = await fs.mkdtemp(path.join(os.tmpdir(), 'zen-home-'));
const vault = path.join(base, 'vault');
process.env.ZEN_WEB_HOME = base;
process.env.ZEN_WEB_ROOT = vault;
const home = await import('./home.ts');

beforeAll(async () => {
  await fs.mkdir(vault);
  await fs.mkdir(path.join(base, 'outside'));
  await fs.writeFile(path.join(vault, 'Target.md'), 'target');
  await fs.writeFile(path.join(vault, 'Source.md'), '---\ntype: note\nstatus: draft\ntags: [one, two]\n---\n[[Target|alias]]');
  await fs.symlink(path.join(base, 'outside'), path.join(vault, 'escape'));
  await fs.symlink(path.join(vault, 'Target.md'), path.join(base, 'outside-link'));
});
afterAll(async () => { await fs.rm(base, { recursive: true, force: true }); });

describe('vault filesystem', () => {
  it('restricts writes to the canonical vault, including symlink escapes', async () => {
    await expect(home.create(base, 'bad')).rejects.toMatchObject({ status: 403 });
    await expect(home.mkdir(path.join(vault, 'escape'), 'bad')).rejects.toMatchObject({ status: 403 });
    await expect(home.rename(path.join(vault, 'escape'), 'bad')).rejects.toMatchObject({ status: 403 });
    await expect(home.rename(path.join(base, 'outside-link'), 'bad')).rejects.toMatchObject({ status: 403 });
    await home.mkdir(vault, 'folder');
    await home.create(path.join(vault, 'folder'), 'new.md');
    await home.rename(path.join(vault, 'folder', 'new.md'), 'renamed.md');
    await expect(fs.stat(path.join(vault, 'folder', 'renamed.md'))).resolves.toBeDefined();
  });
  it('scans facets and backlinks within the vault only', async () => {
    expect(await home.facets(vault)).toMatchObject({ type: [{ value: 'note', count: 1 }], tags: [{ value: 'one', count: 1 }, { value: 'two', count: 1 }] });
    expect((await home.facets(vault)).notes).toContainEqual({ path: path.join(vault, 'Source.md'), type: ['note'], status: ['draft'], tags: ['one', 'two'] });
    expect(await home.backlinks(path.join(vault, 'Target.md'))).toMatchObject({ references: [{ path: path.join(vault, 'Source.md'), title: 'Source' }] });
    await expect(home.facets(base)).rejects.toMatchObject({ status: 403 });
    await expect(home.backlinks(path.join(vault, 'escape'))).rejects.toMatchObject({ status: 403 });
  });

  it('reads above home down from /, but never system secrets or other homes', async () => {
    expect((await home.list('/')).entries.map((e) => e.name)).not.toContain('proc');
    expect((await home.list('/usr')).path).toBe('/usr');
    expect(home.readable('/etc/hostname')).toBe(true);
    for (const p of ['/proc/self/environ', '/etc/shadow', '/etc/ssh/ssh_host_ed25519_key', '/root', '/var/lib', '/run', '/tmp/x', '/dev/null', '/etc/ssl/private/k.pem', '/srv/id_rsa'])
      expect(home.readable(p), p).toBe(false);
    await expect(home.read('/proc/self/environ')).rejects.toMatchObject({ status: 403 });
    await expect(home.list('/root')).rejects.toMatchObject({ status: 403 });
  });

  it('keeps zen secrets in home unreadable and writes in the vault only', async () => {
    await fs.mkdir(path.join(base, '.zenbot', 'matrix'), { recursive: true });
    for (const f of ['token', 'env', 'matrix.env', 'zen.sqlite3']) await fs.writeFile(path.join(base, '.zenbot', f), 'secret');
    for (const f of ['token', 'env', 'matrix.env', 'zen.sqlite3', 'matrix']) await expect(home.read(path.join(base, '.zenbot', f))).rejects.toMatchObject({ status: 403 });
    const names = (await home.list(path.join(base, '.zenbot'))).entries.map((e) => e.name);
    expect(names).toEqual([]);
    await expect(home.create('/usr', 'x')).rejects.toMatchObject({ status: 403 });
    await expect(home.mkdir(base, 'x')).rejects.toMatchObject({ status: 403 });
  });
});
