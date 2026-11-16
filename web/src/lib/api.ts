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

export const api = {
  read: (path: string) => call<{ text: string; hash: string }>('GET', `/api/file?path=${encodeURIComponent(path)}`),
  exists: (path: string) => call<{ exists: boolean }>('GET', `/api/exists?path=${encodeURIComponent(path)}`),
  write: (path: string, text: string, base: string | null) => call<{ hash: string }>('PUT', '/api/file', { path, text, base }),
};
