// The zen kernel, for the writer's AI: one completion (`POST /api/complete`), zenbot's search
// (`POST /api/search`), and the skills that hold the AI's instructions, so what the owner teaches them
// applies without a change here.

import { promises as fs } from 'node:fs';
import path from 'node:path';
import os from 'node:os';

const ZEN_URL = process.env.ZEN_URL ?? 'http://127.0.0.1:8100';
const TOKEN_FILE = process.env.ZEN_TOKEN_FILE ?? path.join(os.homedir(), '.zenbot/token');
const MODEL = process.env.ZEN_WEB_MODEL ?? 'claude/claude-sonnet-5-5';
const SKILLS = process.env.ZEN_WEB_SKILLS ?? path.join(os.homedir(), '.zenbot/global/skills');

async function post<T>(route: string, body: unknown, ms: number): Promise<T> {
  const token = (await fs.readFile(TOKEN_FILE, 'utf8')).trim();
  const res = await fetch(`${ZEN_URL}${route}`, {
    method: 'POST',
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(ms),
  });
  if (!res.ok) throw new Error(`kernel answered ${res.status}: ${(await res.text()).slice(0, 200)}`);
  return (await res.json()) as T;
}

/** One completion without tools; the text and the model that answered. */
export async function complete(system: string, prompt: string, purpose: string, ms = 180_000): Promise<{ text: string; model: string }> {
  const b = await post<{ text?: string; model?: string }>('/api/complete', { model: MODEL, system, prompt, purpose }, ms);
  return { text: b.text ?? '', model: b.model || MODEL };
}

export interface Hit { kind: string; ref: string; title: string; snippet: string; relevance: number | null; path: string | null }

/** zenbot's search: words and meaning, reranked by System One. */
export async function search(query: string, scope = 'wiki', limit = 8): Promise<Hit[]> {
  return (await post<{ results: Hit[] }>('/api/search', { query, scope, limit }, 60_000)).results ?? [];
}

/** A skill's instructions without its front matter (`writing/proofread`, say); the fallback when there's none. */
export async function skill(name: string, fallback: string): Promise<string> {
  try {
    const text = await fs.readFile(path.join(SKILLS, name, 'SKILL.md'), 'utf8');
    return text.replace(/^---\r?\n[\s\S]*?\r?\n---[ \t]*\r?\n/, '').trim() || fallback;
  } catch {
    return fallback; // No skill (another machine, say): the short built-in instructions do.
  }
}

/** The JSON object in a model's answer, or null. */
export function jsonIn(text: string): Record<string, unknown> | null {
  const a = text.indexOf('{'), b = text.lastIndexOf('}');
  if (a < 0 || b <= a) return null;
  try {
    const v = JSON.parse(text.slice(a, b + 1));
    return v && typeof v === 'object' ? v : null;
  } catch {
    return null;
  }
}
