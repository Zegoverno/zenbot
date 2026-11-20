// Routines shared by the Routines tab, its tab badge and Home (mock data, as GET /api/jobs gives it).
// GET /api/jobs: the owner's routines (agent and script jobs) and zenbot's own (system).
export type Job = { name: string; kind: 'agent' | 'script'; when: string; last: 'ok' | 'silent' | 'error' | 'none'; on: boolean };
export type Upkeep = { name: string; what: string; when: string; last: Job['last']; on: boolean };
const jobs: Job[] = ([
  { name: 'zenfitness-morning-checkin', kind: 'agent', when: 'Every day at 5:00', last: 'ok', on: true },
  { name: 'daily-readings', kind: 'agent', when: 'Every day at 6:30', last: 'ok', on: true },
  { name: 'daily-plan', kind: 'agent', when: 'Every day at 8:00', last: 'ok', on: true },
  { name: 'sao-jose-trabalho', kind: 'agent', when: 'Every day at 9:00', last: 'ok', on: true },
  { name: 'angelus', kind: 'agent', when: 'Every day at 12:00', last: 'ok', on: true },
  { name: 'readwise-inbox-triage', kind: 'agent', when: 'Every day at 20:00', last: 'ok', on: true },
  { name: 'readwise-wiki-process', kind: 'agent', when: 'Every day at 20:30', last: 'ok', on: true },
  { name: 'exame-consciencia', kind: 'agent', when: 'Every day at 21:00', last: 'ok', on: true },
  { name: 'wiki-lint', kind: 'agent', when: 'Sundays at 9:00', last: 'silent', on: true },
  { name: 'vault-sync', kind: 'script', when: 'Every 10 minutes', last: 'silent', on: true },
  { name: 'auditoria-fase3-monitor', kind: 'agent', when: 'Every hour', last: 'silent', on: false },
]);
const upkeep: Upkeep[] = ([
  { name: 'sleep', what: 'Sorts memory', when: 'Nightly 0:00', last: 'ok', on: true },
  { name: 'wiki', what: 'Wiki upkeep', when: 'Nightly 0:10', last: 'silent', on: true },
  { name: 'skills', what: 'Skills upkeep', when: 'Nightly 0:20', last: 'silent', on: true },
  { name: 'prune', what: 'Cleans old files', when: 'Nightly 0:40', last: 'ok', on: true },
  { name: 'engines', what: 'Updates Claude and Codex', when: 'Nightly 1:00', last: 'error', on: true },
  { name: 'session-review', what: 'Reviews finished sessions', when: 'When a session ends', last: 'none', on: false },
]);
export const routines = $state({ jobs, upkeep });

/** Routines whose last run failed (the owner's and zenbot's upkeep), for the badge and banner. */
export function failing() {
  return [...routines.jobs, ...routines.upkeep].filter((j) => j.on && j.last === 'error');
}
