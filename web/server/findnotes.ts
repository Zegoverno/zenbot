// Notes > Find: the AI reads the piece, asks zenbot's search for what in the wiki bears on it (it
// never reads the pages themselves), may search once more, and proposes a few short notes, each
// linking a page. The instructions are the writing/find-notes skill; only the answers' shapes are
// fixed here, because the page depends on them.

import { complete, jsonIn, search, skill, type Hit } from './kernel.ts';

export interface Proposal {
  /** One or two sentences: how the page helps this piece. */
  text: string;
  /** The wiki page it links, as its title. */
  page: string;
  quote?: string;
}

const FALLBACK = `You help a writer by finding what in their wiki bears on the piece they are writing, and
proposing a few short notes that each link one page and say how it helps. Never change their text.`;

const SEARCH = `First answer with the searches to run, JSON only: {"search": ["query", ...]} (3 to 6 queries).`;
const AFTER = `Answer with JSON only. Either search once more, narrower (at most 3 queries):
{"search": ["query", ...]}
or propose the notes (at most 6; an empty list is fine):
{"notes": [{"text": "how it helps this piece, in the piece's language", "page": "the result's page, exactly", "quote": "optional short quote from the result"}]}`;
const LAST = `Now propose the notes, JSON only:
{"notes": [{"text": "how it helps this piece, in the piece's language", "page": "the result's page, exactly", "quote": "optional short quote from the result"}]}`;

export const queriesIn = (v: Record<string, unknown> | null, max: number): string[] =>
  Array.isArray(v?.search) ? [...new Set((v.search as unknown[]).filter((q): q is string => typeof q === 'string' && !!q.trim()).map((q) => q.trim().slice(0, 300)))].slice(0, max) : [];

/** The results so far, as the AI reads them: one block per page, its best snippet. */
function render(hits: Map<string, Hit>): string {
  if (!hits.size) return '(nothing found)';
  return [...hits.values()].map((h) => `[[${h.ref}]]${h.title && h.title !== h.ref ? ` (${h.title})` : ''}\n${h.snippet.slice(0, 700)}`).join('\n\n');
}

export function proposals(v: Record<string, unknown> | null, hits: Map<string, Hit>): Proposal[] {
  if (!Array.isArray(v?.notes)) return [];
  const out: Proposal[] = [];
  for (const n of v.notes as unknown[]) {
    if (!n || typeof n !== 'object') continue;
    const { text, page, quote } = n as Record<string, unknown>;
    const ref = typeof page === 'string' ? page.replace(/^\[\[|\]\]$/g, '').trim() : '';
    if (typeof text !== 'string' || !text.trim() || !hits.has(ref)) continue; // only pages search found
    out.push({ text: text.trim().replace(/\s+/g, ' ').slice(0, 400), page: ref, ...(typeof quote === 'string' && quote.trim() ? { quote: quote.trim().slice(0, 240) } : {}) });
    if (out.length === 6) break;
  }
  return out;
}

export async function findNotes(r: { path?: unknown; text?: unknown }): Promise<{ notes: Proposal[]; searched: string[]; model: string }> {
  if (!r || typeof r.text !== 'string' || !r.text.trim()) throw Object.assign(new Error('text is required'), { status: 400 });
  if (r.text.length > 60_000) throw Object.assign(new Error('too long to read at once'), { status: 400 });
  // The piece's own page is not a note for itself.
  const self = typeof r.path === 'string' ? r.path.replace(/^.*\//, '').replace(/\.md$/, '') : '';
  const system = await skill('writing/find-notes', FALLBACK);
  // Results are often in another language than the piece; the notes follow the piece.
  const piece = `Write every note in the language the piece is written in (an English piece gets English notes, whatever the wiki's language).\n\nThe piece:\n\n${r.text}`;
  const hits = new Map<string, Hit>();
  const searched: string[] = [];
  const run = async (queries: string[]) => {
    for (const list of await Promise.all(queries.map((q) => search(q, 'wiki', 8).catch(() => [] as Hit[])))) {
      for (const h of list) if (h.kind === 'wiki' && h.ref !== self && !hits.has(h.ref)) hits.set(h.ref, h);
    }
    searched.push(...queries);
  };

  let a = await complete(system, `${piece}\n\n${SEARCH}`, 'writer find notes');
  let queries = queriesIn(jsonIn(a.text), 6);
  if (!queries.length) return { notes: [], searched, model: a.model };
  await run(queries);
  const found = () => `Searches so far: ${searched.map((q) => `"${q}"`).join(', ')}\n\nWhat they found:\n\n${render(hits)}`;
  a = await complete(system, `${piece}\n\n${found()}\n\n${AFTER}`, 'writer find notes');
  let v = jsonIn(a.text);
  queries = queriesIn(v, 3);
  if (queries.length && !Array.isArray(v?.notes)) {
    await run(queries);
    a = await complete(system, `${piece}\n\n${found()}\n\n${LAST}`, 'writer find notes');
    v = jsonIn(a.text);
  }
  return { notes: proposals(v, hits), searched, model: a.model };
}
