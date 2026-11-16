// The proofreader: the AI reads the whole note and proposes small, local fixes (spelling,
// punctuation, grammar) and clarity improvements. It never rewrites the piece: each fix names the
// exact words it changes, so the page can underline them and the writer accepts or dismisses each.
// Shrink (from Jason Fried's Write_On) is the same machine with another goal: cuts, least needed
// first, enough to take the piece down by half; the page takes them in order for each level
// (10, 20, 30, 50%), so one read serves them all. Marks only point: weak or long sentences and
// filler, with a reason, and change nothing.

import { complete, jsonIn, skill } from './kernel.ts';

export type Kind = 'spelling' | 'punctuation' | 'grammar' | 'clarity' | 'cut' | Mark;
export type Mark = 'weak' | 'long' | 'filler';
const MARKS: Mark[] = ['weak', 'long', 'filler'];
const KINDS: Kind[] = ['spelling', 'punctuation', 'grammar', 'clarity'];

export interface Fix {
  /** The exact text to change, copied from the note, as short as makes it unique. */
  find: string;
  replace: string;
  kind: Kind;
  /** One short reason. */
  why: string;
}

// The proofreader's instructions are the writing/proofread skill, read at every check, so what the owner
// teaches it applies without a change here. Only the answer's shape is fixed in code, because the
// page depends on it.
const FALLBACK = `You are a careful proofreader for a writer's own note. Propose small, local fixes for spelling,
punctuation, grammar and clarity, keeping the writer's meaning, voice and language.`;

const FORMAT = `Answer with JSON only, nothing else:
{"fixes": [{"find": "exact text from the note", "replace": "the same text, fixed", "kind": "spelling|punctuation|grammar|clarity", "why": "short reason"}]}`;

/** The fixes in a model's answer that really occur in the note; `only` gives them all one kind. */
export function parseFixes(text: string, note: string, only?: Kind): Fix[] {
  const list = jsonIn(text)?.fixes;
  if (!Array.isArray(list)) return [];
  const out: Fix[] = [];
  for (const f of list) {
    if (!f || typeof f !== 'object') continue;
    const { find, replace, kind, why } = f as Record<string, unknown>;
    if (typeof find !== 'string' || typeof replace !== 'string' || !find.trim() || find === replace) continue;
    if (only === 'cut' && replace.length >= find.length) continue;
    if (find.includes('\n') || replace.includes('\n') || !note.includes(find)) continue;
    out.push({ find, replace, kind: only ?? (KINDS.includes(kind as Kind) ? (kind as Kind) : 'clarity'), why: typeof why === 'string' ? why.slice(0, 120) : '' });
  }
  return out;
}

function textOf(r: { text?: unknown }): string {
  if (!r || typeof r.text !== 'string' || !r.text.trim()) throw Object.assign(new Error('text is required'), { status: 400 });
  if (r.text.length > 60_000) throw Object.assign(new Error('too long to check at once'), { status: 400 });
  return r.text;
}

export async function check(r: { text?: unknown }): Promise<{ fixes: Fix[]; model: string }> {
  const text = textOf(r);
  const system = `${await skill('writing/proofread', FALLBACK)}\n\n${FORMAT}`;
  const prompt = `Write each "why" in the language the note is written in (an English note gets English reasons).\n\nThe note:\n\n${text}`;
  const reply = await complete(system, prompt, 'writer check');
  return { fixes: parseFixes(reply.text, text), model: reply.model };
}

const SHRINK = `You are an editor who makes a writer's own note shorter without losing anything that matters.
Propose cuts: words, phrases or sentences to drop, or wordy phrases to say in fewer words, keeping
the writer's meaning, voice and language.`;

const CUT_FORMAT = `Answer with JSON only, nothing else:
{"fixes": [{"find": "exact text from the note", "replace": "the shorter text, or empty to drop it", "why": "short reason"}]}`;

const wordsIn = (t: string) => t.split(/\s+/).filter((w) => /[\p{L}\p{N}]/u.test(w)).length;

/** Cuts, least needed first, adding up to about half the note's words. */
export async function shrink(r: { text?: unknown }): Promise<{ fixes: Fix[]; model: string }> {
  const text = textOf(r);
  const total = wordsIn(text);
  const system = `${await skill('writing/shrink', SHRINK)}\n\n${CUT_FORMAT}`;
  const prompt = `The note has ${total} words. List cuts in order, least needed first, adding up to about half of them (${Math.round(total / 2)} words): the writer takes them in that order to cut 10, 20, 30 or 50%. Write each "why" in the language the note is written in (an English note gets English reasons).\n\nThe note:\n\n${text}`;
  const reply = await complete(system, prompt, 'writer shrink');
  return { fixes: parseFixes(reply.text, text, 'cut'), model: reply.model };
}

const MARK = `You are an editor reading a writer's own note to make them a better writer. You change nothing:
you point at weak sentences (vague, flat, passive, saying little), long sentences (hard to follow
in one breath) and filler (words that add nothing), each with a short reason.`;

const MARK_FORMAT = `Answer with JSON only, nothing else:
{"marks": [{"find": "exact text from the note", "kind": "weak|long|filler", "why": "short reason"}]}`;

/** The marks in a model's answer that really occur in the note. */
export function parseMarks(text: string, note: string): Fix[] {
  const list = jsonIn(text)?.marks;
  if (!Array.isArray(list)) return [];
  return list.flatMap((m) => {
    const { find, kind, why } = (m ?? {}) as Record<string, unknown>;
    if (typeof find !== 'string' || !find.trim() || find.includes('\n') || !note.includes(find) || !MARKS.includes(kind as Mark)) return [];
    return [{ find, replace: find, kind: kind as Mark, why: typeof why === 'string' ? why.slice(0, 120) : '' }];
  });
}

export async function marks(r: { text?: unknown }): Promise<{ fixes: Fix[]; model: string }> {
  const text = textOf(r);
  const system = `${await skill('writing/marks', MARK)}\n\n${MARK_FORMAT}`;
  const prompt = `Write each "why" in the language the note is written in (an English note gets English reasons).\n\nThe note:\n\n${text}`;
  const reply = await complete(system, prompt, 'writer marks');
  return { fixes: parseMarks(reply.text, text), model: reply.model };
}
