<script lang="ts">
  // The Tasks view (the TUI's tasks.rs): the vault's open tasks by project, the most pressing
  // first (the kernel sorts them), each with the agent it was handed to. Read-only: tasks are
  // ticked in Obsidian. `a` twice (or the row's button, confirmed) hands the chosen task to an
  // agent: the kernel starts a session briefed with it, and we go there.
  import { onMount } from 'svelte';
  import type { Pane } from '../app.svelte.ts';
  import { openSession, notice, typing } from '../app.svelte.ts';
  import { zen, type Tasks, type Task } from '../api.ts';
  import { pageRows, follow, plain } from './list.ts';

  let { pane, active }: { pane: Pane; active: boolean } = $props();

  const MARK: Record<string, string> = { urgent: '🔺', important: '⏫', medium: '🔼', not_urgent: '🔽', parking_lot: '⏬' };

  let data = $state<Tasks | null>(null);
  let error = $state('');
  let sel = $state<string | null>(null);
  /** The task `a` was pressed on once: a second `a` hands it over. */
  let arming = $state<string | null>(null);
  let handing = $state(false);
  let list: HTMLElement | undefined = $state();
  let last = '';
  let fetching = 0;

  const keyOf = (t: Task) => t.id ?? `${t.file}:${t.line}`;
  const all = $derived(data ? data.projects.flatMap((p) => p.tasks) : []);
  const at = $derived(Math.max(0, all.findIndex((t) => keyOf(t) === sel)));
  const chosen = $derived(all[at]);
  const handed = $derived(all.filter((t) => t.agent).length);

  async function load() {
    const n = ++fetching;
    try {
      const d = await zen.tasks();
      if (n !== fetching) return;
      error = '';
      const s = JSON.stringify(d);
      if (s !== last) {
        last = s;
        data = d;
      }
    } catch (e) {
      if (n === fetching) error = (e as Error).message;
    }
  }

  onMount(() => {
    load();
    const t = setInterval(load, 5000);
    return () => clearInterval(t);
  });

  // Keep a task chosen (by id across refreshes; the first when it's gone).
  $effect(() => {
    if (all.length && (!sel || !all.some((t) => keyOf(t) === sel))) sel = keyOf(all[0]);
  });

  function select(i: number) {
    if (!all.length) return;
    sel = keyOf(all[Math.min(all.length - 1, Math.max(0, i))]);
    arming = null;
    follow(list);
  }

  const cut = (s: string, n: number) => (s.length > n ? s.slice(0, n - 1) + '…' : s);

  function enter(t: Task | undefined) {
    if (!t) return;
    if (t.agent) openSession(t.agent.session);
    else notice('no agent on this task yet · a: hand it to one');
  }

  /** `a` or the row's button: go to its agent, else arm, else hand it over. */
  async function hand(t: Task | undefined) {
    if (!t || handing) return;
    if (t.agent) return openSession(t.agent.session);
    if (!t.id) {
      arming = null;
      notice('this task has no 🆔: add one in Obsidian to hand it to an agent');
      return;
    }
    if (arming !== t.id) {
      arming = t.id;
      notice(`a again: hand “${cut(t.text, 50)}” to an agent`);
      return;
    }
    arming = null;
    handing = true;
    try {
      const r = await zen.handTask(t.id);
      load();
      if (r.session) openSession(r.session);
    } catch (e) {
      notice(`couldn't hand it to an agent: ${(e as Error).message}`);
    } finally {
      handing = false;
    }
  }

  function key(e: KeyboardEvent) {
    if (!active || !plain(e) || typing(e)) return;
    if (['Shift', 'Control', 'Alt', 'Meta'].includes(e.key)) return;
    const wasArmed = arming;
    const page = pageRows(list);
    const acts: Record<string, () => void> = {
      ArrowUp: () => select(at - 1),
      k: () => select(at - 1),
      ArrowDown: () => select(at + 1),
      j: () => select(at + 1),
      PageUp: () => select(at - page),
      PageDown: () => select(at + page),
      Home: () => select(0),
      End: () => select(all.length - 1),
      r: load,
      Enter: () => enter(chosen),
      a: () => hand(chosen),
    };
    // Any other key disarms.
    if (e.key !== 'a') arming = null;
    const act = acts[e.key];
    if (act) {
      e.preventDefault();
      act();
    } else if (wasArmed) arming = null;
  }

  function click(t: Task) {
    if (keyOf(t) === sel && t.agent) return openSession(t.agent.session);
    sel = keyOf(t);
    arming = null;
  }
</script>

<svelte:window onkeydown={key} />

<div class="view" bind:this={list}>
  <div class="tools">
    {#if error}
      <span class="err">can't read the tasks: {error}</span>
    {:else if !data}
      <span class="dim">loading…</span>
    {:else}
      <span class="dim">{data.count} open task{data.count === 1 ? '' : 's'} · {data.today}{handed ? ` · ${handed} with an agent` : ''}</span>
    {/if}
    <span class="grow"></span>
    <button class="chip" onclick={load} title="r">↻</button>
  </div>
  {#each data?.problems ?? [] as p}<p class="err pad">can't read {p}</p>{/each}
  {#if data && !all.length}<p class="dim pad">No open tasks. They live in the vault: Projects/&lt;project&gt;/tasks.md</p>{/if}

  <div role="listbox" aria-label="tasks" tabindex="-1">
    {#each data?.projects ?? [] as p (p.project)}
      <div class="head"><b>{p.project}</b><span class="dim">{p.tasks.length}</span></div>
      {#each p.tasks as t (keyOf(t))}
        {@const on = keyOf(t) === sel}
        <div
          class="row"
          class:sel={on}
          class:active
          data-sel={on ? '1' : '0'}
          role="option"
          tabindex="-1"
          aria-selected={on}
          onclick={() => click(t)}
          onkeydown={() => {}}
        >
          <span class="mark" title={t.priority ?? 'no priority'}>{t.priority ? MARK[t.priority] : ''}</span>
          <span class="text">{t.text || '(no text)'}</span>
          {#if t.goals.length}<span class="goals dim" title="goals">{t.goals.join(' · ')}</span>{/if}
          <span class="tail">
            {#if t.due}<span class={t.overdue ? 'err' : 'dim'}>{t.overdue ? 'overdue' : 'due'} {t.due}</span>{/if}
            {#if t.agent?.state === 'running'}<span class="accent" title={t.agent.title ?? ''}>● working</span>
            {:else if t.agent?.state === 'waiting'}<span class="warn" title={t.agent.title ?? ''}>? waiting on you</span>
            {:else if t.agent}<span class="accent" title={t.agent.title ?? ''}>✓ reported</span>{/if}
            {#if on}
              {#if t.agent}
                <button class="act" onclick={(e) => { e.stopPropagation(); openSession(t.agent!.session); }}>open agent</button>
              {:else if t.id}
                <button
                  class="act"
                  class:armed={arming === t.id}
                  disabled={handing}
                  onclick={(e) => {
                    e.stopPropagation();
                    hand(t);
                  }}>{handing ? 'handing…' : arming === t.id ? 'confirm: hand it over' : 'hand to an agent'}</button
                >
              {/if}
            {/if}
          </span>
        </div>
      {/each}
    {/each}
  </div>
</div>

<style>
  .view { container-type: inline-size; padding: 6px 0 24px; font-family: var(--sans); font-size: 14px; }
  .tools { display: flex; gap: 8px; align-items: center; padding: 4px 14px 6px; font-size: 13px; }
  .grow { flex: 1; }
  .chip { font-size: 12px; color: var(--dim); border: 1px solid var(--faint); border-radius: 6px; padding: 3px 8px; flex: none; }
  .head { display: flex; gap: 8px; align-items: baseline; padding: 12px 14px 4px; font-size: 13px; }
  .head b { font-weight: 650; }
  .pad { padding: 2px 14px; margin: 0; font-size: 13px; }
  .row { display: flex; align-items: center; gap: 8px; min-height: 28px; box-sizing: border-box; padding: 3px 14px; cursor: pointer; white-space: nowrap; overflow: hidden; border-radius: 6px; margin: 0 6px; outline-offset: -1px; }
  .row:hover { background: color-mix(in srgb, var(--ink) 4%, transparent); }
  .row.sel { outline: 1px solid var(--faint); }
  .row.sel.active { outline: none; background: color-mix(in srgb, var(--accent) 14%, transparent); }
  .mark { width: 1.4em; flex: none; text-align: center; font-size: 13px; }
  .text { overflow: hidden; text-overflow: ellipsis; min-width: 4em; flex: 0 1 auto; }
  .goals { overflow: hidden; text-overflow: ellipsis; min-width: 0; flex: 0 2 auto; font-size: 12px; }
  .tail { display: flex; gap: 10px; align-items: center; margin-left: auto; flex: none; font-size: 12.5px; }
  .act { font-size: 12px; color: var(--accent); border: 1px solid color-mix(in srgb, var(--accent) 50%, transparent); border-radius: 6px; padding: 2px 8px; }
  .act.armed { background: var(--accent); color: var(--paper); }
  .act:disabled { opacity: 0.6; }
  .dim { color: var(--dim); }
  .err { color: var(--err); }
  .warn { color: var(--warn); }
  .accent { color: var(--accent); }
  @container (max-width: 560px) {
    .row { flex-wrap: wrap; white-space: normal; row-gap: 2px; }
    .text { flex: 1 1 0; white-space: normal; }
    .goals { flex: 1 1 100%; padding-left: calc(1.4em + 8px); white-space: nowrap; }
    .tail { margin-left: calc(1.4em + 8px); flex: 1 1 100%; min-width: 0; flex-wrap: wrap; row-gap: 4px; }
  }
  @media (pointer: coarse) { .row { min-height: 40px; padding-top: 7px; padding-bottom: 7px; } .act { padding: 6px 10px; } }
</style>
