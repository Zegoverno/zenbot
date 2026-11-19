<script lang="ts">
  // Chat redesign mock (v2, "two panels"): no sidebar. The chat on the left, one view on the right
  // (Sessions, Files, Tasks, Agents, Games, the plugins' views) picked from a floating tab bar at
  // the top centre; clicking the open tab again closes the right panel (focus). Top left, the agent's
  // circle: click to switch agent (and reach the user and Settings). Bottom right, a quick-control
  // ring (after Logitech's Actions Ring): one button that fans out the everyday actions.
  // Panels: Home and the views stand alone (one panel); a chat opens full width, and a tab picked
  // while in a chat opens beside it (two panels, desktop only).
  // On a phone, one thing at a time: the tab bar picks a full-screen view, and a chat is reached by
  // picking it in Sessions; the chat then takes the whole screen, with a back arrow to Sessions.
  // Fake data; nothing here talks to the kernel. The floating "mock" panel switches states.
  import { onMount } from 'svelte';
  import Home from './Home.svelte';
  import { archived, archivedCount, channels, conversation, question, sessions, suggestions, type Msg, type Session } from './data.ts';

  type State = 'new' | 'chat' | 'working' | 'waiting';
  let state = $state<State>('chat');
  let theme = $state<'auto' | 'light' | 'dark'>('auto');
  let narrow = $state(false);
  let screen = $state<'home' | 'chat' | 'view'>('home');
  let view = $state<string | null>(null);
  let back = $state<'home' | 'view'>('home');
  let selected = $state('trip');
  let draft = $state('');
  let mode = $state('Balanced');
  let modes = $state(false);
  let details = $state(false);
  let agentMenu = $state(false);
  let ring = $state(false);
  let hover = $state('');
  let showArchived = $state(false);
  let open = $state<Record<number, boolean>>({});

  // The right panel's share of the width, dragged on the gutter (as Shell.svelte's side panel) and
  // kept across visits; double-click or Enter resets it.
  const SIDE_KEY = 'zen.mock.side';
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

  const AGENTS = [
    { id: 'zenbot', name: 'zenbot', role: 'Chief of staff', a: '#4f7d67', b: '#8fb39f' },
    { id: 'garry', name: 'Garry', role: 'Startup advisor', a: '#7a6a9b', b: '#b3a3cf' },
  ];
  let agent = $state(AGENTS[0]);

  const TABS = [
    { key: 'sessions', label: 'Sessions' }, { key: 'files', label: 'Files' }, { key: 'tasks', label: 'Tasks' },
    { key: 'agents', label: 'Agents' }, { key: 'games', label: 'Games' },
  ];
  const PLUGIN_TABS = [{ key: 'read', label: 'Read' }, { key: 'write', label: 'Write' }];
  const waiting = sessions.filter((s) => s.state === 'waiting').length;

  // Line icons, 24×24, stroked with currentColor.
  const ICONS: Record<string, string> = {
    chat: '<path d="M4 5h16v11H9l-5 4z"/>',
    home: '<path d="M4 11l8-6 8 6v8a1 1 0 0 1-1 1h-4v-5h-6v5H5a1 1 0 0 1-1-1z"/>',
    sessions: '<rect x="4" y="4" width="16" height="6" rx="2"/><rect x="4" y="14" width="16" height="6" rx="2"/>',
    files: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
    tasks: '<path d="M9 11l2 2 4-4"/><rect x="4" y="4" width="16" height="16" rx="3"/>',
    agents: '<circle cx="9" cy="9" r="3"/><circle cx="17" cy="10" r="2.2"/><path d="M3.5 19a5.5 5.5 0 0 1 11 0M14.5 19a3.5 3.5 0 0 1 6 0"/>',
    games: '<rect x="3" y="7" width="18" height="11" rx="4"/><path d="M8 11v3M6.5 12.5h3"/><circle cx="15.5" cy="12" r=".6"/><circle cx="17.5" cy="14" r=".6"/>',
    read: '<path d="M4 5.5A1.5 1.5 0 0 1 5.5 4H11v16H5.5A1.5 1.5 0 0 1 4 18.5zM20 5.5A1.5 1.5 0 0 0 18.5 4H13v16h5.5a1.5 1.5 0 0 0 1.5-1.5z"/>',
    write: '<path d="M4 20h4L19 9l-4-4L4 16z"/><path d="M13.5 6.5l4 4"/>',
    settings: '<circle cx="12" cy="12" r="3"/><path d="M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M5.6 18.4L7 17M17 7l1.4-1.4"/>',
    plus: '<path d="M12 5v14M5 12h14"/>',
    search: '<circle cx="11" cy="11" r="6"/><path d="M20 20l-4.5-4.5"/>',
    split: '<rect x="3" y="5" width="18" height="14" rx="3"/><path d="M12 5v14"/>',
    focus: '<rect x="3" y="5" width="18" height="14" rx="3"/>',
    theme: '<path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z"/>',
    clip: '<path d="M20 11.5l-8 8a5 5 0 0 1-7-7l8.5-8.5a3.3 3.3 0 0 1 4.7 4.7L9.7 17.2a1.7 1.7 0 0 1-2.4-2.4l7.7-7.7"/>',
    stop: '<rect x="6" y="6" width="12" height="12" rx="2"/>',
    ring: '<circle cx="12" cy="12" r="2.5"/><circle cx="12" cy="4.5" r="1.3"/><circle cx="19.5" cy="12" r="1.3"/><circle cx="12" cy="19.5" r="1.3"/><circle cx="4.5" cy="12" r="1.3"/>',
  };

  const current = $derived([...channels, ...sessions, ...archived].find((s) => s.id === selected) ?? sessions[0]);
  const msgs = $derived<Msg[]>(state === 'new' ? [] : state === 'waiting' ? [...conversation, question] : conversation);
  const tabs = [{ key: 'home', label: 'Home' }, ...TABS];
  const split = $derived(screen === 'chat' && !narrow && view !== null);
  const showView = $derived(screen === 'view' || split);
  const tabOn = (k: string) => (k === 'home' ? screen === 'home' : screen !== 'home' && view === k);

  /** The quick-control ring's actions, fanned out on a quarter circle up and to the left. */
  const actions = $derived([
    { key: 'plus', label: 'New chat', run: () => newChat() },
    { key: 'search', label: 'Search', run: () => pickTab('sessions') },
    state === 'working'
      ? { key: 'stop', label: 'Stop zenbot', run: () => (state = 'chat') }
      : { key: 'clip', label: 'Attach a file', run: () => {} },
    screen === 'chat' && !narrow
      ? { key: split ? 'focus' : 'split', label: split ? 'Focus on chat' : 'Show panel', run: () => (view = split ? null : 'sessions') }
      : { key: 'home', label: 'Home', run: () => pickTab('home') },
    { key: 'theme', label: `Theme: ${theme}`, run: () => cycleTheme() },
    { key: 'settings', label: 'Settings', run: () => pickTab('settings') },
  ]);
  const R = 156;
  const at = (i: number, n: number) => {
    const a = Math.PI + (i * (Math.PI / 2)) / (n - 1);
    return `translate(${Math.cos(a) * R}px, ${Math.sin(a) * R}px)`;
  };

  function pickTab(k: string) {
    if (k === 'home') { screen = 'home'; view = null; }
    else if (screen === 'chat' && !narrow) view = view === k ? null : k;
    else { screen = 'view'; view = k; }
  }
  function openChat(st: State) {
    if (screen !== 'chat') back = screen;
    state = st;
    screen = 'chat';
    if (back === 'home' || narrow) view = null;
  }
  function pick(s: Session) {
    selected = s.id;
    openChat(s.state === 'working' ? 'working' : s.state === 'waiting' ? 'waiting' : 'chat');
  }
  /** From Home: a widget's row opens its chat. */
  function openById(id: string) {
    const s = [...channels, ...sessions, ...archived].find((x) => x.id === id);
    if (s) pick(s);
  }
  function newChat() {
    openChat('new');
  }
  function goBack() {
    screen = back;
    if (back === 'home') view = null;
  }
  function cycleTheme() {
    theme = theme === 'auto' ? 'dark' : theme === 'dark' ? 'light' : 'auto';
  }
  function key(e: KeyboardEvent) {
    if (e.key === 'Escape') { ring = false; agentMenu = false; details = false; modes = false; }
    // Hold-free shortcut for the ring, as the mouse button would be on a Logitech mouse.
    if (e.key === '.' && (e.metaKey || e.ctrlKey)) { e.preventDefault(); ring = !ring; }
  }

  onMount(() => {
    const mq = matchMedia('(max-width: 900px)');
    const fit = () => (narrow = mq.matches);
    fit();
    mq.addEventListener('change', fit);
    return () => mq.removeEventListener('change', fit);
  });

  $effect(() => {
    const r = document.documentElement;
    if (theme === 'auto') r.removeAttribute('data-theme');
    else r.setAttribute('data-theme', theme);
  });
</script>

<svelte:window onkeydown={key} />

{#snippet ico(name: string)}
  <svg class="ico" viewBox="0 0 24 24" aria-hidden="true">{@html ICONS[name]}</svg>
{/snippet}

{#snippet row(s: Session)}
  <button class="srow" class:on={state !== 'new' && selected === s.id} onclick={() => pick(s)}>
    <span class="t">{s.title}</span>
    {#if s.state === 'working'}<span class="tag work"><span class="dot work"></span>working</span>
    {:else if s.state === 'waiting'}<span class="tag wait"><span class="dot wait"></span>needs you</span>
    {:else}<span class="when">{s.when}</span>{/if}
  </button>
{/snippet}

<!-- top: the agent, the tabs -->
<div class="top" class:hide={narrow && screen === 'chat'}>
  <div class="agent-wrap">
    <button class="agent" onclick={() => (agentMenu = !agentMenu)} aria-expanded={agentMenu} title="Switch agent">
      <span class="orb" class:busy={state === 'working'} style="--a:{agent.a}; --b:{agent.b}">{agent.name[0].toUpperCase()}</span>
      <span class="aname">{agent.name}<span class="chev">▾</span></span>
    </button>
    {#if agentMenu}
      <div class="menu amenu">
        <div class="mlabel">Agents</div>
        {#each AGENTS as a}
          <button class="arow" class:sel={a.id === agent.id} onclick={() => { agent = a; agentMenu = false; }}>
            <span class="orb sm" style="--a:{a.a}; --b:{a.b}">{a.name[0].toUpperCase()}</span>
            <span class="ameta"><b>{a.name}</b><span>{a.role}</span></span>
            {#if a.id === agent.id}<span class="tick">✓</span>{/if}
          </button>
        {/each}
        <hr />
        <div class="me"><span class="av">A</span><span class="name">Adriane</span>
          <button class="icon" title="Settings" onclick={() => { pickTab('settings'); agentMenu = false; }}>{@render ico('settings')}</button>
        </div>
      </div>
    {/if}
  </div>

  <nav class="tabs" aria-label="Views">
    {#each tabs as t}
      <button class="tab" class:on={tabOn(t.key)} onclick={() => pickTab(t.key)} title={split && view === t.key ? 'Close the panel' : t.label}>
        {@render ico(t.key)}<span class="tl">{t.label}</span>
        {#if t.key === 'sessions' && waiting}<span class="badge">{waiting}</span>{/if}
      </button>
    {/each}
    {#if !narrow}
      <span class="vsep"></span>
      {#each PLUGIN_TABS as t}
        <button class="tab" class:on={tabOn(t.key)} onclick={() => pickTab(t.key)}>{@render ico(t.key)}<span class="tl">{t.label}</span></button>
      {/each}
    {/if}
  </nav>
</div>

<div class="stage" class:split class:dragging bind:this={stage} style={split ? `grid-template-columns: minmax(0, ${1 - side}fr) 14px minmax(0, ${side}fr)` : undefined}>
  <!-- left: the chat -->
  {#if screen === 'home'}
    <section class="pane"><Home {narrow} onopen={openById} /></section>
  {/if}

  {#if screen === 'chat'}
    <section class="pane chat">
      <header class="hd">
        {#if narrow}<button class="icon back" title="Back" aria-label="Back" onclick={goBack}>‹</button>{/if}
        <h1>{state === 'new' ? 'New chat' : current.title}</h1>
        {#if state === 'working'}<span class="tag work"><span class="dot work"></span>working</span>
        {:else if state === 'waiting'}<span class="tag wait"><span class="dot wait"></span>needs you</span>{/if}
        <span class="sp"></span>
        {#if state !== 'new'}
          <div class="menu-wrap">
            <button class="icon" title="Details" onclick={() => (details = !details)}>⋯</button>
            {#if details}
              <div class="menu right">
                <div class="row"><span>Model</span><b>claude-opus-5-5</b></div>
                <div class="row"><span>Effort</span><b>medium</b></div>
                <div class="row"><span>Context</span><b>104k tokens</b></div>
                <div class="row"><span>Session</span><b>9e133506</b></div>
                <hr />
                <button>Rename</button>
                <button>Archive chat</button>
              </div>
            {/if}
          </div>
        {/if}
      </header>

      <div class="scroll">
        {#if state === 'new'}
          <div class="hello">
            <span class="orb lg" style="--a:{agent.a}; --b:{agent.b}">{agent.name[0].toUpperCase()}</span>
            <h2>Good morning, Adriane</h2>
            <p>What can {agent.name} take off your plate today?</p>
            <div class="sugg">{#each suggestions as s}<button onclick={() => (draft = s)}>{s}</button>{/each}</div>
          </div>
        {:else}
          <div class="body">
            {#each msgs as m, i}
              {#if m.k === 'user'}
                <div class="u"><div class="bubble">{m.text}</div></div>
              {:else if m.k === 'bot'}
                <div class="prose">{@html m.html}</div>
              {:else if m.k === 'work'}
                <div class="w">
                  <button class="wline" onclick={() => (open[i] = !open[i])}>
                    <span class="chk" class:bad={m.steps.some((s) => s.failed)}>{m.steps.some((s) => s.failed) ? '!' : '✓'}</span>
                    {m.summary}<span class="n">· {m.steps.length} steps</span><span class="car">{open[i] ? '▾' : '▸'}</span>
                  </button>
                  {#if open[i]}
                    <ol class="steps">
                      {#each m.steps as s}<li class:bad={s.failed}><b>{s.label}</b> <span>{s.detail}</span>{#if s.failed} · didn't work{/if}</li>{/each}
                    </ol>
                  {/if}
                </div>
              {:else if m.k === 'ask'}
                <div class="ask">
                  <div class="q">{m.question}</div>
                  <div class="opts">{#each m.options as o, j}<button class:primary={j === 0} onclick={() => (state = 'chat')}>{o}</button>{/each}</div>
                  <div class="or">or type your own answer below</div>
                </div>
              {/if}
            {/each}
            {#if state === 'working'}
              <div class="thinking"><span class="orb xs busy" style="--a:{agent.a}; --b:{agent.b}"></span> Checking availability for Fri 21 – Sun 23… <span class="dim">1 min</span></div>
            {/if}
          </div>
        {/if}
      </div>

      <footer class="dock">
        <div class="compose">
          <textarea rows="1" placeholder={state === 'waiting' ? `Answer ${agent.name}…` : `Message ${agent.name}`} bind:value={draft}></textarea>
          <div class="bar">
            <button class="icon" title="Attach a file">{@render ico('clip')}</button>
            <div class="menu-wrap">
              <button class="mode" onclick={() => (modes = !modes)}>{mode} ▾</button>
              {#if modes}
                <div class="menu up">
                  {#each [['Fast', 'Quick answers, everyday tasks'], ['Balanced', 'Good for most things'], ['Thorough', 'Hard problems, takes longer']] as [k, d]}
                    <button class="mopt" class:sel={mode === k} onclick={() => { mode = k; modes = false; }}><b>{k}</b><span>{d}</span></button>
                  {/each}
                </div>
              {/if}
            </div>
            <span class="sp"></span>
            {#if state === 'working'}<button class="send stop" title="Stop" onclick={() => (state = 'chat')}>{@render ico('stop')}</button>
            {:else}<button class="send" disabled={!draft.trim()} title="Send">↑</button>{/if}
          </div>
        </div>
      </footer>
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
      {#if view === 'sessions'}
        <header class="vh"><h2>Sessions</h2><span class="sp"></span>
          <button class="ghost" onclick={newChat}>{@render ico('plus')} New</button></header>
        <div class="vsearch">{@render ico('search')}<input placeholder="Search sessions" /></div>
        <div class="vscroll">
          <div class="grp">Channels <span class="cnt">{channels.length}</span></div>
          {#each channels as s}{@render row(s)}{/each}
          <div class="grp">Sessions <span class="cnt">{sessions.length}</span></div>
          {#each sessions as s}{@render row(s)}{/each}
          <button class="grp toggle" onclick={() => (showArchived = !showArchived)} aria-expanded={showArchived}>
            <span class="car">{showArchived ? '▾' : '▸'}</span> Archived <span class="cnt">{archivedCount.toLocaleString()}</span>
          </button>
          {#if showArchived}
            <div class="arch">{#each archived as s}{@render row(s)}{/each}<button class="more">Show all archived…</button></div>
          {/if}
        </div>
      {:else if view === 'files'}
        <header class="vh"><h2>Files</h2></header>
        <div class="vscroll">
          {#each [['📁', 'Goals', '4 notes'], ['📁', 'Projects', '12 notes'], ['📁', 'Daily', 'today'], ['📄', 'Family budget — December.md', '8:41'], ['📄', 'Campos do Jordão trip.md', '9:16'], ['🖼', 'baby-room-ideas.png', 'Wed']] as [i, n, w]}
            <div class="frow"><span>{i}</span><span class="t">{n}</span><span class="when">{w}</span></div>
          {/each}
        </div>
      {:else if view === 'tasks'}
        <header class="vh"><h2>Tasks</h2></header>
        <div class="vscroll">
          {#each [['Book Pousada Villa Bella', 'Fri', false], ['Paediatrician — 6-month check-up', 'Nov 26', false], ['Renew car insurance', 'Nov 30', false], ['Order Christmas gifts', 'Dec 5', false], ['Pay school fee', 'Nov 15', true]] as [t, d, done]}
            <label class="trow" class:done><input type="checkbox" checked={done as boolean} /><span class="t">{t}</span><span class="when">{d}</span></label>
          {/each}
        </div>
      {:else if view === 'agents'}
        <header class="vh"><h2>Agents</h2></header>
        <div class="vscroll cards">
          {#each AGENTS as a}
            <button class="card" class:sel={a.id === agent.id} onclick={() => (agent = a)}>
              <span class="orb" style="--a:{a.a}; --b:{a.b}">{a.name[0].toUpperCase()}</span>
              <span class="ameta"><b>{a.name}</b><span>{a.role}</span></span>
            </button>
          {/each}
        </div>
      {:else if view === 'games'}
        <header class="vh"><h2>Games</h2></header>
        <div class="vscroll cards">
          {#each [['Sudoku', 'A calm puzzle a day'], ['zengame', 'Prototype 0']] as [n, d]}
            <div class="card"><span class="orb" style="--a:#a8762c; --b:#cfa660">{n[0]}</span><span class="ameta"><b>{n}</b><span>{d}</span></span></div>
          {/each}
        </div>
      {:else}
        <header class="vh"><h2>{view ? view[0].toUpperCase() + view.slice(1) : ''}</h2></header>
        <div class="empty">The {view} view goes here.</div>
      {/if}
    </section>
  {/if}
</div>

<!-- bottom right: the quick-control ring -->
{#if ring}<button class="scrim" aria-label="Close" onclick={() => (ring = false)}></button>{/if}
<div class="qc" class:open={ring}>
  {#each actions as a, i}
    <button class="qa" style="transform: {ring ? at(i, actions.length) : 'translate(0,0)'}; transition-delay: {ring ? i * 18 : 0}ms"
      tabindex={ring ? 0 : -1} title={a.label} aria-label={a.label}
      onmouseenter={() => (hover = a.label)} onmouseleave={() => (hover = '')} onfocus={() => (hover = a.label)}
      onclick={() => { a.run(); ring = false; hover = ''; }}>{@render ico(a.key)}</button>
  {/each}
  {#if ring && hover}<span class="qlabel">{hover}</span>{/if}
  <button class="qbtn" onclick={() => (ring = !ring)} aria-expanded={ring} title="Quick actions (Ctrl+.)">{@render ico('ring')}</button>
</div>

<div class="mock">
  <b>mock</b>
  {#each [['new', 'New'], ['chat', 'Chat'], ['working', 'Working'], ['waiting', 'Question']] as [k, l]}
    <button class:on={screen === 'chat' && state === k} onclick={() => openChat(k as State)}>{l}</button>
  {/each}
  <span class="sep"></span>
  <button onclick={cycleTheme}>Theme: {theme}</button>
  <a href="#v1">v1</a>
</div>

<style>
  /* Palette "stone & moss" (v1), plus --bg: the table the panels sit on. */
  :global(:root), :global(:root[data-theme='light']) {
    --bg: #e9e5dc; --paper: #f7f5f0; --side: #efece5; --soft: #ece8e0; --faint: #dfdad0;
    --ink: #2e2d2a; --dim: #736e67; --accent: #4f7d67; --on-accent: #f6f4ef;
    --warn: #a8762c; --err: #b0533c; --ok: #4f7d67; --shadow: rgb(60 50 30 / 0.10); --glass: rgb(247 245 240 / 0.78);
    color-scheme: light;
  }
  :global(:root[data-theme='dark']) {
    --bg: #151716; --paper: #1e2120; --side: #1a1c1b; --soft: #282c2a; --faint: #323734;
    --ink: #cdc9c0; --dim: #868b85; --accent: #8fb39f; --on-accent: #1a1c1b;
    --warn: #cfa660; --err: #d4836d; --ok: #8fb39f; --shadow: rgb(0 0 0 / 0.35); --glass: rgb(36 40 38 / 0.8);
    color-scheme: dark;
  }
  @media (prefers-color-scheme: dark) {
    :global(:root:not([data-theme='light'])) {
      --bg: #151716; --paper: #1e2120; --side: #1a1c1b; --soft: #282c2a; --faint: #323734;
      --ink: #cdc9c0; --dim: #868b85; --accent: #8fb39f; --on-accent: #1a1c1b;
      --warn: #cfa660; --err: #d4836d; --ok: #8fb39f; --shadow: rgb(0 0 0 / 0.35); --glass: rgb(36 40 38 / 0.8);
      color-scheme: dark;
    }
  }
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
  .orb.lg { width: 64px; height: 64px; font-size: 26px; margin: 0 auto 16px; box-shadow: 0 0 0 6px color-mix(in srgb, var(--a) 14%, transparent); }
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
  .tab { position: relative; display: flex; align-items: center; gap: 7px; padding: 7px 14px; border-radius: 99px; font-size: 14px; color: var(--dim); }
  .tab:hover { color: var(--ink); background: var(--soft); }
  .tab.on { color: var(--on-accent); background: var(--accent); }
  .badge { min-width: 17px; height: 17px; padding: 0 5px; border-radius: 99px; font-size: 11px; font-weight: 700; display: grid; place-items: center; background: var(--warn); color: var(--on-accent); }
  .vsep { width: 1px; height: 18px; background: var(--faint); margin: 0 4px; }

  /* the panels */
  .stage { height: 100dvh; padding: 64px 14px 14px; display: grid; grid-template-columns: minmax(0, 1fr); gap: 14px; }
  .stage.split { gap: 0; }
  .stage.dragging { cursor: col-resize; user-select: none; }
  .gutter { display: grid; place-items: center; cursor: col-resize; outline: none; touch-action: none; }
  .gutter span { width: 4px; height: 44px; border-radius: 99px; background: var(--faint); transition: background 0.15s, height 0.15s; }
  .gutter:hover span, .gutter:focus-visible span, .dragging .gutter span { background: var(--accent); height: 72px; }
  .pane { background: var(--paper); border: 1px solid var(--faint); border-radius: 22px; min-height: 0; display: flex; flex-direction: column; overflow: hidden; box-shadow: 0 1px 2px var(--shadow); }

  /* chat */
  .hd { display: flex; align-items: center; gap: 10px; padding: 14px 18px 10px 22px; }
  .hd h1 { font-size: 16px; font-weight: 650; margin: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .scroll { flex: 1; overflow: auto; }
  .body { max-width: 44rem; margin: 0 auto; padding: 14px 26px 12px; display: flex; flex-direction: column; gap: 18px; }
  .u { display: flex; justify-content: flex-end; }
  .bubble { max-width: 82%; background: var(--soft); padding: 10px 15px; border-radius: 18px 18px 5px 18px; line-height: 1.5; }
  .prose { line-height: 1.65; }
  .prose :global(p) { margin: 0 0 10px; }
  .prose :global(p:last-child) { margin-bottom: 0; }
  .thinking { display: flex; align-items: center; gap: 10px; }
  .wline { text-align: left; max-width: 100%; display: inline-flex; align-items: center; gap: 8px; font-size: 13px; color: var(--dim); padding: 5px 11px 5px 6px; border-radius: 99px; border: 1px solid var(--faint); }
  .wline:hover { color: var(--ink); border-color: var(--dim); }
  .chk { width: 18px; height: 18px; border-radius: 50%; display: grid; place-items: center; font-size: 11px; background: color-mix(in srgb, var(--ok) 16%, transparent); color: var(--ok); }
  .chk.bad { background: color-mix(in srgb, var(--warn) 18%, transparent); color: var(--warn); }
  .car { font-size: 10px; }
  .steps { margin: 8px 0 0; padding: 0 0 0 26px; font-size: 13px; color: var(--dim); display: flex; flex-direction: column; gap: 4px; }
  .steps b { color: var(--ink); font-weight: 500; }
  .steps .bad, .steps .bad b { color: var(--err); }
  .ask { border: 1px solid color-mix(in srgb, var(--warn) 45%, var(--faint)); background: color-mix(in srgb, var(--warn) 6%, var(--paper)); border-radius: 16px; padding: 16px; display: flex; flex-direction: column; gap: 12px; }
  .q { font-weight: 600; }
  .opts { display: flex; flex-wrap: wrap; gap: 8px; }
  .opts button { padding: 8px 14px; border-radius: 99px; border: 1px solid var(--faint); background: var(--paper); font-size: 14px; }
  .opts button:hover { border-color: var(--dim); }
  .opts .primary { background: var(--accent); border-color: var(--accent); color: var(--on-accent); }
  .or { font-size: 12px; color: var(--dim); }
  .hello { max-width: 34rem; margin: 14vh auto 0; padding: 0 24px; text-align: center; }
  .hello h2 { font-family: var(--serif); font-size: 30px; font-weight: 600; margin: 0 0 6px; }
  .hello p { color: var(--dim); margin: 0 0 24px; }
  .sugg { display: flex; flex-wrap: wrap; gap: 8px; justify-content: center; }
  .sugg button { padding: 9px 14px; border: 1px solid var(--faint); border-radius: 99px; font-size: 14px; }
  .sugg button:hover { border-color: var(--dim); background: var(--soft); }
  .dock { padding: 6px 16px 16px; }
  .compose { max-width: 44rem; margin: 0 auto; border: 1px solid var(--faint); border-radius: 20px; background: var(--paper); padding: 10px 10px 8px 16px; box-shadow: 0 4px 16px var(--shadow); }
  .compose:focus-within { border-color: var(--accent); }
  .compose textarea { width: 100%; border: 0; outline: none; background: transparent; resize: none; font: inherit; color: inherit; padding: 4px 0 8px; field-sizing: content; max-height: 40vh; }
  .bar { display: flex; align-items: center; gap: 4px; margin-left: -8px; }
  .mode { font-size: 13px; color: var(--dim); padding: 5px 10px; border-radius: 99px; }
  .mode:hover { background: var(--soft); color: var(--ink); }
  .send { width: 34px; height: 34px; border-radius: 50%; background: var(--accent); color: var(--on-accent); font-size: 17px; font-weight: 700; display: grid; place-items: center; }
  .send:disabled { background: var(--soft); color: var(--dim); cursor: default; }
  .send.stop { background: var(--soft); color: var(--ink); border: 1px solid var(--faint); }
  .send.stop .ico { width: 14px; height: 14px; }

  /* menus */
  .menu-wrap { position: relative; }
  .menu { position: absolute; z-index: 12; min-width: 240px; background: var(--paper); border: 1px solid var(--faint); border-radius: 14px; padding: 6px; box-shadow: 0 14px 40px var(--shadow); display: flex; flex-direction: column; }
  .menu.right { right: 0; top: 38px; }
  .menu.up { bottom: 40px; left: 0; }
  .amenu { left: 0; top: 50px; min-width: 260px; }
  .mlabel { font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); font-weight: 600; padding: 6px 10px 4px; }
  .menu .row { display: flex; justify-content: space-between; gap: 16px; padding: 6px 10px; font-size: 13px; color: var(--dim); }
  .menu .row b { color: var(--ink); font-weight: 500; font-family: var(--mono); font-size: 12px; }
  .menu hr { border: 0; border-top: 1px solid var(--faint); margin: 6px 0; width: 100%; }
  .menu > button { text-align: left; padding: 7px 10px; border-radius: 8px; font-size: 14px; }
  .menu > button:hover { background: var(--soft); }
  .arow { display: flex !important; align-items: center; gap: 10px; }
  .ameta { display: flex; flex-direction: column; flex: 1; text-align: left; }
  .ameta b { font-weight: 600; }
  .ameta span { font-size: 12px; color: var(--dim); }
  .tick { color: var(--accent); }
  .me { display: flex; align-items: center; gap: 10px; padding: 4px 4px 2px 10px; font-size: 14px; }
  .me .name { flex: 1; font-weight: 550; }
  .av { width: 26px; height: 26px; border-radius: 50%; display: grid; place-items: center; font-size: 13px; font-weight: 650; background: var(--soft); }
  .mopt { display: flex; flex-direction: column; align-items: flex-start; gap: 1px; }
  .mopt span { font-size: 12px; color: var(--dim); }
  .mopt.sel b { color: var(--accent); }

  /* the right panel's views */
  .vh { display: flex; align-items: center; gap: 8px; padding: 16px 16px 8px 22px; }
  .vh h2 { margin: 0; font-size: 20px; font-family: var(--serif); font-weight: 600; }
  .ghost { display: flex; align-items: center; gap: 6px; font-size: 13px; padding: 6px 12px; border-radius: 99px; border: 1px solid var(--faint); }
  .ghost:hover { border-color: var(--dim); }
  .ghost .ico { width: 14px; height: 14px; }
  .vsearch { display: flex; align-items: center; gap: 8px; margin: 4px 16px 6px; padding: 8px 12px; border-radius: 12px; background: var(--soft); color: var(--dim); }
  .vsearch input { flex: 1; border: 0; outline: none; background: transparent; font-size: 14px; }
  .vscroll { flex: 1; overflow: auto; padding: 0 10px 80px; display: flex; flex-direction: column; gap: 1px; }
  .grp { display: flex; align-items: center; gap: 6px; font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); padding: 14px 12px 5px; font-weight: 600; text-align: left; }
  .grp .cnt { font-weight: 500; letter-spacing: 0; opacity: 0.75; }
  .grp.toggle:hover { color: var(--ink); }
  .grp .car { font-size: 9px; width: 8px; }
  .arch { display: flex; flex-direction: column; gap: 1px; }
  .arch .srow { color: var(--dim); }
  .more { text-align: left; font-size: 13px; color: var(--accent); padding: 6px 12px; }
  .srow, .frow, .trow { display: flex; align-items: center; gap: 10px; text-align: left; padding: 10px 12px; border-radius: 12px; font-size: 14px; }
  .srow:hover, .frow:hover, .trow:hover { background: var(--soft); }
  .srow.on { background: color-mix(in srgb, var(--accent) 14%, var(--paper)); }
  .t { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .when { font-size: 12px; color: var(--dim); white-space: nowrap; }
  .trow input { accent-color: var(--accent); width: 16px; height: 16px; }
  .trow.done .t { text-decoration: line-through; color: var(--dim); }
  .cards { padding-top: 6px; gap: 8px; }
  .card { display: flex; align-items: center; gap: 12px; padding: 14px; border-radius: 16px; border: 1px solid var(--faint); text-align: left; }
  .card.sel { border-color: var(--accent); }
  .empty { padding: 24px; color: var(--dim); }

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

  .mock { position: fixed; left: 14px; bottom: 14px; z-index: 20; display: flex; align-items: center; gap: 4px; padding: 6px 8px; border-radius: 12px; background: #2a2e2c; color: #d6d2c9; font-size: 12px; box-shadow: 0 6px 20px rgb(0 0 0 / 0.25); opacity: 0.9; }
  .mock b { color: #f5c542; margin-right: 4px; }
  .mock button, .mock a { padding: 3px 8px; border-radius: 7px; color: #bbb; text-decoration: none; }
  .mock button.on { background: #3a3f3c; color: #fff; }
  .mock .sep { width: 1px; height: 16px; background: #444; margin: 0 4px; }

  @media (max-width: 900px) {
    .top { height: 60px; justify-content: space-between; gap: 8px; padding: 0 8px; }
    .agent-wrap { position: static; }
    .agent { padding: 2px; }
    .aname { display: none; }
    .orb { width: 34px; height: 34px; }
    .tabs { max-width: 100%; overflow-x: auto; scrollbar-width: none; }
    .tab { padding: 7px 9px; }
    .tab:not(.on) .tl { display: none; }
    .top.hide { display: none; }
    .stage { padding: 60px 8px 8px; }
    .top.hide + .stage { padding-top: 8px; }
    .back { font-size: 26px; margin-left: -10px; color: var(--ink); }
    .body { padding: 10px 14px; }
    .dock { padding: 6px 8px 8px; }
    .qc { right: 16px; bottom: 96px; }
    .mock { left: 8px; bottom: 160px; flex-wrap: wrap; max-width: 60vw; }
  }
</style>
