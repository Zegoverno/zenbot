// Routines shared by the Routines view, its tab badge and Home (GET /api/jobs, in the mock's
// shapes): every job in ONE list (agent and script jobs, and zenbot's own system upkeep, told
// apart by `kind`, `agent` and `plugin`), each with its schedule in plain words, its last result,
// and a derived state: active, paused (enabled=false, whatever a stale next_run_at says) or done
// (a one-off `at …` that has run and has nothing next). Also the runs (GET /api/jobs/runs, newest
// first, `routines.limit` of them) and what the view remembers while you are elsewhere: its tab
// (Routines / Runs) and its filters (type, source, state). Run now and pause/resume write to the
// kernel. Times are shown in the owner's zone (the jobs' own).
import { routines as api } from '../api.ts';

export type Last = 'ok' | 'silent' | 'error' | 'none';
/** active: scheduled and on; paused: switched off; done: a one-off that has already run. */
export type JobState = 'active' | 'paused' | 'done';
export type Job = {
  name: string; kind: 'agent' | 'script' | 'system'; agent: string; plugin: string | null;
  /** In plain words what zenbot's upkeep does (system jobs only). */
  what: string; when: string; last: Last; state: JobState;
  /** Whether it is enabled (the switch); a done one-off is enabled but has no switch. */
  on: boolean; error: string | null; next: string | null; lastRunAt: string | null;
};
export type Run = { id: number; job: string; day: string; at: string; ts: number; took: string; status: 'ok' | 'silent' | 'error' | 'running'; manual: boolean; note: string; session: string | null };

type RawJob = { name: string; kind?: string; agent?: string | null; plugin?: string | null; last_run_at?: string | null; action?: string | null; command?: string | null; schedule: string; tz: string; enabled: boolean; next_run_at: string | null; last_status: string | null; last_error: string | null };
type RawRun = { id: number; job: string; status: string; started_at: string; ended_at?: string | null; error: string | null; session: string | null; trigger?: string; output?: string | null };

/** What zenbot's own jobs do, in plain words. */
const UPKEEP: Record<string, string> = {
  sleep: 'Sorts memory', wiki: 'Wiki upkeep', skills: 'Skills upkeep', prune: 'Cleans old files',
  engines: 'Updates Claude and Codex', 'session-review': 'Reviews finished sessions', backup: 'Backs up zenbot',
};

const pad = (n: string) => n.padStart(2, '0');
const DOW = ['Sundays', 'Mondays', 'Tuesdays', 'Wednesdays', 'Thursdays', 'Fridays', 'Saturdays'];

/** A time of day in `to` (the owner's zone) for h:m in zone `from`. */
function shift(h: number, m: number, from: string, to: string): [number, number] {
  if (!to || from === to) return [h, m];
  try {
    const off = (tz: string) => {
      const d = new Date();
      const p = Object.fromEntries(new Intl.DateTimeFormat('en-US', { timeZone: tz, hourCycle: 'h23', year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }).formatToParts(d).map((x) => [x.type, x.value]));
      return (Date.UTC(+p.year, +p.month - 1, +p.day, +p.hour, +p.minute) - Math.floor(d.getTime() / 60000) * 60000) / 60000;
    };
    const t = (((h * 60 + m - off(from) + off(to)) % 1440) + 1440) % 1440;
    return [Math.floor(t / 60), t % 60];
  } catch {
    return [h, m];
  }
}

/** A schedule in plain words ("0 9 * * *" → "Every day at 9:00", "every 10m" → "Every 10 minutes"),
 *  times in the owner's zone (`to`); the schedule itself when it can't tell. `nightly` says
 *  "Nightly" for small hours (zenbot's upkeep). */
export function cronWords(s: string, from = '', to = '', nightly = false): string {
  if (s === 'event') return 'When a session ends';
  const every = /^every\s+(\d+)\s*([mhd])$/i.exec(s.trim());
  if (every) {
    const n = Number(every[1]);
    const unit = { m: 'minute', h: 'hour', d: 'day' }[every[2].toLowerCase() as 'm' | 'h' | 'd'];
    return n === 1 ? `Every ${unit}` : `Every ${n} ${unit}s`;
  }
  const once = /^at\s+(.+)$/.exec(s.trim());
  if (once) {
    const d = new Date(once[1]);
    return isNaN(+d) ? s : `Once, ${d.toLocaleDateString('en', { month: 'short', day: 'numeric', ...(to ? { timeZone: to } : {}) })} at ${d.toLocaleTimeString('en-GB', { hour: 'numeric', minute: '2-digit', ...(to ? { timeZone: to } : {}) })}`;
  }
  const f = s.trim().split(/\s+/);
  if (f.length !== 5) return s;
  const [m, h, dom, mon, dow] = f;
  if (/^\*\/\d+$/.test(m) && h === '*' && dom === '*' && mon === '*' && dow === '*') return `Every ${m.slice(2)} minutes`;
  if (/^\d+$/.test(m) && h === '*' && dom === '*' && mon === '*' && dow === '*') return 'Every hour';
  if (/^\d+$/.test(m) && /^\*\/\d+$/.test(h) && dom === '*' && mon === '*' && dow === '*') return `Every ${h.slice(2)} hours`;
  if (/^\d+$/.test(m) && /^\d+$/.test(h) && dom === '*' && mon === '*') {
    const [hh, mm] = shift(Number(h), Number(m), from, to);
    const at = `${hh}:${pad(String(mm))}`;
    if (dow === '*') return nightly && hh < 6 ? `Nightly ${at}` : `Every day at ${at}`;
    if (/^\d$/.test(dow)) return `${DOW[Number(dow) % 7]} at ${at}`;
    if (dow === '1-5') return `Weekdays at ${at}`;
  }
  return s;
}

const lastOf = (s: string | null): Last => (s === 'ok' ? 'ok' : s === 'silent' ? 'silent' : s === 'error' || s === 'failed' ? 'error' : 'none');

/** The owner's zone, when it is a zone the browser knows. */
function zone(tz: string): { timeZone?: string } {
  if (!tz) return {};
  try { new Intl.DateTimeFormat('en', { timeZone: tz }); return { timeZone: tz }; } catch { return {}; }
}
/** "9:28" in the zone. */
export const clock = (d: Date, tz: string) => d.toLocaleTimeString('en-GB', { hour: 'numeric', minute: '2-digit', hourCycle: 'h23', ...zone(tz) }).replace(/^0/, '');
/** "Nov 19" in the zone. */
export const dateOf = (d: Date, tz: string) => d.toLocaleDateString('en', { month: 'short', day: 'numeric', ...zone(tz) });
const ymd = (d: Date, tz: string) => d.toLocaleDateString('en-CA', zone(tz));
/** "Today", "Tomorrow", "Yesterday" or "Thu, Nov 19" in the zone. */
export function dayWord(d: Date, tz: string): string {
  const k = ymd(d, tz);
  if (k === ymd(new Date(), tz)) return 'Today';
  if (k === ymd(new Date(Date.now() + 864e5), tz)) return 'Tomorrow';
  if (k === ymd(new Date(Date.now() - 864e5), tz)) return 'Yesterday';
  return d.toLocaleDateString('en', { weekday: 'short', month: 'short', day: 'numeric', ...zone(tz) });
}

/** What the view remembers while you are on another view. */
export const view = $state({ tab: 'routines' as 'routines' | 'runs', type: 'all' as 'all' | 'agent' | 'script', source: 'all', state: 'active' as 'active' | 'inactive' | 'all' });
export const routines = $state({ jobs: [] as Job[], runs: [] as Run[], limit: 20, tz: '', scheduler: true, loaded: false, error: '' });

/** The job's state: enabled=false is paused whatever next_run_at says; a one-off that has run with nothing next is done. */
function stateOf(x: RawJob): JobState {
  if (!x.enabled) return 'paused';
  if (/^at\s/.test(x.schedule.trim()) && !x.next_run_at && x.last_status) return 'done';
  return 'active';
}

export async function load() {
  try {
    const [j, r] = await Promise.all([api.list(), api.runs(routines.limit)]);
    const raw = j.jobs as unknown as RawJob[];
    routines.scheduler = j.scheduler;
    routines.tz = raw.find((x) => x.kind !== 'system')?.tz ?? raw[0]?.tz ?? '';
    routines.jobs = raw.map((x) => {
      const state = stateOf(x);
      const system = x.kind === 'system';
      const at = /^at\s+(.+)$/.exec(x.schedule.trim());
      const ranAt = x.last_run_at ?? (at ? at[1] : null);
      const rd = ranAt ? new Date(ranAt) : null;
      const when = state === 'done' && rd && !isNaN(+rd) ? `Ran once, ${dateOf(rd, routines.tz)} at ${clock(rd, routines.tz)}` : state === 'done' ? 'Ran once' : state === 'paused' ? 'Paused' : cronWords(x.schedule, x.tz, routines.tz, system);
      return {
        name: x.name, kind: system ? 'system' : x.kind === 'script' || (x.command && x.kind !== 'agent') ? 'script' : 'agent', agent: x.agent ?? 'zenbot', plugin: x.plugin ?? null,
        what: system ? (UPKEEP[x.name] ?? x.action ?? '') : '', when, last: lastOf(x.last_status), state, on: x.enabled, error: x.last_error, next: state === 'active' ? x.next_run_at : null, lastRunAt: x.last_run_at ?? null,
      } satisfies Job;
    });
    routines.runs = (r as unknown as RawRun[]).map(runOf);
    routines.error = '';
  } catch (e) {
    routines.error = (e as Error).message;
  }
  routines.loaded = true;
}

/** Shows more past runs (the kernel gives at most 200) and reloads. */
export function moreRuns() {
  routines.limit = Math.min(200, routines.limit + 50);
  return load();
}
/** At least `n` past runs are loaded (the Runs tab wants a page of them). */
export function wantRuns(n: number) {
  if (routines.limit < n) { routines.limit = n; void load(); }
}

function runOf(r: RawRun): Run {
  const s = new Date(r.started_at);
  const e = r.ended_at ? new Date(r.ended_at) : null;
  const tz = routines.tz;
  const valid = !isNaN(+s);
  const secs = e ? Math.max(0, Math.round((+e - +s) / 1000)) : null;
  const status = r.status === 'running' ? 'running' : lastOf(r.status) === 'error' ? 'error' : r.status === 'silent' ? 'silent' : 'ok';
  return {
    id: r.id, job: r.job, day: valid ? dayWord(s, tz) : '', at: valid ? clock(s, tz) : '', ts: valid ? +s : 0,
    took: secs === null ? '' : secs < 60 ? `${secs} s` : `${Math.round(secs / 60)} min`,
    status, manual: r.trigger === 'manual', session: r.session,
    note: status === 'error' ? `Failed: ${r.error ?? 'error'}` : status === 'running' ? 'Running…' : (r.output?.split('\n').find((l) => l.trim())?.slice(0, 140) ?? (status === 'silent' ? 'Nothing to report' : 'Done')),
  };
}

/** Each routine's own past runs, read when its row opens. */
export const jobRuns = $state<Record<string, Run[]>>({});
export const runsOf = (name: string) => (jobRuns[name] ?? routines.runs.filter((r) => r.job === name)).slice(0, 5);
export async function loadRuns(name: string) {
  try {
    const r = await fetch(`/api/zen/jobs/runs?job=${encodeURIComponent(name)}&limit=5`);
    if (r.ok) jobRuns[name] = ((await r.json()) as RawRun[]).map(runOf);
  } catch {}
}

/** The past runs of every routine: a running one first, then newest first. */
export const pastRuns = (): Run[] => [...routines.runs].sort((a, b) => Number(b.status === 'running') - Number(a.status === 'running') || b.ts - a.ts || b.id - a.id);

/** Every active routine's next run, soonest first (the Runs tab's "Up next"). */
export function upNext(): (Job & { at: Date })[] {
  return routines.jobs.filter((j) => j.state === 'active' && j.next && !isNaN(+new Date(j.next)))
    .map((j) => ({ ...j, at: new Date(j.next!) })).sort((a, b) => +a.at - +b.at || a.name.localeCompare(b.name));
}

/** A job's source: zenbot's own upkeep, a plugin, or made by an agent. */
export const sourceOf = (j: Job) => (j.kind === 'system' ? 'system' : j.plugin ? `plugin:${j.plugin}` : `agent:${j.agent}`);
export const sourceName = (j: Job) => (j.kind === 'system' ? 'upkeep' : j.plugin ?? j.agent);

/** The sources present, for the Source filter. */
export function sources(): { value: string; label: string }[] {
  const out = [{ value: 'all', label: 'All sources' }];
  const seen = new Set<string>();
  const add = (value: string, label: string) => { if (!seen.has(value)) { seen.add(value); out.push({ value, label }); } };
  for (const j of routines.jobs) if (j.kind === 'system') add('system', 'zenbot upkeep');
  for (const j of routines.jobs) if (j.kind !== 'system' && !j.plugin) add(`agent:${j.agent}`, `Made by ${j.agent}`);
  for (const j of routines.jobs) if (j.plugin) add(`plugin:${j.plugin}`, j.plugin);
  return out;
}

const RANK: Record<JobState, number> = { active: 0, paused: 1, done: 2 };
/** The routines the filters keep: active first (soonest next run), then paused, then finished one-offs (latest first). */
export function listed(): Job[] {
  const valid = sources().some((s) => s.value === view.source) ? view.source : 'all';
  const t = (x: string | null) => (x ? +new Date(x) || 0 : 0);
  return routines.jobs
    .filter((j) => (view.type === 'all' || j.kind === view.type) && (valid === 'all' || sourceOf(j) === valid)
      && (view.state === 'all' || (view.state === 'active') === (j.state === 'active')))
    .sort((a, b) => RANK[a.state] - RANK[b.state]
      || (a.state === 'active' ? (a.next ? +new Date(a.next) : Infinity) - (b.next ? +new Date(b.next) : Infinity) : 0)
      || (a.state === 'done' ? t(b.lastRunAt) - t(a.lastRunAt) : 0)
      || a.name.localeCompare(b.name));
}

/** Routines whose last run failed and are not paused, for the badge and banner. */
export function failing(): Job[] {
  return routines.jobs.filter((j) => j.state !== 'paused' && j.last === 'error');
}

export async function runNow(name: string) {
  await api.run(name);
  setTimeout(load, 1500);
}
export async function setOn(name: string, on: boolean) {
  await api.enable(name, on);
  await load();
}
