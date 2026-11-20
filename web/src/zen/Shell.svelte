<script lang="ts">
  // The zen app, as the redesign lays it out (web/mock/ChatMock2.svelte): no sidebar. The chat on
  // the left, one view on the right (Sessions, Files, Tasks, Routines, Agents, Games, the plugins'
  // views) picked from a floating tab bar at the top centre; clicking the open tab again closes the
  // right panel (focus). Top left, the agent's orb: click to switch agent (and reach the owner and
  // Settings). Bottom right, a quick-control ring (after Logitech's Actions Ring): one button that
  // fans out the everyday actions.
  // Panels: Home and the views stand alone (one panel); a chat opens full width, and a tab picked
  // while in a chat opens beside it (two panels, desktop only), with Preview and a plugin command's
  // panel as extra tabs there.
  // On a phone, one thing at a time: the tab bar picks a full-screen view, and a chat is reached by
  // picking it in Sessions; the chat then takes the whole screen, with a back arrow.
  // Live data: the board, agents, models and the owner come from live.svelte.ts; where the page is
  // lives in app.svelte.ts (and the address).
  import { onMount, tick } from 'svelte';
  import Files from './views/Files.svelte';
  import Tasks from './views/Tasks.svelte';
  import Agents from './views/Agents.svelte';
  import Settings from './views/Settings.svelte';
  import Routines from './views/Routines.svelte';
  import Home from './views/Home.svelte';
  import Preview from './views/Preview.svelte';
  import Chat from './Chat.svelte';
  import PluginFrame from './PluginFrame.svelte';
  import { failing, load as loadRoutines } from './views/routinestore.svelte.ts';
  import { app, start, pickTab, openTab, openSession, closeChat, showFile, split as isSplit, tabOn, notice, typing } from './app.svelte.ts';
  import { me, board, agents, agentOf, kids, loadKid, loadMe, loadBoard, loadArchived, loadAgents, loadModels, waitingCount, findSession, type Session } from './live.svelte.ts';
  import { findView, games, loadPlugins, panel, plugins, views as pluginViews } from './plugins.svelte.ts';
  import { render as mdRender } from './md.ts';
  import { zen } from './api.ts';

  let theme = $state<'auto' | 'light' | 'dark'>((['auto', 'light', 'dark'].includes(localStorage.getItem('zen.theme') ?? '') ? localStorage.getItem('zen.theme') : 'auto') as 'auto' | 'light' | 'dark');
  let agentMenu = $state(false);
  let ring = $state(false);
  let hover = $state('');
  let showArchived = $state(false);
  let archivedShown = $state(30);
  let sq = $state('');

  // The right panel's share of the width, dragged on the gutter and kept across visits; double-click
  // or Enter resets it.
  const SIDE_KEY = 'zen.side';
  const clamp = (f: number) => Math.min(0.75, Math.max(0.25, f));
  let side = $state(clamp(Number(localStorage.getItem(SIDE_KEY)) || 0.45));
  let stage: HTMLElement | undefined = $state();
  let dragging = $state(false);
  const keepSide = () => localStorage.setItem(SIDE_KEY, String(side));
  function grab(e: PointerEvent) {
    if (!stage || e.button !== 0) return;
    e.preventDefault();
    const bar = e.currentTarget as HTMLElement;
    bar.setPointerCapture(e.pointerId);
    dragging = true;
    const box = stage.getBoundingClientRect();
    const move = (m: PointerEvent) => (side = clamp((box.right - 14 - m.clientX) / (box.width - 28)));
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
    else if (e.key === 'Home' || e.key === 'Enter') side = 0.45;
    else return;
    e.preventDefault();
    keepSide();
  }

  /** The agent picked at the top: a new chat starts with it. */
  const agent = $derived(agentOf(app.agent));

  const TABS = [
    { key: 'sessions', label: 'Sessions' }, { key: 'files', label: 'Files' }, { key: 'tasks', label: 'Tasks' }, { key: 'routines', label: 'Routines' },
    { key: 'agents', label: 'Agents' }, { key: 'games', label: 'Games' },
  ];
  // Read and Write aren't tabs: zenread and zenwrite open a note from Files (their openers).
  const waiting = $derived(waitingCount());

  // Line icons, 24×24, stroked with currentColor.
  const ICONS: Record<string, string> = {
    chat: '<path d="M4 5h16v11H9l-5 4z"/>',
    home: '<path d="M4 11l8-6 8 6v8a1 1 0 0 1-1 1h-4v-5h-6v5H5a1 1 0 0 1-1-1z"/>',
    sessions: '<rect x="4" y="4" width="16" height="6" rx="2"/><rect x="4" y="14" width="16" height="6" rx="2"/>',
    files: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
    tasks: '<path d="M9 11l2 2 4-4"/><rect x="4" y="4" width="16" height="16" rx="3"/>',
    agents: '<circle cx="9" cy="9" r="3"/><circle cx="17" cy="10" r="2.2"/><path d="M3.5 19a5.5 5.5 0 0 1 11 0M14.5 19a3.5 3.5 0 0 1 6 0"/>',
    routines: '<circle cx="12" cy="12" r="8"/><path d="M12 8v4l2.5 2.5"/><path d="M4 4l2 2M20 4l-2 2"/>',
    games: '<rect x="3" y="7" width="18" height="11" rx="4"/><path d="M8 11v3M6.5 12.5h3"/><circle cx="15.5" cy="12" r=".6"/><circle cx="17.5" cy="14" r=".6"/>',
    read: '<path d="M4 5.5A1.5 1.5 0 0 1 5.5 4H11v16H5.5A1.5 1.5 0 0 1 4 18.5zM20 5.5A1.5 1.5 0 0 0 18.5 4H13v16h5.5a1.5 1.5 0 0 0 1.5-1.5z"/>',
    write: '<path d="M4 20h4L19 9l-4-4L4 16z"/><path d="M13.5 6.5l4 4"/>',
    settings: '<path d="M4 7h9M17 7h3M4 17h3M11 17h9"/><circle cx="15" cy="7" r="2"/><circle cx="9" cy="17" r="2"/>',
    plus: '<path d="M12 5v14M5 12h14"/>',
    task: '<path d="M9 11l2 2 4-4"/><rect x="4" y="4" width="16" height="16" rx="3"/><path d="M18 2v4M16 4h4"/>',
    sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M2 12h2M20 12h2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>',
    search: '<circle cx="11" cy="11" r="6"/><path d="M20 20l-4.5-4.5"/>',
    split: '<rect x="3" y="5" width="18" height="14" rx="3"/><path d="M12 5v14"/>',
    focus: '<rect x="3" y="5" width="18" height="14" rx="3"/>',
    theme: '<path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z"/>',
    clip: '<path d="M20 11.5l-8 8a5 5 0 0 1-7-7l8.5-8.5a3.3 3.3 0 0 1 4.7 4.7L9.7 17.2a1.7 1.7 0 0 1-2.4-2.4l7.7-7.7"/>',
    stop: '<rect x="6" y="6" width="12" height="12" rx="2"/>',
    ring: '<circle cx="12" cy="12" r="2.5"/><circle cx="12" cy="4.5" r="1.3"/><circle cx="19.5" cy="12" r="1.3"/><circle cx="12" cy="19.5" r="1.3"/><circle cx="4.5" cy="12" r="1.3"/>',
    preview: '<rect x="3" y="4" width="18" height="13" rx="2"/><path d="M8 20h8M12 17v3"/>',
    plugin: '<rect x="4" y="4" width="7" height="7" rx="2"/><rect x="13" y="4" width="7" height="7" rx="2"/><rect x="4" y="13" width="7" height="7" rx="2"/><rect x="13" y="13" width="7" height="7" rx="2"/>',
  };

  const split = $derived(isSplit());
  const showView = $derived(app.screen === 'view' || split);
  /** The tabs: Home, the built-ins, each enabled plugin's views; beside a chat, Preview and a plugin command's panel too. */
  const tabs = $derived([
    { key: 'home', label: 'Home', icon: 'home' },
    ...TABS.map((t) => ({ ...t, icon: t.key })),
    ...pluginViews().map((v) => ({ key: v.key, label: v.title, icon: 'plugin' })),
    ...(app.screen === 'chat' && !app.narrow ? [{ key: 'preview', label: 'Preview', icon: 'preview' }] : []),
    ...(app.screen === 'chat' && !app.narrow && panel.current ? [{ key: 'panel', label: panel.current.title, icon: 'plugin' }] : []),
  ]);

  // The theme as shown: auto follows the system until the ring's toggle picks light or dark.
  let sysDark = $state(matchMedia('(prefers-color-scheme: dark)').matches);
  const dark = $derived(theme === 'dark' || (theme === 'auto' && sysDark));

  /** The quick-control ring's actions, fanned out on a quarter circle up and to the left. */
  const actions = $derived([
    { key: 'plus', label: 'New chat', run: () => newChat() },
    { key: 'search', label: 'Search files', run: () => openAndFocus('files', '.fsearch input') },
    { key: 'clip', label: 'Attach a file', run: () => attach() },
    { key: 'task', label: 'Add a task', run: () => openAndFocus('tasks', '.add input') },
    { key: dark ? 'sun' : 'theme', label: dark ? 'Light mode' : 'Dark mode', run: () => (theme = dark ? 'light' : 'dark') },
    { key: 'settings', label: 'Settings', run: () => openTab('settings') },
  ]);
  function openAndFocus(tab: string, sel: string) {
    openTab(tab);
    tick().then(() => setTimeout(() => (document.querySelector(sel) as HTMLElement | null)?.focus(), 30));
  }
  /** Attach: files go to the open chat's message, else a new chat starts with them. */
  function attach() {
    if (app.screen === 'chat') (document.querySelector('.dock input[type=file]') as HTMLInputElement | null)?.click();
    else {
      app.attachOnOpen = true;
      openSession('new', app.agent);
    }
  }
  const R = 156;
  const at = (i: number, n: number) => {
    const a = Math.PI + (i * (Math.PI / 2)) / (n - 1);
    return `translate(${Math.cos(a) * R}px, ${Math.sin(a) * R}px)`;
  };

  function newChat() {
    openSession('new', app.agent);
  }
  function pick(s: Session) {
    openSession(s.id);
  }
  /** From Home: a widget's row opens its chat. */
  function openById(id: string) {
    openSession(id);
  }
  /** Files' "Ask zenbot": beside a chat, the note goes into its message box; else a new chat starts with it. */
  function askAbout(path: string) {
    const text = `About \`${path}\`: `;
    if (app.screen === 'chat' && !app.narrow && app.session) {
      app.chatInsert = { text, n: app.chatInsert.n + 1 };
      return;
    }
    app.chatDraft = text;
    openSession('new', app.agent);
  }
  /** Agents' and Routines' asks ("Ask zenbot to change it", "New agent"): a new chat with the request started, always with zenbot. */
  function askAgent(text: string) {
    app.chatDraft = text;
    openSession('new', 'zenbot');
  }
  /** Tasks' "Hand to <agent>" made (or found) its session: open that chat, Tasks stays beside it. */
  function openTaskChat(id: string) {
    openSession(id);
  }

  // ---- the subagent panel ----
  const kid = $derived(app.subagent ? kids[app.subagent] : null);
  const kidParent = $derived(app.subagent ? Object.entries(board.children).find(([, l]) => l.some((c) => c.id === app.subagent))?.[0] : null);
  const kidAgent = $derived(agentOf(findSession(kidParent ?? '')?.agent ?? app.agent));
  $effect(() => {
    const id = app.subagent;
    if (!id || app.view !== 'subagent') return;
    void loadKid(id);
    const t = setInterval(() => { if (!kids[id]?.done) void loadKid(id); }, 3000);
    return () => clearInterval(t);
  });
  function closeSub() {
    if (app.narrow || app.screen === 'view') { app.screen = 'chat'; app.view = null; } else app.view = null;
  }
  async function stopSub(id: string) {
    try {
      await fetch(`/api/zen/sessions/${encodeURIComponent(id)}/stop`, { method: 'POST', headers: { 'x-zen-web': '1', 'content-type': 'application/json' }, body: '{}' }).then((r) => { if (!r.ok) throw new Error(`HTTP ${r.status}`); });
      notice('Stopped the subagent');
      void loadKid(id);
    } catch (e) {
      notice(`Couldn't stop it: ${(e as Error).message}`);
    }
  }
  function markdown(node: HTMLElement, text: string) {
    const draw = (t: string) => node.replaceChildren(mdRender(t).el);
    draw(text);
    return { update: draw };
  }

  // ---- Games: each enabled plugin's games; Play opens one here ----
  const GAME_KEY = 'zen.games.last';
  let playing = $state<string | null>(null);
  const game = $derived(games().find((g) => g.key === playing) ?? null);
  function play(key: string) {
    playing = key;
    localStorage.setItem(GAME_KEY, key);
  }

  // ---- sessions, filtered by the search field ----
  const match = (s: Session) => !sq.trim() || s.title.toLowerCase().includes(sq.trim().toLowerCase());
  $effect(() => { if (showArchived) void loadArchived(); });

  function key(e: KeyboardEvent) {
    if (e.key === 'Escape') { ring = false; agentMenu = false; }
    // Hold-free shortcut for the ring, as the mouse button would be on a Logitech mouse.
    if (e.key === '.' && (e.metaKey || e.ctrlKey)) { e.preventDefault(); ring = !ring; return; }
    if (e.defaultPrevented || e.isComposing || typing(e) || e.ctrlKey || e.metaKey || e.altKey || app.screen === 'chat') return;
    // Away from a chat, 1–9 pick a tab.
    const keys = tabs.map((t) => t.key);
    const i = /^[1-9]$/.test(e.key) ? Number(e.key) - 1 : -1;
    if (i >= 0 && i < keys.length) {
      e.preventDefault();
      openTab(keys[i]);
    }
  }

  onMount(() => {
    void start();
    void loadPlugins();
    void loadMe();
    void loadAgents();
    void loadModels();
    void loadRoutines();
    void loadBoard();
    const poll = setInterval(() => void loadBoard(), 4000);
    const mq = matchMedia('(max-width: 900px)');
    const fit = () => (app.narrow = mq.matches);
    fit();
    mq.addEventListener('change', fit);
    const sys = matchMedia('(prefers-color-scheme: dark)');
    const sysFit = () => (sysDark = sys.matches);
    sys.addEventListener('change', sysFit);
    // Settings › Appearance changes the theme through this event.
    const sync = (e: Event) => (theme = (e as CustomEvent).detail);
    addEventListener('zen-theme', sync);
    const last = localStorage.getItem(GAME_KEY);
    if (last) playing = last;
    return () => {
      clearInterval(poll);
      mq.removeEventListener('change', fit);
      sys.removeEventListener('change', sysFit);
      removeEventListener('zen-theme', sync);
    };
  });

  $effect(() => {
    document.title = `${waiting ? `(${waiting}) ` : ''}zen`;
  });

  $effect(() => {
    const r = document.documentElement;
    if (theme === 'auto') r.removeAttribute('data-theme');
    else r.setAttribute('data-theme', theme);
    localStorage.setItem('zen.theme', theme);
  });
</script>

<svelte:window onkeydown={key} />

{#snippet ico(name: string)}
  <svg class="ico" viewBox="0 0 24 24" aria-hidden="true">{@html ICONS[name] ?? ICONS.plugin}</svg>
{/snippet}

{#snippet vclose()}
  {#if split}<button class="icon vclose" title="Close the panel" aria-label="Close the panel" onclick={() => (app.view = null)}>×</button>{/if}
{/snippet}

{#snippet row(s: Session)}
  <button class="srow" class:on={app.screen === 'chat' && app.session === s.id} onclick={() => pick(s)}>
    <span class="t">{s.title}</span>
    {#if s.state === 'working'}<span class="tag work"><span class="dot work"></span>working</span>
    {:else if s.state === 'waiting'}<span class="tag wait"><span class="dot wait"></span>needs you</span>
    {:else}<span class="when">{s.when}</span>{/if}
  </button>
{/snippet}

<!-- top: the agent, the tabs -->
<div class="top" class:hide={app.narrow && app.screen === 'chat'}>
  <div class="agent-wrap">
    <button class="agent" onclick={() => (agentMenu = !agentMenu)} aria-expanded={agentMenu} title="Switch agent">
      <span class="orb" class:busy={app.working} style="--a:{agent.a}; --b:{agent.b}">{agent.name[0].toUpperCase()}</span>
      <span class="aname">{agent.name}<span class="chev">▾</span></span>
    </button>
    {#if agentMenu}
      <div class="menu amenu">
        <div class="mlabel">Agents</div>
        {#each agents.list as a}
          <button class="arow" class:sel={a.id === agent.id} onclick={() => { app.agent = a.id; agentMenu = false; }}>
            <span class="orb sm" style="--a:{a.a}; --b:{a.b}">{a.name[0].toUpperCase()}</span>
            <span class="ameta"><b>{a.name}</b><span>{a.role}</span></span>
            {#if a.id === agent.id}<span class="tick">✓</span>{/if}
          </button>
        {/each}
        <hr />
        <div class="me"><span class="av">{(me.name || '·')[0]?.toUpperCase()}</span><span class="name">{me.name || 'You'}</span>
          <button class="icon" title="Settings" onclick={() => { openTab('settings'); agentMenu = false; }}>{@render ico('settings')}</button>
        </div>
      </div>
    {/if}
  </div>

  <nav class="tabs" aria-label="Views">
    {#each tabs as t (t.key)}
      <button class="tab" class:on={tabOn(t.key)} onclick={() => pickTab(t.key)} title={split && app.view === t.key ? 'Close the panel' : t.label}>
        {@render ico(t.icon)}<span class="tl">{t.label}</span>
        {#if t.key === 'sessions' && waiting}<span class="badge">{waiting}</span>{/if}
        {#if t.key === 'routines' && failing().length}<span class="badge bad" title="{failing().length} routine failed">{failing().length}</span>{/if}
      </button>
    {/each}
  </nav>
</div>

<div class="stage" class:split class:dragging bind:this={stage} style={split ? `grid-template-columns: minmax(0, ${1 - side}fr) 14px minmax(0, ${side}fr)` : undefined}>
  <!-- left: Home, or the chat -->
  {#if app.screen === 'home'}
    <section class="pane"><Home narrow={app.narrow} onopen={openById} ontab={(t) => openTab(t)} /></section>
  {/if}

  {#if app.screen === 'chat' && app.session}
    <section class="pane chat">
      {#key app.session}
        <Chat id={app.session} {split} />
      {/key}
    </section>
  {/if}

  {#if split}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
    <div class="gutter" role="separator" aria-orientation="vertical" aria-label="Resize the panels"
      aria-valuenow={Math.round((1 - side) * 100)} aria-valuemin={25} aria-valuemax={75} tabindex="0"
      onpointerdown={grab} onkeydown={nudge} ondblclick={() => { side = 0.45; keepSide(); }}><span></span></div>
  {/if}

  <!-- right: the view picked in the tab bar -->
  {#if showView}
    <section class="pane view">
      {#if app.view === 'sessions'}
        <header class="vh"><h2>Sessions</h2><span class="sp"></span>
          <button class="ghost" onclick={newChat}>{@render ico('plus')} New</button>{@render vclose()}</header>
        <div class="vsearch">{@render ico('search')}<input placeholder="Search sessions" bind:value={sq} aria-label="Search sessions" /></div>
        <div class="vscroll">
          {#if board.channels.length}
            <div class="grp">Channels <span class="cnt">{board.channels.length}</span></div>
            {#each board.channels.filter(match) as s (s.id)}{@render row(s)}{/each}
          {/if}
          <div class="grp">Sessions <span class="cnt">{board.sessions.length}</span></div>
          {#each board.sessions.filter(match) as s (s.id)}{@render row(s)}{/each}
          {#if board.loaded && !board.sessions.length}<div class="empty">No open sessions. <button class="more" onclick={newChat}>Start one</button></div>{/if}
          {#if board.error}<div class="empty">Couldn't reach zenbot: {board.error}</div>{/if}
          <button class="grp toggle" onclick={() => (showArchived = !showArchived)} aria-expanded={showArchived}>
            <span class="car">{showArchived ? '▾' : '▸'}</span> Archived <span class="cnt">{board.archivedCount.toLocaleString()}</span>
          </button>
          {#if showArchived}
            <div class="arch">{#each board.archived.filter(match).slice(0, archivedShown) as s (s.id)}{@render row(s)}{/each}
              {#if board.archived.filter(match).length > archivedShown}<button class="more" onclick={() => (archivedShown += 100)}>Show all archived…</button>{/if}</div>
          {/if}
        </div>
      {:else if app.view === 'subagent'}
        <header class="vh">
          {#if app.narrow}<button class="icon back" title="Back to the chat" aria-label="Back to the chat" onclick={closeSub}>‹</button>{/if}
          <span class="orb xs hdot" class:busy={kid && !kid.done} style="--a:{kidAgent.a}; --b:{kidAgent.b}"></span>
          <div class="subh"><h2>{kid?.name ?? 'Subagent'}</h2>{#if kid}<span>{kid.role} subagent of {kidAgent.name} · {kid.model}</span>{/if}</div>
          <span class="sp"></span>
          {#if !app.narrow}<button class="icon" title="Close the panel" aria-label="Close the panel" onclick={closeSub}>×</button>{/if}
        </header>
        <div class="vscroll subv">
          {#if !kid}
            <div class="empty">Opening…</div>
          {:else}
            {#if kid.task}<div class="sbrief"><span class="k">Asked by {kidAgent.name}</span>{kid.task}</div>{/if}
            <ol class="slist">
              {#each kid.steps as st, q}
                {@const now = !kid.done && q === kid.steps.length - 1}
                <li class:now class:bad={st.failed && !now}><span class="g">{#if now}<i class="pulse"></i>{:else}{st.failed ? '!' : '✓'}{/if}</span><span class="lt">{st.label}{" "}<span class="ld">{st.detail}</span></span><span class="st">{st.t}</span></li>
              {/each}
            </ol>
            {#if kid.done}
              <div class="sres"><span class="k">Result · {kid.t}</span><div class="sresmd" use:markdown={kid.result || 'No result.'}></div></div>
            {:else}
              <div class="swork"><span class="orb xs busy" style="--a:{kidAgent.a}; --b:{kidAgent.b}"></span>Still working · {kid.t}<span class="sp"></span><button class="ghost" onclick={() => stopSub(kid.id)}>{@render ico('stop')} Stop</button></div>
            {/if}
          {/if}
        </div>
      {:else if app.view === 'files'}
        <header class="vh"><h2>Files</h2><span class="sp"></span>{@render vclose()}</header>
        <Files ask={askAbout} reveal={app.reveal} back={app.filesFrom ? { label: 'Settings', run: () => pickTab('settings') } : null} />
      {:else if app.view === 'tasks'}
        <header class="vh"><h2>Tasks</h2><span class="sp"></span>{@render vclose()}</header>
        <Tasks open={openTaskChat} agent={agent.name} />
      {:else if app.view === 'agents'}
        <header class="vh"><h2>Agents</h2><span class="sp"></span>{@render vclose()}</header>
        <Agents agents={agents.list} current={agent} pick={(a) => (app.agent = a.id)} ask={askAgent} />
      {:else if app.view === 'games'}
        {#if game}
          <header class="vh"><button class="icon back" title="Back to Games" aria-label="Back to Games" onclick={() => (playing = null)}>‹</button><h2>{game.title}</h2><span class="sp"></span>{@render vclose()}</header>
          <div class="frame">{#key game.key}<PluginFrame plugin={game.plugin} ui={game.ui} pane={app.screen === 'chat' ? 'side' : 'home'} slot="game" title={game.title} />{/key}</div>
        {:else}
          <header class="vh"><h2>Games</h2><span class="sp"></span>{@render vclose()}</header>
          <div class="vscroll cards">
            {#each games() as g (g.key)}
              <div class="card game"><span class="orb" style="--a:#a8762c; --b:#cfa660">{g.title[0]?.toUpperCase()}</span><span class="ameta"><b>{g.title}</b><span>{g.description ?? plugins.list.find((p) => p.name === g.plugin)?.title ?? g.plugin}</span></span>
                <button class="play" title="Play {g.title}" aria-label="Play {g.title}" onclick={() => play(g.key)}><svg viewBox="0 0 24 24"><path d="M8 5.5v13l11-6.5z"/></svg>Play</button></div>
            {/each}
            {#if plugins.loaded && !games().length}<div class="empty">No games yet. <button class="more" onclick={() => { app.settings = 'marketplace'; openTab('settings'); }}>Find one in the marketplace</button></div>{/if}
          </div>
        {/if}
      {:else if app.view === 'routines'}
        <header class="vh"><h2>Routines</h2><span class="sp"></span>{@render vclose()}</header>
        <Routines ask={askAgent} openRun={(s) => openSession(s)} />
      {:else if app.view === 'settings'}
        <header class="vh"><h2>Settings</h2><span class="sp"></span>{@render vclose()}</header>
        <Settings {theme} setTheme={(t) => (theme = t)} ask={askAgent} openFile={showFile} />
      {:else if app.view === 'preview'}
        <header class="vh"><h2>Preview</h2><span class="sp"></span>{@render vclose()}</header>
        <div class="frame"><Preview /></div>
      {:else if app.view === 'panel' && panel.current}
        <header class="vh"><h2>{panel.current.title}</h2><span class="sp"></span>{@render vclose()}</header>
        <div class="frame">{#key panel.current.n}<PluginFrame plugin={panel.current.plugin} ui={panel.current.ui} pane={app.screen === 'chat' ? 'side' : 'home'} slot="view" title={panel.current.title} input={panel.current.input} result={panel.current.result} />{/key}</div>
      {:else if app.view && findView(app.view)}
        {@const pv = findView(app.view)!}
        <header class="vh"><h2>{pv.title}</h2><span class="sp"></span>{@render vclose()}</header>
        <div class="frame">{#key pv.key}<PluginFrame plugin={pv.plugin} ui={pv.ui} pane={app.screen === 'chat' ? 'side' : 'home'} slot="view" title={pv.title} />{/key}</div>
      {:else}
        <header class="vh"><h2>{app.view ? app.view.replace(/^plugin:/, '') : ''}</h2><span class="sp"></span>{@render vclose()}</header>
        <div class="empty">This view isn't available: its plugin is off or removed.</div>
      {/if}
    </section>
  {/if}
</div>

<!-- bottom right: the quick-control ring -->
{#if ring}<button class="scrim" aria-label="Close" onclick={() => (ring = false)}></button>{/if}
<div class="qc" class:open={ring} class:hide={app.narrow && app.screen === 'chat'}>
  {#each actions as a, i}
    <button class="qa" style="transform: {ring ? at(i, actions.length) : 'translate(0,0)'}; transition-delay: {ring ? i * 18 : 0}ms"
      tabindex={ring ? 0 : -1} title={a.label} aria-label={a.label}
      onmouseenter={() => (hover = a.label)} onmouseleave={() => (hover = '')} onfocus={() => (hover = a.label)}
      onclick={() => { a.run(); ring = false; hover = ''; }}>{@render ico(a.key)}</button>
  {/each}
  {#if ring && hover}<span class="qlabel">{hover}</span>{/if}
  <button class="qbtn" onclick={() => (ring = !ring)} aria-expanded={ring} title="Quick actions (Ctrl+.)" aria-label="Quick actions">{@render ico('ring')}</button>
</div>

{#if app.notice}
  <div class="toast" role="status">{app.notice}{#if app.undo}<button onclick={() => { app.undo?.(); notice(''); }}>Undo</button>{/if}</div>
{/if}

<style>
  :global(html), :global(body) { background: var(--bg); }
  :global(body) { font-size: 15px; overflow: hidden; }

  .ico { width: 18px; height: 18px; fill: none; stroke: currentColor; stroke-width: 1.6; stroke-linecap: round; stroke-linejoin: round; flex: none; }
  .sp { flex: 1; }
  .icon { width: 32px; height: 32px; border-radius: 9px; display: grid; place-items: center; color: var(--dim); font-size: 16px; }
  .icon:hover { background: var(--soft); color: var(--ink); }

  /* the agent orb: a soft two-tone sphere; it breathes while the agent works */
  .orb { width: 38px; height: 38px; border-radius: 50%; display: grid; place-items: center; flex: none; font-weight: 700; font-size: 16px; color: #fbfaf6;
    background: radial-gradient(circle at 32% 28%, var(--b), var(--a) 70%); box-shadow: 0 0 0 3px color-mix(in srgb, var(--a) 18%, transparent); }
  .orb.sm { width: 30px; height: 30px; font-size: 13px; }
  .orb.xs { width: 14px; height: 14px; box-shadow: none; }
  .orb.xs.hdot { width: 10px; height: 10px; margin-right: 2px; }
  .orb.busy { animation: breathe 1.8s ease-in-out infinite; }
  @keyframes breathe { 50% { box-shadow: 0 0 0 7px color-mix(in srgb, var(--a) 8%, transparent); transform: scale(0.96); } }

  /* top bar */
  .top { position: fixed; inset: 0 0 auto 0; height: 64px; display: flex; align-items: center; justify-content: center; z-index: 8; pointer-events: none; }
  .top > * { pointer-events: auto; }
  .agent-wrap { position: absolute; left: 14px; top: 12px; }
  .agent { display: flex; align-items: center; gap: 10px; padding: 2px 12px 2px 2px; border-radius: 99px; }
  .agent:hover { background: var(--glass); }
  .aname { font-weight: 650; font-size: 15px; display: flex; align-items: center; gap: 4px; }
  .chev { font-size: 10px; color: var(--dim); }

  .tabs { display: flex; align-items: center; gap: 2px; padding: 5px; border-radius: 99px; background: var(--glass); backdrop-filter: blur(14px);
    border: 1px solid var(--faint); box-shadow: 0 6px 24px var(--shadow); }
  .tab { position: relative; display: flex; align-items: center; gap: 7px; padding: 7px 14px; border-radius: 99px; font-size: 14px; color: var(--dim); white-space: nowrap; }
  .tab:hover { color: var(--ink); background: var(--soft); }
  .tab.on { color: var(--on-accent); background: var(--accent); }
  .badge { min-width: 17px; height: 17px; padding: 0 5px; border-radius: 99px; font-size: 11px; font-weight: 700; display: grid; place-items: center; background: var(--warn); color: var(--on-accent); }
  .badge.bad { background: var(--bad, #b5543c); }

  /* the panels */
  .stage { height: 100dvh; padding: 64px 14px 14px; display: grid; grid-template-columns: minmax(0, 1fr); gap: 14px; }
  .stage.split { gap: 0; }
  .stage.dragging { cursor: col-resize; user-select: none; }
  .gutter { display: grid; place-items: center; cursor: col-resize; outline: none; touch-action: none; }
  .gutter span { width: 4px; height: 44px; border-radius: 99px; background: var(--faint); transition: background 0.15s, height 0.15s; }
  .gutter:hover span, .gutter:focus-visible span, .dragging .gutter span { background: var(--accent); height: 72px; }
  .pane { background: var(--paper); border: 1px solid var(--faint); border-radius: 22px; min-height: 0; display: flex; flex-direction: column; overflow: hidden; box-shadow: 0 1px 2px var(--shadow); }
  .frame { flex: 1; min-height: 0; display: flex; flex-direction: column; }

  .lt { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .ld { color: var(--dim); }
  .subh { display: flex; flex-direction: column; min-width: 0; }
  .subh span { font-size: 12.5px; color: var(--dim); }
  .subv { padding: 4px 16px 20px 22px; }
  .subv .slist { padding: 0; margin: 10px 0; }
  .subv .slist li { font-size: 14px; padding: 5px 0; }
  .sbrief, .sres { border: 1px solid var(--faint); border-radius: 12px; padding: 10px 12px; font-size: 14px; line-height: 1.5; background: var(--paper); white-space: pre-wrap; overflow-wrap: anywhere; }
  .sres { border-color: color-mix(in srgb, var(--accent) 35%, var(--faint)); white-space: normal; }
  .sresmd :global(p) { margin: 0 0 8px; }
  .sresmd :global(p:last-child) { margin-bottom: 0; }
  .sresmd :global(pre) { white-space: pre-wrap; font-family: var(--mono); font-size: 12.5px; }
  .sbrief .k, .sres .k { display: block; font-size: 11.5px; color: var(--dim); text-transform: uppercase; letter-spacing: 0.04em; margin-bottom: 2px; }
  .swork { display: flex; align-items: center; gap: 10px; font-size: 13.5px; color: var(--dim); padding: 6px 0; }
  .swork .sp { flex: 1; }
  .g { width: 14px; flex: none; text-align: center; font-size: 11px; font-weight: 700; color: var(--accent); }
  .pulse { display: inline-block; width: 7px; height: 7px; border-radius: 50%; background: var(--accent); animation: pulse 1.4s ease-in-out infinite; }
  @keyframes pulse { 50% { opacity: 0.3; transform: scale(0.75); } }
  .slist li.now { font-weight: 550; }
  .slist { list-style: none; margin: 4px 0; padding: 0 10px; }
  .slist li { display: flex; align-items: center; gap: 10px; padding: 3px 0; font-size: 13px; animation: rise 0.25s both; }
  .slist li.bad .g, .slist li.bad .ld { color: var(--err); }
  .st { font-size: 11.5px; color: var(--dim); font-variant-numeric: tabular-nums; }
  .car { font-size: 10px; opacity: 0.7; }
  @keyframes rise { from { opacity: 0; transform: translateY(6px); } }

  /* menus */
  .menu { position: absolute; z-index: 12; min-width: 240px; background: var(--paper); border: 1px solid var(--faint); border-radius: 14px; padding: 6px; box-shadow: 0 14px 40px var(--shadow); display: flex; flex-direction: column; }
  .amenu { left: 0; top: 50px; min-width: 260px; }
  .mlabel { font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); font-weight: 600; padding: 6px 10px 4px; }
  .vclose { font-size: 20px; }
  .toast { position: fixed; left: 50%; bottom: 28px; transform: translateX(-50%); z-index: 30; display: flex; align-items: center; gap: 14px; padding: 10px 16px; border-radius: 12px; background: var(--ink); color: var(--paper); font-size: 14px; box-shadow: 0 10px 30px var(--shadow); animation: tin 0.2s both; max-width: calc(100vw - 32px); }
  @keyframes tin { from { opacity: 0; transform: translate(-50%, 6px); } }
  .toast button { color: var(--accent); font-weight: 650; filter: brightness(1.6); }
  .menu hr { border: 0; border-top: 1px solid var(--faint); margin: 6px 0; width: 100%; }
  .menu > button { text-align: left; padding: 7px 10px; border-radius: 8px; font-size: 14px; }
  .menu > button:hover { background: var(--soft); }
  .arow { display: flex !important; align-items: center; gap: 10px; }
  .ameta { display: flex; flex-direction: column; flex: 1; text-align: left; min-width: 0; }
  .ameta b { font-weight: 600; }
  .ameta span { font-size: 12px; color: var(--dim); }
  .tick { color: var(--accent); }
  .me { display: flex; align-items: center; gap: 10px; padding: 4px 4px 2px 10px; font-size: 14px; }
  .me .name { flex: 1; font-weight: 550; }
  .av { width: 26px; height: 26px; border-radius: 50%; display: grid; place-items: center; font-size: 13px; font-weight: 650; background: var(--soft); }
  .qc.hide { display: none; }

  /* the right panel's views */
  .vh { display: flex; align-items: center; gap: 8px; padding: 16px 16px 8px 22px; }
  .vh h2 { margin: 0; font-size: 20px; font-family: var(--serif); font-weight: 600; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .ghost { display: flex; align-items: center; gap: 6px; font-size: 13px; padding: 6px 12px; border-radius: 99px; border: 1px solid var(--faint); }
  .ghost:hover { border-color: var(--dim); }
  .ghost .ico { width: 14px; height: 14px; }
  .vsearch { display: flex; align-items: center; gap: 8px; margin: 4px 16px 6px; padding: 8px 12px; border-radius: 12px; background: var(--soft); color: var(--dim); }
  .vsearch input { flex: 1; border: 0; outline: none; background: transparent; font-size: 14px; min-width: 0; }
  .vscroll { flex: 1; overflow: auto; padding: 0 10px 80px; display: flex; flex-direction: column; gap: 1px; }
  .grp { display: flex; align-items: center; gap: 6px; font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); padding: 14px 12px 5px; font-weight: 600; text-align: left; }
  .grp .cnt { font-weight: 500; letter-spacing: 0; opacity: 0.75; }
  .grp.toggle:hover { color: var(--ink); }
  .grp .car { font-size: 9px; width: 8px; }
  .arch { display: flex; flex-direction: column; gap: 1px; }
  .arch .srow { color: var(--dim); }
  .more { text-align: left; font-size: 13px; color: var(--accent); padding: 6px 12px; }
  .srow { display: flex; align-items: center; gap: 10px; text-align: left; padding: 10px 12px; border-radius: 12px; font-size: 14px; }
  .srow:hover { background: var(--soft); }
  .srow.on { background: color-mix(in srgb, var(--accent) 14%, var(--paper)); }
  .t { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .when { font-size: 12px; color: var(--dim); white-space: nowrap; }
  .cards { padding-top: 6px; gap: 8px; }
  .card { display: flex; align-items: center; gap: 12px; padding: 14px; border-radius: 16px; border: 1px solid var(--faint); text-align: left; }
  .card.game { transition: border-color 0.15s, box-shadow 0.15s; }
  .card.game:hover, .card.game:focus-within { border-color: color-mix(in srgb, var(--accent) 45%, var(--faint)); box-shadow: 0 4px 16px var(--shadow); }
  .play { display: inline-flex; align-items: center; gap: 6px; padding: 6px 14px 6px 11px; border-radius: 99px; background: var(--accent); color: var(--on-accent); font-size: 13px; font-weight: 650;
    opacity: 0; transform: scale(0.92); transition: opacity 0.15s, transform 0.15s; }
  .play svg { width: 13px; height: 13px; fill: currentColor; }
  .card.game:hover .play, .play:focus-visible { opacity: 1; transform: none; }
  @media (hover: none) { .play { opacity: 1; transform: none; } }
  .empty { padding: 24px; color: var(--dim); }
  .empty .more { padding: 0; display: inline; }

  .tag { display: inline-flex; align-items: center; gap: 5px; font-size: 11px; font-weight: 600; padding: 2px 9px 2px 7px; border-radius: 99px; white-space: nowrap; }
  .tag.work { color: var(--accent); background: color-mix(in srgb, var(--accent) 14%, transparent); }
  .tag.wait { color: var(--warn); background: color-mix(in srgb, var(--warn) 16%, transparent); }
  .dot { width: 6px; height: 6px; border-radius: 50%; flex: none; }
  .dot.work { background: var(--accent); animation: blink 1.2s ease-in-out infinite; }
  .dot.wait { background: var(--warn); }
  @keyframes blink { 50% { opacity: 0.3; } }

  /* quick-control ring */
  .scrim { position: fixed; inset: 0; z-index: 14; background: color-mix(in srgb, var(--bg) 35%, transparent); backdrop-filter: blur(2px); cursor: default; }
  .qc { position: fixed; right: 26px; bottom: 26px; width: 56px; height: 56px; z-index: 15; }
  .qbtn { position: absolute; inset: 0; border-radius: 50%; display: grid; place-items: center; color: var(--on-accent); background: radial-gradient(circle at 32% 28%, color-mix(in srgb, var(--accent) 70%, white), var(--accent) 72%);
    box-shadow: 0 8px 24px var(--shadow), 0 0 0 4px color-mix(in srgb, var(--accent) 16%, transparent); transition: transform 0.2s; }
  .qbtn .ico { width: 22px; height: 22px; }
  .qc.open .qbtn { transform: rotate(45deg); }
  .qa { position: absolute; left: 4px; top: 4px; width: 48px; height: 48px; border-radius: 50%; display: grid; place-items: center; color: var(--ink);
    background: var(--paper); border: 1px solid var(--faint); box-shadow: 0 6px 18px var(--shadow); opacity: 0; pointer-events: none;
    transition: transform 0.22s cubic-bezier(.2,.9,.3,1.2), opacity 0.15s; }
  .qc.open .qa { opacity: 1; pointer-events: auto; }
  .qa:hover, .qa:focus-visible { color: var(--on-accent); background: var(--accent); border-color: var(--accent); outline: none; }
  .qlabel { position: absolute; right: 200px; bottom: 14px; white-space: nowrap; font-size: 13px; font-weight: 600; padding: 5px 12px; border-radius: 99px; background: var(--ink); color: var(--paper); pointer-events: none; }

  /* Calm scrolling everywhere: a thin, ink-tinted bar that only darkens on hover, and content
     that fades in and out at the edges instead of being cut. */
  :global(.scroll, .vscroll, .list, .tlist, .rscroll, .fvals, .sibs) {
    scrollbar-width: thin; scrollbar-color: color-mix(in srgb, var(--ink) 18%, transparent) transparent; scroll-behavior: smooth; overscroll-behavior: contain;
  }
  :global(.scroll:hover, .vscroll:hover, .list:hover, .tlist:hover, .rscroll:hover) { scrollbar-color: color-mix(in srgb, var(--ink) 32%, transparent) transparent; }
  :global(.scroll::-webkit-scrollbar, .vscroll::-webkit-scrollbar, .list::-webkit-scrollbar, .tlist::-webkit-scrollbar, .rscroll::-webkit-scrollbar) { width: 8px; }
  :global(.scroll::-webkit-scrollbar-thumb, .vscroll::-webkit-scrollbar-thumb, .list::-webkit-scrollbar-thumb, .tlist::-webkit-scrollbar-thumb, .rscroll::-webkit-scrollbar-thumb) {
    background: color-mix(in srgb, var(--ink) 18%, transparent); border-radius: 99px; border: 2px solid transparent; background-clip: padding-box; }
  :global(.vscroll, .list, .tlist, .rscroll) {
    mask-image: linear-gradient(to bottom, transparent 0, #000 14px, #000 calc(100% - 40px), transparent 100%); }

  @media (max-width: 900px) {
    .top { height: 60px; justify-content: space-between; gap: 8px; padding: 0 8px; }
    .agent-wrap { position: static; }
    .agent { padding: 2px; }
    .aname { display: none; }
    .orb { width: 34px; height: 34px; }
    .orb.xs { width: 14px; height: 14px; }
    .orb.xs.hdot { width: 10px; height: 10px; }
    .tabs { max-width: 100%; overflow-x: auto; scrollbar-width: none; }
    .tab { padding: 7px 8px; }
    .tab .tl { display: none; }
    .tab .badge { position: absolute; top: -1px; right: -3px; min-width: 15px; height: 15px; font-size: 10px; padding: 0 4px; }
    .top.hide { display: none; }
    .stage { padding: 60px 8px 8px; }
    .top.hide + .stage { padding-top: 8px; }
    .back { font-size: 26px; margin-left: -10px; color: var(--ink); }
    .qc { right: 16px; bottom: 96px; }
  }
</style>
