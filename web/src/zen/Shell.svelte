<script lang="ts">
  // The zen app: the board (Sessions · Files · Tasks · Agents · Games, then the plugins' views) or a
  // session's chat with the side panel (the same, plus Preview: a dev server beside the chat), as
  // the terminal app lays them out; Settings off the board's top bar. Keys that act everywhere live
  // here; each view handles its own when its pane has the keys (`active`).
  import { onMount, tick } from 'svelte';
  import { app, start, board, setView, openSession, toggleSide, settings, VIEWS, typing, type View, type Pane } from './app.svelte.ts';
  import { findView, loadPlugins, panel, views as pluginViews } from './plugins.svelte.ts';
  import PluginFrame from './PluginFrame.svelte';
  import Games from './views/Games.svelte';
  import Home from './views/Home.svelte';
  import Routines from './views/Routines.svelte';
  import Settings from './views/Settings.svelte';
  import Chat from './Chat.svelte';
  import Sessions from './views/Sessions.svelte';
  import Files from './views/Files.svelte';
  import Tasks from './views/Tasks.svelte';
  import Agents from './views/Agents.svelte';
  import Preview from './views/Preview.svelte';
  import { zen, type BoardSession } from './api.ts';

  const LABEL: Record<string, string> = { home: 'Home', sessions: 'Sessions', files: 'Files', tasks: 'Tasks', routines: 'Routines', agents: 'Agents', games: 'Games' };
  /** The tabs in order: the built-ins, then each enabled plugin's views. */
  const tabList = $derived([...(['home', ...VIEWS] as View[]).map((v) => ({ key: v, label: LABEL[v] })), ...pluginViews().map((v) => ({ key: v.key as View, label: v.title }))]);
  const keysOf = () => tabList.map((t) => t.key);
  let wide = $state(true);
  let ring = $state(false), agentMenu = $state(false);
  let theme = $state<'auto' | 'light' | 'dark'>((['auto','light','dark'].includes(localStorage.getItem('zen.theme') || '') ? localStorage.getItem('zen.theme') : 'auto') as 'auto' | 'light' | 'dark');
  const icons: Record<string, string> = { home: '⌂', sessions: '◬', files: '▤', tasks: '✓', routines: '◷', agents: '◉', games: '◇' };
  $effect(() => { document.documentElement.dataset.theme = theme; localStorage.setItem('zen.theme', theme); });
  onMount(() => { const sync = (e: Event) => { theme = (e as CustomEvent).detail; }; window.addEventListener('zen-theme', sync); return () => window.removeEventListener('zen-theme', sync); });
  function cycleTheme() { theme = theme === 'auto' ? 'light' : theme === 'light' ? 'dark' : 'auto'; }
  async function action(which: string) {
    ring = false;
    if (which === 'New chat') openSession('new');
    else if (which === 'Search files') { if (app.session && wide) setView('side', 'files'); else board('files'); await tick(); (document.querySelector('.side-body input[type=search], .board input[type=search], .side-body input[placeholder], .board input[placeholder]') as HTMLElement | null)?.focus(); }
    else if (which === 'Attach') { if (app.session) (document.querySelector('[aria-label="attach files"]') as HTMLElement | null)?.click(); else { app.attachOnOpen = true; openSession('new'); } }
    else if (which === 'Add task') { if (app.session && wide) setView('side', 'tasks'); else board('tasks'); await tick(); (document.querySelector('.tasks input') as HTMLInputElement | null)?.focus(); }
    else if (which === 'Theme') cycleTheme();
    else settings();
  }

  // The side panel's share of the split, dragged on the separator and kept across visits.
  const SIDE_KEY = 'zen.side.width';
  const MIN = 0.2, MAX = 0.8;
  const clamp = (f: number) => Math.min(MAX, Math.max(MIN, f));
  let side = $state(clamp(Number(localStorage.getItem(SIDE_KEY)) || 0.5));
  let split: HTMLElement | undefined = $state();
  let dragging = $state(false);
  const keepSide = () => localStorage.setItem(SIDE_KEY, String(side));

  function grab(e: PointerEvent) {
    if (!split || e.button !== 0) return;
    e.preventDefault();
    const bar = e.currentTarget as HTMLElement;
    bar.setPointerCapture(e.pointerId);
    dragging = true;
    const box = split.getBoundingClientRect();
    const move = (m: PointerEvent) => (side = clamp((box.right - m.clientX) / box.width));
    const drop = () => {
      dragging = false;
      bar.removeEventListener('pointermove', move);
      bar.removeEventListener('pointerup', drop);
      bar.removeEventListener('pointercancel', drop);
      keepSide();
    };
    bar.addEventListener('pointermove', move);
    bar.addEventListener('pointerup', drop);
    bar.addEventListener('pointercancel', drop);
  }

  function nudge(e: KeyboardEvent) {
    const step = e.shiftKey ? 0.1 : 0.02;
    if (e.key === 'ArrowLeft') side = clamp(side + step);
    else if (e.key === 'ArrowRight') side = clamp(side - step);
    else if (e.key === 'Home' || e.key === 'Enter') side = 0.5;
    else return;
    e.preventDefault();
    keepSide();
  }
  let counts = $state({ sessions: 0, running: 0, waiting: 0 });

  onMount(() => {
    start();
    void loadPlugins();
    const mq = matchMedia('(min-width: 900px)');
    const fit = () => (wide = mq.matches);
    fit();
    mq.addEventListener('change', fit);
    const poll = async () => {
      try {
        const b = await zen.board();
        const top = b.sessions.filter((s: BoardSession) => !s.parent);
        counts = { sessions: top.length, running: b.sessions.filter((s) => s.busy).length, waiting: top.filter((s) => s.waiting && !s.busy).length };
        app.waiting = counts.waiting;
      } catch {}
    };
    poll();
    const t = setInterval(poll, 4000);
    return () => {
      clearInterval(t);
      mq.removeEventListener('change', fit);
    };
  });

  $effect(() => {
    document.title = `${app.waiting ? `(${app.waiting}) ` : ''}zen`;
  });

  function key(e: KeyboardEvent) {
    if (e.defaultPrevented || e.isComposing) return;
    if (e.key === 'Escape') { ring = false; agentMenu = false; }
    if ((e.ctrlKey || e.metaKey) && e.key === '.') { e.preventDefault(); ring = !ring; return; }
    const inSession = app.session !== null;
    if (e.ctrlKey && !e.altKey && !e.metaKey && e.key.toLowerCase() === 'b' && inSession) {
      e.preventDefault();
      toggleSide();
      return;
    }
    if (inSession && e.shiftKey && e.key === 'Tab' && app.side.open) {
      e.preventDefault();
      app.focus = app.focus === 'chat' ? 'side' : 'chat';
      return;
    }
    if (inSession || typing(e) || e.ctrlKey || e.metaKey || e.altKey) return;
    // The board.
    const keys: View[] = keysOf().filter((v) => v !== 'home');
    const i = /^[1-9]$/.test(e.key) ? Number(e.key) - 1 : -1;
    if (i >= 0 && i < keys.length) {
      e.preventDefault();
      setView('home', keys[i]);
    } else if (e.key === 'Tab') {
      e.preventDefault();
      const n = Math.max(0, keys.indexOf(app.view));
      setView('home', keys[(n + (e.shiftKey ? keys.length - 1 : 1)) % keys.length]);
    } else if (e.key === 'n' && !(app.view === 'files' && app.file.home) && ['sessions', 'files', 'tasks'].includes(app.view)) {
      // (The Agents view's own `n` starts a session with the chosen agent.)
      e.preventDefault();
      openSession('new');
    }
  }
</script>

<svelte:window onkeydown={key} />

{#snippet tabs(pane: Pane, current: View)}
  <div class="ztabs" role="tablist">
    {#each tabList as t, i (t.key)}
      <button role="tab" class:on={current === t.key} aria-selected={current === t.key} onclick={() => setView(pane, t.key)} title={i < 9 ? String(i + 1) : undefined} aria-label={t.label}><span class="tab-icon" aria-hidden="true">{icons[t.key] || '◫'}</span><span class="tab-label">{t.label}</span>{#if t.key === 'sessions' && counts.waiting}<span class="badge">{counts.waiting}</span>{/if}</button>
    {/each}
    {#if pane === 'side'}
      <button role="tab" class:on={current === 'preview'} aria-selected={current === 'preview'} onclick={() => setView(pane, 'preview')}>Preview</button>
    {/if}
    {#if pane === 'side' && panel.current}
      <button role="tab" class:on={current === 'panel'} aria-selected={current === 'panel'} onclick={() => setView(pane, 'panel')}>{panel.current.title}</button>
    {/if}
  </div>
{/snippet}

{#snippet view(pane: Pane, v: View, active: boolean)}
  {#if v === 'home'}
    <Home />
  {:else if v === 'sessions'}
    <Sessions {pane} {active} hide={pane === 'side' ? app.session : null} />
  {:else if v === 'files'}
    <Files {pane} {active} />
  {:else if v === 'tasks'}
    <Tasks {pane} {active} />
  {:else if v === 'routines'}
    <Routines />
  {:else if v === 'agents'}
    <Agents {pane} {active} />
  {:else if v === 'games'}
    <Games {pane} {active} />
  {:else if v === 'preview'}
    <Preview />
  {:else if v === 'settings'}
    <Settings />
  {:else if v === 'panel' && panel.current}
    {#key panel.current.n}
      <PluginFrame plugin={panel.current.plugin} ui={panel.current.ui} {pane} slot="view" title={panel.current.title} input={panel.current.input} result={panel.current.result} />
    {/key}
  {:else if findView(v)}
    {@const pv = findView(v)!}
    {#key pv.key}
      <PluginFrame plugin={pv.plugin} ui={pv.ui} {pane} slot="view" title={pv.title} />
    {/key}
  {:else}
    <p class="dim gone">This view isn't available: its plugin is off or removed.</p>
  {/if}
{/snippet}

<div class="zen" class:in-session={app.session !== null}>
  {#if app.session === null}
    <div class="bar">
      <div class="agent-wrap"><button class="brand" aria-label="Agent menu" aria-expanded={agentMenu} onclick={() => agentMenu = !agentMenu}>◉ <span>zen</span></button>{#if agentMenu}<div class="agent-menu"><strong>zenbot</strong><button onclick={() => { agentMenu = false; board('agents'); }}>Agents</button><button onclick={() => { agentMenu = false; settings(); }}>Settings</button><button onclick={cycleTheme}>Theme: {theme}</button></div>{/if}</div>
      {@render tabs('home', app.view)}
      <span class="grow"></span>
      {#if counts.waiting}<span class="waiting">? {counts.waiting} waiting on you</span>{/if}
      <span class="dim">{counts.sessions} sessions · {counts.running ? `${counts.running} running` : 'all idle'}</span>
      <button class="dim settings" class:on={app.view === 'settings'} onclick={() => settings()}>Settings</button>
      <button class="new" onclick={() => openSession('new')} title="n">New session</button>
    </div>
    <div class="board">{@render view('home', app.view, true)}</div>
  {:else}
    <div class="split" class:with-side={app.side.open} class:dragging bind:this={split} style:--side={`${side * 100}%`}>
      <section class="chat-pane" class:has-keys={app.focus === 'chat'}>
        {#key app.session}
          <Chat id={app.session} active={app.focus === 'chat' || !app.side.open} {wide} />
        {/key}
      </section>
      {#if app.side.open && wide}
        <!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_no_noninteractive_tabindex -->
        <div
          class="gutter"
          role="separator"
          aria-orientation="vertical"
          aria-label="Resize the side panel"
          aria-valuenow={Math.round((1 - side) * 100)}
          aria-valuemin={Math.round((1 - MAX) * 100)}
          aria-valuemax={Math.round((1 - MIN) * 100)}
          tabindex="0"
          title="drag to resize · double-click resets"
          onpointerdown={grab}
          ondblclick={() => ((side = 0.5), keepSide())}
          onkeydown={nudge}
        ></div>
      {/if}
      {#if app.side.open}
        <aside class="zside" class:overlay={!wide} class:has-keys={app.focus === 'side'} onpointerdown={() => (app.focus = 'side')}>
          <div class="bar">
            {@render tabs('side', app.side.view)}
            <span class="grow"></span>
            <button class="dim settings" onclick={() => settings()}>Settings</button>
            {#if app.focus === 'side'}<span class="dim" title="shift+tab gives the keys back">●</span>{/if}
            <button class="dim" onclick={() => toggleSide()} title="ctrl+b">✕</button>
          </div>
          <div class="side-body">{@render view('side', app.side.view, app.focus === 'side')}</div>
        </aside>
      {/if}
    </div>
  {/if}
  <div class="quick"><button class="ring-toggle" aria-label="Command ring" aria-expanded={ring} onclick={() => ring = !ring}>✣</button>{#if ring}<div class="ring" aria-label="Quick actions">{#each ['New chat', 'Search files', 'Attach', 'Add task', 'Theme', 'Settings'] as item}<button onclick={() => action(item)}>{item}{item === 'Theme' ? ` · ${theme}` : ''}</button>{/each}</div>{/if}</div>
  {#if app.notice}<div class="notice" role="status">{app.notice}</div>{/if}
</div>

<style>
  .zen { background:var(--canvas); padding: 10px; gap:10px; height: 100dvh; display: flex; flex-direction: column; font-family: var(--sans); font-size: 15px; }
  .bar { background:var(--paper); border-radius:16px; display: flex; align-items: center; gap: 14px; padding: 10px 16px; border-bottom: 1px solid var(--faint); font-size: 14px; flex: none; }
  @media (max-width: 600px) { .bar { flex-wrap: wrap; gap: 8px; padding: 8px; } .ztabs { flex-wrap: wrap; } .ztabs button { padding: 4px 7px; } }
  .brand { font-weight: 650; letter-spacing: 0.02em; }
  .ztabs { display: flex; gap: 4px; }
  .ztabs button { padding: 4px 10px; border-radius: 6px; color: var(--dim); }
  .ztabs button.on { color: var(--ink); background: color-mix(in srgb, var(--ink) 8%, transparent); }
  .ztabs button:hover { color: var(--ink); }
  .grow { flex: 1; }
  .waiting { color: var(--warn); }
  .new { color: var(--accent); }
  .settings:hover, .settings.on { color: var(--ink); }
  .gone { padding: 12px 16px; margin: 0; }
  .board { background:var(--paper); border:1px solid var(--faint); border-radius:20px; flex: 1; min-height: 0; overflow: auto; }
  .split { flex: 1; min-height: 0; display: grid; grid-template-columns: 1fr; position: relative; }
  .split.with-side { grid-template-columns: minmax(0, 1fr) 0 minmax(0, var(--side, 50%)); }
  .split.dragging { cursor: col-resize; user-select: none; }
  .gutter { position: relative; z-index: 5; width: 0; cursor: col-resize; touch-action: none; }
  .gutter::before { content: ''; position: absolute; inset: 0 -4px; }
  .gutter::after { content: ''; position: absolute; top: 0; bottom: 0; left: -1px; width: 2px; background: transparent; transition: background 0.12s; }
  .gutter:hover::after, .gutter:focus-visible::after, .dragging .gutter::after { background: var(--accent); }
  .gutter:focus-visible { outline: none; }
  .chat-pane { background:var(--paper); border:1px solid var(--faint); border-radius:20px; overflow:hidden; min-height: 0; min-width: 0; display: flex; flex-direction: column; }
  .zside { border-radius:20px; overflow:hidden; min-height: 0; min-width: 0; display: flex; flex-direction: column; border-left: 1px solid var(--faint); background: var(--paper); }
  .zside.overlay { position: absolute; inset: 0; z-index: 20; border-left: 0; }
  .side-body { flex: 1; min-height: 0; overflow: auto; }
  .notice { position: fixed; bottom: 104px; left: 50%; transform: translateX(-50%); background: var(--paper); border: 1px solid var(--faint); border-radius: 8px; padding: 8px 14px; font-size: 13px; z-index: 50; max-width: 90vw; box-shadow: 0 4px 18px rgb(0 0 0 / 0.08); }
  .agent-wrap { position:relative; } .brand { color:var(--accent); font-size:20px; } .agent-menu { position:absolute; z-index:40; top:36px; left:0; min-width:160px; background:var(--paper); border:1px solid var(--faint); border-radius:14px; padding:12px; box-shadow:0 8px 30px #0002; display:grid; gap:12px; } .agent-menu button { text-align:left; } .tab-icon { display:none; } .badge { margin-left:4px; color:white; background:var(--err); border-radius:20px; font-size:10px; padding:1px 5px; } .quick { position:fixed; bottom:24px; right:24px; z-index:45; } .ring-toggle { background:var(--accent); color:white; border-radius:50%; width:52px; height:52px; font-size:26px; box-shadow:0 6px 22px #0003; } .ring { position:absolute; bottom:60px; right:0; background:var(--paper); border:1px solid var(--faint); box-shadow:0 8px 30px #0002; border-radius:18px; padding:8px; width:160px; display:grid; } .ring button { text-align:left; padding:10px; border-radius:8px; } .ring button:hover { background:var(--canvas); }
  @media(max-width: 600px) { .zen { padding:5px; } .bar { flex-wrap:nowrap; } .ztabs { flex-wrap:nowrap; flex:1; justify-content:space-around; } .ztabs button { padding:7px 4px; } .tab-icon { display:inline; font-size:18px; } .tab-label { display:none; } .bar > .dim, .bar > .waiting, .bar > .new, .bar > .settings { display:none; } .brand span { display:none; } }
  @media (max-width: 899px) { .split.with-side { grid-template-columns: 1fr; } }
</style>
