// Task API adapter: vault remains authoritative, writes carry the read revision.
export type Pri = 'urgent' | 'important' | 'medium' | 'not_urgent' | 'parking_lot' | null;
export type T = { id:string; rev:string; text:string; project:string; goal:string|null; due:string|null; pri:Pri; done?:boolean; agent?:'working'|'waiting'; by?:string; session?:string };
export let TODAY = new Date().toLocaleDateString('en-CA');
export const PRI: Record<string, { l: string; c: string; h: string }> = {
  urgent: { l: 'Urgent', c: 'var(--bad, #b5543c)', h: 'Must be done now, not tied to a goal' },
  important: { l: 'Important', c: 'var(--warn)', h: 'Moves a goal forward' },
  medium: { l: 'Medium', c: 'var(--accent)', h: 'From Obsidian Tasks' },
  not_urgent: { l: 'Not urgent', c: 'var(--dim)', h: 'Nice to do before it’s due or becomes urgent' },
  parking_lot: { l: 'Parking lot', c: 'var(--faint)', h: 'Someday, maybe' },
};
export const OFFER = ['urgent', 'important', 'not_urgent', 'parking_lot'];
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
export const PORDER = ['urgent', 'important', 'medium', 'not_urgent', null, 'parking_lot'];
export const sort = (a: T, b: T) => PORDER.indexOf(a.pri) - PORDER.indexOf(b.pri) || (a.due ?? '9').localeCompare(b.due ?? '9');



export const store = $state<{tasks:T[]}>({tasks:[]});
type Raw = {id:string; rev:string; text:string; project:string; goal:string|null; due:string|null; priority:Pri; done:boolean; agent?:string; session?:string; agent_state?:string};
export async function call<T = unknown>(method:string, url:string, body?:unknown):Promise<T> { const r = await fetch(url,{method,headers:method === 'GET' ? {} : {'content-type':'application/json','x-zen-web':'1'},body:body === undefined ? undefined : JSON.stringify(body)}); const data = await r.json().catch(() => ({})); if (!r.ok) throw new Error(data.error ?? `HTTP ${r.status}`); return data as T; }
export async function load() { const data = await call<{today:string; problems:string[]; projects:{tasks:Raw[]}[]}>('GET','/api/zen/tasks?done=7d'); TODAY = data.today; store.tasks = data.projects.flatMap(p => p.tasks.map(t => ({...t, pri:t.priority, agent:t.agent ? (t.agent_state === 'waiting' ? 'waiting' as const : 'working' as const) : undefined, by:t.agent}))); if (data.problems.length) throw new Error(`Can't read: ${data.problems.join(', ')}`); }
export async function patch(t:T, fields:Record<string,unknown>) { await call('PATCH', `/api/zen/tasks/${encodeURIComponent(t.id)}`, {...fields, rev:t.rev}); }
export async function remove(t:T) { await call('DELETE', `/api/zen/tasks/${encodeURIComponent(t.id)}?rev=${encodeURIComponent(t.rev)}`); }
