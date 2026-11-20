import type { Task } from '../api.ts';
export type Kind = 'needs'|'working'|'today'|'recent'|'reports'|'agents'|'files';
export type Filter = { show: 'today'|'week'|'all'; order: 'when'|'priority'; project: string; goal: string; priority: string; limit: number };
export type Slot = { id: string; kind: Kind; size: 1|2|3; filter: Filter };
export const catalog: Record<Kind, { title: string; hint: string; size: 1|2|3 }> = {
  needs: {title:'Needs you', hint:'Questions waiting for your answer', size:2}, working: {title:'Working now', hint:'What your agents are doing', size:1}, today: {title:'Tasks', hint:'A list of your tasks', size:1}, recent: {title:'Recent chats', hint:'Pick up where you left off', size:1}, reports: {title:'From your routines', hint:'Results of scheduled jobs', size:1}, agents: {title:'Agents', hint:'Who is on duty', size:1}, files: {title:'Recent files', hint:'Recently changed files in the vault root', size:1},
};
export const filter = (): Filter => ({show:'today',order:'when',project:'',goal:'',priority:'',limit:5});
export const slot = (kind: Kind, id: string = kind): Slot => ({id, kind, size:catalog[kind].size, filter:filter()});
export const defaults = (): Slot[] => (['needs','working','today','recent','reports'] as Kind[]).map(k => slot(k));
export function layoutOf(value: unknown): Slot[] {
  if (!Array.isArray(value)) return defaults();
  const ids = new Set<string>();
  const result: Slot[] = [];
  for (const v of value) {
    if (!v || typeof v !== 'object' || typeof v.id !== 'string' || ids.has(v.id) || (typeof v.kind !== 'string' || !Object.hasOwn(catalog, v.kind)) || ![1,2,3].includes(v.size)) return defaults();
    ids.add(v.id);
    const f = v.filter ?? {};
    result.push({id:v.id, kind:v.kind, size:v.size, filter:{show:['today','week','all'].includes(f.show) ? f.show : 'today', order:f.order === 'priority' ? 'priority':'when', project:typeof f.project === 'string' ? f.project:'', goal:typeof f.goal === 'string' ? f.goal:'', priority:typeof f.priority === 'string' ? f.priority:'', limit:[5,8,12].includes(f.limit) ? f.limit:5}});
  }
  return result;
}
export type LiveTask = Task & { done: boolean; done_on: string|null; rev: string };
export function selectTasks(tasks: LiveTask[], f: Filter, today: string): LiveTask[] {
  const date = new Date(`${today}T12:00:00`); date.setDate(date.getDate()+7);
  const week = `${date.getFullYear()}-${String(date.getMonth()+1).padStart(2,'0')}-${String(date.getDate()).padStart(2,'0')}`;
  const ranks: Record<string,number> = {urgent:0,important:1,medium:2,not_urgent:3,parking_lot:4};
  return tasks.filter(t => !t.done && (!f.project || t.project === f.project) && (!f.goal || (f.goal === '-' ? !t.goals.length : t.goals.includes(f.goal))) && (!f.priority || (f.priority === 'none' ? !t.priority : t.priority === f.priority)) && (f.show === 'all' || !!t.due && t.due <= (f.show === 'today' ? today : week)))
    .sort((a,b) => f.order === 'priority' ? (ranks[a.priority ?? ''] ?? 5)-(ranks[b.priority ?? ''] ?? 5) || (a.due ?? '9999').localeCompare(b.due ?? '9999') : (a.due ?? '9999').localeCompare(b.due ?? '9999'));
}
