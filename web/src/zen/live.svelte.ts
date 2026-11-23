// The live data the redesign's screens read, in the shapes the mock's fake modules had
// (web/mock/data.ts, profilestore.svelte.ts and ChatMock2's MODELS and AGENTS), from the kernel:
// the owner's name (/prefs), the board (/board: sessions, channels, who is working or waiting and on
// what question, the subagents), the agents (/agents), the models (/models) and new-chat ideas
// (/ideas). The board is polled while the page is open.

import { zen, ago, type Agent, type BoardSession, type Models } from './api.ts';

// ---- the owner ----

export const me = $state({ name: '', full: '', loaded: false, defaultModel: '' as string, defaultEffort: null as string | null });

async function prefs<T>(method: 'GET' | 'PATCH', body?: unknown): Promise<T> {
  const r = await fetch('/api/zen/prefs', { method, headers: method === 'GET' ? {} : { 'content-type': 'application/json', 'x-zen-web': '1' }, body: body === undefined ? undefined : JSON.stringify(body) });
  const data = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(data.error ?? `HTTP ${r.status}`);
  return data as T;
}
export type Prefs = { name?: string; home_layout?: unknown; default_model?: string; default_effort?: string | null };
export const getPrefs = () => prefs<Prefs>('GET');
export const patchPrefs = (b: Prefs) => prefs<Prefs>('PATCH', b);

export async function loadMe() {
  try {
    const p = await getPrefs();
    me.full = p.name ?? '';
    me.name = me.full.split(/\s+/)[0] ?? '';
    me.defaultModel = p.default_model ?? '';
    me.defaultEffort = p.default_effort ?? null;
  } catch {}
  me.loaded = true;
}
/** The owner's name, as their agents call them; false when empty. */
export async function setName(n: string) {
  const v = n.trim();
  if (!v) return false;
  await patchPrefs({ name: v });
  me.full = v;
  me.name = v.split(/\s+/)[0];
  return true;
}

// ---- sessions (the board) ----

export type State = 'idle' | 'working' | 'waiting';
export type Question = { question: string; options: string[] };
export type Session = { id: string; title: string; when: string; state: State; sub?: number; agent: string; kind: string | null; question?: string; questions?: Question[]; updated: string; archived?: boolean };

const asSession = (s: BoardSession & { questions?: Question[]; question?: string }): Session => ({
  id: s.id,
  title: s.title || 'New chat',
  when: when(s.updated_at),
  state: s.busy ? 'working' : s.waiting ? 'waiting' : 'idle',
  sub: s.children || undefined,
  agent: s.agent ?? 'zenbot',
  kind: s.kind,
  question: s.question ?? s.questions?.[0]?.question,
  questions: s.questions,
  updated: s.updated_at,
  archived: s.archived,
});

/** "9:12" today, "Wed" this week, "Nov 12" before. */
export function when(iso: string, now = new Date()): string {
  const d = new Date(iso);
  if (isNaN(+d)) return '';
  if (d.toDateString() === now.toDateString()) return d.toLocaleTimeString('en-GB', { hour: 'numeric', minute: '2-digit' });
  if (now.getTime() - d.getTime() < 6 * 864e5) return d.toLocaleDateString('en', { weekday: 'short' });
  return d.toLocaleDateString('en', { month: 'short', day: 'numeric' });
}

export const board = $state({
  /** Long-lived rooms (Matrix…). */
  channels: [] as Session[],
  /** The owner's open sessions, newest first (jobs' runs included, as the kernel lists them). */
  sessions: [] as Session[],
  /** Every child (subagent, verifier) by its parent. */
  children: {} as Record<string, (BoardSession & { role?: string | null })[]>,
  archivedCount: 0,
  /** How many of the archived are job runs (kind `job`). */
  archivedJobs: 0,
  /** The archive as last loaded: the owner's sessions, or with job runs too. */
  archived: [] as Session[],
  loaded: false,
  error: '',
});

export async function loadBoard() {
  try {
    const b = await zen.board();
    const all = b.sessions as (BoardSession & { questions?: Question[]; question?: string; role?: string | null })[];
    const top = all.filter((s) => !s.parent && !s.archived);
    board.channels = top.filter((s) => s.kind === 'channel').map(asSession);
    board.sessions = top.filter((s) => s.kind !== 'channel').sort((a, b) => b.updated_at.localeCompare(a.updated_at)).map(asSession);
    const kids: Record<string, typeof all> = {};
    for (const s of all) if (s.parent) (kids[s.parent] ??= []).push(s);
    board.children = kids;
    board.archivedCount = b.archived ?? 0;
    board.archivedJobs = b.archived_jobs ?? 0;
    board.error = '';
  } catch (e) {
    board.error = (e as Error).message;
  }
  board.loaded = true;
}

/** The archive: the owner's sessions only, or with `jobs` their runs too. */
export async function loadArchived(jobs = false) {
  try {
    const b = await zen.board(true, jobs);
    board.archived = b.sessions.filter((s) => s.archived && !s.parent).sort((a, b) => b.updated_at.localeCompare(a.updated_at)).map(asSession);
    board.archivedCount = Math.max(board.archivedCount, b.archived ?? 0);
  } catch {}
}

export const waitingCount = () => board.sessions.filter((s) => s.state === 'waiting').length;
export const findSession = (id: string) => [...board.channels, ...board.sessions, ...board.archived].find((s) => s.id === id) ?? null;
export { ago };

// ---- agents ----

/** An agent as the screens show it: its orb's two colours (zenbot's moss, then a few calm others). */
export type A = { id: string; name: string; role: string; a: string; b: string; info: Agent & { busy?: { id: string; title: string }[]; can_use?: string; chats?: number; roles?: Agent['roles'] } };
const ORBS: [string, string][] = [['#4f7d67', '#8fb39f'], ['#7a6a9b', '#b3a3cf'], ['#a8762c', '#cfa660'], ['#4f6f8f', '#9ab3cc'], ['#8f5a4f', '#c99a8f']];
const display = (n: string) => (n === 'zenbot' ? 'zenbot' : n[0].toUpperCase() + n.slice(1));
export const agents = $state({ list: [{ id: 'zenbot', name: 'zenbot', role: 'Chief of staff', a: ORBS[0][0], b: ORBS[0][1], info: { name: 'zenbot' } }] as A[], loaded: false });

export async function loadAgents() {
  try {
    const l = await zen.agents();
    l.sort((x, y) => (x.name === 'zenbot' ? -1 : y.name === 'zenbot' ? 1 : x.name.localeCompare(y.name)));
    agents.list = l.map((a, i) => ({
      id: a.name,
      name: display(a.name),
      role: a.error ? `Error: ${a.error}` : a.description ? a.description[0].toUpperCase() + a.description.slice(1) : a.name === 'zenbot' ? 'Chief of staff' : 'Agent',
      a: ORBS[i % ORBS.length][0],
      b: ORBS[i % ORBS.length][1],
      info: a as A['info'],
    }));
  } catch {}
  agents.loaded = true;
}
export const agentOf = (id: string | null | undefined) => agents.list.find((a) => a.id === (id || 'zenbot')) ?? agents.list[0];

// ---- models ----

export type Model = { id: string; name: string; group: string; hint: string; efforts: string[]; def: string; more?: boolean };
export const EFFORT: Record<string, [string, string]> = {
  low: ['Low', 'Quick replies'], medium: ['Medium', 'Everyday tasks'], high: ['High', 'Thinks it through'],
  xhigh: ['Extra high', 'Careful, slower'], max: ['Max', 'Hardest problems'], ultra: ['Ultra', 'Longest thinking'],
  minimal: ['Minimal', 'Fastest'], none: ['None', 'No thinking'],
};
export const effortName = (e: string) => EFFORT[e]?.[0] ?? e;
const HINTS: [RegExp, string][] = [[/opus/i, 'Smartest, for hard problems'], [/sonnet/i, 'Great for most things'], [/haiku/i, 'Fast and cheap']];
const GROUP: Record<string, string> = { 'claude-code': 'Claude', claude: 'Claude', codex: 'OpenAI', openrouter: 'OpenRouter' };

/** "Claude Opus 5.5" → "Opus 5.5", "Codex GPT-6.2-Sol" → "GPT-6.2 Sol". */
export const modelName = (name: string) => name.replace(/^(Claude|Codex)\s+/, '').replace(/^(GPT-[\d.]+)-/, '$1 ');

export const models = $state({ list: [] as Model[], default: '', raw: null as Models | null });

export async function loadModels() {
  try {
    const m = await zen.models();
    const seen: Record<string, number> = {};
    models.raw = m;
    models.list = m.models.map((x) => {
      const prov = x.id.split('/')[0];
      const group = GROUP[x.engine ?? ''] ?? GROUP[prov] ?? prov;
      const n = (seen[group] = (seen[group] ?? 0) + 1);
      const name = modelName(x.name ?? x.id.split('/').pop() ?? x.id);
      return {
        id: x.id, name, group,
        hint: HINTS.find(([re]) => re.test(x.id))?.[1] ?? (group === 'OpenAI' && n === 1 ? 'Newest from OpenAI' : ''),
        efforts: x.efforts ?? [],
        def: x.default_effort ?? x.efforts?.[0] ?? '',
        more: n > (group === 'Claude' ? 3 : 2),
      };
    });
    models.default = m.default;
  } catch {}
}
export const modelOf = (id: string) => models.list.find((m) => m.id === id) ?? { id, name: modelName(id.split('/').pop() ?? id), group: '', hint: '', efforts: [], def: '' };

// ---- new-chat ideas (the kernel's assist model, cached for the day) ----

export type Sugg = { title: string; why: string; prompt: string };
const ideaCache: Record<string, Sugg[]> = {};
export async function loadIdeas(agent: string, signal?: AbortSignal): Promise<Sugg[]> {
  if (ideaCache[agent]) return ideaCache[agent];
  const r = await fetch(`/api/zen/ideas?agent=${encodeURIComponent(agent)}`, { signal });
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  const d = (await r.json()) as { ideas?: unknown };
  // The kernel gives `{text, why}`; older kernels plain strings (no why), the mock `{title, why, prompt}`.
  const list = (Array.isArray(d.ideas) ? d.ideas : [])
    .map((x): Sugg | null => {
      if (typeof x === 'string') return x.trim() ? { title: x, why: '', prompt: x } : null;
      if (!x || typeof x !== 'object') return null;
      const o = x as { text?: unknown; title?: unknown; why?: unknown; prompt?: unknown };
      const title = typeof o.text === 'string' ? o.text : typeof o.title === 'string' ? o.title : '';
      return title.trim() ? { title, why: typeof o.why === 'string' ? o.why : '', prompt: typeof o.prompt === 'string' ? o.prompt : title } : null;
    })
    .filter((x): x is Sugg => !!x)
    .slice(0, 3);
  if (!list.length) return list; // none today (the model failed): ask again next time
  return (ideaCache[agent] = list);
}

// ---- steps in plain words, and the subagents' sessions ----

/** A tool call as a step the owner reads: "Searched the web · cheap flights". */
export type Step = { label: string; detail: string; t?: string; failed?: boolean; now?: boolean };
const LABEL: Record<string, string> = {
  read: 'Read a file', write: 'Wrote a file', edit: 'Edited a file', bash: 'Ran a command', web_search: 'Searched the web',
  web_fetch: 'Opened a page', search: 'Searched your notes', history: 'Read the history', brain: 'Looked in the brain',
  remember: 'Remembered', find_skills: 'Looked for a skill', load_skill: 'Loaded a skill', save_skill: 'Saved a skill',
  delegate: 'Started a subagent', subagent: 'Started a subagent', spawn: 'Started a subagent', judge: 'Judged a batch',
  verify: 'Checked the work', ask: 'Asked you', questions: 'Asked you', find_tools: 'Looked for a tool', load_tool: 'Loaded a tool', call_tool: 'Used a tool',
};
export function stepOf(name: string, args: Record<string, unknown> | undefined): Step {
  const base = name.split('__').pop() ?? name;
  const a = args ?? {};
  const pick = (...k: string[]) => k.map((x) => a[x]).find((v) => typeof v === 'string' && v.trim()) as string | undefined;
  let detail = pick('query', 'url', 'path', 'command', 'title', 'task', 'name', 'question', 'text', 'action') ?? '';
  if (!detail && a.from) detail = `${a.from} → ${a.to ?? ''}`;
  if (!detail) { const j = JSON.stringify(a); detail = j === '{}' ? '' : j; }
  detail = detail.split('\n')[0].replace(/^\/home\/[^/]+/, '~').slice(0, 140);
  return { label: LABEL[base] ?? base.replace(/_/g, ' ').replace(/^./, (c) => c.toUpperCase()), detail };
}
/** "0:42" since a start. */
export const clock = (ms: number) => { const s = Math.max(0, Math.floor(ms / 1000)); return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`; };

export type Kid = { id: string; name: string; role: string; model: string; task: string; steps: Step[]; result: string; done: boolean; t: string; loaded: boolean };
/** Subagents' sessions as read (their steps and result), by id. */
export const kids = $state<Record<string, Kid>>({});

/** Read a subagent's session: its task, steps, and its last words when done. */
export async function loadKid(id: string) {
  const b = Object.values(board.children).flat().find((s) => s.id === id);
  try {
    const d = await zen.session(id, 60);
    const start = Date.parse(d.created_at);
    const steps: Step[] = [];
    let result = '';
    let task = b?.task ?? '';
    for (const m of d.messages) {
      if (m.role === 'user' && !task) task = typeof m.content === 'string' ? m.content : m.content.filter((x) => x.type === 'text').map((x) => x.text ?? '').join('\n');
      if (m.role === 'assistant' && typeof m.content !== 'string') {
        for (const c of m.content) if (c.type === 'toolCall') steps.push({ ...stepOf(c.name ?? '', c.arguments), t: m.timestamp ? clock(m.timestamp - start) : '' });
        const text = m.content.filter((x) => x.type === 'text').map((x) => x.text ?? '').join('').trim();
        if (text) result = text;
      } else if (m.role === 'toolResult' && m.isError && steps.length) steps[steps.length - 1].failed = true;
    }
    const done = !d.busy;
    const end = done ? Date.parse(d.updated_at) : Date.now();
    kids[id] = {
      id, name: (d.title ?? '').replace(/^subagent:\s*/i, '') || (b?.role ?? 'Subagent'), role: (b as { role?: string } | undefined)?.role ?? d.kind ?? 'subagent',
      model: modelOf(d.model).name, task: task.trim(), steps, result: done ? result : '', done, t: clock(end - start), loaded: true,
    };
  } catch {}
}
