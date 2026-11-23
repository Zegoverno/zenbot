// Routines shared by the Routines tab, its tab badge and Home (GET /api/jobs, in the mock's
// shapes): the owner's routines (agent and script jobs) and zenbot's own upkeep (system jobs),
// each with its schedule in plain words, its last result and whether it's on; and the past runs
// (GET /api/jobs/runs). Run now and pause/resume write to the kernel.
import { routines as api } from '../api.ts';

export type Last = 'ok' | 'silent' | 'error' | 'none';
export type Job = { name: string; kind: 'agent' | 'script'; plugin: string | null; when: string; last: Last; on: boolean; error: string | null; next: string | null };
export type Upkeep = { name: string; what: string; when: string; last: Last; on: boolean; error: string | null };
export type Run = { id: number; job: string; day: string; at: string; took: string; status: 'ok' | 'silent' | 'error' | 'running'; manual: boolean; note: string; session: string | null };

type RawJob = { name: string; kind?: string; plugin?: string | null; action?: string | null; command?: string | null; schedule: string; tz: string; enabled: boolean; next_run_at: string | null; last_status: string | null; last_error: string | null };
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

export const routines = $state({ jobs: [] as Job[], upkeep: [] as Upkeep[], runs: [] as Run[], tz: '', scheduler: true, loaded: false, error: '' });

export async function load() {
  try {
    const [j, r] = await Promise.all([api.list(), api.runs()]);
    const raw = j.jobs as unknown as RawJob[];
    routines.scheduler = j.scheduler;
    routines.tz = raw.find((x) => x.kind !== 'system')?.tz ?? raw[0]?.tz ?? '';
    routines.jobs = raw.filter((x) => x.kind !== 'system').map((x) => ({
      name: x.name, kind: x.kind === 'script' || (x.command && x.kind !== 'agent') ? 'script' : 'agent', plugin: x.plugin ?? null,
      when: cronWords(x.schedule, x.tz, routines.tz), last: lastOf(x.last_status), on: x.enabled, error: x.last_error, next: x.next_run_at,
    }));
    routines.upkeep = raw.filter((x) => x.kind === 'system').map((x) => ({
      name: x.name, what: UPKEEP[x.name] ?? x.action ?? '', when: cronWords(x.schedule, x.tz, routines.tz, true), last: lastOf(x.last_status), on: x.enabled, error: x.last_error,
    }));
    routines.runs = (r as unknown as RawRun[]).map(runOf);
    routines.error = '';
  } catch (e) {
    routines.error = (e as Error).message;
  }
  routines.loaded = true;
}

function runOf(r: RawRun): Run {
  const s = new Date(r.started_at);
  const e = r.ended_at ? new Date(r.ended_at) : null;
  const today = new Date();
  const yest = new Date(Date.now() - 864e5);
  const day = s.toDateString() === today.toDateString() ? 'Today' : s.toDateString() === yest.toDateString() ? 'Yesterday' : s.toLocaleDateString('en', { weekday: 'short', day: 'numeric' });
  const secs = e ? Math.max(0, Math.round((+e - +s) / 1000)) : null;
  const status = r.status === 'running' ? 'running' : lastOf(r.status) === 'error' ? 'error' : r.status === 'silent' ? 'silent' : 'ok';
  return {
    id: r.id, job: r.job, day, at: isNaN(+s) ? '' : s.toLocaleTimeString('en-GB', { hour: 'numeric', minute: '2-digit' }),
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

/** Routines whose last run failed (the owner's and zenbot's upkeep), for the badge and banner. */
export function failing(): (Job | Upkeep)[] {
  return [...routines.jobs, ...routines.upkeep].filter((j) => j.on && j.last === 'error');
}

export async function runNow(name: string) {
  await api.run(name);
  setTimeout(load, 1500);
}
export async function setOn(name: string, on: boolean) {
  await api.enable(name, on);
  await load();
}
