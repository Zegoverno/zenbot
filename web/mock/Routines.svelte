<script lang="ts">
  import { routines, failing } from './routinestore.svelte.ts';
  // Routines tab (mock): the owner's scheduled jobs, and zenbot's own upkeep folded underneath.
  // Everything here exists today: GET /api/jobs, POST /jobs/{name}/run, PATCH /jobs/{name}
  // (enabled). A new routine is made in a chat with zenbot.
  let { ask, openRun }: { ask: (text: string) => void; openRun?: (job: string) => void } = $props();
  // Past runs: GET /api/jobs/runs?job=<name> (newest first: status, trigger, start/end, output or
  // error, and the run's session). Faked here from the schedule.
  type Run = { day: string; at: string; took: string; status: 'ok' | 'silent' | 'error'; manual?: boolean; note: string };
  const NOTES: Record<string, string[]> = {
    'daily-plan': ['3 tasks today, 1 overdue; no clashes', 'Plan written; dentist moved to Friday', '4 tasks today'],
    'daily-readings': ['Lk 19:41–44 · Thursday, 33rd week', 'Lk 19:11–28', 'Lk 19:1–10'],
    angelus: ['Angelus sent', 'Angelus sent', 'Angelus sent'],
  };
  function runsOf(j: { name: string; kind: string; when: string; last: string }): Run[] {
    const at = j.when.match(/\d+:\d+/)?.[0] ?? '9:40';
    return ['Today', 'Yesterday', 'Mon 17', 'Sun 16', 'Sat 15'].map((day, i) => {
      const fail = i === 3 && j.kind === 'agent';
      return {
        day, at, took: j.kind === 'script' ? '1 s' : `${2 + ((i * 3) % 5)} min`,
        status: i === 0 ? (j.last as Run['status']) : fail ? 'error' : j.last === 'silent' ? 'silent' : 'ok',
        manual: i === 2 && j.kind === 'agent',
        note: fail ? 'Failed: the model timed out' : (NOTES[j.name]?.[i % 3] ?? (j.last === 'silent' ? 'Nothing to report' : 'Done')),
      };
    });
  }
  let sel = $state<string | null>(null);
  const jobs = routines.jobs;
  const upkeep = routines.upkeep;
  let showUpkeep = $state(false);
  let ran = $state<string | null>(null);
  const runNow = (n: string) => { ran = n; setTimeout(() => (ran = null), 2500); };

</script>

{#snippet toggle(on: boolean, flip: () => void, label: string)}
  <button class="sw" class:on role="switch" aria-checked={on} aria-label={label} onclick={flip}><i></i></button>
{/snippet}

<div class="rwrap">
    <p class="lead">Jobs that run on a schedule, in your time zone (America/Sao_Paulo).</p>
    {#each failing() as f (f.name)}
      <div class="fail" role="status">
        <span class="dot error"></span>
        <div class="cm"><b>{f.name} failed {'what' in f ? 'last night' : 'on its last run'}</b><span>{'what' in f ? `zenbot's own upkeep: ${f.what}. ` : ''}The other routines are fine.</span></div>
        <button class="pill" onclick={() => openRun?.(f.name)}>See the run</button>
        <button class="pill" onclick={() => { runNow(f.name); setTimeout(() => (f.last = 'ok'), 1200); }}>{ran === f.name ? 'Running…' : 'Run again'}</button>
        <button class="pill on" onclick={() => ask(`The ${f.name} routine failed. Find out why and fix it: `)}>Ask zenbot to fix it</button>
      </div>
    {/each}
    <div class="rows">
      {#each jobs as j}
        <div class="li" class:off={!j.on}>
          <span class="dot {j.last}" title="Last run: {j.last}"></span>
          <button class="cm" aria-expanded={sel === j.name} title="Past runs" onclick={() => (sel = sel === j.name ? null : j.name)}><b>{j.name}</b><span>{j.on ? j.when : 'Paused'}{j.kind === 'script' ? ' · a script' : ''}</span></button>
          <button class="pill" onclick={() => runNow(j.name)}>{ran === j.name ? 'Started ✓' : 'Run now'}</button>
          {@render toggle(j.on, () => (j.on = !j.on), `${j.name} on or paused`)}
        </div>
        {#if sel === j.name}
          <div class="runs">
            <div class="rh">Past runs</div>
            {#each runsOf(j) as r}
              <div class="run">
                <span class="dot {r.status}"></span>
                <span class="rd">{r.day} <small>{r.at}{r.manual ? ' · by you' : ''}</small></span>
                <span class="rn" class:bad={r.status === 'error'}>{r.note}</span>
                <span class="rt">{r.took}</span>
                {#if j.kind === 'agent'}<button class="open" onclick={() => openRun?.(j.name)}>Open →</button>{:else}<span></span>{/if}
              </div>
            {/each}
          </div>
        {/if}
      {/each}
      <button class="li add" onclick={() => ask('Create a new routine: ')}><span class="plus">+</span><div class="cm"><b>New routine</b><span>Tell zenbot what to do and when</span></div></button>
    </div>
    <button class="more" onclick={() => (showUpkeep = !showUpkeep)}>{showUpkeep ? '▾' : '▸'} zenbot's own upkeep ({upkeep.length})</button>
    {#if showUpkeep}
      <div class="rows dimlist">{#each upkeep as u}
        <div class="li" class:off={!u.on}><span class="dot {u.last}"></span><div class="cm"><b>{u.name}</b><span>{u.what} · {u.on ? u.when : 'Paused'}{#if u.last === 'error'} · <em>failed last night</em>{/if}</span></div>
          {@render toggle(u.on, () => (u.on = !u.on), `${u.name} on or paused`)}</div>
      {/each}</div>
    {/if}
</div>

<style>
  .rwrap { flex: 1; overflow: auto; padding: 4px 22px 100px; display: flex; flex-direction: column; gap: 14px; max-width: 820px; width: 100%; margin: 0 auto; }
  .lead { margin: 0; color: var(--dim); font-size: 13.5px; }
  .rows { border: 1px solid var(--faint); border-radius: 14px; background: var(--paper); overflow: hidden; flex: none; }
  .li { display: flex; align-items: center; gap: 12px; padding: 11px 14px; width: 100%; text-align: left; }
  .li + .li { border-top: 1px solid var(--faint); }
  .li.off .cm { opacity: 0.55; }
  .li.add:hover { background: var(--soft); }
  .li.add .plus { width: 10px; color: var(--accent); font-size: 18px; text-align: center; }
  .dimlist .cm { opacity: 0.8; }
  .cm { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
  .cm b { font-size: 14px; font-weight: 600; }
  .cm > span { font-size: 12.5px; color: var(--dim); }
  .cm em { color: var(--bad, #b5543c); font-style: normal; font-weight: 600; }
  .dot { width: 10px; height: 10px; border-radius: 50%; flex: none; background: var(--faint); }
  .dot.ok { background: var(--accent); }
  .dot.silent { background: color-mix(in srgb, var(--accent) 45%, var(--faint)); }
  .dot.error { background: var(--bad, #b5543c); }
  .pill { display: inline-flex; align-items: center; padding: 4px 12px; border-radius: 99px; border: 1px solid var(--faint); font-size: 12.5px; background: var(--paper); color: var(--ink); white-space: nowrap; }
  .pill:hover { border-color: var(--dim); }
  .sw { width: 36px; height: 21px; border-radius: 99px; background: var(--faint); position: relative; flex: none; transition: background 0.15s; }
  .sw i { position: absolute; top: 2.5px; left: 2.5px; width: 16px; height: 16px; border-radius: 50%; background: var(--paper); box-shadow: 0 1px 2px var(--shadow); transition: transform 0.15s; }
  .sw.on { background: var(--accent); }
  .sw.on i { transform: translateX(15px); }
  .li.open, .li:has(+ .runs) { background: var(--soft); }
  button.cm { text-align: left; cursor: pointer; }
  button.cm:hover b { text-decoration: underline; text-decoration-color: var(--dim); text-underline-offset: 3px; }
  .runs { background: var(--soft); padding: 0 14px 8px 36px; }
  .rh { font-size: 10.5px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); font-weight: 650; padding: 2px 0 4px; }
  .run { display: grid; grid-template-columns: 10px 150px minmax(0, 1fr) auto 56px; align-items: center; gap: 10px; padding: 6px 0; font-size: 13px; }
  .run + .run { border-top: 1px dashed var(--faint); }
  .rd small { color: var(--dim); font-size: 12px; }
  .rn { color: var(--dim); overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .rn.bad { color: var(--bad, #b5543c); font-weight: 600; }
  .rt { color: var(--dim); font-size: 12px; }
  .open { color: var(--accent); font-weight: 600; font-size: 12.5px; text-align: right; }
  .fail .cm { flex: 1 1 260px; }
  .fail { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; padding: 10px 14px; border-radius: 14px; flex: none;
    background: color-mix(in srgb, var(--bad, #b5543c) 8%, var(--paper)); border: 1px solid color-mix(in srgb, var(--bad, #b5543c) 30%, var(--faint)); }
  .pill.on { background: var(--accent); border-color: var(--accent); color: var(--on-accent); font-weight: 600; }
  .more { align-self: flex-start; font-size: 13px; color: var(--dim); }
  .more:hover { color: var(--ink); }
  @media (max-width: 900px) { .rwrap { padding: 4px 12px 100px; } }
</style>
