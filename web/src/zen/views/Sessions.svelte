<script lang="ts">
  // The Sessions view (the TUI's board.rs): every session in sections (Channels, Sessions, Jobs,
  // Archived), whether it runs, idles or waits on the owner, with the subagents and verifiers
  // working under it. Polled from the kernel every 2s while shown. In the side panel `hide` leaves
  // out the session open next to it.
  import { onMount } from 'svelte';
  import type { Pane } from '../app.svelte.ts';
  import { openSession, typing } from '../app.svelte.ts';
  import { zen, ago, agentTag, type BoardSession } from '../api.ts';
  import { SPINNER, pageRows, follow, plain, clean } from './list.ts';

  let { pane, active, hide = null }: { pane: Pane; active: boolean; hide?: string | null } = $props();

  /** Finished children shown under their parent (the newest); running ones always show. */
  const FINISHED_CHILDREN = 3;
  const SECTIONS = ['Channels', 'Sessions', 'Jobs', 'Archived'] as const;
  type Section = (typeof SECTIONS)[number];

  let sessions = $state<BoardSession[] | null>(null);
  let archivedCount = $state(0);
  let error = $state('');
  let now = $state(Date.now());
  let showArchived = $state(false);
  /** Archived jobs (the frequent ones, like vault-sync) are left out of Archived unless asked for. */
  const JOBS_KEY = 'zen.sessions.archivedJobs';
  let showJobs = $state(stored());
  let filter = $state('');
  let selected = $state<string | null>(null);
  let spin = $state(0);
  let list: HTMLElement | undefined = $state();
  let input: HTMLInputElement | undefined = $state();
  let last = '';
  let fetching = 0;

  function stored(): boolean {
    try {
      return localStorage.getItem(JOBS_KEY) === '1';
    } catch {
      return false;
    }
  }

  async function load() {
    const n = ++fetching;
    try {
      const b = await zen.board(showArchived);
      if (n !== fetching) return;
      now = Date.now();
      error = '';
      archivedCount = b.archived ?? 0;
      const s = JSON.stringify(b.sessions);
      if (s !== last) {
        last = s;
        sessions = b.sessions;
      }
    } catch (e) {
      if (n === fetching) error = (e as Error).message;
    }
  }

  onMount(() => {
    load();
    const t = setInterval(load, 2000);
    return () => clearInterval(t);
  });

  const title = (s: BoardSession) => clean(s.title || '') || '(untitled)';
  const task = (s: BoardSession) => clean(s.task || '') || title(s);
  const model = (s: BoardSession) => s.model.split('/').pop() ?? s.model;
  const section = (s: BoardSession): Section =>
    s.archived ? 'Archived' : s.kind === 'channel' ? 'Channels' : s.kind === 'job' ? 'Jobs' : 'Sessions';

  const archivedJob = (s: BoardSession) => s.archived && s.kind === 'job';
  /** Archived jobs the list holds but Archived leaves out (only known once it is loaded). */
  const jobsHidden = $derived(!showJobs && sessions ? sessions.filter((s) => archivedJob(s) && !s.parent && s.id !== hide).length : 0);

  interface Row {
    id: string;
    s: BoardSession;
    child: boolean;
    runningKids: number;
    kids: number;
    more: number;
  }
  interface Group {
    name: Section;
    count: number;
    hidden: boolean;
    rows: Row[];
  }

  const groups = $derived.by((): Group[] => {
    if (!sessions) return [];
    const all = sessions.filter((s) => s.id !== hide);
    const children = new Map<string, BoardSession[]>();
    for (const s of all) if (s.parent) children.set(s.parent, [...(children.get(s.parent) ?? []), s]);
    const kids = (s: BoardSession) => children.get(s.id) ?? [];
    const busyish = (s: BoardSession) => s.busy || kids(s).some((c) => c.busy);
    const needle = filter.trim().toLowerCase();
    const hit = (s: BoardSession) => !needle || title(s).toLowerCase().includes(needle) || task(s).toLowerCase().includes(needle) || (needle.startsWith('@') && s.agent === needle.slice(1));
    const top = all.filter((s) => !s.parent && (showJobs || !archivedJob(s)));
    const out: Group[] = [];
    for (const name of SECTIONS) {
      const group = top.filter((s) => section(s) === name && (hit(s) || kids(s).some(hit)));
      // Waiting on the owner first, then running; otherwise the kernel's order (latest first).
      const rank = (s: BoardSession) => (s.waiting ? 0 : 2) + (busyish(s) ? 0 : 1);
      group.sort((a, b) => rank(a) - rank(b));
      const archived = name === 'Archived';
      const count = archived && !showArchived ? Math.max(group.length, archivedCount) : group.length;
      if (count === 0 && name !== 'Sessions' && !(archived && showArchived && jobsHidden > 0)) continue;
      const hidden = archived && !showArchived && !needle;
      const rows: Row[] = [];
      if (!hidden)
        for (const s of group) {
          const ks = kids(s).filter((c) => !needle || hit(s) || hit(c));
          const run = ks.filter((c) => c.busy);
          const done = ks.filter((c) => !c.busy);
          const shownDone = archived ? 0 : FINISHED_CHILDREN;
          const sent = ks.length;
          // The kernel sends only the latest children; `children` counts them all.
          const all = needle ? sent : Math.max(s.children ?? sent, sent);
          const more = all - run.length - Math.min(done.length, shownDone);
          rows.push({ id: s.id, s, child: false, runningKids: run.length, kids: all, more });
          for (const c of [...run, ...done.slice(0, shownDone)]) rows.push({ id: c.id, s: c, child: true, runningKids: 0, kids: 0, more: 0 });
        }
      out.push({ name, count, hidden, rows });
    }
    return out;
  });

  const ids = $derived(groups.flatMap((g) => g.rows.map((r) => r.id)));
  const spinning = $derived(groups.some((g) => g.rows.some((r) => r.s.busy || r.runningKids > 0)));

  // Keep the selection on a session that is listed.
  $effect(() => {
    if (!selected || !ids.includes(selected)) selected = ids[0] ?? null;
  });

  $effect(() => {
    if (!spinning) return;
    const t = setInterval(() => (spin = (spin + 1) % SPINNER.length), 100);
    return () => clearInterval(t);
  });

  function step(by: number) {
    if (!ids.length) return;
    const at = Math.max(0, selected ? ids.indexOf(selected) : 0);
    selected = ids[Math.min(ids.length - 1, Math.max(0, at + by))];
    follow(list);
  }

  function toggleArchived() {
    showArchived = !showArchived;
    load();
  }

  function toggleJobs() {
    if (!showArchived) return;
    showJobs = !showJobs;
    try {
      localStorage.setItem(JOBS_KEY, showJobs ? '1' : '0');
    } catch {}
  }

  function open(id: string | null) {
    if (id) openSession(id);
  }

  function key(e: KeyboardEvent) {
    if (!active || !plain(e)) return;
    if (e.target === input) return; // the filter's own keys (inputKey)
    if (typing(e)) return;
    const page = pageRows(list);
    const acts: Record<string, () => void> = {
      ArrowUp: () => step(-1),
      k: () => step(-1),
      ArrowDown: () => step(1),
      j: () => step(1),
      PageUp: () => step(-page),
      PageDown: () => step(page),
      Home: () => step(-1e9),
      End: () => step(1e9),
      Enter: () => open(selected),
      '/': () => input?.focus(),
      a: toggleArchived,
      J: toggleJobs,
      r: load,
    };
    const act = acts[e.key];
    if (act) {
      e.preventDefault();
      act();
    }
  }

  function inputKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      filter = '';
      input?.blur();
    } else if (e.key === 'Enter') {
      e.preventDefault();
      input?.blur();
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      step(e.key === 'ArrowDown' ? 1 : -1);
    }
  }

  const mark = (r: Row) =>
    r.s.waiting && !r.s.busy ? { c: '?', k: 'warn' } : r.s.busy || r.runningKids > 0 ? { c: SPINNER[spin], k: 'run' } : { c: '○', k: 'idle' };

  function meta(r: Row): string {
    const s = r.s;
    let m = `· ${agentTag(s.agent)}${model(s)}`;
    if (s.busy) m += ' · running';
    else {
      const a = s.updated_at ? ago(s.updated_at, now) : '';
      if (a) m += ` · ${a}`;
    }
    if (r.kids > 0) m += r.runningKids > 0 ? ` · ${r.runningKids}/${r.kids} subagents working` : ` · ${r.kids} subagent${r.kids === 1 ? '' : 's'}`;
    if (r.more > 0) m += ` (${r.more} older not shown)`;
    return m;
  }
</script>

<svelte:window onkeydown={key} />

<div class="view" bind:this={list}>
  <div class="tools">
    <input
      bind:this={input}
      bind:value={filter}
      onkeydown={inputKey}
      type="search"
      placeholder="/ filter"
      aria-label="filter sessions"
      spellcheck="false"
      autocomplete="off"
    />
    <button class="chip" class:on={showArchived} onclick={toggleArchived} title="a">archived</button>
    {#if showArchived}
      <button class="chip" class:on={showJobs} onclick={toggleJobs} title="J">jobs</button>
    {/if}
    <button class="chip new" onclick={() => openSession('new')} title="n">+ new session</button>
  </div>

  {#if error}<p class="err">can't reach the kernel: {error}</p>{/if}
  {#if sessions === null && !error}<p class="dim pad">loading…</p>{/if}

  <div role="listbox" aria-label="sessions" tabindex="-1">
    {#each groups as g (g.name)}
      <div class="head">
        <b>{g.name.toUpperCase()}</b><span class="dim">{g.count}</span>
        {#if g.name === 'Archived'}
          <button class="dim link" onclick={toggleArchived}>· a {showArchived ? 'hides' : 'shows'}</button>
          {#if showArchived}
            <button class="dim link" onclick={toggleJobs}>· J {showJobs ? 'hides jobs' : `shows jobs (${jobsHidden})`}</button>
          {/if}
        {/if}
      </div>
      {#if !g.hidden && g.rows.length === 0}
        <p class="dim pad">nothing here · <button class="link" onclick={() => openSession('new')}>n starts a session</button></p>
      {/if}
      {#each g.rows as r (r.id)}
        {@const m = mark(r)}
        <div
          class="row"
          class:child={r.child}
          class:sel={selected === r.id}
          class:active
          data-sel={selected === r.id ? '1' : '0'}
          role="option"
          tabindex="-1"
          aria-selected={selected === r.id}
          onclick={() => {
            selected = r.id;
            open(r.id);
          }}
          onkeydown={() => {}}
        >
          {#if r.child}<span class="dim arrow">↳</span>{/if}
          <span class="mark {m.k}">{m.c}</span>
          <span class="body">
            {#if r.child}
              <span class="text"><span class="dim">{r.s.kind ?? 'child'}:</span> {task(r.s)}</span>
            {:else}
              <span class="text" class:bold={r.s.busy || r.s.waiting || r.runningKids > 0}>{title(r.s)}</span>
              {#if r.s.archived && r.s.outcome}<span class="outcome {r.s.outcome}" title={r.s.outcome_note ?? r.s.outcome}>{r.s.outcome}</span>{/if}
            {/if}
            {#if r.s.waiting}<span class="warn nowrap"> · waiting on you</span>{/if}
            <span class="dim meta">{meta(r)}</span>
          </span>
        </div>
      {/each}
    {/each}
  </div>
</div>

<style>
  .view { container-type: inline-size; padding: 6px 0 24px; font-family: var(--sans); font-size: 14px; }
  .tools { display: flex; gap: 8px; align-items: center; padding: 4px 14px 6px; }
  .tools input { flex: 1; max-width: 320px; min-width: 0; font: inherit; font-size: 13px; color: var(--ink); background: transparent; border: 1px solid var(--faint); border-radius: 6px; padding: 4px 8px; }
  .tools input:focus { outline: none; border-color: var(--accent); }
  .chip { font-size: 12px; color: var(--dim); border: 1px solid var(--faint); border-radius: 6px; padding: 3px 8px; }
  .chip.on { color: var(--accent); border-color: var(--accent); }
  .head { display: flex; gap: 8px; align-items: baseline; padding: 12px 14px 4px; font-size: 12px; letter-spacing: 0.04em; }
  .head b { font-weight: 650; }
  .link { font-size: 12px; padding: 0; }
  .pad .link { font-size: inherit; color: var(--accent); }
  .chip.new { margin-left: auto; }
  .link:hover { color: var(--ink); }
  .pad { padding: 2px 14px 2px 34px; margin: 0; font-size: 13px; }
  .err { color: var(--err); padding: 4px 14px; margin: 0; font-size: 13px; }
  .row { display: flex; align-items: baseline; gap: 6px; min-height: 28px; box-sizing: border-box; padding: 4px 14px 4px 14px; cursor: pointer; white-space: nowrap; overflow: hidden; border-radius: 6px; margin: 0 6px; outline-offset: -1px; }
  .row:hover { background: color-mix(in srgb, var(--ink) 4%, transparent); }
  .row.sel { outline: 1px solid var(--faint); }
  .row.sel.active { outline: none; background: color-mix(in srgb, var(--accent) 14%, transparent); }
  .row.child { padding-left: 34px; font-size: 13px; }
  .mark { width: 1.1em; flex: none; text-align: center; }
  .mark.warn { color: var(--warn); font-weight: 650; }
  .mark.run { color: var(--accent); }
  .mark.idle { color: var(--dim); }
  .body { display: flex; align-items: baseline; gap: 6px; min-width: 0; flex: 1; }
  .text { overflow: hidden; text-overflow: ellipsis; min-width: 3em; flex: 0 1 auto; }
  .bold { font-weight: 620; }
  .warn { color: var(--warn); }
  .nowrap { flex: none; }
  .meta { flex: 1 1 0; min-width: 0; overflow: hidden; text-overflow: ellipsis; font-size: 12.5px; }
  .dim { color: var(--dim); }
  .arrow { flex: none; }
  @container (max-width: 560px) {
    .row { white-space: normal; }
    .body { display: block; }
    .text { overflow: visible; }
    .meta { display: block; white-space: nowrap; }
  }
  @media (pointer: coarse) { .row { min-height: 40px; padding-top: 8px; padding-bottom: 8px; } }
  .outcome {
    font-size: 0.75em;
    padding: 0 0.4em;
    border-radius: 0.6em;
    border: 1px solid currentColor;
    opacity: 0.8;
    margin-left: 0.4em;
  }
  .outcome.done { color: var(--ok, #3a3); }
  .outcome.partial { color: var(--warn, #c90); }
  .outcome.not-done { color: var(--err, #c44); }
</style>
