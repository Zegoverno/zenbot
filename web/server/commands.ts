// zen's commands for the web page. The command table is zenbot's own
// (`crates/zen-client/src/commands.rs`): `list` asks `zen --commands` for it, so the page keeps no
// copy. The kernel commands run here through the `zen` command line, which runs the same code as
// every other client, so their answers read the same everywhere; session commands, panels and
// pickers are the page's own.
//
// Only listed commands with listed sub-commands and plain-word arguments run, with no shell.

import { execFile } from 'node:child_process';
import os from 'node:os';
import path from 'node:path';
import { HttpError } from './files.ts';

export type Scope = 'kernel' | 'session' | 'screen';
export interface Command { name: string; args: string; help: string; scope: Scope }

const WORD = /^[A-Za-z0-9._-]{1,80}$/;
/** For each kernel command the command line runs, the argument lists it accepts. */
const SHAPES: Record<string, (a: string[]) => boolean> = {
  tasks: (a) => a.length === 0,
  skills: (a) => a.length === 0,
  models: (a) => a.length === 0,
  suggestions: (a) => a.length === 0,
  status: (a) => a.length === 0,
  mcp: (a) => a.length === 0,
  upgrade: (a) => a.length === 1 && a[0] === 'check',
  memory: (a) => a.length === 0 || (a.length === 1 && ['short', 'long', 'archived', 'all', 'sleep'].includes(a[0])),
  jobs: (a) =>
    a.length === 0 ||
    (a.length === 1 && ['list', 'runs'].includes(a[0])) ||
    (a.length === 2 && ['run', 'resume', 'pause', 'rm', 'runs'].includes(a[0]) && WORD.test(a[1])),
};

const ZEN = process.env.ZEN_BIN ?? path.join(os.homedir(), '.zenbot/bin/zen');

/** Read the shared table afresh: plugin installs and enable/disable change it at runtime. */
export function list(bin = ZEN): Promise<Command[]> {
  return load(bin);
}

function load(bin: string): Promise<Command[]> {
  return new Promise((done, fail) => {
    execFile(bin, ['--commands'], { timeout: 10_000, maxBuffer: 1024 * 1024 }, (err, stdout) => {
      if (err) return fail(new HttpError(502, `zen --commands: ${err.message}`));
      try {
        const v = JSON.parse(stdout) as { commands?: unknown };
        if (!Array.isArray(v.commands)) throw new Error('no list');
        done(v.commands as Command[]);
      } catch (e) {
        fail(new HttpError(502, `zen --commands: ${(e as Error).message}`));
      }
    });
  });
}

/** Runs kernel command `name` with `args` through `zen`; its output, as the terminal shows it. */
export function run(name: unknown, args: unknown): Promise<{ output: string; ok: boolean }> {
  const a = Array.isArray(args) ? args : [];
  if (typeof name !== 'string' || !(name in SHAPES) || !a.every((x) => typeof x === 'string') || !SHAPES[name](a as string[])) {
    return Promise.reject(new HttpError(400, 'not a command this page runs'));
  }
  const argv = name === 'jobs' && a[0] === 'list' ? ['jobs'] : [name, ...(a as string[])];
  return new Promise((done) => {
    execFile(ZEN, argv, { timeout: 120_000, maxBuffer: 4 * 1024 * 1024, env: { ...process.env, NO_COLOR: '1', TERM: 'dumb' } }, (err, stdout, stderr) => {
      const output = (stdout + (stderr ? (stdout ? '\n' : '') + stderr : '')).replace(/\x1b\[[0-9;?]*[A-Za-z]/g, '').trimEnd();
      done({ output: output || (err ? String(err.message) : '(nothing)'), ok: !err });
    });
  });
}
