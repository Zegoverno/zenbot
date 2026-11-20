<script lang="ts">
  import { onMount } from 'svelte';
  import { routines, type Routine, type RoutineRun } from '../api.ts';
  import { app, openSession } from '../app.svelte.ts';
  let jobs = $state<Routine[]>([]), runs = $state<RoutineRun[]>([]), error = $state(''), scheduler = $state(true), busy = $state('');
  async function load() {
    try { const [j, r] = await Promise.all([routines.list(), routines.runs()]); jobs = j.jobs; scheduler = j.scheduler; runs = r; error = ''; }
    catch (e) { error = (e as Error).message; }
  }
  async function act(j: Routine, run: boolean) {
    busy = j.name;
    try { if (run) await routines.run(j.name); else await routines.enable(j.name, !j.enabled); await load(); }
    catch (e) { error = (e as Error).message; } finally { busy = ''; }
  }
  function create() { app.chatDraft = 'Help me create a new routine. Ask me what it should do and when it should run.'; openSession('new'); }
  onMount(() => { void load(); const timer = setInterval(load, 10000); return () => clearInterval(timer); });
</script>
<div class="routines">
  <header><div><h1>Routines</h1><p>Scheduled work, quietly taken care of.</p></div><button class="primary" onclick={create}>+ New routine</button></header>
  {#if error}<div class="failure" role="alert">Could not load or update routines: {error}</div>{/if}
  {#if !scheduler}<div class="failure" role="alert">The scheduler is disabled. Routines will not run automatically.</div>{/if}
  {#if jobs.some(j => j.failures > 0 || j.last_status === 'failed')}<div class="failure" role="alert">Some routines need attention. Check their latest result below.</div>{/if}
  {#each jobs as job (job.name)}
    <article><div><strong>{job.name}</strong><p>{job.schedule} · {job.tz} · {job.enabled ? 'Active' : 'Paused'}</p><p>{job.last_error || (job.next_run_at ? `Next: ${job.next_run_at}` : 'No upcoming run')}</p></div><div class="actions"><button disabled={!!busy} onclick={() => act(job, true)}>Run now</button><button disabled={!!busy} onclick={() => act(job, false)}>{job.enabled ? 'Pause' : 'Resume'}</button></div></article>
  {:else}<p>No routines yet.</p>{/each}
  <h2>Recent runs</h2>
  {#each runs as run (run.id)}<article><div><strong>{run.job}</strong><p>{run.status} · {run.started_at}{run.error ? ` · ${run.error}` : ''}</p></div>{#if run.session}<button onclick={() => openSession(run.session!)}>Open session</button>{/if}</article>{:else}<p>No recent runs.</p>{/each}
</div>
<style>
  .routines { max-width: 960px; margin: auto; padding: 36px 28px; } header, article { display:flex; justify-content:space-between; gap:16px; align-items:center; } h1 { font-family:var(--serif); margin:0; } h2 { margin-top:36px; font-size:18px; } p { color:var(--dim); font-size:14px; } article { border:1px solid var(--faint); border-radius:18px; padding:18px; margin:10px 0; background:var(--paper); } article p { margin:6px 0 0; } .actions { display:flex; gap:8px; } button { border:1px solid var(--faint); border-radius:10px; padding:8px 12px; white-space:nowrap; } .primary { background:var(--accent); color:white; border:0; } .failure { padding:14px; border:1px solid var(--err); border-radius:12px; color:var(--err); margin:12px 0; } @media(max-width:600px) { .routines { padding:24px 16px; } article { align-items:flex-start; flex-direction:column; } }
</style>
