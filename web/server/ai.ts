// AI alternatives, through the zen kernel: one completion without tools (`POST /api/complete`),
// with the writer's own models and no key of our own. The kernel's token stays on this side.

import { complete, jsonIn } from './kernel.ts';

export interface SuggestRequest {
  title?: string;
  /** The whole paragraph the target sits in. */
  paragraph: string;
  /** The text to rewrite, as it shows now. */
  target: string;
  scope: 'word' | 'sentence' | 'paragraph';
  /** Versions already written, so the AI doesn't repeat them. */
  existing?: string[];
  count?: number;
}

export const SYSTEM = `You help a writer find other ways to say one piece of their text.
You never rewrite anything else, and you never explain.
Keep the writer's meaning, voice, register and language (if the text is in Portuguese, answer in Portuguese).
Each alternative must drop in exactly where the original stands: same grammatical role, no surrounding words, no quotes around it.
Make the alternatives genuinely different from each other and from the versions already written; vary length and rhythm, and prefer plain, concrete words.
Answer with JSON only: {"alternatives": ["...", "..."]}`;

export function buildPrompt(r: SuggestRequest): string {
  const n = r.count ?? 3;
  const what = r.scope === 'word' ? 'word or phrase' : r.scope;
  const lines = [
    r.title ? `Note title: ${r.title}` : '',
    r.scope === 'paragraph' ? '' : `The paragraph, with the ${what} to replace between ⟦ and ⟧:\n${r.paragraph.replace(r.target, `⟦${r.target}⟧`)}`,
    `The ${what} to replace:\n${r.target}`,
    r.existing && r.existing.length > 1 ? `Versions already written (don't repeat them):\n${r.existing.map((e) => `- ${e}`).join('\n')}` : '',
    `Give ${n} alternatives for the ${what}.`,
  ];
  return lines.filter(Boolean).join('\n\n');
}

/** The alternatives in a reply: the JSON object's list, trimmed, deduplicated, never the target. */
export function parseReply(text: string, r: SuggestRequest): string[] {
  const list = jsonIn(text)?.alternatives;
  if (!Array.isArray(list)) return [];
  const seen = new Set([r.target.trim(), ...(r.existing ?? []).map((e) => e.trim())]);
  const out: string[] = [];
  for (const item of list) {
    if (typeof item !== 'string') continue;
    const t = item.trim().replace(/^["“'‘⟦]+|["”'’⟧]+$/g, '').trim();
    if (!t || seen.has(t) || t.includes('\n')) continue;
    seen.add(t);
    out.push(t);
  }
  return out;
}

export async function suggest(r: SuggestRequest): Promise<{ alternatives: string[]; model: string }> {
  if (!r || typeof r.target !== 'string' || !r.target.trim() || typeof r.paragraph !== 'string') {
    throw Object.assign(new Error('target and paragraph are required'), { status: 400 });
  }
  if (r.target.length > 4000 || r.paragraph.length > 8000) throw Object.assign(new Error('too long'), { status: 400 });
  const reply = await complete(SYSTEM, buildPrompt(r), 'writer alternatives', 90_000);
  return { alternatives: parseReply(reply.text, r), model: reply.model };
}
