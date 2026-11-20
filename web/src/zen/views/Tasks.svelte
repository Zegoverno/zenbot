<script lang="ts">
  // Tasks tab: the vault's tasks (Projects/<p>/tasks.md, through the kernel), ticked, added and
  // edited here; every write carries the line's revision. Grouped by when (default), by priority,
  // by project or by goal. One user per zenbot: no owners. (The redesign mock's Tasks, live.)
  let { hand, open: openChat, agent = 'zenbot' }: { hand?: (t: T) => Promise<string | null>; open?: (session: string) => void; agent?: string } = $props();

  import { onMount } from 'svelte';
  import { box, flag, facts } from './TaskBits.svelte';
  import { store, toggle, load, loadMeta, meta, day, plus, patch, remove, add, PRI, OFFER, days, sort, type T, type Pri } from './taskstore.svelte.ts';
  const PROJECTS = $derived(meta.projects);
  const GOALS = $derived(meta.goals);
  const KEY = 'zen.tasks';
  let by = $state<'when' | 'priority' | 'project' | 'goal'>((sessionStorage.getItem(KEY + '.by') as 'when') ?? 'when');
  $effect(() => { sessionStorage.setItem(KEY + '.by', by); });
  onMount(() => {
    void load(); void loadMeta();
    const t = setInterval(() => { if (!editing && !adding) void load(); }, 10000);
    return () => clearInterval(t);
  });

  // ---- groups ----
  const open = $derived(store.tasks.filter((t) => !t.done && !t.dropped));
  const doneN = $derived(store.tasks.filter((t) => t.done));
  const groups = $derived.by(() => {
    const g: { key: string; label: string; tone?: string; items: T[] }[] = [];
    const add = (key: string, label: string, items: T[], tone?: string) => items.length && g.push({ key, label, tone, items: [...items].sort(sort) });
    if (by === 'when') {
      add('late', 'Overdue', open.filter((t) => t.due && days(t.due) < 0), 'bad');
      add('today', 'Today', open.filter((t) => t.due && days(t.due) === 0));
      add('week', 'Next 7 days', open.filter((t) => t.due && days(t.due) > 0 && days(t.due) <= 7));
      add('later', 'Later', open.filter((t) => t.due && days(t.due) > 7));
      add('none', 'No date', open.filter((t) => !t.due));
    } else if (by === 'priority') {
      // Same order as the sort; medium (Obsidian only) shows only when a task has it.
      for (const p of ['urgent', 'important', 'medium', 'not_urgent']) add(p, PRI[p].l, open.filter((t) => t.pri === p), p === 'urgent' ? 'bad' : undefined);
      add('none', 'No priority', open.filter((t) => !t.pri));
      add('parking_lot', PRI.parking_lot.l, open.filter((t) => t.pri === 'parking_lot'));
    } else if (by === 'project') {
      for (const p of [...new Set([...PROJECTS, ...open.map((t) => t.project)])]) add(p, p, open.filter((t) => t.project === p));
    } else {
      for (const x of [...new Set([...GOALS, ...open.map((t) => t.goal).filter((x): x is string => !!x)])]) add(x, x, open.filter((t) => t.goal === x));
      add('none', 'No goal', open.filter((t) => !t.goal));
    }
    return g;
  });
  const lateN = $derived(open.filter((t) => t.due && days(t.due) < 0).length);
  const todayN = $derived(open.filter((t) => t.due && days(t.due) === 0).length);

  // ---- actions ----
  let toast = $state<{ text: string; id: string; added?: T } | null>(null);
  let tt: ReturnType<typeof setTimeout> | undefined;
  function say(t: typeof toast, ms = 5000) { toast = t; clearTimeout(tt); tt = setTimeout(() => (toast = null), ms); }
  async function tick(t: T) {
    const text = t.text, id = t.id, was = t.done;
    await toggle(t);
    if (!was) say({ text: `Done: ${text}`, id });
  }
  async function undo() {
    const t0 = toast;
    toast = null;
    if (!t0) return;
    const t = store.tasks.find((x) => x.id === t0.id || (t0.added && x.text === t0.added.text && x.project === t0.added.project));
    if (!t) return;
    if (t0.added) await run(() => remove(t));
    else await toggle(t);
  }
  async function run(f: () => Promise<unknown>) {
    try { await f(); store.error = ''; } catch (e) { store.error = (e as Error).message; }
    await load();
  }

  let editing = $state<string | null>(null);
  /** The task being edited, a copy saved with Done (the vault changes once, not per keystroke). */
  let ed = $state<T | null>(null);
  function startEdit(t: T) {
    if (editing === t.id) return saveEdit();
    void saveEdit();
    editing = t.id; ed = { ...$state.snapshot(t) } as T;
  }
  async function saveEdit() {
    const t = store.tasks.find((x) => x.id === editing), e = ed;
    editing = null; ed = null;
    if (!t || !e) return;
    const f: Record<string, unknown> = {};
    if (e.text.trim() && e.text.trim() !== t.text) f.text = e.text.trim();
    if (e.due !== t.due) f.due = e.due;
    if (e.pri !== t.pri) f.priority = e.pri;
    if (e.goal !== t.goal) f.goal = e.goal;
    if (e.project !== t.project) f.project = e.project;
    if (Object.keys(f).length) await run(() => patch(t, f));
  }
  let showDone = $state(false);

  // Quick add: Enter adds to the chosen project; due defaults to today in the "when" view.
  // The adder: the text, then the same fields as the editor, open while it has focus or text.
  // Defaults come from the last task added (project) and the view (a project or goal group).
  const blank = (p = PROJECTS[0] ?? ''): T => ({ id: '', rev: '', text: '', project: p, goal: null, due: null, pri: null });
  let nt = $state<T>(blank());
  $effect(() => { if (!nt.project && PROJECTS[0]) nt.project = PROJECTS[0]; });
  let adding = $state(false);
  let addEl = $state<HTMLInputElement>();
  async function addTask() {
    const text = nt.text.trim();
    if (!text || !nt.project) return;
    const t = { ...$state.snapshot(nt), text };
    nt = blank(nt.project);
    addEl?.focus();
    try {
      const made = await add(t);
      say({ text: `Added to ${t.project}`, id: made.id, added: made }, 3000);
      store.error = '';
    } catch (e) { store.error = (e as Error).message; }
    await load();
  }
  function cancelAdd() { nt = blank(nt.project); adding = false; addEl?.blur(); }
  const QUICK = $derived<[string, string | null][]>([['Today', day.today], ['Tomorrow', plus(1)], ['Next week', plus(7)], ['No date', null]]);
  async function handTo(t: T) {
    const s = await hand?.(t);
    if (s) { t.agent = 'working'; t.by = agent; t.session = s; }
  }
</script>

{#snippet fields(t: T)}
  <div class="ef">
    <span class="lbl">Due</span>
    {#each QUICK as [l, d]}<button class="pill" class:on={t.due === d} onclick={() => (t.due = d)}>{l}</button>{/each}
    <input type="date" class="pill date" value={t.due ?? ''} onchange={(e) => (t.due = (e.target as HTMLInputElement).value || null)} />
  </div>
  <div class="ef">
    <span class="lbl">Priority</span>
    {#each OFFER as p}
      <button class="pill" class:on={t.pri === p} title={PRI[p].h} onclick={() => (t.pri = t.pri === p ? null : (p as Pri))}><i class="pd" style="background:{PRI[p].c}"></i>{PRI[p].l}</button>
    {/each}
    {#if t.pri === 'medium'}<span class="hint">Medium (from Obsidian): pick one of these</span>{/if}
  </div>
  <div class="ef">
    <span class="lbl"></span>
    <span class="hint">{t.pri ? PRI[t.pri].h : 'No priority: sorts after not urgent, before the parking lot'}{#if t.pri === 'important' && !t.goal} · <b>which goal does it move?</b>{/if}</span>
  </div>
  <div class="ef">
    <span class="lbl">Project</span>
    <select class="dd" bind:value={t.project} aria-label="Project">{#each PROJECTS.includes(t.project) || !t.project ? PROJECTS : [t.project, ...PROJECTS] as p}<option value={p}>{p}</option>{/each}</select>
  </div>
  <div class="ef" class:want={t.pri === 'important' && !t.goal}>
    <span class="lbl">Goal</span>
    <select class="dd" bind:value={t.goal} aria-label="Goal" title={t.goal ?? 'No goal'}><option value={null}>No goal</option>{#each !t.goal || GOALS.includes(t.goal) ? GOALS : [t.goal, ...GOALS] as x}<option value={x}>{x}</option>{/each}</select>
  </div>
{/snippet}

{#snippet row(t: T)}
  <div class="task" class:done={t.done} class:ed={editing === t.id}>
    {@render box(t, () => tick(t))}
    <button class="main" onclick={() => startEdit(t)}>
      <span class="tx">{@render flag(t)}{t.text}</span>
      <span class="meta">
        {@render facts(t, { project: by !== 'project', goal: by !== 'goal' })}
        {#if t.agent}<span class="ag {t.agent}" role="link" tabindex="-1" onclick={(e) => { if (t.session && openChat) { e.stopPropagation(); openChat(t.session); } }} onkeydown={() => {}}><i></i>{t.by ?? 'zenbot'} {t.agent === 'working' ? 'is on it' : 'needs you'}</span>{/if}
      </span>
    </button>
    {#if t.session && openChat}
      <button class="hand on" title="Open the chat working on this task" onclick={() => openChat?.(t.session!)}>Open chat</button>
    {:else if hand && !t.done && !t.agent}
      <button class="hand" title={`Hand it to ${agent}: a new chat starts working on this task`} onclick={() => handTo(t)}>Hand<span class="ha">{" "}to {agent}</span></button>
    {/if}
  </div>
  {#if editing === t.id && ed}
    <div class="edit">
      <!-- svelte-ignore a11y_autofocus -->
      <input class="et" bind:value={ed.text} aria-label="Task" autofocus onkeydown={(e) => { if (e.key === 'Enter') saveEdit(); if (e.key === 'Escape') { e.stopPropagation(); editing = null; ed = null; } }} />
      {@render fields(ed)}
      <div class="ef end">
        <button class="del" onclick={() => { editing = null; ed = null; void run(() => remove(t)); }}>Delete</button>
        <button class="del drop" title="Keep the line, marked dropped ([-])" onclick={() => { editing = null; ed = null; void run(() => patch(t, { dropped: true })); }}>Drop</button>
        <span class="sp"></span>
        <button class="pill on" onclick={saveEdit}>Done</button>
      </div>
    </div>
  {/if}
{/snippet}

<div class="twrap">
  {#if store.error}<div class="err" role="alert">{store.error}</div>{/if}
  <div class="sum">
    <span>{#if lateN}<b class="bad">{lateN} overdue</b>{" · "}{/if}<b>{todayN} today</b> · {open.length} open</span>
    <span class="sp"></span>
    <div class="seg" role="tablist" aria-label="Group by">
      {#each [['when', 'When'], ['priority', 'Priority'], ['project', 'Project'], ['goal', 'Goal']] as [k, l]}
        <button role="tab" aria-selected={by === k} class:on={by === k} onclick={() => (by = k as 'when')}>{l}</button>
      {/each}
    </div>
  </div>

  <div class="add" class:open={adding || nt.text.trim()} onfocusout={(e) => { if (!nt.text.trim() && !(e.currentTarget as HTMLElement).contains(e.relatedTarget as Node)) adding = false; }}>
    <div class="addl">
      <span class="plus">+</span>
      <input placeholder="Add a task" bind:this={addEl} bind:value={nt.text} onfocus={() => (adding = true)}
        onkeydown={(e) => { if (e.key === 'Enter') addTask(); if (e.key === 'Escape') cancelAdd(); }} />
    </div>
    {#if adding || nt.text.trim()}
      <div class="addf">
        {@render fields(nt)}
        <div class="ef end">
          <span class="hint">Enter adds it and keeps this open for the next one · Esc closes</span>
          <span class="sp"></span>
          <button class="pill" onclick={cancelAdd}>Cancel</button>
          <button class="pill on" disabled={!nt.text.trim()} onclick={addTask}>Add task</button>
        </div>
      </div>
    {/if}
  </div>

  <div class="tlist">
    {#each groups as g (g.key)}
      <div class="gh" class:bad={g.tone === 'bad'}>{#if by === 'priority' && PRI[g.key]}<i class="pd" style="background:{PRI[g.key].c}"></i>{/if}{g.label}<span>{g.items.length}</span>{#if by === 'priority' && PRI[g.key]}<em>{PRI[g.key].h}</em>{/if}</div>
      {#each g.items as t (t.id)}{@render row(t)}{/each}
    {:else}
      <div class="empty">{store.loaded ? 'Nothing open. Enjoy the day.' : 'Loading…'}</div>
    {/each}
    {#if doneN.length}
      <button class="gh tog" onclick={() => (showDone = !showDone)}>{showDone ? '▾' : '▸'} Done<span>{doneN.length}</span></button>
      {#if showDone}{#each doneN as t (t.id)}{@render row(t)}{/each}{/if}
    {/if}
  </div>
</div>

{#if toast}
  <div class="toast" role="status">{toast.text}<button onclick={undo}>Undo</button></div>
{/if}

<style>
  .twrap { flex: 1; min-height: 0; display: flex; flex-direction: column; width: 100%; max-width: 760px; margin: 0 auto; }
  .sum { display: flex; align-items: center; gap: 8px; padding: 2px 18px 8px 22px; font-size: 13px; color: var(--dim); flex-wrap: wrap; }
  .sum b { color: var(--ink); font-weight: 600; }
  .sum .bad, .gh.bad { color: var(--bad, #b5543c); }
  .sp { flex: 1; }
  .seg { display: flex; background: var(--soft); border-radius: 99px; padding: 2px; }
  .seg button { padding: 4px 12px; border-radius: 99px; font-size: 13px; color: var(--dim); }
  .seg button.on { background: var(--paper); color: var(--ink); font-weight: 600; box-shadow: 0 1px 3px var(--shadow); }
  .add { margin: 0 16px 6px; padding: 6px 10px 6px 12px; border-radius: 12px; border: 1px dashed var(--faint); }
  .add:focus-within, .add.open { border-style: solid; border-color: color-mix(in srgb, var(--accent) 55%, var(--faint)); background: var(--paper); box-shadow: 0 4px 18px var(--shadow); }
  .addl { display: flex; align-items: center; gap: 8px; }
  .addf { display: flex; flex-direction: column; gap: 8px; padding: 8px 0 4px 28px; border-top: 1px solid var(--faint); margin-top: 6px; }
  .pill:disabled { opacity: 0.45; cursor: default; }
  .dd { appearance: none; -webkit-appearance: none; font: inherit; font-size: 13px; color: var(--ink); background: var(--paper) url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%23888' stroke-width='2' stroke-linecap='round'%3E%3Cpath d='M7 10l5 5 5-5'/%3E%3C/svg%3E") no-repeat right 8px center / 14px;
    border: 1px solid var(--faint); border-radius: 9px; padding: 5px 30px 5px 10px; min-width: 12rem; max-width: 100%; cursor: pointer; text-overflow: ellipsis; }
  .dd:hover { border-color: var(--dim); }
  .dd:focus-visible { outline: none; border-color: var(--accent); box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 15%, transparent); }
  .ef.want .dd { border-color: var(--warn); }
  .pill.gl { max-width: 15rem; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; display: inline-block; }
  .ef.want .lbl { color: var(--warn); }
  .plus { color: var(--accent); font-size: 18px; width: 20px; text-align: center; }
  .add input { flex: 1; min-width: 10rem; border: 0; outline: none; background: transparent; font-size: 14.5px; color: var(--ink); padding: 4px 0; }
  .tlist { flex: 1; overflow: auto; padding: 0 10px 90px; }
  .gh { position: sticky; top: 0; z-index: 2; background: var(--paper); border-radius: 0 0 10px 10px;
    display: flex; align-items: center; gap: 6px; font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); font-weight: 650; padding: 16px 12px 6px; text-align: left; width: 100%; }
  .gh span { font-weight: 500; letter-spacing: 0; opacity: 0.75; }
  .gh em { font-style: normal; text-transform: none; letter-spacing: 0; font-weight: 400; margin-left: 6px; opacity: 0.8; }
  .gh.tog:hover { color: var(--ink); }
  .task { display: flex; align-items: flex-start; gap: 10px; padding: 8px 10px; border-radius: 12px; }
  .task:hover, .task.ed { background: var(--soft); }
  .box { --p: var(--dim); flex: none; width: 20px; height: 20px; margin-top: 1px; border-radius: 50%; border: 1.6px solid var(--p); display: grid; place-items: center; font-size: 12px; color: var(--on-accent); transition: background 0.15s; }
  .box:hover { background: color-mix(in srgb, var(--p) 15%, transparent); }
  .task.done .box { background: var(--accent); border-color: var(--accent); }
  .main { flex: 1; min-width: 0; text-align: left; display: flex; flex-direction: column; gap: 3px; }
  .tx { font-size: 14.5px; }
  .task.done .tx { color: var(--dim); text-decoration: line-through; }
  .meta { display: flex; flex-wrap: wrap; gap: 4px 10px; font-size: 12px; color: var(--dim); }
  .meta:empty { display: none; }
  .due.today { color: var(--accent); font-weight: 600; }
  .due.late { color: var(--bad, #b5543c); font-weight: 600; }
  .goal { max-width: 22rem; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .ag { display: inline-flex; align-items: center; gap: 5px; font-weight: 600; }
  .ag:hover { text-decoration: underline; cursor: pointer; }
  .ag i { width: 6px; height: 6px; border-radius: 50%; }
  .ag.working { color: var(--accent); } .ag.working i { background: var(--accent); animation: bl 1.2s infinite; }
  .ag.waiting { color: var(--warn); } .ag.waiting i { background: var(--warn); }
  @keyframes bl { 50% { opacity: 0.3; } }
  .hand { flex: none; font-size: 12.5px; font-weight: 600; color: var(--accent); padding: 3px 10px; border-radius: 99px; border: 1px solid color-mix(in srgb, var(--accent) 40%, var(--faint)); opacity: 0; transition: opacity 0.15s; }
  .task:hover .hand, .hand:focus-visible, .hand.on { opacity: 1; }
  .hand:hover { background: color-mix(in srgb, var(--accent) 10%, transparent); }
  .edit { margin: -2px 10px 8px 40px; padding: 10px 12px; border: 1px solid var(--faint); border-radius: 12px; display: flex; flex-direction: column; gap: 8px; background: var(--paper); }
  .et { font-size: 14.5px; border: 0; border-bottom: 1px solid var(--faint); padding: 4px 0; outline: none; background: transparent; color: var(--ink); }
  .et:focus { border-color: var(--accent); }
  .ef { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; }
  .lbl { font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); font-weight: 650; width: 62px; }
  .pill { display: inline-flex; align-items: center; gap: 5px; padding: 3px 10px; border-radius: 99px; border: 1px solid var(--faint); font-size: 12.5px; background: var(--paper); color: var(--ink); }
  .pill:hover { border-color: var(--dim); }
  .pill.on { background: var(--accent); border-color: var(--accent); color: var(--on-accent); }
  .pill.sel { max-width: 16rem; font: inherit; font-size: 12.5px; }
  .pill.date { font: inherit; font-size: 12.5px; }
  .hint { font-size: 12px; color: var(--dim); margin-top: -4px; }
  .hint b { color: var(--warn); font-weight: 600; }
  .pill.want { border-color: var(--warn); box-shadow: 0 0 0 3px color-mix(in srgb, var(--warn) 15%, transparent); }
  .pd { width: 8px; height: 8px; border-radius: 50%; }
  .del { font-size: 12.5px; color: var(--bad, #b5543c); }
  .del.drop { color: var(--dim); margin-left: 10px; }
  .err { margin: 0 16px 8px; padding: 8px 12px; border-radius: 10px; font-size: 13px; color: var(--err); background: color-mix(in srgb, var(--err) 8%, var(--paper)); }
  .empty { padding: 30px 12px; color: var(--dim); }
  .toast { position: fixed; left: 50%; bottom: 28px; transform: translateX(-50%); z-index: 30; display: flex; align-items: center; gap: 14px; padding: 10px 16px; border-radius: 12px; background: var(--ink); color: var(--paper); font-size: 14px; box-shadow: 0 10px 30px var(--shadow); }
  .toast button { color: var(--accent); font-weight: 650; filter: brightness(1.6); }
  @media (max-width: 900px) { .hand { opacity: 1; padding: 3px 8px; } .ha { display: none; } .edit { margin-left: 10px; } .lbl { width: 100%; } }
</style>
