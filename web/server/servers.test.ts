import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { appName, list, parseListening, restart, shortCommand, stop, userUnit } from './servers.ts';

const HEADER = '  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n';
const row = (n: number, local: string, st: string, inode: number) => `   ${n}: ${local} 00000000:0000 ${st} 00000000:00000000 00:00000000 00000000  1000        0 ${inode} 1 0000000000000000 100 0 0 10 0\n`;
const TCP = HEADER + row(0, '0100007F:1435', '0A', 5001) + row(1, '0100007F:D4F2', '01', 5002) + row(2, '00000000:0050', '0A', 5003) + row(3, '0100007F:1133', '0A', 5004);
const TCP6 = HEADER + row(0, '00000000000000000000000001000000:1435', '0A', 5005) + row(1, '00000000000000000000000000000000:3039', '0A', 5006);

describe('reading /proc/net/tcp', () => {
  it('keeps listening sockets (state 0A) with port and inode', () => {
    expect(parseListening(TCP)).toEqual([{ port: 5173, inode: 5001 }, { port: 80, inode: 5003 }, { port: 4403, inode: 5004 }]);
    expect(parseListening(TCP6)).toEqual([{ port: 5173, inode: 5005 }, { port: 12345, inode: 5006 }]);
    expect(parseListening('')).toEqual([]);
  });
  it('finds the user service in a cgroup, and only a service', () => {
    expect(userUnit('0::/user.slice/user-1000.slice/user@1000.service/app.slice/zengame-web.service\n')).toBe('zengame-web.service');
    expect(userUnit('0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-vite-1.scope\n')).toBeNull();
    expect(userUnit('0::/user.slice/user-1000.slice/session-4.scope\n')).toBeNull();
    expect(userUnit('0::/system.slice/zenbot.service\n')).toBeNull();
  });
  it('shows the command short: program name, relative and home paths, cut to size', () => {
    expect(shortCommand(['/usr/bin/python3', '-m', 'http.server', '8765'], '/x', '/home/u')).toBe('python3 -m http.server 8765');
    expect(shortCommand(['node', '/home/u/app/node_modules/vite/bin/vite.js', '--port', '5173'], '/home/u/app', '/home/u')).toBe('node ./node_modules/vite/bin/vite.js --port 5173');
    expect(shortCommand(['/usr/bin/python3', '-I', '/home/u/work/serve.py', '5191'], '/srv', '/home/u')).toBe('python3 -I ~/work/serve.py 5191');
    const long = shortCommand(['x', 'a'.repeat(300)], '/', '/h');
    expect(long.length).toBe(120);
    expect(long.endsWith('…')).toBe(true);
  });
});

let dir: string;
beforeAll(() => { dir = mkdtempSync(path.join(os.tmpdir(), 'zen-servers-')); });
afterAll(() => rmSync(dir, { recursive: true, force: true }));

const put = (rel: string, text: string) => { mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true }); writeFileSync(path.join(dir, rel), text); };

describe('naming an app', () => {
  it('uses the nearest package.json name, without its scope, up to home', async () => {
    put('home/a/package.json', JSON.stringify({ name: '@acme/shop' }));
    put('home/a/web/src/x', '');
    put('home/a/pkgless/package.json', '{"private":true}');
    expect(await appName(path.join(dir, 'home/a/web/src'), null, path.join(dir, 'home'))).toBe('shop');
    expect(await appName(path.join(dir, 'home/a/pkgless'), null, path.join(dir, 'home'))).toBe('shop');
  });
  it('falls back to the git folder, the unit, then the cwd folder', async () => {
    const home = path.join(dir, 'home');
    mkdirSync(path.join(home, 'repo/.git'), { recursive: true });
    mkdirSync(path.join(home, 'repo/build/web'), { recursive: true });
    mkdirSync(path.join(home, 'plain/site'), { recursive: true });
    expect(await appName(path.join(home, 'repo/build/web'), 'x.service', home)).toBe('repo');
    expect(await appName(path.join(home, 'plain/site'), 'blog.service', home)).toBe('blog');
    expect(await appName(path.join(home, 'plain/site'), null, home)).toBe('site');
    // The unit a restart makes isn't a name.
    expect(await appName(path.join(home, 'plain/site'), 'zen-preview-3456-abc.service', home)).toBe('site');
  });
  it('does not look above home', async () => {
    put('up/package.json', JSON.stringify({ name: 'above-home' }));
    mkdirSync(path.join(dir, 'up/home/proj'), { recursive: true });
    expect(await appName(path.join(dir, 'up/home/proj'), null, path.join(dir, 'up/home'))).toBe('proj');
  });
});

/** A fake /proc: the sockets of 4 processes, one of them a system service, one a worker of another. */
function fakeProc(): string {
  const proc = path.join(dir, 'proc');
  const home = path.join(dir, 'home');
  rmSync(proc, { recursive: true, force: true });
  mkdirSync(path.join(proc, 'net'), { recursive: true });
  writeFileSync(path.join(proc, 'net/tcp'), TCP);
  writeFileSync(path.join(proc, 'net/tcp6'), TCP6);
  const mk = (pid: number, cgroup: string, argv: string[], cwd: string, ppid: number, inodes: number[]) => {
    const d = path.join(proc, String(pid));
    mkdirSync(path.join(d, 'fd'), { recursive: true });
    writeFileSync(path.join(d, 'cgroup'), cgroup);
    writeFileSync(path.join(d, 'cmdline'), argv.join('\0') + '\0');
    writeFileSync(path.join(d, 'stat'), `${pid} (a b) S ${ppid} 1 1 0\n`);
    symlinkSync(cwd, path.join(d, 'cwd'));
    inodes.forEach((i, n) => symlinkSync(`socket:[${i}]`, path.join(d, 'fd', String(n + 3))));
    symlinkSync('/dev/null', path.join(d, 'fd', '0'));
  };
  mk(100, '0::/user.slice/user-1000.slice/user@1000.service/app.slice/x.service\n', ['node', 'vite.js', '--port', '5173'], path.join(home, 'a/web/src'), 1, [5001, 5005]);
  mk(101, '0::/user.slice/user-1000.slice/user@1000.service/app.slice/x.service\n', ['node', 'vite.js', '--port', '5173'], path.join(home, 'a/web/src'), 100, [5001]);
  mk(102, '0::/system.slice/shelley.service\n', ['shelley'], home, 1, [5003]);
  mk(103, '0::/user.slice/user-1000.slice/session-2.scope\n', ['/usr/bin/python3', '-m', 'http.server', '4403'], path.join(home, 'plain/site'), 1, [5004]);
  mk(104, '0::/user.slice/user-1000.slice/session-2.scope\n', ['python3', '-m', 'http.server', '12345'], path.join(home, 'plain/site'), 1, [5006]);
  return proc;
}

describe('the list', () => {
  it('lists user dev servers by app, one per port, without system services or blocked ports', async () => {
    const proc = fakeProc();
    const base = { proc, uid: process.getuid!(), home: path.join(dir, 'home') };
    const got = await list({ ...base, blocked: [] });
    expect(got).toEqual([
      { port: 4403, name: 'site', command: 'python3 -m http.server 4403', cwd: path.join(dir, 'home/plain/site'), unit: null, pid: 103 },
      { port: 5173, name: 'shop', command: 'node vite.js --port 5173', cwd: path.join(dir, 'home/a/web/src'), unit: 'x.service', pid: 100 },
    ]);
    expect((await list({ ...base, blocked: [4403] })).map((s) => s.port)).toEqual([5173]);
    expect((await list({ ...base, blocked: [], includeSystem: true })).map((s) => s.port)).toEqual([4403, 5173]);
    expect(await list({ ...base, blocked: [], uid: process.getuid!() + 1 })).toEqual([]);
  });

  it('stops and restarts only what is listed, a unit through systemctl', async () => {
    const proc = fakeProc();
    const calls: string[][] = [];
    const o = { proc, uid: process.getuid!(), home: path.join(dir, 'home'), blocked: [4403], exec: async (c: string, a: string[]) => { calls.push([c, ...a]); } };
    await expect(stop(80, o)).rejects.toMatchObject({ status: 400 });
    await expect(stop(4403, o)).rejects.toMatchObject({ status: 400 });
    await expect(stop(7777, o)).rejects.toMatchObject({ status: 404 });
    await expect(restart('abc', o)).rejects.toMatchObject({ status: 400 });
    // 5173 is a unit: systemctl stops it (the fake /proc keeps listing it, so the port never frees).
    await expect(stop(5173, { ...o, blocked: [] })).rejects.toMatchObject({ status: 500 });
    expect(calls).toEqual([['systemctl', '--user', 'stop', 'x.service']]);
    calls.length = 0;
    expect((await restart(5173, { ...o, blocked: [] })).port).toBe(5173);
    expect(calls).toEqual([['systemctl', '--user', 'restart', 'x.service']]);
  }, 20000);
});
