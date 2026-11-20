<script lang="ts">
  // Chat redesign mock (v0): a sidebar of chats, a quiet header, a transcript that reads like a
  // conversation (tool runs as one plain-language line), a question card, and one composer.
  // Technical details (model, effort, tokens, session id) move behind the header's ⋯ menu.
  // The floating "mock" panel switches states; nothing here talks to the kernel.
  import { archived, archivedCount, channels, conversation, question, sessions, suggestions, type Msg, type Session } from './data.ts';

  type State = 'new' | 'chat' | 'working' | 'waiting';
  let state = $state<State>('chat');
  let sidebar = $state(true);
  let theme = $state<'auto' | 'light' | 'dark'>('auto');
  let details = $state(false);
  let open = $state<Record<number, boolean>>({});
  let draft = $state('');
  let mode = $state('Balanced');
  let modes = $state(false);

  const current = $derived(state === 'new' ? null : sessions[0]);
  const msgs = $derived<Msg[]>(
    state === 'new' ? [] : state === 'waiting' ? [...conversation, question] : conversation,
  );
  let showArchived = $state(false);
  // The board's tabs but Sessions (the list above is Sessions), then the plugins' views.
  const TABS = [
    { key: 'files', label: 'Files' }, { key: 'tasks', label: 'Tasks' }, { key: 'agents', label: 'Agents' },
    { key: 'games', label: 'Games' }, { key: 'read', label: 'Read' }, { key: 'write', label: 'Write' },
  ];
  // Line icons, 24×24, stroked with currentColor.
  const ICONS: Record<string, string> = {
    files: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
    tasks: '<path d="M9 11l2 2 4-4"/><rect x="4" y="4" width="16" height="16" rx="3"/>',
    agents: '<circle cx="9" cy="9" r="3"/><circle cx="17" cy="10" r="2.2"/><path d="M3.5 19a5.5 5.5 0 0 1 11 0M14.5 19a3.5 3.5 0 0 1 6 0"/>',
    games: '<rect x="3" y="7" width="18" height="11" rx="4"/><path d="M8 11v3M6.5 12.5h3"/><circle cx="15.5" cy="12" r=".6"/><circle cx="17.5" cy="14" r=".6"/>',
    read: '<path d="M4 5.5A1.5 1.5 0 0 1 5.5 4H11v16H5.5A1.5 1.5 0 0 1 4 18.5zM20 5.5A1.5 1.5 0 0 0 18.5 4H13v16h5.5a1.5 1.5 0 0 0 1.5-1.5z"/>',
    write: '<path d="M4 20h4L19 9l-4-4L4 16z"/><path d="M13.5 6.5l4 4"/>',
    settings: '<circle cx="12" cy="12" r="3"/><path d="M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M5.6 18.4L7 17M17 7l1.4-1.4"/>',
  };
  let tab = $state('');
  let selected = $state('trip');
  const pick = (id: string) => { selected = id; state = 'chat'; };

  $effect(() => {
    const r = document.documentElement;
    if (theme === 'auto') r.removeAttribute('data-theme');
    else r.setAttribute('data-theme', theme);
  });
</script>

<div class="app" class:nosb={!sidebar}>
  {#if sidebar}
    <aside class="sb">
      <div class="sb-top">
        <span class="brand">zen</span>
        <button class="icon" title="Hide sidebar" onclick={() => (sidebar = false)}>⟨</button>
      </div>
      <button class="newchat" onclick={() => (state = 'new')}><span>＋</span> New chat</button>
      <input class="search" placeholder="Search chats" />
      {#snippet row(s: Session)}
        <button class="item" class:on={state !== 'new' && selected === s.id} onclick={() => pick(s.id)}>
          <span class="t">{s.title}</span>
          {#if s.state === 'working'}<span class="tag work"><span class="dot work"></span>working</span>
          {:else if s.state === 'waiting'}<span class="tag wait"><span class="dot wait"></span>needs you</span>
          {:else}<span class="when">{s.when}</span>{/if}
        </button>
      {/snippet}
      <nav class="list">
        <div class="grp">Channels <span class="cnt">{channels.length}</span></div>
        {#each channels as s}{@render row(s)}{/each}

        <div class="grp">Sessions <span class="cnt">{sessions.length}</span></div>
        {#each sessions as s}{@render row(s)}{/each}

        <button class="grp toggle" onclick={() => (showArchived = !showArchived)} aria-expanded={showArchived}>
          <span class="car">{showArchived ? '▾' : '▸'}</span> Archived <span class="cnt">{archivedCount.toLocaleString()}</span>
        </button>
        {#if showArchived}
          <div class="arch">
            {#each archived as s}{@render row(s)}{/each}
            <button class="more">Show all archived…</button>
          </div>
        {/if}
      </nav>
      {#snippet ico(name: string)}
        <svg class="ico" viewBox="0 0 24 24" aria-hidden="true">{@html ICONS[name]}</svg>
      {/snippet}
      <div class="sb-foot">
        {#each TABS as t}
          <button class="nav" class:on={tab === t.key} onclick={() => (tab = t.key)}>{@render ico(t.key)}{t.label}</button>
        {/each}
        <div class="me">
          <span class="av">A</span><span class="name">Adriane</span>
          <button class="icon" class:on={tab === 'settings'} title="Settings" onclick={() => (tab = 'settings')}>{@render ico('settings')}</button>
        </div>
      </div>
    </aside>
  {/if}

  <main class="main">
    <header class="hd">
      {#if !sidebar}<button class="icon" title="Show sidebar" onclick={() => (sidebar = true)}>☰</button>{/if}
      <h1>{current ? current.title : 'New chat'}</h1>
      {#if state === 'working'}<span class="pill work"><span class="dot work"></span> Working</span>
      {:else if state === 'waiting'}<span class="pill wait">Needs your answer</span>{/if}
      <span class="sp"></span>
      {#if current}
        <div class="menu-wrap">
          <button class="icon" title="Details" onclick={() => (details = !details)}>⋯</button>
          {#if details}
            <div class="menu">
              <div class="row"><span>Model</span><b>claude-opus-5-5</b></div>
              <div class="row"><span>Effort</span><b>medium</b></div>
              <div class="row"><span>Context</span><b>104k tokens</b></div>
              <div class="row"><span>Session</span><b>9e133506</b></div>
              <hr />
              <button>Rename</button>
              <button>Open side panel</button>
              <button>Archive chat</button>
            </div>
          {/if}
        </div>
      {/if}
    </header>

    <section class="scroll">
      {#if state === 'new'}
        <div class="hello">
          <div class="logo">z</div>
          <h2>Good morning, Adriane</h2>
          <p>What can I take off your plate today?</p>
          <div class="sugg">
            {#each suggestions as s}<button onclick={() => (draft = s)}>{s}</button>{/each}
          </div>
        </div>
      {:else}
        <div class="body">
          {#each msgs as m, i}
            {#if m.k === 'user'}
              <div class="u"><div class="bubble">{m.text}</div></div>
            {:else if m.k === 'bot'}
              <div class="b">
                <div class="av bot">z</div>
                <div class="prose">{@html m.html}</div>
              </div>
            {:else if m.k === 'work'}
              <div class="w">
                <button class="wline" onclick={() => (open[i] = !open[i])}>
                  <span class="chk">{m.steps.some((s) => s.failed) ? '!' : '✓'}</span>
                  {m.summary}
                  <span class="n">· {m.steps.length} steps</span>
                  <span class="car">{open[i] ? '▾' : '▸'}</span>
                </button>
                {#if open[i]}
                  <ol class="steps">
                    {#each m.steps as s}
                      <li class:bad={s.failed}><b>{s.label}</b> <span>{s.detail}</span>{#if s.failed} · didn't work{/if}</li>
                    {/each}
                  </ol>
                {/if}
              </div>
            {:else if m.k === 'ask'}
              <div class="ask">
                <div class="q">{m.question}</div>
                <div class="opts">
                  {#each m.options as o, j}<button class:primary={j === 0} onclick={() => (state = 'chat')}>{o}</button>{/each}
                </div>
                <div class="or">or type your own answer below</div>
              </div>
            {/if}
          {/each}
          {#if state === 'working'}
            <div class="b">
              <div class="av bot pulse">z</div>
              <div class="thinking">Checking availability for Fri 21 – Sun 23… <span class="dim">1 min</span></div>
            </div>
          {/if}
        </div>
      {/if}
    </section>

    <footer class="dock">
      <div class="compose">
        <textarea rows="1" placeholder={state === 'waiting' ? 'Answer zenbot…' : 'Message zenbot'} bind:value={draft}></textarea>
        <div class="bar">
          <button class="icon" title="Attach a file">📎</button>
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
          {#if state === 'working'}
            <button class="send stop" title="Stop">■</button>
          {:else}
            <button class="send" disabled={!draft.trim()} title="Send">↑</button>
          {/if}
        </div>
      </div>
      <div class="hint">zenbot asks before sending, spending or deleting anything.</div>
    </footer>
  </main>
</div>

<div class="mock">
  <b>mock</b>
  {#each [['new', 'New'], ['chat', 'Chat'], ['working', 'Working'], ['waiting', 'Question']] as [k, l]}
    <button class:on={state === k} onclick={() => (state = k as State)}>{l}</button>
  {/each}
  <span class="sep"></span>
  <button class:on={sidebar} onclick={() => (sidebar = !sidebar)}>Sidebar</button>
  <button onclick={() => (theme = theme === 'auto' ? 'dark' : theme === 'dark' ? 'light' : 'auto')}>Theme: {theme}</button>
</div>

<style>
  /* Palette "stone & moss": warm paper and soft ink by day; a green-tinted charcoal with warm grey
     text by night (no pure black or white, contrast kept around WCAG AA, not maximal). */
  :global(:root), :global(:root[data-theme='light']) {
    --paper: #f6f4ef; --side: #efece5; --soft: #e8e4db; --faint: #dfdad0;
    --ink: #2e2d2a; --dim: #736e67; --accent: #4f7d67; --on-accent: #f6f4ef;
    --warn: #a8762c; --err: #b0533c; --ok: #4f7d67; --shadow: rgb(60 50 30 / 0.08);
    --r: 14px; color-scheme: light;
  }
  :global(:root[data-theme='dark']) {
    --paper: #1e2120; --side: #1a1c1b; --soft: #282c2a; --faint: #323734;
    --ink: #cdc9c0; --dim: #868b85; --accent: #8fb39f; --on-accent: #1a1c1b;
    --warn: #cfa660; --err: #d4836d; --ok: #8fb39f; --shadow: rgb(0 0 0 / 0.25);
    color-scheme: dark;
  }
  @media (prefers-color-scheme: dark) {
    :global(:root:not([data-theme='light'])) {
      --paper: #1e2120; --side: #1a1c1b; --soft: #282c2a; --faint: #323734;
      --ink: #cdc9c0; --dim: #868b85; --accent: #8fb39f; --on-accent: #1a1c1b;
      --warn: #cfa660; --err: #d4836d; --ok: #8fb39f; --shadow: rgb(0 0 0 / 0.25);
      color-scheme: dark;
    }
  }
  :global(body) { font-size: 15px; }

  .app { display: grid; grid-template-columns: 264px 1fr; height: 100dvh; }
  .app.nosb { grid-template-columns: 1fr; }

  /* sidebar */
  .sb { background: var(--side); border-right: 1px solid var(--faint); display: flex; flex-direction: column; min-height: 0; padding: 12px 10px; gap: 8px; }
  .sb-top { display: flex; align-items: center; justify-content: space-between; padding: 2px 6px 6px; }
  .brand { font-weight: 700; font-size: 18px; letter-spacing: -0.02em; }
  .newchat { display: flex; align-items: center; gap: 8px; padding: 10px 12px; border-radius: 10px; background: var(--paper); border: 1px solid var(--faint); font-weight: 550; }
  .newchat:hover { border-color: var(--dim); }
  .newchat span { color: var(--accent); }
  .search { padding: 8px 12px; border-radius: 10px; border: 1px solid transparent; background: var(--soft); outline: none; font-size: 14px; }
  .search:focus { border-color: var(--accent); background: var(--paper); }
  .list { flex: 1; overflow: auto; display: flex; flex-direction: column; gap: 1px; margin-top: 4px; }
  .grp { display: flex; align-items: center; gap: 6px; font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); padding: 14px 10px 5px; font-weight: 600; text-align: left; }
  .grp .cnt { font-weight: 500; letter-spacing: 0; opacity: 0.75; }
  .grp.toggle { border-radius: 8px; margin-top: 6px; }
  .grp.toggle:hover { color: var(--ink); }
  .grp .car { font-size: 9px; width: 8px; }
  .arch { display: flex; flex-direction: column; gap: 1px; }
  .arch .item { color: var(--dim); }
  .more { text-align: left; font-size: 13px; color: var(--accent); padding: 6px 10px; }
  .item { display: flex; align-items: center; gap: 8px; text-align: left; padding: 8px 10px; border-radius: 8px; font-size: 14px; }
  .item:hover { background: var(--soft); }
  .item.on { background: color-mix(in srgb, var(--accent) 16%, var(--side)); }
  .item .t { flex: 1; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .when { font-size: 12px; color: var(--dim); white-space: nowrap; }
  .tag { display: inline-flex; align-items: center; gap: 5px; font-size: 11px; font-weight: 550; padding: 2px 8px 2px 7px; border-radius: 99px; white-space: nowrap; }
  .tag .dot { width: 6px; height: 6px; }
  .tag.work { color: var(--accent); background: color-mix(in srgb, var(--accent) 14%, transparent); }
  .tag.wait { color: var(--warn); background: color-mix(in srgb, var(--warn) 16%, transparent); }
  .sb-foot { border-top: 1px solid var(--faint); padding-top: 8px; display: flex; flex-direction: column; gap: 1px; }
  .nav { display: flex; align-items: center; gap: 10px; text-align: left; padding: 7px 10px; border-radius: 8px; font-size: 14px; color: var(--dim); }
  .nav:hover { background: var(--soft); color: var(--ink); }
  .nav.on { color: var(--ink); background: var(--soft); }
  .ico { width: 17px; height: 17px; fill: none; stroke: currentColor; stroke-width: 1.6; stroke-linecap: round; stroke-linejoin: round; flex: none; }
  .me { display: flex; align-items: center; gap: 10px; margin-top: 6px; padding: 8px 4px 0 8px; border-top: 1px solid var(--faint); font-size: 14px; }
  .me .name { flex: 1; font-weight: 550; }
  .me .icon.on { color: var(--accent); }
  .av { width: 26px; height: 26px; border-radius: 50%; display: grid; place-items: center; font-size: 13px; font-weight: 650; background: var(--soft); flex: none; }
  .av.bot { background: color-mix(in srgb, var(--accent) 22%, var(--paper)); color: var(--accent); }

  .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--dim); flex: none; }
  .dot.work { background: var(--accent); animation: blink 1.2s ease-in-out infinite; }
  .dot.wait { background: var(--warn); }
  @keyframes blink { 50% { opacity: 0.3; } }

  /* main */
  .main { display: flex; flex-direction: column; min-width: 0; min-height: 0; }
  .hd { display: flex; align-items: center; gap: 10px; padding: 12px 20px; border-bottom: 1px solid var(--faint); }
  .hd h1 { font-size: 16px; font-weight: 600; margin: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .sp { flex: 1; }
  .icon { width: 32px; height: 32px; border-radius: 8px; display: grid; place-items: center; color: var(--dim); font-size: 16px; }
  .icon:hover { background: var(--soft); color: var(--ink); }
  .pill { display: inline-flex; align-items: center; gap: 6px; font-size: 12px; padding: 3px 10px; border-radius: 99px; white-space: nowrap; }
  .pill.work { background: color-mix(in srgb, var(--accent) 12%, transparent); color: var(--accent); }
  .pill.wait { background: color-mix(in srgb, var(--warn) 14%, transparent); color: var(--warn); }

  .menu-wrap { position: relative; }
  .menu { position: absolute; right: 0; top: 38px; z-index: 5; min-width: 240px; background: var(--paper); border: 1px solid var(--faint); border-radius: 12px; padding: 6px; box-shadow: 0 10px 30px var(--shadow); display: flex; flex-direction: column; }
  .menu.up { top: auto; bottom: 40px; left: 0; right: auto; }
  .menu .row { display: flex; justify-content: space-between; gap: 16px; padding: 6px 10px; font-size: 13px; color: var(--dim); }
  .menu .row b { color: var(--ink); font-weight: 500; font-family: var(--mono); font-size: 12px; }
  .menu hr { border: 0; border-top: 1px solid var(--faint); margin: 6px 0; width: 100%; }
  .menu > button { text-align: left; padding: 7px 10px; border-radius: 8px; font-size: 14px; }
  .menu > button:hover { background: var(--soft); }
  .mopt { display: flex; flex-direction: column; align-items: flex-start; gap: 1px; }
  .mopt span { font-size: 12px; color: var(--dim); }
  .mopt.sel b { color: var(--accent); }

  .scroll { flex: 1; overflow: auto; }
  .body { max-width: 46rem; margin: 0 auto; padding: 28px 24px 12px; display: flex; flex-direction: column; gap: 18px; }

  .u { display: flex; justify-content: flex-end; }
  .bubble { max-width: 80%; background: var(--soft); padding: 10px 14px; border-radius: 18px 18px 4px 18px; line-height: 1.5; }
  .b { display: flex; gap: 12px; align-items: flex-start; }
  .prose { line-height: 1.6; min-width: 0; }
  .prose :global(p) { margin: 0 0 10px; }
  .prose :global(p:last-child) { margin-bottom: 0; }
  .pulse { animation: blink 1.2s ease-in-out infinite; }
  .thinking { padding-top: 3px; color: var(--ink); }

  .w { margin-left: 38px; }
  .wline { display: inline-flex; align-items: center; gap: 8px; font-size: 13px; color: var(--dim); padding: 5px 10px 5px 6px; border-radius: 99px; border: 1px solid var(--faint); }
  .wline:hover { color: var(--ink); border-color: var(--dim); }
  .chk { width: 18px; height: 18px; border-radius: 50%; display: grid; place-items: center; font-size: 11px; background: color-mix(in srgb, var(--ok) 16%, transparent); color: var(--ok); }
  .n { opacity: 0.8; }
  .car { font-size: 10px; }
  .steps { margin: 8px 0 0; padding: 0 0 0 26px; font-size: 13px; color: var(--dim); display: flex; flex-direction: column; gap: 4px; }
  .steps b { color: var(--ink); font-weight: 500; }
  .steps .bad, .steps .bad b { color: var(--err); }

  .ask { margin-left: 38px; border: 1px solid color-mix(in srgb, var(--warn) 45%, var(--faint)); background: color-mix(in srgb, var(--warn) 6%, var(--paper)); border-radius: var(--r); padding: 16px; display: flex; flex-direction: column; gap: 12px; }
  .q { font-weight: 600; }
  .opts { display: flex; flex-wrap: wrap; gap: 8px; }
  .opts button { padding: 8px 14px; border-radius: 99px; border: 1px solid var(--faint); background: var(--paper); font-size: 14px; }
  .opts button:hover { border-color: var(--dim); }
  .opts .primary { background: var(--accent); border-color: var(--accent); color: var(--on-accent); }
  .or { font-size: 12px; color: var(--dim); }

  .hello { max-width: 40rem; margin: 18vh auto 0; padding: 0 24px; text-align: center; }
  .logo { width: 48px; height: 48px; margin: 0 auto 14px; border-radius: 14px; background: color-mix(in srgb, var(--accent) 22%, var(--paper)); color: var(--accent); display: grid; place-items: center; font-weight: 700; font-size: 22px; }
  .hello h2 { font-family: var(--serif); font-size: 30px; font-weight: 600; margin: 0 0 6px; }
  .hello p { color: var(--dim); margin: 0 0 24px; }
  .sugg { display: flex; flex-wrap: wrap; gap: 8px; justify-content: center; }
  .sugg button { padding: 9px 14px; border: 1px solid var(--faint); border-radius: 99px; font-size: 14px; }
  .sugg button:hover { border-color: var(--dim); background: var(--soft); }

  .dock { padding: 8px 24px 14px; }
  .compose { max-width: 46rem; margin: 0 auto; border: 1px solid var(--faint); border-radius: 20px; background: var(--paper); padding: 10px 10px 8px 16px; box-shadow: 0 2px 12px var(--shadow); }
  .compose:focus-within { border-color: var(--accent); }
  .compose textarea { width: 100%; border: 0; outline: none; background: transparent; resize: none; font: inherit; color: inherit; padding: 4px 0 8px; field-sizing: content; max-height: 40vh; }
  .bar { display: flex; align-items: center; gap: 4px; margin-left: -8px; }
  .mode { font-size: 13px; color: var(--dim); padding: 5px 10px; border-radius: 99px; }
  .mode:hover { background: var(--soft); color: var(--ink); }
  .send { width: 34px; height: 34px; border-radius: 50%; background: var(--accent); color: var(--on-accent); font-size: 17px; font-weight: 700; display: grid; place-items: center; }
  .send:disabled { background: var(--soft); color: var(--dim); cursor: default; }
  .send.stop { background: var(--soft); color: var(--ink); border: 1px solid var(--faint); font-size: 12px; }
  .hint { max-width: 46rem; margin: 6px auto 0; text-align: center; font-size: 12px; color: var(--dim); }

  .mock { position: fixed; left: 50%; transform: translateX(-50%); top: 10px; z-index: 20; display: flex; align-items: center; gap: 4px; padding: 6px 8px; border-radius: 12px; background: #2a2e2c; color: #d6d2c9; font-size: 12px; box-shadow: 0 6px 20px rgb(0 0 0 / 0.25); opacity: 0.92; }
  .mock b { color: #f5c542; margin-right: 4px; }
  .mock button { padding: 3px 8px; border-radius: 7px; color: #bbb; }
  .mock button.on { background: #333; color: #fff; }
  .mock .sep { width: 1px; height: 16px; background: #444; margin: 0 4px; }

  @media (max-width: 760px) {
    .app { grid-template-columns: 1fr; }
    .sb { position: fixed; inset: 0 auto 0 0; width: 82vw; max-width: 300px; z-index: 10; box-shadow: 0 0 40px rgb(0 0 0 / 0.25); }
    .body, .dock { padding-left: 14px; padding-right: 14px; }
    .w, .ask { margin-left: 0; }
    .hint { display: none; }
    .mock { left: 8px; right: 8px; transform: none; top: auto; bottom: 96px; flex-wrap: wrap; justify-content: center; }
  }
</style>
