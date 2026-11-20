<script lang="ts">
  // Routines tab: the owner's scheduled jobs, and zenbot's own upkeep folded underneath
  // (GET /api/jobs; POST /jobs/{name}/run, PATCH /jobs/{name} to pause). Past runs come from
  // GET /api/jobs/runs, each opening its session. A new routine is made in a chat with zenbot.
  // (The redesign mock's Routines, live.)
  import { onMount } from 'svelte';
  import { routines, failing, runsOf, loadRuns, load, runNow as start, setOn } from './routinestore.svelte.ts';
  let { ask, openRun }: { ask: (text: string) => void; openRun?: (session: string) => void } = $props();
  let sel = $state<string | null>(null);
  const jobs = $derived(routines.jobs);
  const upkeep = $derived(routines.upkeep);
  let showUpkeep = $state(false);
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
    if (jobs.some((j) => j.name === name)) { sel = name; void loadRuns(name); } else showUpkeep = true;
    setTimeout(() => document.querySelector(`[data-job="${CSS.escape(name)}"]`)?.scrollIntoView({ block: 'nearest' }), 30);
  }
  onMount(() => { void load(); const t = setInterval(load, 10000); return () => clearInterval(t); });
</script>

{#snippet toggle(on: boolean, flip: () => void, label: string)}
  <button class="sw" class:on role="switch" aria-checked={on} aria-label={label} onclick={flip}><i></i></button>
{/snippet}

<div class="rwrap">
    <p class="lead">Jobs that run on a schedule, in your time zone{routines.tz ? ` (${routines.tz})` : ''}.</p>
    {#if error || routines.error}<div class="fail" role="alert"><span class="dot error"></span><div class="cm"><b>Couldn't reach the routines</b><span>{error || routines.error}</span></div></div>{/if}
    {#if !routines.scheduler}<div class="fail" role="status"><span class="dot error"></span><div class="cm"><b>The scheduler is off</b><span>Routines won't run on their own until it's back on.</span></div></div>{/if}
    {#each failing() as f (f.name)}
      <div class="fail" role="status">
        <span class="dot error"></span>
        <div class="cm"><b>{f.name} failed {'what' in f ? 'last night' : 'on its last run'}</b><span>{'what' in f ? `zenbot's own upkeep: ${f.what}. ` : ''}{f.error ? `${f.error}. ` : ''}{failing().length === 1 ? 'The other routines are fine.' : ''}</span></div>
        <button class="pill" onclick={() => seeRun(f.name)}>See the run</button>
        <button class="pill" onclick={() => runNow(f.name)}>{ran === f.name ? 'Running…' : 'Run again'}</button>
        <button class="pill on" onclick={() => ask(`The ${f.name} routine failed. Find out why and fix it: `)}>Ask zenbot to fix it</button>
      </div>
    {/each}
    <div class="rows">
      {#each jobs as j (j.name)}
        <div class="li" class:off={!j.on} data-job={j.name}>
          <span class="dot {j.last}" title="Last run: {j.last}"></span>
          <button class="cm" aria-expanded={sel === j.name} title="Past runs" onclick={() => { sel = sel === j.name ? null : j.name; if (sel) void loadRuns(j.name); }}><b>{j.name}</b><span>{j.on ? j.when : 'Paused'}{j.kind === 'script' ? ' · a script' : ''}</span></button>
          <button class="pill" onclick={() => runNow(j.name)}>{ran === j.name ? 'Started ✓' : 'Run now'}</button>
          {@render toggle(j.on, () => flip(j.name, !j.on), `${j.name} on or paused`)}
        </div>
        {#if sel === j.name}
          <div class="runs">
            <div class="rh">Past runs</div>
            {#each runsOf(j.name) as r (r.id)}
              <div class="run">
                <span class="dot {r.status}"></span>
                <span class="rd">{r.day} <small>{r.at}{r.manual ? ' · by you' : ''}</small></span>
                <span class="rn" class:bad={r.status === 'error'}>{r.note}</span>
                <span class="rt">{r.took}</span>
                {#if r.session}<button class="open" onclick={() => openRun?.(r.session!)}>Open →</button>{:else}<span></span>{/if}
              </div>
            {:else}
              <div class="run"><span></span><span class="rn">No runs yet</span></div>
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
          {@render toggle(u.on, () => flip(u.name, !u.on), `${u.name} on or paused`)}</div>
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
