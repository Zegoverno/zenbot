// The mock's tasks, shared by the Tasks tab and Home's task widgets (one list, as the vault's
// tasks.md files are in the real app). Kept for the browser tab's life in sessionStorage.
export const TODAY = '2026-11-19';
export type Pri = 'urgent' | 'important' | 'medium' | 'not_urgent' | 'parking' | null;
export type T = { id: string; text: string; project: string; goal: string | null; due: string | null; pri: Pri; done?: boolean; doneOn?: string; agent?: 'working' | 'waiting'; by?: string; session?: string };
export const PROJECTS = ['Family', 'Campos do Jordão', 'Baby room', 'House', 'Health'];
export const GOALS = ['A present and courageous family life', 'Friendship with God and faithful stewardship', 'Rebuild a sustainable athletic life'];
// The vault's priorities (its AGENTS.md, "Tasks"); 🔼 medium is still read and sorted, never offered.
export const PRI: Record<string, { l: string; c: string; h: string }> = {
  urgent: { l: 'Urgent', c: 'var(--bad, #b5543c)', h: 'Must be done now, not tied to a goal' },
  important: { l: 'Important', c: 'var(--warn)', h: 'Moves a goal forward' },
  medium: { l: 'Medium', c: 'var(--accent)', h: 'From Obsidian Tasks' },
  not_urgent: { l: 'Not urgent', c: 'var(--dim)', h: 'Nice to do before it’s due or becomes urgent' },
  parking: { l: 'Parking lot', c: 'var(--faint)', h: 'Someday, maybe' },
};
export const OFFER = ['urgent', 'important', 'not_urgent', 'parking'];
const G = (i: number) => GOALS[i];
export const SEED: T[] = [
  { id: 'a1', text: 'Pay the electricity bill', project: 'House', goal: null, due: '2026-11-17', pri: 'urgent' },
  { id: 'a2', text: 'Call Mom: her birthday', project: 'Family', goal: G(0), due: TODAY, pri: 'important' },
  { id: 'a3', text: 'Pick up the dry cleaning', project: 'House', goal: null, due: TODAY, pri: null },
  { id: 'a4', text: 'Ask Mom to water the plants', project: 'Campos do Jordão', goal: null, due: '2026-11-20', pri: null },
  { id: 'a5', text: 'Fill the tank', project: 'Campos do Jordão', goal: null, due: '2026-11-20', pri: null },
  { id: 'a6', text: 'Pack the baby’s warm clothes', project: 'Campos do Jordão', goal: G(0), due: '2026-11-21', pri: 'important' },
  { id: 'a7', text: 'Paediatrician: 6-month check-up', project: 'Family', goal: G(0), due: '2026-11-26', pri: 'important', agent: 'waiting', session: 'task-a7' },
  { id: 'a8', text: 'Renew car insurance', project: 'House', goal: null, due: '2026-11-30', pri: 'not_urgent', agent: 'working', session: 'task-a8' },
  { id: 'a9', text: 'Choose the crib mattress', project: 'Baby room', goal: G(0), due: '2026-12-02', pri: null },
  { id: 'b1', text: 'Set up a monthly tithe', project: 'Family', goal: G(1), due: '2026-12-05', pri: 'important' },
  { id: 'b2', text: 'Book the physio follow-up', project: 'Health', goal: G(2), due: '2026-12-08', pri: null },
  { id: 'b3', text: 'Plan Advent prayer at home', project: 'Family', goal: G(1), due: null, pri: 'not_urgent' },
  { id: 'b4', text: 'Paint the baby room’s shelf', project: 'Baby room', goal: null, due: null, pri: 'parking' },
  { id: 'b5', text: 'Try swimming again on Saturdays', project: 'Health', goal: G(2), due: null, pri: 'not_urgent' },
];
// ---- dates ----
const D = (iso: string) => new Date(iso + 'T12:00:00');
export const days = (iso: string) => Math.round((D(iso).getTime() - D(TODAY).getTime()) / 864e5);
export function when(iso: string | null) {
  if (!iso) return '';
  const n = days(iso);
  if (n < 0) return n === -1 ? 'Yesterday' : `${-n} days late`;
  if (n === 0) return 'Today';
  if (n === 1) return 'Tomorrow';
  if (n < 7) return D(iso).toLocaleDateString('en', { weekday: 'short' });
  return D(iso).toLocaleDateString('en', { month: 'short', day: 'numeric' });
}
export const PORDER = ['urgent', 'important', 'medium', 'not_urgent', null, 'parking'];
export const sort = (a: T, b: T) => PORDER.indexOf(a.pri) - PORDER.indexOf(b.pri) || (a.due ?? '9').localeCompare(b.due ?? '9');


const KEY = 'zen.mock.tasks3';
export const store = $state<{ tasks: T[] }>({ tasks: JSON.parse(sessionStorage.getItem(KEY) ?? 'null') ?? SEED });
$effect.root(() => { $effect(() => { sessionStorage.setItem(KEY, JSON.stringify(store.tasks)); }); });

/** Tick or untick; a task ticked today stays in Home's widgets, struck through, until tomorrow. */
export function toggle(t: T) {
  t.done = !t.done;
  t.doneOn = t.done ? TODAY : undefined;
}

/** A Home task widget's settings, chosen in Customize. */
export type TaskView = { show: 'today' | 'week' | 'all'; order: 'when' | 'priority'; project: string | null; goal: string | null; pri: string[]; limit: number };
export const TASK_VIEW: TaskView = { show: 'today', order: 'when', project: null, goal: null, pri: [], limit: 5 };
const priKey = (t: T) => t.pri ?? 'none';
export function viewTasks(v: TaskView) {
  const fits = (t: T) =>
    (v.show === 'all' || (t.due != null && days(t.due) <= (v.show === 'today' ? 0 : 7))) &&
    (!v.project || t.project === v.project) &&
    (!v.goal || (v.goal === '-' ? !t.goal : t.goal === v.goal)) &&
    (!v.pri.length || v.pri.includes(priKey(t)));
  const byWhen = (a: T, b: T) => (a.due ?? '9').localeCompare(b.due ?? '9') || sort(a, b);
  const open = store.tasks.filter((t) => !t.done && fits(t)).sort(v.order === 'when' ? byWhen : sort);
  const doneToday = store.tasks.filter((t) => t.done && t.doneOn === TODAY && fits(t));
  return { open, doneToday };
}
export function viewTitle(v: TaskView) {
  const base = v.show === 'today' ? 'Today' : v.show === 'week' ? 'Next 7 days' : 'Tasks';
  const what = v.project ?? (v.goal && v.goal !== '-' ? v.goal : null) ?? (v.pri.length === 1 ? PRI[v.pri[0]]?.l ?? 'No priority' : null);
  return what ? `${base} · ${what}` : base;
}
