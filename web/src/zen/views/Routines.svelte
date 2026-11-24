<script lang="ts">
  // Routines view, two tabs (a segmented control; `t` switches, the choice is kept in the store):
  //  · Routines: ONE list of every job (GET /api/jobs): the owner's and the agents' routines and
  //    zenbot's own upkeep (system jobs, tagged "upkeep", with what they do in plain words).
  //    Filters above it: Type (agent / script), Source (zenbot upkeep, made by an agent, or a
  //    plugin) and State (Active by default; Inactive = paused or a one-off that has run). Active
  //    rows come first, soonest next run first; then paused, then finished one-offs ("Ran once,
  //    Nov 19 at 9:28", no switch). Each row tags its type and source; click it for its past runs.
  //  · Runs: "Up next" (each active routine's next run, soonest first) and "Past runs" (all
  //    routines, newest first, a running one on top, "Show more" for older ones).
  // Run now and pause/resume are POST /jobs/{name}/run and PATCH /jobs/{name}; past runs come from
  // GET /api/jobs/runs and open their session. A new routine is made in a chat with zenbot.
  // (The redesign mock's Routines, live.)
  // Keys (registered for the focused Routines panel, keys.svelte.ts): t switches tab, f cycles the
  // state filter; on Routines j/k walk the rows (the cursor), Enter or p its past runs, r runs it
  // now, Space pauses or resumes it, n a new routine and a a change to this one (both ask zenbot);
  // on Runs j/k walk the past runs and Enter opens the one under the cursor.
  import { onMount } from 'svelte';
  import { commands as registerCommands, cursorRow, moveCursor } from '../keys.svelte.ts';
  import { notice } from '../app.svelte.ts';
  import { routines, view, failing, listed, sources, sourceName, upNext, pastRuns, dayWord, clock, runsOf, loadRuns, load, moreRuns, wantRuns, runNow as start, setOn, type Job, type Run } from './routinestore.svelte.ts';
  let { ask, openRun }: { ask: (text: string) => void; openRun?: (session: string) => void } = $props();
  let sel = $state<string | null>(null);
  const jobs = $derived(routines.jobs);
  const shown = $derived(listed());
  const srcs = $derived(sources());
  const next = $derived(upNext());
  const past = $derived(pastRuns());
  let ran = $state<string | null>(null);
  let error = $state('');
  async function runNow(n: string) {
    ran = n;
    try { await start(n); error = ''; } catch (e) { error = (e as Error).message; }
    setTimeout(() => { if (ran === n) ran = null; }, 2500);
  }
  async function flip(n: string, on: boolean) {
    try { await setOn(n, on); error = ''; } catch (e) { error = (e as Error).message; }
  }
  /** The failed routine's latest run with a session, to open it. */
  const lastRun = (name: string) => runsOf(name).find((r) => r.session)?.session ?? null;
  /** See the run: its session when it has one, else the routine's past runs (with their errors) unfolded. */
  function seeRun(name: string) {
    const s = lastRun(name);
    if (s) return openRun?.(s);
    // The row may be hidden by a filter: show everything.
    view.tab = 'routines'; view.type = 'all'; view.source = 'all'; view.state = 'all';
    sel = name; void loadRuns(name);
    setTimeout(() => document.querySelector(`[data-job="${CSS.escape(name)}"]`)?.scrollIntoView({ block: 'nearest' }), 30);
  }
  const ROWS = '.rwrap .li[data-job]';
  const RUN_ROWS = '.rwrap .run[data-run]';
  const rows = () => (view.tab === 'runs' ? RUN_ROWS : ROWS);
  const curJob = () => { const n = cursorRow(ROWS)?.dataset.job; return jobs.find((j) => j.name === n) ?? null; };
  const withJob = (f: (j: Job) => void) => { const j = curJob(); if (j) f(j); else notice('Pick a routine first (j / k)'); };
  const seePast = (n: string) => { sel = sel === n ? null : n; if (sel) void loadRuns(n); };
  const inRoutines = () => view.tab === 'routines';
  const STATES = ['active', 'inactive', 'all'] as const;
  function openCurRun() {
    const id = Number(cursorRow(RUN_ROWS)?.dataset.run);
    const r = past.find((x) => x.id === id);
    if (!r) notice('Pick a run first (j / k)'); else if (r.session) openRun?.(r.session); else notice('That run has no session to open');
  }
  registerCommands('routines', () => {
    // The title follows the row under the cursor, read when the list is drawn (the ring, Ctrl+K).
    const on = () => curJob()?.state !== 'paused';
    const done = () => curJob()?.state === 'done';
    return [
      { id: 'routines-run', title: 'Run now', icon: 'play', key: 'r', ring: 0, enabled: inRoutines, run: () => withJob((x) => void runNow(x.name)) },
      { id: 'routines-runs', title: 'Past runs', icon: 'list', key: 'p', alt: ['Enter'], ring: 1, enabled: inRoutines, run: () => withJob((x) => seePast(x.name)) },
      { id: 'routines-pause', get title() { return on() ? 'Pause' : 'Resume'; }, get icon() { return on() ? 'pause' : 'play'; }, key: 'Space', ring: 2, enabled: () => inRoutines() && !done(), run: () => withJob((x) => void flip(x.name, !x.on)) },
      { id: 'routines-new', title: 'New routine', icon: 'plus', key: 'n', ring: 3, run: () => ask('Create a new routine: ') },
      { id: 'routines-ask', title: 'Ask to change', icon: 'ask', key: 'a', ring: 4, enabled: inRoutines, run: () => withJob((x) => ask(`Change the ${x.name} routine: `)) },
      { id: 'routines-failing', title: 'Show failing', icon: 'bell', ring: 5, enabled: () => failing().length > 0, run: () => seeRun(failing()[0].name) },
      { id: 'routines-tab', get title() { return inRoutines() ? 'Show runs' : 'Show routines'; }, icon: 'swap', key: 't', run: () => { view.tab = inRoutines() ? 'runs' : 'routines'; } },
      { id: 'routines-filter', title: 'Next state filter', icon: 'filter', key: 'f', enabled: inRoutines, run: () => { view.state = STATES[(STATES.indexOf(view.state) + 1) % STATES.length]; notice(`Showing ${view.state} routines`); } },
      { id: 'routines-open', title: 'Open the run', icon: 'next', key: 'Enter', enabled: () => !inRoutines(), run: openCurRun },
      { id: 'routines-down', title: 'Next routine', icon: 'down', key: 'j', alt: ['ArrowDown'], quiet: true, run: () => void moveCursor(rows(), 1) },
      { id: 'routines-up', title: 'Previous routine', icon: 'up', key: 'k', alt: ['ArrowUp'], quiet: true, run: () => void moveCursor(rows(), -1) },
    ];
  });
  $effect(() => { if (view.tab === 'runs') wantRuns(50); });
  onMount(() => { void load(); const t = setInterval(load, 10000); return () => clearInterval(t); });
  const typeOf = (j: Job) => j.kind;
  const sub = (j: Job) => (j.kind === 'system' && j.what ? `${j.what} · ${j.when}` : j.when);
</script>

{#snippet toggle(on: boolean, flip: () => void, label: string)}
  <button class="sw" class:on role="switch" aria-checked={on} aria-label={label} onclick={flip}><i></i></button>
{/snippet}
{#snippet tags(j: Job)}
  {#if j.kind !== 'system'}<small class="plugin-tag" title="Type: {typeOf(j)}">{typeOf(j)}</small>{/if}
  {#if j.kind === 'system'}<small class="plugin-tag" title="zenbot's own upkeep">upkeep</small>
  {:else if j.plugin}<small class="plugin-tag" title="Plugin: {j.plugin}">{j.plugin}</small>
  {:else}<small class="plugin-tag" title="Made by {j.agent}">{j.agent}</small>{/if}
{/snippet}
{#snippet runRow(r: Run, named: boolean)}
  <div class="run" class:all={named} data-run={named ? r.id : undefined}>
    <span class="dot {r.status}"></span>
    {#if named}<span class="rj">{r.job}</span>{/if}
    <span class="rd">{r.day} <small>{r.at}{r.manual ? ' · by you' : ''}</small></span>
    <span class="rn" class:bad={r.status === 'error'}>{r.note}</span>
    <span class="rt">{r.took}</span>
    {#if r.session}<button class="open" onclick={() => openRun?.(r.session!)}>Open →</button>{:else}<span></span>{/if}
  </div>
{/snippet}

<div class="rwrap">
    <div class="seg" role="group" aria-label="Routines view">
      <button class="pill" class:on={view.tab === 'routines'} aria-pressed={view.tab === 'routines'} onclick={() => (view.tab = 'routines')}>Routines</button>
      <button class="pill" class:on={view.tab === 'runs'} aria-pressed={view.tab === 'runs'} onclick={() => (view.tab = 'runs')}>Runs</button>
    </div>
    <p class="lead">{view.tab === 'runs' ? 'Every run of every routine' : 'Jobs that run on a schedule'}, in your time zone{routines.tz ? ` (${routines.tz})` : ''}.</p>
    {#if error || routines.error}<div class="fail" role="alert"><span class="dot error"></span><div class="cm"><b>Couldn't reach the routines</b><span>{error || routines.error}</span></div></div>{/if}
    {#if !routines.scheduler}<div class="fail" role="status"><span class="dot error"></span><div class="cm"><b>The scheduler is off</b><span>Routines won't run on their own until it's back on.</span></div></div>{/if}
    {#each failing() as f (f.name)}
      <div class="fail" role="status">
        <span class="dot error"></span>
        <div class="cm"><b>{f.name} failed {f.kind === 'system' ? 'last night' : 'on its last run'}</b><span>{f.kind === 'system' ? `zenbot's own upkeep: ${f.what}. ` : ''}{f.error ? `${f.error}. ` : ''}{failing().length === 1 ? 'The other routines are fine.' : ''}</span></div>
        <button class="pill" onclick={() => seeRun(f.name)}>See the run</button>
        <button class="pill" onclick={() => runNow(f.name)}>{ran === f.name ? 'Running…' : 'Run again'}</button>
        <button class="pill on" onclick={() => ask(`The ${f.name} routine failed. Find out why and fix it: `)}>Ask zenbot to fix it</button>
      </div>
    {/each}
    {#if view.tab === 'routines'}
      <div class="filters" role="group" aria-label="Filter routines">
        <span class="chips" role="group" aria-label="Type">
          {#each [['all', 'All'], ['agent', 'Agent'], ['script', 'Script']] as [v, l] (v)}
            <button class="chip" class:on={view.type === v} aria-pressed={view.type === v} onclick={() => (view.type = v as typeof view.type)}>{l}</button>
          {/each}
        </span>
        <select class="chip" aria-label="Source" value={srcs.some((o) => o.value === view.source) ? view.source : 'all'} onchange={(e) => (view.source = e.currentTarget.value)}>
          {#each srcs as o (o.value)}<option value={o.value}>{o.label}</option>{/each}
        </select>
        <span class="chips" role="group" aria-label="State">
          {#each [['active', 'Active'], ['inactive', 'Inactive'], ['all', 'All']] as [v, l] (v)}
            <button class="chip" class:on={view.state === v} aria-pressed={view.state === v} onclick={() => (view.state = v as typeof view.state)}>{l}</button>
          {/each}
        </span>
      </div>
      <div class="rows">
        {#each shown as j (j.name)}
          <div class="li" class:off={j.state !== 'active'} data-job={j.name} data-state={j.state}>
            <span class="dot {j.last}" title="Last run: {j.last}"></span>
            <button class="cm" aria-expanded={sel === j.name} title="Past runs" onclick={() => seePast(j.name)}><b>{j.name}{@render tags(j)}</b><span>{sub(j)}{#if j.kind === 'system' && j.last === 'error'} · <em>failed last night</em>{/if}</span></button>
            <button class="pill" onclick={() => runNow(j.name)}>{ran === j.name ? 'Started ✓' : 'Run now'}</button>
            {#if j.state === 'done'}<span class="sw-gap"></span>{:else}{@render toggle(j.on, () => flip(j.name, !j.on), `${j.name} on or paused`)}{/if}
          </div>
          {#if sel === j.name}
            <div class="runs">
              <div class="rh">Past runs</div>
              {#each runsOf(j.name) as r (r.id)}
                {@render runRow(r, false)}
              {:else}
                <div class="run"><span></span><span class="rn">No runs yet</span></div>
              {/each}
            </div>
          {/if}
        {:else}
          <div class="li empty"><div class="cm"><span>{routines.loaded ? 'No routines match these filters.' : 'Loading…'}</span></div></div>
        {/each}
        <button class="li add" onclick={() => ask('Create a new routine: ')}><span class="plus">+</span><div class="cm"><b>New routine</b><span>Tell zenbot what to do and when</span></div></button>
      </div>
    {:else}
      <div class="rh sec">Up next</div>
      <div class="rows" data-section="next">
        {#each next as j (j.name)}
          <div class="li nx" data-next={j.name}>
            <div class="cm"><b>{j.name}{@render tags(j)}</b><span>{sub(j)}</span></div>
            <span class="wh">{#if j.at}{dayWord(j.at, routines.tz)} <small>{clock(j.at, routines.tz)}</small>{/if}</span>
          </div>
        {:else}
          <div class="li empty"><div class="cm"><span>{routines.loaded ? 'Nothing is scheduled.' : 'Loading…'}</span></div></div>
        {/each}
      </div>
      <div class="rh sec">Past runs</div>
      <div class="rows runsall" data-section="past">
        {#each past as r (r.id)}
          {@render runRow(r, true)}
        {:else}
          <div class="li empty"><div class="cm"><span>{routines.loaded ? 'No runs yet.' : 'Loading…'}</span></div></div>
        {/each}
      </div>
      {#if routines.runs.length >= routines.limit && routines.limit < 200}<button class="more" onclick={() => void moreRuns()}>Show more</button>{/if}
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
  .cm { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
  .cm b { font-size: 14px; font-weight: 600; }
  .plugin-tag { display: inline-block; vertical-align: middle; max-width: 100%; overflow-wrap: anywhere; margin-inline-start: 6px; padding: 1px 6px; border: 1px solid var(--faint); border-radius: 99px; color: var(--accent); font-size: 11px; font-weight: 500; }
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
  @media (max-width: 900px) { .runs { padding-left: 14px; } }
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
  .seg { display: flex; gap: 6px; flex: none; }
  .filters { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 14px; flex: none; }
  .chips { display: inline-flex; gap: 4px; flex-wrap: wrap; }
  .chip { padding: 3px 10px; border-radius: 99px; border: 1px solid var(--faint); font-size: 12.5px; background: var(--paper); color: var(--dim); white-space: nowrap; }
  .chip:hover { border-color: var(--dim); color: var(--ink); }
  .chip.on { background: var(--soft); border-color: var(--dim); color: var(--ink); font-weight: 600; }
  select.chip { padding-right: 6px; color: var(--ink); }
  .sw-gap { width: 36px; flex: none; }
  .li.empty .cm span { padding: 2px 0; }
  .rh.sec { padding: 4px 2px 0; margin-bottom: -6px; }
  .wh { font-size: 13px; text-align: right; white-space: nowrap; }
  .wh small { color: var(--dim); font-size: 12.5px; }
  .runsall .run.all { padding: 8px 14px; grid-template-columns: 10px minmax(90px, 1.1fr) 150px minmax(0, 2fr) 56px 64px; }
  .runsall .run + .run { border-top: 1px solid var(--faint); }
  .rj { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .more { align-self: flex-start; font-size: 13px; color: var(--dim); }
  .more:hover { color: var(--ink); }
  @media (max-width: 900px) {
    .rwrap { padding: 4px 12px 100px; }
    .runsall .run.all { grid-template-columns: 10px minmax(0, 1fr) auto auto; grid-template-areas: 'd j j o' '. r r r' '. n n t'; row-gap: 2px; }
    .runs .run:not(.all) { grid-template-columns: 10px minmax(0, 1fr) auto auto; grid-template-areas: 'd r r o' '. n n t'; row-gap: 2px; }
    .runs .run:not(.all) > :nth-child(1) { grid-area: d; }
    .runs .run:not(.all) > :nth-child(2) { grid-area: r; }
    .runs .run:not(.all) > :nth-child(3) { grid-area: n; }
    .runs .run:not(.all) > :nth-child(4) { grid-area: t; }
    .runs .run:not(.all) > :nth-child(5) { grid-area: o; }
    .runsall .run.all > :nth-child(1) { grid-area: d; }
    .runsall .run.all > :nth-child(2) { grid-area: j; }
    .runsall .run.all > :nth-child(3) { grid-area: r; }
    .runsall .run.all > :nth-child(4) { grid-area: n; }
    .runsall .run.all > :nth-child(5) { grid-area: t; }
    .runsall .run.all > :nth-child(6) { grid-area: o; }
  }
</style>
