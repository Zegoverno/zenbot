// The page's side of the server's API. Writes carry `X-Zen-Web: 1` (see server/main.ts).

export class Conflict extends Error {
  text: string | null;
  hash: string | null;
  constructor(text: string | null, hash: string | null) {
    super('changed on disk');
    this.text = text;
    this.hash = hash;
  }
}

async function call<T>(method: string, url: string, body?: unknown): Promise<T> {
  const res = await fetch(url, {
    method,
    headers: body === undefined ? {} : { 'content-type': 'application/json', 'x-zen-web': '1' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const data = await res.json().catch(() => ({}));
  if (res.status === 409) throw new Conflict(data.text ?? null, data.hash ?? null);
  if (!res.ok) throw new Error(data.error ?? `HTTP ${res.status}`);
  return data as T;
}

// The answers' shapes are the server's own types (types only: no server code reaches the page).
import type { Version, VersionState } from '../../server/versions.ts';
import type { Fix as FixProposal, Kind as FixKind } from '../../server/check.ts';
import type { Proposal as NoteProposal } from '../../server/findnotes.ts';
import type { SuggestRequest } from '../../server/ai.ts';
export type { Version, VersionState, FixProposal, FixKind, NoteProposal };

export const api = {
  changelog: (path: string) => call<{ log: string; model: string }>('POST', '/api/changelog', { path }),
  findNotes: (path: string, text: string) => call<{ notes: NoteProposal[]; searched: string[]; model: string }>('POST', '/api/notes/find', { path, text }),
  check: (text: string) => call<{ fixes: FixProposal[]; model: string }>('POST', '/api/check', { text }),
  shrink: (text: string) => call<{ fixes: FixProposal[]; model: string }>('POST', '/api/shrink', { text }),
  marks: (text: string) => call<{ fixes: FixProposal[]; model: string }>('POST', '/api/marks', { text }),
  versions: (path: string) => call<VersionState>('GET', `/api/versions?path=${encodeURIComponent(path)}`),
  version: (path: string, n: number) => call<{ text: string }>('GET', `/api/version?path=${encodeURIComponent(path)}&n=${n}`),
  close: (path: string, base: string, log: string) => call<VersionState & { text: string; hash: string }>('POST', '/api/close', { path, base, log }),
  branch: (path: string, base: string, n: number) => call<VersionState & { text: string; hash: string }>('POST', '/api/branch', { path, base, n }),
  read: (path: string) => call<{ text: string; hash: string }>('GET', `/api/file?path=${encodeURIComponent(path)}`),
  exists: (path: string) => call<{ exists: boolean }>('GET', `/api/exists?path=${encodeURIComponent(path)}`),
  files: () => call<{ files: string[] }>('GET', '/api/files'),
  write: (path: string, text: string, base: string | null) => call<{ hash: string }>('PUT', '/api/file', { path, text, base }),
  suggest: (req: SuggestRequest) =>
    call<{ alternatives: string[]; model: string }>('POST', '/api/suggest', req),
};
