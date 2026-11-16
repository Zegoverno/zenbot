// zen's commands for the web page. The command table is zenbot's own
// (`crates/zen-client/src/commands.rs`); `COMMANDS` mirrors it (a test keeps them in
// step). The kernel commands run here through the `zen` command line, which
// runs the same code as every other client, so their answers read the same everywhere; session
// commands, panels and pickers are the page's own.
//
// Only listed commands with listed sub-commands and plain-word arguments run, with no shell.

import { execFile } from 'node:child_process';
import os from 'node:os';
import path from 'node:path';
import { HttpError } from './files.ts';

export type Scope = 'kernel' | 'session' | 'screen';
export interface Command { name: string; args: string; help: string; scope: Scope }

/** The table as the page offers it, in help's order (zenbot's `Line` commands are left out). */
export const COMMANDS: Command[] = [
  { name: 'new', args: '[@agent] [title]', help: 'start a new session (with another agent: `new @garry`)', scope: 'session' },
  { name: 'resume', args: '', help: 'switch to another session', scope: 'screen' },
  { name: 'board', args: '', help: 'every session, running or idle, with its subagents', scope: 'screen' },
  { name: 'model', args: '[model]', help: "this session's model (no argument: list them)", scope: 'session' },
  { name: 'effort', args: '[level]', help: "this session's thinking level (no argument: the model's levels)", scope: 'session' },
  { name: 'done', args: 'accept|more|reshape|drop [note]', help: 'your verdict on the work so far', scope: 'session' },
  { name: 'rename', args: '<title>', help: 'rename this session', scope: 'session' },
  { name: 'archive', args: '', help: 'archive this session', scope: 'session' },
  { name: 'steer', args: '<message>', help: 'change direction at the next tool boundary', scope: 'session' },
  { name: 'files', args: '[dir]', help: "the side panel's Files view: the folder tree (ctrl+b)", scope: 'screen' },
  { name: 'open', args: '[path]', help: 'show a file next to the chat', scope: 'screen' },
  { name: 'close', args: '', help: 'close the side panel', scope: 'screen' },
  { name: 'mouse', args: '', help: 'mouse wheel scrolling on/off', scope: 'screen' },
  { name: 'agents', args: '[new <name> [description] | rm <name>]', help: 'your agents and what each may use; `agents new` makes one, `agents rm` removes one (kept in backups)', scope: 'kernel' },
  { name: 'tasks', args: '', help: "open tasks from the vault's projects, most pressing first", scope: 'kernel' },
  { name: 'jobs', args: '[run|resume|pause|rm <name> | runs [name]]', help: 'scheduled jobs and their runs', scope: 'kernel' },
  { name: 'skills', args: '', help: "skills and how often they're used", scope: 'kernel' },
  { name: 'memory', args: '[short|long|archived|all | sleep]', help: "zenbot's memory; `memory sleep` tidies it now", scope: 'kernel' },
  { name: 'mcp', args: '[login <server>]', help: 'MCP servers; `mcp login <server>` signs in to one', scope: 'kernel' },
  { name: 'models', args: '', help: 'the models and their thinking levels', scope: 'kernel' },
  { name: 'suggestions', args: '', help: 'what became of suggested next prompts', scope: 'kernel' },
  { name: 'status', args: '', help: 'kernel, workers, sign-ins and version', scope: 'kernel' },
  { name: 'upgrade', args: '[check]', help: 'update zenbot to the latest main and restart it', scope: 'kernel' },
  { name: 'restart', args: '', help: 'restart zen on the installed version, back in this session', scope: 'screen' },
  { name: 'help', args: '', help: 'keys and commands', scope: 'screen' },
  { name: 'exit', args: '', help: 'quit zen', scope: 'screen' },
];

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
