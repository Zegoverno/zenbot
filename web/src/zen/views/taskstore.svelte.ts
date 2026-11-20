// The vault's tasks (Projects/<p>/tasks.md through the kernel's /api/tasks), shared by the Tasks
// tab and Home's task widgets, in the shapes the mock's taskstore had. The vault stays the truth:
// every write carries the revision it read (409 when the line changed meanwhile), and the list
// reloads after each one.
export type Pri = 'urgent' | 'important' | 'medium' | 'not_urgent' | 'parking_lot' | null;
export type T = { id: string; rev: string; text: string; project: string; goal: string | null; due: string | null; pri: Pri; done?: boolean; doneOn?: string | null; dropped?: boolean; agent?: 'working' | 'waiting'; by?: string; session?: string };

/** Today in the vault's time zone (the kernel's); the browser's until the first load. */
export const day = $state({ today: new Date().toLocaleDateString('en-CA') });
export const meta = $state({ projects: [] as string[], goals: [] as string[] });

// The vault's priorities (its AGENTS.md, "Tasks"); 🔼 medium is still read and sorted, never offered.
export const PRI: Record<string, { l: string; c: string; h: string }> = {
  urgent: { l: 'Urgent', c: 'var(--bad, #b5543c)', h: 'Must be done now, not tied to a goal' },
  important: { l: 'Important', c: 'var(--warn)', h: 'Moves a goal forward' },
  medium: { l: 'Medium', c: 'var(--accent)', h: 'From Obsidian Tasks' },
  not_urgent: { l: 'Not urgent', c: 'var(--dim)', h: 'Nice to do before it’s due or becomes urgent' },
  parking_lot: { l: 'Parking lot', c: 'var(--faint)', h: 'Someday, maybe' },
};
export const OFFER = ['urgent', 'important', 'not_urgent', 'parking_lot'];

// ---- dates ----
const D = (iso: string) => new Date(iso + 'T12:00:00');
const iso = (d: Date) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
export const days = (d: string) => Math.round((D(d).getTime() - D(day.today).getTime()) / 864e5);
/** A day `n` days from today. */
export const plus = (n: number) => { const d = D(day.today); d.setDate(d.getDate() + n); return iso(d); };
export function when(d: string | null) {
  if (!d) return '';
  const n = days(d);
  if (n < 0) return n === -1 ? 'Yesterday' : `${-n} days late`;
  if (n === 0) return 'Today';
  if (n === 1) return 'Tomorrow';
  if (n < 7) return D(d).toLocaleDateString('en', { weekday: 'short' });
  return D(d).toLocaleDateString('en', { month: 'short', day: 'numeric' });
}
export const PORDER = ['urgent', 'important', 'medium', 'not_urgent', null, 'parking_lot'];
export const sort = (a: T, b: T) => PORDER.indexOf(a.pri) - PORDER.indexOf(b.pri) || (a.due ?? '9').localeCompare(b.due ?? '9');

// ---- the store and the kernel ----
export const store = $state<{ tasks: T[]; error: string; loaded: boolean }>({ tasks: [], error: '', loaded: false });

type Raw = { id: string; rev: string; text: string; project: string; goal?: string | null; goals?: string[]; due: string | null; priority: Pri; done?: boolean; done_on?: string | null; dropped?: boolean; agent?: string | { session: string; state?: string } | null; agent_state?: string; session?: string | null };

export async function call<R = unknown>(method: string, url: string, body?: unknown): Promise<R> {
  const r = await fetch(url, { method, headers: method === 'GET' ? {} : { 'content-type': 'application/json', 'x-zen-web': '1' }, body: body === undefined ? undefined : JSON.stringify(body) });
  const data = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(data.error ?? `HTTP ${r.status}`);
  return data as R;
}

export function fromRaw(t: Raw): T {
  const ag = typeof t.agent === 'object' && t.agent ? t.agent : null;
  const by = typeof t.agent === 'string' ? t.agent : ag ? 'zenbot' : undefined;
  const state = t.agent_state ?? ag?.state;
  const session = t.session ?? ag?.session ?? undefined;
  return {
    id: t.id, rev: t.rev, text: t.text, project: t.project,
    goal: t.goal ?? t.goals?.[0] ?? null, due: t.due, pri: t.priority,
    done: !!t.done, doneOn: t.done_on ?? null, dropped: t.dropped,
    agent: by || session ? (state === 'waiting' ? 'waiting' : state === 'idle' ? undefined : 'working') : undefined,
    by, session: session || undefined,
  };
}

export async function load() {
  try {
    const data = await call<{ today: string; problems: string[]; projects: { tasks: Raw[] }[] }>('GET', '/api/zen/tasks?done=7d');
    day.today = data.today || day.today;
    store.tasks = data.projects.flatMap((p) => p.tasks.map(fromRaw));
    store.error = data.problems?.length ? `Can't read: ${data.problems.join(', ')}` : '';
  } catch (e) {
    store.error = (e as Error).message;
  }
  store.loaded = true;
}
export async function loadMeta() {
  try {
    const m = await call<{ projects: { id: string }[]; goals: { id: string; status: string }[] }>('GET', '/api/zen/tasks/meta');
    meta.projects = m.projects.map((p) => p.id);
    meta.goals = m.goals.filter((g) => g.status === 'active').map((g) => g.id);
  } catch {}
}
export async function patch(t: T, fields: Record<string, unknown>) {
  await call('PATCH', `/api/zen/tasks/${encodeURIComponent(t.id)}`, { ...fields, rev: t.rev });
}
export async function remove(t: T) {
  await call('DELETE', `/api/zen/tasks/${encodeURIComponent(t.id)}?rev=${encodeURIComponent(t.rev)}`);
}
export async function add(t: { text: string; project: string; due: string | null; pri: Pri; goal: string | null }) {
  return fromRaw(await call<Raw>('POST', '/api/zen/tasks', { text: t.text, project: t.project, due: t.due, priority: t.pri, goal: t.goal }));
}
/** Hand a task to an agent: a session starts on it (or the one already on it is returned). */
export async function hand(t: T, agent: string) {
  return call<{ session: string; existing?: boolean }>('POST', `/api/zen/tasks/${encodeURIComponent(t.id)}/agent`, { agent, rev: t.rev });
}

/** Tick or untick; a task ticked today stays in Home's widgets, struck through, until tomorrow. */
export async function toggle(t: T) {
  const done = !t.done;
  t.done = done;
  t.doneOn = done ? day.today : null;
  try {
    await patch(t, { done });
  } catch (e) {
    t.done = !done;
    store.error = `Could not update the task: ${(e as Error).message}`;
  }
  await load();
}

/** A Home task widget's settings, chosen in Customize. */
export type TaskView = { show: 'today' | 'week' | 'all'; order: 'when' | 'priority'; project: string | null; goal: string | null; pri: string[]; limit: number };
export const TASK_VIEW: TaskView = { show: 'today', order: 'when', project: null, goal: null, pri: [], limit: 5 };
const priKey = (t: T) => t.pri ?? 'none';
export function viewTasks(v: TaskView, tasks: T[] = store.tasks) {
  const fits = (t: T) =>
    (v.show === 'all' || (t.due != null && days(t.due) <= (v.show === 'today' ? 0 : 7))) &&
    (!v.project || t.project === v.project) &&
    (!v.goal || (v.goal === '-' ? !t.goal : t.goal === v.goal)) &&
    (!v.pri.length || v.pri.includes(priKey(t)));
  const byWhen = (a: T, b: T) => (a.due ?? '9').localeCompare(b.due ?? '9') || sort(a, b);
  const open = tasks.filter((t) => !t.done && !t.dropped && fits(t)).sort(v.order === 'when' ? byWhen : sort);
  const doneToday = tasks.filter((t) => t.done && t.doneOn === day.today && fits(t));
  return { open, doneToday };
}
export function viewTitle(v: TaskView) {
  const base = v.show === 'today' ? 'Today' : v.show === 'week' ? 'Next 7 days' : 'Tasks';
  const what = v.project ?? (v.goal && v.goal !== '-' ? v.goal : null) ?? (v.pri.length === 1 ? PRI[v.pri[0]]?.l ?? 'No priority' : null);
  return what ? `${base} · ${what}` : base;
}
