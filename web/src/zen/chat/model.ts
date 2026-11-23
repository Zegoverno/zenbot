// The chat's conversation as entries, kept as their source (as the terminal's transcript.rs):
// owner and kernel prompts, assistant Markdown, runs of tool calls ("Work") with their results,
// questions, command output and dim notes. Pure helpers; Chat.svelte holds the state.

import type { Block, Message, TurnRecord } from '../api.ts';

export interface Step {
  name: string;
  args: Record<string, unknown>;
  /** The tool call's id, to match its result. */
  id?: string;
  result?: Result;
}

export interface Result {
  /** All non-empty lines (the first 3 show; a click shows the rest). */
  lines: string[];
  error: boolean;
}

export type Sty = 'dim' | 'warn' | 'err' | 'accent' | 'plain';

export type Entry =
  | { k: 'user'; key: number; text: string; kernel: boolean }
  | { k: 'md'; key: number; text: string }
  | { k: 'work'; key: number; steps: Step[]; open: boolean | null }
  | { k: 'result'; key: number; result: Result; id?: string }
  | { k: 'line'; key: number; text: string; sty: Sty }
  | { k: 'mono'; key: number; text: string; title?: string }
  | { k: 'link'; key: number; text: string; url: string }
  | { k: 'questions'; key: number; questions: { question: string; options: string[] }[] }
  | { k: 'help'; key: number; rows: [string, string][]; keys: [string, string][] };

/** An entry without its key (the list adds one). */
export type New = Entry extends infer E ? (E extends { key: number } ? Omit<E, 'key'> : never) : never;

let next = 1;
export const keyed = (e: New): Entry => ({ ...e, key: next++ }) as Entry;

/** Invisible format characters that can hide or reorder text (bidi controls, zero-width), and control characters. */
// eslint-disable-next-line no-control-regex
const HIDDEN = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f؜​‎‏‪-‮⁠-⁤⁦-⁩﻿]/g;
export const clean = (s: string) => s.replace(HIDDEN, '');

export function textOf(content: Message['content']): string {
  if (typeof content === 'string') return content;
  return (content ?? []).filter((b) => b.type === 'text' && typeof b.text === 'string').map((b) => b.text).join('\n');
}

export function assistantText(m: Message): string {
  if (typeof m.content === 'string') return m.content;
  return (m.content ?? []).filter((b) => b.type === 'text').map((b) => b.text ?? '').join('');
}

export function toolCalls(m: Message): Block[] {
  return typeof m.content === 'string' ? [] : (m.content ?? []).filter((b) => b.type === 'toolCall');
}

export function resultOf(m: Message): Result {
  const lines = clean(textOf(m.content)).split('\n').filter((l) => l.trim() !== '');
  return { lines, error: m.isError === true };
}

/** Tokens of one model call (cache reads and writes included). */
export const usageTotal = (u: Message['usage']) => (u?.input ?? 0) + (u?.output ?? 0) + (u?.cacheRead ?? 0) + (u?.cacheWrite ?? 0);
/** Tokens of a whole turn, from the kernel's record of it. */
export const recordTotal = (r: TurnRecord | undefined) => (r?.input_tokens ?? 0) + (r?.output_tokens ?? 0) + (r?.cache_read ?? 0) + (r?.cache_write ?? 0);

export function fmtTokens(n: number): string {
  if (n >= 10_000) return `${Math.floor(n / 1000)}k`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

/** Fold a tool call or result into a trailing Work run; anything else is added as is. */
export function absorb(list: Entry[], e: New) {
  const last = list[list.length - 1];
  if (e.k === 'work') {
    if (last?.k === 'work') last.steps.push(...e.steps);
    else list.push(keyed(e));
    return;
  }
  if (e.k === 'result') {
    // The run it belongs to: the latest, past questions or notes shown since (not past text or a prompt).
    for (let i = list.length - 1; i >= 0; i--) {
      const w = list[i];
      if (w.k === 'md' || w.k === 'user') break;
      if (w.k !== 'work') continue;
      const step = (e.id && w.steps.find((s) => s.id === e.id && !s.result)) || w.steps.find((s) => !s.result);
      if (step) {
        step.result = e.result;
        return;
      }
      break;
    }
  }
  list.push(keyed(e));
}

/** A stored message as entries (its tool calls as Work steps). */
export function messageEntries(m: Message, opts: { aborting?: boolean; streamed?: string } = {}): New[] {
  if (m.role === 'user') return [{ k: 'user', text: clean(textOf(m.content)), kernel: m.kernel === true }];
  if (m.role === 'toolResult') return [{ k: 'result', result: resultOf(m), id: m.toolCallId }];
  if (m.role !== 'assistant') return [];
  const out: New[] = [];
  const full = clean(assistantText(m));
  const text = full.trim() ? full : (opts.streamed ?? '');
  if (text.trim()) out.push({ k: 'md', text });
  for (const c of toolCalls(m)) out.push({ k: 'work', steps: [{ name: c.name ?? '', args: c.arguments ?? {}, id: c.id }], open: null });
  if (m.stopReason === 'error' && !opts.aborting) out.push({ k: 'line', text: clean(m.errorMessage ?? 'error'), sty: 'err' });
  return out;
}

export function build(messages: Message[]): Entry[] {
  const list: Entry[] = [];
  for (const m of messages) for (const e of messageEntries(m)) absorb(list, e);
  return list;
}

/** Whether `m` is the kernel's echo of prompt `p` (an owner's message with the same text). */
export const echoes = (m: Message, p: string) => m.role === 'user' && m.kernel !== true && textOf(m.content).trim() === p.trim();

// ---- prompt history (localStorage, ≤1000) ----

const HIST = 'zen.chat.history';
export function loadHistory(): string[] {
  try {
    const h = JSON.parse(localStorage.getItem(HIST) ?? '[]');
    return Array.isArray(h) ? h.filter((x) => typeof x === 'string') : [];
  } catch {
    return [];
  }
}
export function saveHistory(text: string) {
  const h = loadHistory();
  if (h[h.length - 1] !== text) h.push(text);
  try {
    localStorage.setItem(HIST, JSON.stringify(h.slice(-1000)));
  } catch {}
}

// ---- unsent text, per session ('new' for a chat not started), kept across tabs, sessions and reloads ----

const DRAFTS = 'zen.drafts';
/** The most drafts kept: the oldest go first. */
const DRAFTS_MAX = 50;

function drafts(): Record<string, string> {
  try {
    const d = JSON.parse(localStorage.getItem(DRAFTS) ?? '{}');
    return d && typeof d === 'object' && !Array.isArray(d) ? d : {};
  } catch {
    return {};
  }
}
/** The text the owner typed in session `key` and didn't send ('' when none). */
export function loadDraft(key: string): string {
  const t = drafts()[key];
  return typeof t === 'string' ? t : '';
}
/** Keep (or, when blank, forget) session `key`'s unsent text; the latest kept moves last. */
export function saveDraft(key: string, text: string) {
  const d = drafts();
  if (d[key] === text || (!text.trim() && !(key in d))) return;
  delete d[key];
  if (text.trim()) d[key] = text;
  const keys = Object.keys(d);
  for (const k of keys.slice(0, Math.max(0, keys.length - DRAFTS_MAX))) delete d[k];
  try {
    localStorage.setItem(DRAFTS, JSON.stringify(d));
  } catch {}
}

// ---- the first prompt of a new chat, handed to the chat re-created with the session's id ----

export interface Handoff {
  id: string;
  text: string;
  message: Record<string, unknown>;
}
let handoff: Handoff | null = null;
export function handOver(h: Handoff) {
  handoff = h;
}
/** The pending first prompt for session `id`, once. */
export function takeHandoff(id: string): Handoff | null {
  if (handoff?.id !== id) return null;
  const h = handoff;
  handoff = null;
  return h;
}

/** How a session can end (`/archive [outcome] [1-10] [note]`); `-` archives without saying. */
export const OUTCOMES: [string, string][] = [
  ['done', 'the job is done'],
  ['partial', 'part done: continue in a new session with a handoff'],
  ['not-done', 'not done: stop here'],
  ['-', 'just archive'],
];

/**
 * `/archive`'s arguments as the shared command table reads them (zen-client commands.rs): `[outcome]
 * [1-10] [note]`, where a bare whole number right after the outcome (or first) is the rating. Null
 * when the shape is wrong: an unknown word first, 0 or 11+ as the number, a note with neither.
 */
export function parseArchive(arg: string): { outcome: string | null; rating: number | null; note: string } | null {
  let rest = arg.trim();
  const take = () => {
    const m = /^(\S*)\s*([\s\S]*)$/.exec(rest)!;
    return [m[1], m[2]] as const;
  };
  let outcome: string | null = null;
  const [first, afterFirst] = take();
  if (OUTCOMES.some(([v]) => v === first.toLowerCase() && v !== '-')) {
    outcome = first.toLowerCase();
    rest = afterFirst;
  }
  let rating: number | null = null;
  const [next, afterNext] = take();
  if (/^[0-9]+$/.test(next)) {
    const n = Number(next);
    if (n < 1 || n > 10) return null;
    rating = n;
    rest = afterNext;
  }
  const note = rest.trim();
  if (!outcome && rating === null && note) return null;
  return { outcome, rating, note };
}

export const KEYS: [string, string][] = [
  ['enter', 'send'],
  ['shift+enter', 'new line (also alt+enter, ctrl+enter)'],
  ['esc', 'interrupt zenbot; with a /command, clear it; on an empty input, back to the board'],
  ['tab', 'take the suggested next prompt (grey); in the / menu, complete the command'],
  ['↑ ↓', 'previous prompts (at the first/last line); in the / menu, choose a command'],
  ['ctrl+c', 'interrupt zenbot (when nothing is selected); otherwise copy'],
  ['ctrl+o', 'show or fold the steps of every tool run (a click folds one)'],
  ['ctrl+b', 'open or close the side panel; shift+tab moves the keys between chat and panel'],
  ['PgUp/PgDn', 'scroll the conversation'],
];
