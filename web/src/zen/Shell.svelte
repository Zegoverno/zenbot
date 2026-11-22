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
  // Keyboard only: everything here has a key (keys.svelte.ts holds the registry, the handler and
  // the focus). One panel is always focused (the chat on the left or the view on the right; click
  // it, Ctrl+\ switches, Ctrl+Shift+\ closes or reopens the side panel) and has a thin accent
  // outline. Each view registers its own commands (the ring, Ctrl+K and `?` show the focused
  // panel's): Sessions' and the subagent's and Games' are registered here, the rest in their views.
  // Ctrl+K command list (Palette.svelte), Ctrl+. ring (Ring.svelte), `?` shortcuts (KeyHelp.svelte),
  // `g` then h/s/f/t/r/a/m/p goes to a view, 1–9 pick a tab, `c` a new chat, `@` the agent menu
  // (arrows, Enter), Esc backs out one level.
  import { onMount, tick, untrack } from 'svelte';
  import Files from './views/Files.svelte';
  import Tasks from './views/Tasks.svelte';
  import Agents from './views/Agents.svelte';
  import Settings from './views/Settings.svelte';
  import Routines from './views/Routines.svelte';
  import Home from './views/Home.svelte';
  import Preview from './views/Preview.svelte';
  import Chat from './Chat.svelte';
  import PluginFrame from './PluginFrame.svelte';
  import { hand as handToAgent, type T as TaskT } from './views/taskstore.svelte.ts';
  import { failing, load as loadRoutines } from './views/routinestore.svelte.ts';
  import Ring from './Ring.svelte';
  import Palette from './Palette.svelte';
  import KeyHelp from './KeyHelp.svelte';
  import Icon from './Icon.svelte';
  import { ICONS } from './icons.ts';
  import { commands as registerCommands, ui, install, setPanes, focusPane, focused, toggleFocus, toggleSide, filesPanel, cursorRow, moveCursor, pressCursor, findCursor, GOTO, type PanelKind } from './keys.svelte.ts';
  import { app, start, pickTab, openTab, openSession, closeChat, showFile, split as isSplit, tabOn, notice, setAgent } from './app.svelte.ts';
  import { me, board, agents, agentOf, kids, loadKid, loadMe, loadBoard, loadArchived, loadAgents, loadModels, waitingCount, findSession, type Session } from './live.svelte.ts';
  import { chat, findView, games, loadPlugins, panel, plugins, views as pluginViews } from './plugins.svelte.ts';
  import { render as mdRender } from './md.ts';
  import { zen } from './api.ts';

  let theme = $state<'auto' | 'light' | 'dark'>((['auto', 'light', 'dark'].includes(localStorage.getItem('zen.theme') ?? '') ? localStorage.getItem('zen.theme') : 'auto') as 'auto' | 'light' | 'dark');
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

  const split = $derived(isSplit());
  const showView = $derived(app.screen === 'view' || split);
  /** The tabs: Home, the built-ins, each enabled plugin's views; beside a chat, Preview and a plugin command's panel too. */
  const tabs = $derived([
    { key: 'home', label: 'Home', icon: 'home' },
    ...TABS.map((t) => ({ ...t, icon: t.key })),
    ...pluginViews().map((v) => ({ key: v.key, label: v.title, icon: 'plugin' })),
    ...(app.session ? [{ key: 'preview', label: 'Preview', icon: 'preview' }] : []),
    ...(app.screen === 'chat' && !app.narrow && panel.current ? [{ key: 'panel', label: panel.current.title, icon: 'plugin' }] : []),
  ]);

  // The theme as shown: auto follows the system until the ring's toggle picks light or dark.
  let sysDark = $state(matchMedia('(prefers-color-scheme: dark)').matches);
  const dark = $derived(theme === 'dark' || (theme === 'auto' && sysDark));

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
  function newChat() {
    openSession('new', app.agent);
  }
  /** Opens a chat and gives it the focus (it is the left panel). */
  function openChat(id: string) {
    openSession(id);
    focusPane(0);
  }
  function pick(s: Session) {
    openChat(s.id);
  }
  /** From Home: a widget's row opens its chat. */
  function openById(id: string) {
    openChat(id);
  }
  /** Files' "Ask zenbot": beside a chat, the note goes into its message box; else a new chat starts with it. */
  function askAbout(path: string) {
    const text = `About \`${path}\`: `;
    if (app.screen === 'chat' && !app.narrow && app.session) {
      app.chatInsert = { text, n: app.chatInsert.n + 1 };
      focusPane(0);
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
    openChat(id);
  }
  /** Hand a task to the agent at the top: a chat starts on it, and opens. */
  async function handTask(t: TaskT, agentId: string) {
    const r = await handToAgent(t, agentId);
    if (r?.session) openChat(r.session);
    return r?.session ?? null;
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

  // The agent menu (`@`, or the orb): ↑↓ move over the agents, Enter picks, Esc closes.
  function menuOpen(node: HTMLElement) {
    void tick().then(() => (node.querySelector<HTMLElement>('.arow.sel') ?? node.querySelector<HTMLElement>('.arow:not(:disabled)'))?.focus());
    return { destroy() { if (node.contains(document.activeElement)) (document.activeElement as HTMLElement).blur(); } };
  }
  function menuKeys(e: KeyboardEvent) {
    const rows = [...(e.currentTarget as HTMLElement).querySelectorAll<HTMLElement>('.arow:not(:disabled)')];
    const i = rows.indexOf(document.activeElement as HTMLElement);
    const to = e.key === 'ArrowDown' ? i + 1 : e.key === 'ArrowUp' ? i - 1 : e.key === 'Home' ? 0 : e.key === 'End' ? rows.length - 1 : null;
    if (to === null || !rows.length) return;
    e.preventDefault();
    rows[(to + rows.length) % rows.length].focus();
  }

  // ---- the panels, their kinds, and the focus ----
  const viewKind = $derived.by((): PanelKind | null => {
    const v = app.view;
    if (!v) return null;
    if (v === 'files') return filesPanel.kind;
    if (v === 'panel' || v.startsWith('plugin:')) return 'plugin';
    return (['sessions', 'subagent', 'tasks', 'routines', 'agents', 'games', 'preview', 'settings'] as string[]).includes(v) ? (v as PanelKind) : 'plugin';
  });
  const kinds = $derived.by((): PanelKind[] => {
    if (app.screen === 'home') return ['home'];
    if (app.screen === 'chat') return split && viewKind ? ['chat', viewKind] : ['chat'];
    return [viewKind ?? 'home'];
  });
  $effect(() => setPanes(kinds));
  $effect(() => { if (split && app.view) ui.side = app.view; });
  /** The panel's index on screen: the chat or Home on the left; a view on the right only beside a chat. */
  const viewAt = $derived(split ? 1 : 0);
  let pointed = false;
  /** A click or Tab inside a panel focuses it (the keyboard stays where the click put it). */
  function touch(i: number) {
    if (ui.focus === i) return;
    pointed = true;
    focusPane(i);
  }
  // Keys moved the focus: put the keyboard in the panel (the chat's box; a plugin frame does its own).
  $effect(() => {
    const i = ui.focus, k = focused();
    void ui.panes.length;
    if (pointed) { pointed = false; return; }
    untrack(() => void tick().then(() => {
      const sec = document.querySelector<HTMLElement>(`[data-pane="${i}"]`);
      if (!sec || (sec.contains(document.activeElement) && document.activeElement !== sec)) return;
      if (k === 'chat') chat.focus?.();
      else if (k !== 'plugin' && k !== 'games' && k !== 'files-reader' && k !== 'files-writer') sec.focus({ preventScroll: true });
      else if (!sec.querySelector('iframe')) sec.focus({ preventScroll: true });
    }));
  });

  // ---- the global commands ----
  const letterOf = (tab: string) => Object.entries(GOTO).find(([, g]) => g[0] === tab)?.[0];
  registerCommands('global', () => [
    { id: 'new-chat', title: 'New chat', icon: 'plus', key: 'c', ring: 0, run: () => { newChat(); focusPane(0); } },
    { id: 'search-files', title: 'Search files', icon: 'search', ring: 1, run: () => openAndFocus('files', '.fsearch input') },
    { id: 'attach', title: 'Attach a file', icon: 'clip', ring: 2, run: () => attach() },
    { id: 'add-task', title: 'Add a task', icon: 'task', ring: 3, run: () => openAndFocus('tasks', '.add input') },
    { id: 'theme', title: dark ? 'Light mode' : 'Dark mode', icon: dark ? 'sun' : 'theme', ring: 4, run: () => (theme = dark ? 'light' : 'dark') },
    { id: 'settings', title: 'Settings', icon: 'settings', ring: 5, run: () => openTab('settings') },
    { id: 'agent', title: 'Switch agent', icon: 'agents', key: '@', run: () => (ui.agentMenu = true) },
    { id: 'help', title: 'Show shortcuts', icon: 'keyboard', key: '?', reserved: true, run: () => (ui.help = true) },
    { id: 'focus', title: 'Switch focus between panels', icon: 'swap', key: 'Mod+\\', reserved: true, enabled: () => ui.panes.length > 1, run: toggleFocus },
    { id: 'side', title: 'Close the side panel, or reopen it', icon: 'split', key: 'Mod+Shift+\\', reserved: true, enabled: () => app.screen === 'chat' && !app.narrow, run: toggleSide },
    ...tabs.map((t, i) => ({
      id: `go-${t.key}`, title: `Go to ${t.label}`, icon: t.icon, key: i < 9 ? String(i + 1) : undefined,
      hint: letterOf(t.key) ? `g ${letterOf(t.key)}` : undefined, run: () => (t.key === 'home' ? pickTab('home') : openTab(t.key)),
    })),
  ]);

  // ---- Sessions: the row under the cursor (j / k) ----
  let rn = $state<{ id: string; value: string } | null>(null);
  const curSession = () => { const id = cursorRow('.srow')?.dataset.id; return id ? findSession(id) : null; };
  function renameStart() {
    const s = curSession();
    if (!s) return void notice('Pick a session first (j / k)');
    rn = { id: s.id, value: s.title };
    void tick().then(() => document.querySelector<HTMLInputElement>('[data-pane] input.rename')?.select());
  }
  /** Closes the rename box; the row is a new element then, so the cursor goes back onto it. */
  async function endRename(id: string) {
    rn = null;
    await tick();
    findCursor('.srow', (r) => r.dataset.id === id);
  }
  async function renameSave() {
    const r = rn;
    if (!r) return;
    const t = r.value.trim();
    await endRename(r.id);
    if (!t || t === findSession(r.id)?.title) return;
    try { await zen.update(r.id, { title: t }); await loadBoard(); await tick(); findCursor('.srow', (x) => x.dataset.id === r.id); } catch (e) { notice(String((e as Error).message ?? e)); }
  }
  async function archiveRow() {
    const s = curSession();
    if (!s) return void notice('Pick a session first (j / k)');
    try {
      await zen.update(s.id, { archived: true });
      await loadBoard();
      if (showArchived) void loadArchived();
      notice(`Archived “${s.title}”`, 6000, () => { void zen.update(s.id, { archived: false }).then(() => loadBoard()).catch((e) => notice(String((e as Error).message ?? e))); });
    } catch (e) { notice(String((e as Error).message ?? e)); }
  }
  const searchSessions = () => document.querySelector<HTMLInputElement>('[data-pane] .vsearch input')?.focus();
  registerCommands('sessions', () => [
    { id: 'sessions-new', title: 'New chat', icon: 'plus', key: 'n', ring: 0, run: () => { newChat(); focusPane(0); } },
    { id: 'sessions-search', title: 'Search', icon: 'search', key: '/', ring: 1, run: searchSessions },
    { id: 'sessions-waiting', title: "Next 'needs you'", icon: 'bell', key: 'w', ring: 2, enabled: () => board.sessions.some((x) => x.state === 'waiting'), run: () => void findCursor('.srow', (r) => r.dataset.state === 'waiting') },
    { id: 'sessions-rename', title: 'Rename', icon: 'rename', key: 'r', ring: 3, run: renameStart },
    { id: 'sessions-archive', title: 'Archive', icon: 'archive', key: 'x', ring: 4, run: archiveRow },
    { id: 'sessions-archived', title: showArchived ? 'Hide archived' : 'Show archived', icon: 'eye', key: 'u', ring: 5, run: () => (showArchived = !showArchived) },
    { id: 'sessions-down', title: 'Next session', icon: 'down', key: 'j', alt: ['ArrowDown'], quiet: true, run: () => void moveCursor('.srow', 1) },
    { id: 'sessions-up', title: 'Previous session', icon: 'up', key: 'k', alt: ['ArrowUp'], quiet: true, run: () => void moveCursor('.srow', -1) },
    { id: 'sessions-open', title: 'Open the session', icon: 'chat', key: 'Enter', quiet: true, run: () => { const s = curSession(); if (s) openChat(s.id); else moveCursor('.srow', 'first'); } },
    { id: 'sessions-close', title: 'Close the panel', icon: 'close', key: 'Escape', quiet: true, enabled: () => split, run: () => pickTab(app.view!) },
  ]);
  registerCommands('subagent', () => [
    { id: 'sub-stop', title: 'Stop', icon: 'stop', ring: 0, enabled: () => !!kid && !kid.done, run: () => app.subagent && void stopSub(app.subagent) },
    { id: 'sub-close', title: 'Close the panel', icon: 'close', key: 'Escape', ring: 1, run: closeSub },
  ]);
  registerCommands('games', () => (game
    ? [{ id: 'games-switch', title: 'Switch game', icon: 'games', key: 'Escape', ring: 0, run: () => (playing = null) }]
    : [
      { id: 'games-play', title: 'Play', icon: 'play', key: 'Enter', ring: 0, run: () => void (pressCursor('.card.game', '.play') || moveCursor('.card.game', 'first')) },
      { id: 'games-down', title: 'Next game', icon: 'down', key: 'j', alt: ['ArrowDown'], quiet: true, run: () => void moveCursor('.card.game', 1) },
      { id: 'games-up', title: 'Previous game', icon: 'up', key: 'k', alt: ['ArrowUp'], quiet: true, run: () => void moveCursor('.card.game', -1) },
    ]));

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
    const uninstall = install();
    return () => {
      uninstall();
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


{#snippet ico(name: string)}
  <svg class="ico" viewBox="0 0 24 24" aria-hidden="true">{@html ICONS[name] ?? ICONS.plugin}</svg>
{/snippet}

{#snippet vclose()}
  {#if app.narrow && app.screen === 'view' && app.session}<button class="icon back" title="Back to the chat" aria-label="Back to the chat" onclick={() => { app.screen = 'chat'; app.view = null; }}>‹</button>{:else if split}<button class="icon vclose" title="Close the panel" aria-label="Close the panel" onclick={() => (app.view = null)}>×</button>{/if}
{/snippet}

{#snippet row(s: Session)}
  {#if rn?.id === s.id}
    <div class="srow" data-id={s.id} data-state={s.state}>
      <!-- svelte-ignore a11y_autofocus -->
      <input class="rename" bind:value={rn.value} autofocus aria-label="Session name" onblur={renameSave}
        onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); void renameSave(); } else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); void endRename(s.id); } }} />
    </div>
  {:else}
    <button class="srow" class:on={app.screen === 'chat' && app.session === s.id} data-id={s.id} data-state={s.state} onclick={() => pick(s)}>
      <span class="t">{s.title}</span>
      {#if s.state === 'working'}<span class="tag work"><span class="dot work"></span>working</span>
      {:else if s.state === 'waiting'}<span class="tag wait"><span class="dot wait"></span>needs you</span>
      {:else}<span class="when">{s.when}</span>{/if}
    </button>
  {/if}
{/snippet}

<!-- top: the agent, the tabs -->
<div class="top" class:hide={app.narrow && app.screen === 'chat'}>
  <div class="agent-wrap">
    <button class="agent" onclick={() => (ui.agentMenu = !ui.agentMenu)} aria-expanded={ui.agentMenu} title="Switch agent (@)">
      <span class="orb" class:busy={app.working} style="--a:{agent.a}; --b:{agent.b}">{agent.name[0].toUpperCase()}</span>
      <span class="aname">{agent.name}<span class="chev">▾</span></span>
    </button>
    {#if ui.agentMenu}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div class="menu amenu" onkeydown={menuKeys} use:menuOpen>
        <div class="mlabel">Agents</div>
        {#each agents.list as a}
          <button class="arow" disabled={!!a.info.error} class:sel={a.id === agent.id} onclick={() => { setAgent(a.id); ui.agentMenu = false; }}>
            <span class="orb sm" style="--a:{a.a}; --b:{a.b}">{a.name[0].toUpperCase()}</span>
            <span class="ameta"><b>{a.name}</b><span>{a.role}</span></span>
            {#if a.id === agent.id}<span class="tick">✓</span>{/if}
          </button>
        {/each}
        <hr />
        <div class="me"><span class="av">{(me.name || '·')[0]?.toUpperCase()}</span><span class="name">{me.name || 'You'}</span>
          <button class="icon" title="Settings" onclick={() => { openTab('settings'); ui.agentMenu = false; }}>{@render ico('settings')}</button>
        </div>
      </div>
    {/if}
  </div>

  <div class="tabs" aria-label="Views" role="tablist">
    {#each tabs as t (t.key)}
      <button class="tab" role="tab" aria-label={t.label} aria-selected={tabOn(t.key)} class:on={tabOn(t.key)} onclick={() => pickTab(t.key)} title={split && app.view === t.key ? 'Close the panel' : t.label}>
        {@render ico(t.icon)}<span class="tl">{t.label}</span>
        {#if t.key === 'sessions' && waiting}<span class="badge">{waiting}</span>{/if}
        {#if t.key === 'routines' && failing().length}<span class="badge bad" title="{failing().length} routine failed">{failing().length}</span>{/if}
      </button>
    {/each}
  </div>
</div>

<div class="stage" class:split class:dragging bind:this={stage} style={split ? `grid-template-columns: minmax(0, ${1 - side}fr) 14px minmax(0, ${side}fr)` : undefined}>
  <!-- left: Home, or the chat -->
  {#if app.screen === 'home'}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions, a11y_no_static_element_interactions -->
    <section class="pane" class:focused={ui.focus === 0} data-pane="0" tabindex="-1" onpointerdown={() => touch(0)} onfocusin={() => touch(0)}><Home narrow={app.narrow} onopen={openById} ontab={(t) => openTab(t)} onfile={(p) => showFile(p, { open: true })} /></section>
  {/if}

  {#if app.session && (app.screen === 'chat' || (app.narrow && app.screen === 'view'))}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions, a11y_no_static_element_interactions -->
    <section class="pane chat" class:focused={ui.focus === 0} hidden={app.screen !== 'chat'} data-pane={app.screen === 'chat' ? 0 : undefined} tabindex="-1" onpointerdown={() => touch(0)} onfocusin={() => touch(0)}>
      {#key app.session}
        <Chat id={app.session} {split} />
      {/key}
    </section>
  {/if}

  {#if split}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions, a11y_no_static_element_interactions -->
    <div class="gutter" role="separator" aria-orientation="vertical" aria-label="Resize the panels"
      aria-valuenow={Math.round((1 - side) * 100)} aria-valuemin={25} aria-valuemax={75} tabindex="0"
      onpointerdown={grab} onkeydown={nudge} ondblclick={() => { side = 0.45; keepSide(); }}><span></span></div>
  {/if}

  <!-- right: the view picked in the tab bar -->
  {#if showView}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions, a11y_no_static_element_interactions -->
    <section class="pane view" class:focused={ui.focus === viewAt} data-pane={viewAt} tabindex="-1" onpointerdown={() => touch(viewAt)} onfocusin={() => touch(viewAt)}>
      {#if app.view === 'sessions'}
        <header class="vh"><h2>Sessions</h2><span class="sp"></span>
          <button class="ghost" onclick={newChat}>{@render ico('plus')} New</button>{@render vclose()}</header>
        <div class="vsearch">{@render ico('search')}<input placeholder="Search sessions (/)" bind:value={sq} aria-label="Search sessions"
          onkeydown={(e) => { if (e.key === 'Enter' || e.key === 'ArrowDown') { e.preventDefault(); (e.currentTarget as HTMLInputElement).blur(); moveCursor('.srow', 'first'); } else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); if (sq) sq = ''; else (e.currentTarget as HTMLInputElement).blur(); } }} /></div>
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
        <Tasks open={openTaskChat} ask={askAgent} agent={agent.id} agents={agents.list.filter((a) => !a.info.error)} hand={handTask} />
      {:else if app.view === 'agents'}
        <header class="vh"><h2>Agents</h2><span class="sp"></span>{@render vclose()}</header>
        <Agents agents={agents.list} current={agent} pick={(a) => { setAgent(a.id); openChat('new'); }} ask={askAgent} />
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

<!-- bottom right: the quick-control ring; Ctrl+K's command list and `?`'s shortcuts float above -->
<Ring />
<Palette />
<KeyHelp />
{#if ui.goto}
  <div class="gohint" role="status">g · {#each Object.entries(GOTO) as [k, g]}<span><kbd>{k}</kbd> {g[1]}</span>{/each}</div>
{/if}

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
  .ameta span { font-size: 12px; color: var(--dim); overflow: hidden; display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow-wrap: anywhere; }
  .tick { color: var(--accent); }
  .me { display: flex; align-items: center; gap: 10px; padding: 4px 4px 2px 10px; font-size: 14px; }
  .me .name { flex: 1; font-weight: 550; }
  .av { width: 26px; height: 26px; border-radius: 50%; display: grid; place-items: center; font-size: 13px; font-weight: 650; background: var(--soft); }

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

  /* the focused panel: a thin accent outline; a list's cursor row; the go-to's hint */
  .pane.focused { border-color: color-mix(in srgb, var(--accent) 70%, var(--faint)); box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent) 45%, transparent), 0 1px 2px var(--shadow); }
  .pane { outline: none; }
  :global([data-cur]) { background: color-mix(in srgb, var(--accent) 14%, var(--paper)) !important; box-shadow: inset 0 0 0 1.5px color-mix(in srgb, var(--accent) 60%, transparent); }
  .gohint { position: fixed; left: 50%; bottom: 28px; transform: translateX(-50%); z-index: 30; display: flex; gap: 12px; flex-wrap: wrap; justify-content: center; padding: 9px 16px; border-radius: 12px; background: var(--ink); color: var(--paper); font-size: 13px; max-width: calc(100vw - 32px); box-shadow: 0 10px 30px var(--shadow); }
  .gohint kbd { border-color: color-mix(in srgb, var(--paper) 40%, transparent); color: var(--paper); }
  .srow input.rename { flex: 1; min-width: 0; border: 0; outline: 0; background: transparent; font-size: 14px; font-family: inherit; }

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
  }
</style>
