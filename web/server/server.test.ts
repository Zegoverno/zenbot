import { describe, expect, it, vi } from 'vitest';
import { promises as fs, existsSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import * as versions from './versions.ts';
import { locked, resolvePath, writeAtomic } from './files.ts';
import { parseFixes, parseMarks } from './check.ts';
import { parseReply } from './ai.ts';
import { proposals, queriesIn } from './findnotes.ts';
import type { Hit } from './kernel.ts';
import { cleanName, save as saveUpload } from './uploads.ts';
import { Readable } from 'node:stream';

const tmp = () => fs.mkdtemp(path.join(os.tmpdir(), 'zw-'));

describe('versions', () => {
  it('baseline, open, close, and branch only when nothing is open', async () => {
    const root = await tmp();
    await versions.baseline(root, 'a.md', '# A\n\nOne.\n');
    expect(await versions.state(root, 'a.md', '# A\n\nOne.\n')).toMatchObject({ current: 1, open: false });
    const edited = '# A\n\nTwo.\n';
    expect((await versions.state(root, 'a.md', edited)).open).toBe(true);
    await expect(versions.branch(root, 'a.md', edited, 1)).rejects.toThrow(/close v2/);
    const closed = await versions.close(root, 'a.md', edited, 'second', 'owner');
    expect(closed).toMatch(/## Changelog\n\n- \d{4}-\d\d-\d\d — v2: second\n/);
    expect(await versions.state(root, 'a.md', closed)).toMatchObject({ current: 2, open: false });
    expect(await versions.read(root, 'a.md', 1)).toBe('# A\n\nOne.\n');
  });

  it('a broken index stops instead of being overwritten', async () => {
    const root = await tmp();
    await fs.mkdir(path.join(root, '.versions/a.md'), { recursive: true });
    await fs.writeFile(path.join(root, '.versions/a.md/index.json'), '{oops');
    await expect(versions.state(root, 'a.md', 'x')).rejects.toThrow();
  });
});

describe('files', () => {
  it('keeps paths inside the root', async () => {
    const root = await tmp();
    for (const bad of ['../x.md', '/etc/x.md', '.versions/a.md', 'a.txt', 'a//b.md']) {
      await expect(resolvePath(root, bad)).rejects.toThrow();
    }
    await fs.symlink(os.tmpdir(), path.join(root, 'out'));
    await expect(resolvePath(root, 'out/x.md')).rejects.toThrow();
    expect(await resolvePath(root, 'wiki/A.md')).toBe(path.join(root, 'wiki/A.md'));
  });

  it('runs one write at a time per key, and writes atomically', async () => {
    const order: string[] = [];
    const slow = (tag: string, ms: number) => locked('k', async () => {
      order.push(`${tag}+`);
      await new Promise((r) => setTimeout(r, ms));
      order.push(`${tag}-`);
    });
    await Promise.all([slow('a', 20), slow('b', 0)]);
    expect(order).toEqual(['a+', 'a-', 'b+', 'b-']);
    const root = await tmp();
    const f = path.join(root, 'n/A.md');
    await Promise.all([writeAtomic(f, 'one'), writeAtomic(f, 'two')]);
    expect(['one', 'two']).toContain(await fs.readFile(f, 'utf8'));
  });
});

describe('reading the AI answers', () => {
  it('fixes must name text that is in the note', () => {
    const note = 'This are fine.';
    const reply = 'Sure: {"fixes":[{"find":"This are","replace":"This is","kind":"grammar","why":"agreement"},{"find":"missing","replace":"x","kind":"spelling","why":""},{"find":"fine","replace":"fine","kind":"odd","why":""}]}';
    expect(parseFixes(reply, note)).toEqual([{ find: 'This are', replace: 'This is', kind: 'grammar', why: 'agreement' }]);
  });

  it('cuts are shorter, found in the note, all of kind cut', () => {
    const note = 'It is really very good. We go.';
    const reply = '{"fixes":[{"find":" really very","replace":"","why":"filler"},{"find":"We go.","replace":"We all go now.","why":"longer"},{"find":"nope","replace":""}]}';
    expect(parseFixes(reply, note, 'cut')).toEqual([{ find: ' really very', replace: '', kind: 'cut', why: 'filler' }]);
  });

  it('marks point at text in the note and change nothing', () => {
    const reply = '{"marks":[{"find":"It is","kind":"weak","why":"flat"},{"find":"nope","kind":"long"},{"find":"We go.","kind":"odd"}]}';
    expect(parseMarks(reply, 'It is good. We go.')).toEqual([{ find: 'It is', replace: 'It is', kind: 'weak', why: 'flat' }]);
  });

  it('alternatives are trimmed, unquoted, new and on one line', () => {
    const r = { paragraph: 'A b.', target: 'b', scope: 'word' as const, existing: ['b', 'c'] };
    expect(parseReply('{"alternatives":["“d”"," b ","c","e\\nf","d"]}', r)).toEqual(['d']);
  });

  it('notes link only pages the search found', () => {
    const hits = new Map<string, Hit>([['Angels', { kind: 'wiki', ref: 'Angels', title: 'Angels', snippet: '', relevance: null, path: null }]]);
    const v = { notes: [{ text: 'Backs the angels part.', page: '[[Angels]]', quote: 'pure spirits' }, { text: 'Made up.', page: 'Nowhere' }] };
    expect(proposals(v, hits)).toEqual([{ text: 'Backs the angels part.', page: 'Angels', quote: 'pure spirits' }]);
    expect(queriesIn({ search: ['a', ' a ', 'b', 3] }, 6)).toEqual(['a', 'b']);
  });
});

describe('uploads', () => {
  const body = (chunks: Buffer[], headers: Record<string, string> = {}) => {
    const r = Readable.from(chunks) as Readable & { headers: Record<string, string> };
    r.headers = headers;
    return r as unknown as import('node:http').IncomingMessage;
  };

  it('cleans names: last segment, no dot files, no separators', () => {
    expect(cleanName('../../etc/passwd')).toBe('passwd');
    expect(cleanName('C:\\x\\video.mp4')).toBe('video.mp4');
    expect(cleanName('.bashrc')).toBe('bashrc');
    expect(cleanName('a;b|c$.png')).toBe('a_b_c_.png');
    expect(cleanName('Ação final.MOV')).toBe('Ação final.MOV');
    for (const bad of ['', '..', '/', '...', null, 7]) expect(cleanName(bad)).toBeNull();
    expect(cleanName('x'.repeat(300) + '.mp4')!.length).toBe(120);
  });

  it('saves into the day folder, never overwrites, and caps the size', async () => {
    const inbox = path.join(await tmp(), 'inbox');
    const a = await saveUpload(inbox, '../clip.mp4', body([Buffer.from('one')]));
    expect(path.dirname(a.path)).toBe(path.join(await fs.realpath(path.dirname(inbox)), 'inbox', new Date().toISOString().slice(0, 10)));
    expect(path.basename(a.path)).toBe('clip.mp4');
    const b = await saveUpload(inbox, 'clip.mp4', body([Buffer.from('two')]));
    expect(path.basename(b.path)).toBe('clip-2.mp4');
    expect(await fs.readFile(a.path, 'utf8')).toBe('one');
    await expect(saveUpload(inbox, 'big.bin', body([Buffer.alloc(6), Buffer.alloc(6)]), 10)).rejects.toThrow(/too large/);
    await expect(saveUpload(inbox, 'big2.bin', body([Buffer.alloc(1)], { 'content-length': '11' }), 10)).rejects.toThrow(/too large/);
    await expect(saveUpload(inbox, 'empty.txt', body([]))).rejects.toThrow(/empty/);
    const left = await fs.readdir(path.dirname(a.path));
    expect(left.sort()).toEqual(['clip-2.mp4', 'clip.mp4']);
  });

  it('refuses a day folder that is a symlink out of the inbox', async () => {
    const base = await tmp();
    const inbox = path.join(base, 'inbox');
    await fs.mkdir(inbox);
    await fs.symlink(os.tmpdir(), path.join(inbox, new Date().toISOString().slice(0, 10)));
    await expect(saveUpload(inbox, 'x.txt', body([Buffer.from('x')]))).rejects.toThrow(/plain folder/);
  });
});

describe('home folder writes', () => {
  // home.ts reads its home at import, so the scratch home is set first.
  async function scratch() {
    const dir = await fs.realpath(await tmp());
    process.env.ZEN_WEB_HOME = dir;
    vi.resetModules();
    const home = await import('./home.ts');
    await fs.mkdir(path.join(dir, '.zenbot/global/wiki'), { recursive: true });
    await fs.mkdir(path.join(dir, '.zenbot/agents/zenbot'), { recursive: true });
    await fs.mkdir(path.join(dir, '.zenbot/agents/zenbot/deep'), { recursive: true });
    await fs.mkdir(path.join(dir, 'work'));
    return { dir, home };
  }
  const status = async (p: Promise<unknown>) => p.then(() => 200, (e: { status?: number }) => e.status ?? 500);

  it('creates a file and a folder, never over anything', async () => {
    const { dir, home } = await scratch();
    expect(await home.create(dir, 'a.txt')).toEqual({ path: path.join(dir, 'a.txt') });
    expect(await fs.readFile(path.join(dir, 'a.txt'), 'utf8')).toBe('');
    expect(await home.mkdir('~/work', 'sub')).toEqual({ path: path.join(dir, 'work/sub') });
    expect((await fs.stat(path.join(dir, 'work/sub'))).isDirectory()).toBe(true);
    await fs.writeFile(path.join(dir, 'b.txt'), 'keep');
    expect(await status(home.create(dir, 'b.txt'))).toBe(409);
    expect(await status(home.mkdir(dir, 'b.txt'))).toBe(409);
    expect(await status(home.mkdir(dir, 'work'))).toBe(409);
    expect(await fs.readFile(path.join(dir, 'b.txt'), 'utf8')).toBe('keep');
  });

  it('takes one plain segment, a folder inside home, and no secret names', async () => {
    const { dir, home } = await scratch();
    for (const bad of ['', '.', '..', '../x', 'a/b', '/etc/x', 'a\\b', 'a\0b', 'x'.repeat(256), 7, null])
      for (const act of [home.create, home.mkdir]) expect(await status(act(dir, bad)), String(bad)).toBe(400);
    expect(await status(home.create(dir, 'é'.repeat(128)))).toBe(400); // 256 bytes
    expect(await status(home.create(dir, 'x'.repeat(255)))).toBe(200);
    for (const name of ['.env', '.env.local', 'id_rsa', 'id_ed25519', 'server.pem', 'k.key']) {
      expect(await status(home.create(dir, name)), name).toBe(403);
      expect(await status(home.mkdir(dir, name)), name).toBe(403);
    }
    expect(await status(home.create(path.join(dir, '.ssh'), 'x'))).toBe(403);
    expect(await status(home.create(path.dirname(dir), 'x'))).toBe(403); // outside home
    expect(await status(home.create(path.join(dir, 'missing'), 'x'))).toBe(404);
    await fs.writeFile(path.join(dir, 'f.txt'), '');
    expect(await status(home.create(path.join(dir, 'f.txt'), 'x'))).toBe(400); // not a folder
    expect(existsSync(path.join(dir, '.env'))).toBe(false);
  });

  it('renames in place, never over anything, and leaves the protected alone', async () => {
    const { dir, home } = await scratch();
    await fs.writeFile(path.join(dir, 'work/a.md'), 'text');
    await fs.writeFile(path.join(dir, 'work/b.md'), 'other');
    expect(await home.rename(path.join(dir, 'work/a.md'), 'c.md')).toEqual({ path: path.join(dir, 'work/c.md') });
    expect(await fs.readFile(path.join(dir, 'work/c.md'), 'utf8')).toBe('text');
    expect(existsSync(path.join(dir, 'work/a.md'))).toBe(false);
    expect(await status(home.rename('~/work/c.md', 'b.md'))).toBe(409);
    expect(await fs.readFile(path.join(dir, 'work/b.md'), 'utf8')).toBe('other');
    for (const bad of ['', '..', '../x', 'a/b', '.env', 'id_rsa']) expect(await status(home.rename(path.join(dir, 'work/c.md'), bad)), bad).toBe(bad.includes('.env') || bad === 'id_rsa' ? 403 : 400);
    expect(await status(home.rename(path.join(dir, 'work/none'), 'x'))).toBe(404);
    // Protected: home, ~/.zenbot, and what is within two levels of it.
    for (const p of [dir, '~', '.zenbot', '.zenbot/global', '.zenbot/global/wiki', '.zenbot/agents', '.zenbot/agents/zenbot'])
      expect(await status(home.rename(path.isAbsolute(p) || p === '~' ? p : path.join(dir, p), 'renamed')), p).toBe(403);
    expect(existsSync(path.join(dir, '.zenbot/global/wiki'))).toBe(true);
    // Three levels in is free.
    expect(await status(home.rename(path.join(dir, '.zenbot/agents/zenbot/deep'), 'deeper'))).toBe(200);
    // Renaming to the same name changes nothing.
    expect(await home.rename(path.join(dir, 'work/c.md'), 'c.md')).toEqual({ path: path.join(dir, 'work/c.md') });
  });
});
