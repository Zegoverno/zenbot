// Closing a version: the AI reads what changed since the last closed version and suggests its
// changelog line, which the writer takes with Tab or rewrites. Instructions: the writing/changelog
// skill.

import { parseNote, changelogEntries, withoutGhosts } from '../src/doc/note.ts';
import { complete, skill } from './kernel.ts';
import * as versions from './versions.ts';

const FALLBACK = `You write one changelog line for a new version of a writer's note: what changed since the last
version, in a few plain words.`;

const piece = (text: string) => {
  // As it reads: ghosts and zenbot's unkept suggestions are not part of it.
  const n = parseNote(text);
  return [n.frontmatter, withoutGhosts(n.body, n.ghosts ?? []), n.notes].filter(Boolean).join('\n').slice(0, 40_000);
};

export async function suggestLog(root: string, rel: string, text: string | null): Promise<{ log: string; model: string }> {
  if (text === null) throw Object.assign(new Error('not found'), { status: 404 });
  const s = await versions.state(root, rel, text);
  if (!s.open) return { log: '', model: '' };
  const before = s.current ? await versions.read(root, rel, s.current) : null;
  const earlier = changelogEntries(parseNote(text).changelog ?? '').slice(-5);
  const prompt = [
    earlier.length ? `Earlier changelog lines, for their style:\n${earlier.map((l) => `- ${l}`).join('\n')}` : '',
    before === null ? 'There is no earlier version: this is the first.' : `The last version (v${s.current}):\n\n${piece(before)}`,
    `The new version (v${s.current + 1}):\n\n${piece(text)}`,
    `Answer with the changelog line only: no date, no version number, no quotes, in the note's language.`,
  ].filter(Boolean).join('\n\n---\n\n');
  const r = await complete(await skill('writing/changelog', FALLBACK), prompt, 'writer changelog', 60_000);
  const log = r.text.trim().split('\n')[0].replace(/^[-*•]\s*|^["“']|["”']$/g, '').replace(/^v\d+:\s*/i, '').trim().slice(0, 200);
  return { log, model: r.model };
}
