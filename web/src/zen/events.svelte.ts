// The page's live updates: one EventSource on the server's `/api/events` (server/events.ts: the
// kernel's change stream, live.rs, merged with the vault's file changes). Each event names a topic
// (`board`, `agents`, `models`, `prefs`, `jobs`, `tasks`, `plugins`, `memory`, `skills`, `mcp`,
// `version`, `files`); views say what to refetch with `onTopic`, and the store or view refetches it,
// debounced, so a burst is one fetch.
//
// Everything a view registered is refetched (`resync`) when the stream reconnects after a drop,
// when the tab comes back into view, when the server says it missed events, and every
// `FALLBACK_MS` as a safety net. A view keeps its own guards: a form or an edit in progress is
// never overwritten (the handler checks before it reloads).

export type LiveEvent = { topic: string; id?: string; paths?: string[] };
type Fn = (events: LiveEvent[]) => unknown;
type Sub = { topics: Set<string>; fn: Fn; delay: number; queued: LiveEvent[]; timer: ReturnType<typeof setTimeout> | null };

const FALLBACK_MS = 60_000;
const MAX_BACKOFF_MS = 30_000;

const subs = new Set<Sub>();
export const live = $state({ connected: false, events: 0 });

function queue(s: Sub, ev: LiveEvent) {
  s.queued.push(ev);
  s.timer ??= setTimeout(() => {
    const evs = s.queued;
    s.queued = [];
    s.timer = null;
    try { void Promise.resolve(s.fn(evs)).catch(() => {}); } catch {}
  }, s.delay);
}

/** Call `fn` (debounced by `delay` ms) when any of `topics` changes, and on every resync. Returns the unsubscribe. */
export function onTopic(topics: string | string[], fn: Fn, delay = 250): () => void {
  const s: Sub = { topics: new Set(Array.isArray(topics) ? topics : [topics]), fn, delay, queued: [], timer: null };
  subs.add(s);
  return () => {
    subs.delete(s);
    if (s.timer) clearTimeout(s.timer);
  };
}

/** Hand an event to the views that asked for its topic (`resync`: to all). */
export function dispatch(ev: LiveEvent) {
  live.events++;
  for (const s of subs) if (ev.topic === 'resync' || s.topics.has(ev.topic)) queue(s, ev);
}

/** Refetch everything the views registered. */
export const resync = () => dispatch({ topic: 'resync' });

let started = false;
/** Open the stream (once per page) and the resync triggers. Returns the stop. */
export function startLive(): () => void {
  if (started || typeof EventSource === 'undefined') return () => {};
  started = true;
  let es: EventSource | null = null;
  let backoff = 1000;
  let seen = false;
  let retry: ReturnType<typeof setTimeout> | null = null;
  const connect = () => {
    retry = null;
    es = new EventSource('/api/events');
    es.onmessage = (m) => {
      let ev: LiveEvent;
      try { ev = JSON.parse(m.data); } catch { return; }
      if (!ev || typeof ev.topic !== 'string') return;
      if (ev.topic === 'hello') {
        live.connected = true;
        backoff = 1000;
        // Back after a drop: what changed meanwhile was never sent.
        if (seen) resync();
        seen = true;
        return;
      }
      dispatch(ev);
    };
    es.onerror = () => {
      // Our own backoff instead of the browser's fixed retry, so a server that's down isn't hammered.
      es?.close();
      es = null;
      live.connected = false;
      retry = setTimeout(connect, backoff);
      backoff = Math.min(backoff * 2, MAX_BACKOFF_MS);
    };
  };
  connect();
  let hiddenAt = 0;
  const vis = () => {
    if (document.visibilityState === 'hidden') hiddenAt = Date.now();
    else if (hiddenAt && Date.now() - hiddenAt > 2000) resync();
  };
  document.addEventListener('visibilitychange', vis);
  const fallback = setInterval(() => { if (document.visibilityState !== 'hidden') resync(); }, FALLBACK_MS);
  return () => {
    started = false;
    es?.close();
    if (retry) clearTimeout(retry);
    clearInterval(fallback);
    document.removeEventListener('visibilitychange', vis);
    live.connected = false;
  };
}
