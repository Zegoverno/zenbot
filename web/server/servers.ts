// The Preview tab's list of dev servers: what listens on this machine's ports 3000–9999, named by
// app (not port), each one stoppable and restartable, so the owner never types a port.
//
// Source: the kernel's own tables, no tools and no dependencies. `/proc/net/tcp{,6}` give the
// listening sockets (state 0A) and their inodes; `/proc/<pid>/fd` links say which process holds each
// inode. Only processes of this server's own user count (others' fds can't be read, and are skipped
// by the uid check as well). One entry per port: IPv4 and IPv6 sockets of a port, and the workers
// of a forking server, collapse into the root-most process holding it.
//
// What is never listed, so never touched: zen's own ports (`blocked`: the web app's, the kernel's),
// this very process and its own service unit, and anything under `/system.slice/` in its cgroup
// (zend, searxng, shelley… are system services, not dev servers), except zen's own service
// (`zenbot.service`): dev servers zenbot starts from its shell live there, and the kernel's own port
// is blocked anyway. `includeSystem` lifts the cgroup rule for tests, whose stand-in servers run
// wherever the test was started.
//
// The name is the app's: the nearest package.json "name" from the process's cwd up to $HOME (scope
// stripped), else the git repo's folder, else its systemd user unit, else the cwd's folder. The
// unit is set only when the process runs in a user service (`user@<uid>.service/…/x.service`), not
// in a session scope.
//
// Stop and restart look the port up again on every call and never take a pid from the client; a
// port that isn't listed answers 404. A service unit is stopped or restarted with
// `systemctl --user`. Any other process gets SIGTERM, then SIGKILL after 3 s; to restart it, its
// argv, cwd and environment are read from /proc first, and it is started again with `systemd-run
// --user` as a transient unit (`zen-preview-<port>-<time>`): as a child of the web server it would
// die with the web server's restarts. Commands run with execFile and argument arrays, no shell.

import { execFile } from 'node:child_process';
import { promises as fs } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { HttpError } from './files.ts';
import { MAX_PORT, MIN_PORT, targetPort } from './preview.ts';

export type Server = { port: number; name: string; command: string; cwd: string; unit: string | null; pid: number };
export type Options = {
  /** Ports never listed or touched: zen's own. */
  blocked: number[];
  /** Test seams. */
  proc?: string;
  uid?: number;
  home?: string;
  includeSystem?: boolean;
  /** Runs a command (execFile semantics); the default really runs it. */
  exec?: (cmd: string, args: string[]) => Promise<void>;
};

const STOP_GRACE_MS = 3000;
const FREE_WAIT_MS = 5000;
const BACK_WAIT_MS = 10000;
const MAX_COMMAND = 120;
/** Variables of the old process's service that must not carry over to the new one. */
const SESSION_ENV = /^(INVOCATION_ID|JOURNAL_STREAM|SYSTEMD_EXEC_PID|NOTIFY_SOCKET|WATCHDOG_PID|WATCHDOG_USEC|LISTEN_PID|LISTEN_FDS|LISTEN_FDNAMES|MAINPID|MANAGED_OOM.*|_)$/;

/** zen's own service: what zenbot starts from its shell runs here, so it counts as a dev server. */
const AGENT_SLICE = '/system.slice/zenbot.service';

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

// ---- reading /proc ----

export type Socket = { port: number; inode: number };

/** The listening sockets (state 0A) of a /proc/net/tcp or tcp6 text, with their ports and inodes. */
export function parseListening(text: string): Socket[] {
  const out: Socket[] = [];
  for (const line of text.split('\n').slice(1)) {
    const f = line.trim().split(/\s+/);
    if (f.length < 10 || f[3] !== '0A') continue;
    const port = parseInt(f[1].slice(f[1].lastIndexOf(':') + 1), 16);
    const inode = Number(f[9]);
    if (Number.isInteger(port) && inode > 0) out.push({ port, inode });
  }
  return out;
}

/** The user service a cgroup path puts the process in (`x.service` under `user@<uid>.service`), or null. */
export function userUnit(cgroup: string): string | null {
  const line = cgroup.split('\n').find((l) => l.startsWith('0::')) ?? cgroup.split('\n')[0] ?? '';
  const p = line.slice(line.indexOf('::') + 2).trim();
  const m = /\/user@\d+\.service\/(.*)$/.exec(p);
  return m ? (m[1].split('/').find((s) => s.endsWith('.service')) ?? null) : null;
}

const cgroupPath = (cgroup: string) => (cgroup.split('\n').find((l) => l.startsWith('0::')) ?? '').slice(3).trim();

/** The command as the owner reads it: program name, own folder's paths relative, home's as `~/`, cut to size. */
export function shortCommand(argv: string[], cwd: string, home: string): string {
  const shorten = (a: string, i: number) => {
    if (i === 0) return path.basename(a);
    if (!a.startsWith('/')) return a;
    if (cwd && (a === cwd || a.startsWith(`${cwd}/`))) return a === cwd ? '.' : `./${a.slice(cwd.length + 1)}`;
    return home && a.startsWith(`${home}/`) ? `~/${a.slice(home.length + 1)}` : a;
  };
  const s = argv.map(shorten).join(' ').replace(/[\u0000-\u001f\u007f]/g, ' ');
  return s.length > MAX_COMMAND ? `${s.slice(0, MAX_COMMAND - 1)}…` : s;
}

const readText = (p: string) => fs.readFile(p, 'utf8').catch(() => '');

/** The app's name for a process: package.json, git repo, user unit, then the cwd's folder. */
export async function appName(cwd: string, unit: string | null, home: string): Promise<string> {
  const under = cwd === home || cwd.startsWith(`${home}/`);
  const dirs: string[] = [];
  for (let d = cwd; ; d = path.dirname(d)) {
    dirs.push(d);
    if (!under || d === home || d === path.dirname(d)) break;
  }
  if (!under) dirs.length = 1;
  for (const d of dirs) {
    try {
      const pkg = JSON.parse(await fs.readFile(path.join(d, 'package.json'), 'utf8')) as { name?: unknown };
      if (typeof pkg.name === 'string') {
        const n = pkg.name.replace(/^@[^/]*\//, '').trim();
        if (n) return n;
      }
    } catch { /* none here, or not JSON */ }
  }
  for (const d of dirs) if (await fs.stat(path.join(d, '.git')).then(() => true, () => false)) return path.basename(d);
  // A unit made by a restart here says nothing about the app: the folder does.
  if (unit && !/^zen-preview-\d+-/.test(unit)) return unit.replace(/\.service$/, '');
  return path.basename(cwd) || cwd || 'app';
}

async function ppidOf(proc: string, pid: number): Promise<number> {
  const s = await readText(`${proc}/${pid}/stat`);
  const f = s.slice(s.lastIndexOf(')') + 2).split(' ');
  return Number(f[1]) || 0;
}

/** Whether the process still runs (a zombie is gone: its port is free). */
async function alive(proc: string, pid: number): Promise<boolean> {
  const s = await readText(`${proc}/${pid}/stat`);
  if (!s) return false;
  return s.slice(s.lastIndexOf(')') + 2, s.lastIndexOf(')') + 3) !== 'Z';
}

/** For each wanted inode, the processes of `uid` holding it. */
async function holders(proc: string, uid: number, inodes: Set<number>): Promise<Map<number, number[]>> {
  const found = new Map<number, number[]>();
  for (const e of await fs.readdir(proc).catch(() => [] as string[])) {
    if (!/^\d+$/.test(e)) continue;
    const pid = Number(e);
    if ((await fs.stat(`${proc}/${e}`).catch(() => null))?.uid !== uid) continue;
    for (const fd of await fs.readdir(`${proc}/${e}/fd`).catch(() => [] as string[])) {
      const m = /^socket:\[(\d+)\]$/.exec(await fs.readlink(`${proc}/${e}/fd/${fd}`).catch(() => ''));
      if (m && inodes.has(Number(m[1]))) found.set(Number(m[1]), [...(found.get(Number(m[1])) ?? []), pid]);
    }
  }
  return found;
}

/** The dev servers on this machine now, by port. */
export async function list(o: Options): Promise<Server[]> {
  const proc = o.proc ?? '/proc';
  const uid = o.uid ?? process.getuid!();
  const home = o.home ?? os.homedir();
  const sockets = [...parseListening(await readText(`${proc}/net/tcp`)), ...parseListening(await readText(`${proc}/net/tcp6`))]
    .filter((s) => s.port >= MIN_PORT && s.port <= MAX_PORT && !o.blocked.includes(s.port));
  if (!sockets.length) return [];
  const held = await holders(proc, uid, new Set(sockets.map((s) => s.inode)));
  // Our own service unit is never a dev server (a unit that also listens elsewhere would stop zen).
  const own = userUnit(await readText(`${proc}/self/cgroup`));
  const byPort = new Map<number, Set<number>>();
  for (const s of sockets) for (const pid of held.get(s.inode) ?? []) byPort.set(s.port, (byPort.get(s.port) ?? new Set()).add(pid));
  const out: Server[] = [];
  for (const [port, pids] of byPort) {
    // The root-most holder: a worker's parent holds the socket too.
    let pid = Math.min(...pids);
    for (const p of [...pids].sort((a, b) => a - b)) if (!pids.has(await ppidOf(proc, p))) { pid = p; break; }
    if (pid === process.pid) continue;
    const cgroup = await readText(`${proc}/${pid}/cgroup`);
    const cg = cgroupPath(cgroup);
    if (!o.includeSystem && cg.startsWith('/system.slice/') && !cg.startsWith(AGENT_SLICE)) continue;
    const unit = userUnit(cgroup);
    if (unit && unit === own) continue;
    const argv = (await readText(`${proc}/${pid}/cmdline`)).split('\0').filter((a, i, all) => a || i < all.length - 1);
    if (!argv.length || !argv[0]) continue;
    const cwd = await fs.readlink(`${proc}/${pid}/cwd`).catch(() => '');
    out.push({ port, name: await appName(cwd, unit, home), command: shortCommand(argv, cwd, home), cwd, unit, pid });
  }
  return out.sort((a, b) => a.port - b.port);
}

// ---- stop and restart ----

function execDefault(cmd: string, args: string[]): Promise<void> {
  const env = { ...process.env };
  env.XDG_RUNTIME_DIR ||= `/run/user/${process.getuid!()}`;
  return new Promise((ok, fail) => {
    execFile(cmd, args, { env, timeout: 30000 }, (err, _out, stderr) => {
      if (err) fail(new HttpError(500, `${cmd} ${args[0]} failed: ${String(stderr || err.message).trim().slice(0, 300)}`));
      else ok();
    });
  });
}

/** The port's server as listed now; 404 when it isn't one (blocked and system ones never are). */
async function lookup(raw: unknown, o: Options): Promise<Server> {
  const port = targetPort(typeof raw === 'number' && Number.isInteger(raw) ? String(raw) : raw, o.blocked);
  if (!port) throw new HttpError(400, `a port ${MIN_PORT}–${MAX_PORT} is needed`);
  const s = (await list(o)).find((x) => x.port === port);
  if (!s) throw new HttpError(404, `no dev server listens on :${port}`);
  return s;
}

const listens = async (port: number, o: Options) => (await list(o)).some((x) => x.port === port);

async function waitFor(ok: () => Promise<boolean>, ms: number): Promise<boolean> {
  for (const end = Date.now() + ms; ; ) {
    if (await ok()) return true;
    if (Date.now() >= end) return false;
    await sleep(100);
  }
}

async function stopServer(s: Server, o: Options) {
  const proc = o.proc ?? '/proc';
  const exec = o.exec ?? execDefault;
  if (s.unit) await exec('systemctl', ['--user', 'stop', s.unit]);
  else {
    try {
      process.kill(s.pid, 'SIGTERM');
    } catch (e) {
      if ((e as { code?: string }).code !== 'ESRCH') throw new HttpError(500, `couldn't stop ${s.name}: ${(e as Error).message}`);
    }
    if (!(await waitFor(async () => !(await alive(proc, s.pid)), STOP_GRACE_MS))) {
      try { process.kill(s.pid, 'SIGKILL'); } catch { /* gone */ }
      await waitFor(async () => !(await alive(proc, s.pid)), 1000);
    }
  }
  if (!(await waitFor(async () => !(await listens(s.port, o)), FREE_WAIT_MS))) throw new HttpError(500, `${s.name} still listens on :${s.port}`);
}

/** Stops the app on `port`. */
export async function stop(raw: unknown, o: Options): Promise<void> {
  await stopServer(await lookup(raw, o), o);
}

/** Restarts the app on `port` and returns it once it listens again (504 if it doesn't come back). */
export async function restart(raw: unknown, o: Options): Promise<Server> {
  const s = await lookup(raw, o);
  const proc = o.proc ?? '/proc';
  const exec = o.exec ?? execDefault;
  if (s.unit) await exec('systemctl', ['--user', 'restart', s.unit]);
  else {
    // What it was started with, read before it goes.
    const argv = (await readText(`${proc}/${s.pid}/cmdline`)).split('\0');
    if (argv.at(-1) === '') argv.pop();
    const env = (await readText(`${proc}/${s.pid}/environ`)).split('\0').filter((kv) => kv.includes('=') && !kv.includes('\n') && !SESSION_ENV.test(kv.slice(0, kv.indexOf('='))));
    if (!argv.length || !s.cwd) throw new HttpError(500, `can't read how ${s.name} was started`);
    await stopServer(s, o);
    const unit = `zen-preview-${s.port}-${Date.now().toString(36)}`;
    await exec('systemd-run', ['--user', '--quiet', '--collect', `--unit=${unit}`, `--working-directory=${s.cwd}`, ...env.flatMap((kv) => ['-E', kv]), '--', ...argv]);
  }
  let back: Server | undefined;
  await waitFor(async () => (back = (await list(o)).find((x) => x.port === s.port)) !== undefined, BACK_WAIT_MS);
  if (!back) throw new HttpError(504, `${s.name} didn't come back on :${s.port}; look at its logs`);
  return back;
}
