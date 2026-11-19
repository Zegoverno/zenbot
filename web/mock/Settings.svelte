<script lang="ts">
  // Settings (mock), kept to what zenbot has today (checked against the kernel's API on
  // 2026-11-19): only Appearance is new, and it's the web app's own (this browser). Adding things
  // (a connection, a routine) opens a chat with zenbot, which knows how.
  let { theme, setTheme, ask, openFile }: { theme: 'auto' | 'light' | 'dark'; setTheme: (t: 'auto' | 'light' | 'dark') => void; ask: (text: string) => void; openFile?: (path: string, how?: { open?: boolean; edit?: boolean }) => void } = $props();

  import { me, setName } from './profilestore.svelte.ts';
  let nameDraft = $state(me.name);
  let nameSaved = $state(false);
  function saveName() {
    if (nameDraft.trim() === me.name) return;
    if (setName(nameDraft)) { nameSaved = true; setTimeout(() => (nameSaved = false), 1500); } else nameDraft = me.name;
  }

  const SECTIONS = [
    ['you', 'You'], ['appearance', 'Appearance'], ['models', 'Models'], ['connections', 'Connections'],
    ['plugins', 'Plugins'], ['memory', 'Memory'], ['updates', 'Updates'],
  ] as const;
  type Sec = (typeof SECTIONS)[number][0];
  const saved = sessionStorage.getItem('zen.mock.settings2') as Sec | null;
  let sec = $state<Sec>(saved && SECTIONS.some(([k]) => k === saved) ? saved : 'you');
  $effect(() => sessionStorage.setItem('zen.mock.settings2', sec));

  // GET /api/models: which engines are signed in, and the default model (ZEN_DEFAULT_MODEL).
  // New (Ze, 2026-11-19): sign in and out from here, and pick the default model and effort.
  // Claude and Codex sign in with their own login flow (a link and a code); OpenRouter is an API key.
  let engines = $state([
    { id: 'claude', name: 'Claude', ok: true, via: 'claude.ai account', key: false },
    { id: 'codex', name: 'Codex (OpenAI)', ok: true, via: 'ChatGPT account', key: false },
    { id: 'openrouter', name: 'OpenRouter', ok: true, via: 'API key', key: true },
  ]);
  let confirmOut = $state<string | null>(null);
  let signing = $state<string | null>(null);
  const MODELS = [
    { id: 'claude/claude-opus-5-5', name: 'Claude Opus 5.5', engine: 'claude', efforts: ['low', 'medium', 'high', 'xhigh', 'max'], def: 'medium' },
    { id: 'claude/claude-sonnet-5-5', name: 'Claude Sonnet 5.5', engine: 'claude', efforts: ['low', 'medium', 'high', 'xhigh', 'max'], def: 'high' },
    { id: 'claude/claude-haiku-5-5', name: 'Claude Haiku 5.5', engine: 'claude', efforts: ['low', 'medium', 'high', 'xhigh', 'max'], def: 'medium' },
    { id: 'codex/gpt-6.2-sol', name: 'Codex GPT-6.2-Sol', engine: 'codex', efforts: ['low', 'medium', 'high', 'xhigh', 'max', 'ultra'], def: 'low' },
  ];
  let defModel = $state('claude/claude-opus-5-5');
  let defEffort = $state('medium');
  const cur = $derived(MODELS.find((m) => m.id === defModel)!);
  const usable = $derived(MODELS.filter((m) => engines.find((e) => e.id === m.engine)?.ok));
  function pickModel(id: string) { defModel = id; const m = MODELS.find((x) => x.id === id)!; if (!m.efforts.includes(defEffort)) defEffort = m.def; }

  // GET /api/mcp: the global servers in ~/.zenbot/mcp.json, sign-in state, problems.
  const CONNS = [
    { name: 'zenfinance', login: null as string | null },
    { name: 'supabase', login: 'signed in' },
  ];

  // GET /api/plugins and /api/plugins/marketplace: all four installed, all from the marketplace.
  const PERM: Record<string, string> = { 'files:read': 'Read your files', 'files:write': 'Write your files', ai: 'Use AI' };
  let plugins = $state<{ name: string; what: string; v: string; next?: string; kernel: string[]; on: boolean; ai?: boolean }[]>([
    { name: 'zenvault', what: 'A vault method: daily plan, SMART goals, projects, tasks and a wiki, with templates and a linter.', v: '0.1.2', kernel: ['files:read', 'files:write'], on: true },
    { name: 'zenread', what: 'A calm Markdown reader: width, spacing, focus, play word by word.', v: '0.1.2', next: '0.1.3', kernel: ['files:read'], on: true },
    { name: 'zenwrite', what: 'A calm Markdown writing app: live preview, versions, AI proofreading and shrink.', v: '0.2.0', kernel: ['files:read', 'files:write', 'ai'], on: true, ai: true },
    { name: 'zensudoku', what: 'Sudoku with notes, colors, undo and hints.', v: '0.1.0', kernel: [], on: true },
  ]);

  let upd = $state<string | null>(null);

  // GET /api/memory: short-term notes, the size limit (ZEN_MEMORY_CHARS), the last sleep.
  const MEM = { used: 2962, max: 4000, notes: 14, sleep: 'Today 14:51', why: 'memory was full', promoted: 4, archived: 1, kept: 13 };

  // GET /api/version: the running commit, the latest on main, how far behind.
  const VER = { running: 'd963974', latest: 'd963974', behind: 0, checked: '15:16', commits: [] as string[] };
  let checking = $state(false);
  // New (Ze, 2026-11-19): release notes in plain words, written with each release, instead of
  // commit subjects. Shown for an update waiting and for the version running.
  const NOTES = [
    { day: 'Nov 18', items: ['Hand a task to an agent from Tasks; it keeps its chat', 'Routines can be paused and run from the app', 'Fixed: links in chat opened the wrong page'] },
    { day: 'Nov 12', items: ['Plugins: zenwrite can proofread with AI', 'Faster search in long chats'] },
  ];
</script>

{#snippet toggle(on: boolean, flip: () => void, label: string)}
  <button class="sw" class:on role="switch" aria-checked={on} aria-label={label} onclick={flip}><i></i></button>
{/snippet}

<div class="setc"><div class="set">
  <nav class="snav" aria-label="Settings sections">
    {#each SECTIONS as [k, l]}
      <button class:on={sec === k} aria-current={sec === k ? 'page' : undefined} onclick={() => (sec = k)}>{l}</button>
    {/each}
  </nav>

  <div class="sbody">
    {#if sec === 'you'}
      <h3>You</h3>
      <p class="lead">What every agent knows about you: who you are, how you like to work and what you're working on. It lives in <code>~/.zenbot/USER.md</code>; zenbot also adds to it as it learns, keeping it short.</p>
      <div class="field"><label for="me-name">Your name</label>
        <input id="me-name" bind:value={nameDraft} onblur={saveName} onkeydown={(e) => { if (e.key === 'Enter') (e.currentTarget as HTMLInputElement).blur(); if (e.key === 'Escape') { nameDraft = me.name; (e.currentTarget as HTMLInputElement).blur(); } }} />
        <span class="hint">{nameSaved ? 'Saved' : 'What your agents call you. zenbot filled it in from your first chat.'}</span></div>
      <div class="card big"><div class="cm"><b>USER.md</b><span>Updated Nov 18 · read by zenbot and Garry</span></div>
        <div class="acts">
          <button class="pill on" onclick={() => openFile?.('/home/exedev/.zenbot/USER.md', { edit: true })}>Edit</button>
          <button class="pill" onclick={() => ask('Update what you know about me: ')}>Ask zenbot to update it</button>
        </div></div>

    {:else if sec === 'appearance'}
      <h3>Appearance</h3>
      <div class="field"><span class="lb">Theme</span>
        <div class="seg">{#each [['auto', 'Auto'], ['light', 'Light'], ['dark', 'Dark']] as [k, l]}<button class:on={theme === k} onclick={() => setTheme(k as 'auto')}>{l}</button>{/each}</div>
        <span class="hint">Auto follows your device. Kept in this browser.</span></div>

    {:else if sec === 'models'}
      <h3>Models</h3>
      <p class="lead">The engines zenbot runs on. Signing out stops every model of that engine until you sign in again.</p>
      <div class="rows">
        {#each engines as e}
          <div class="li"><span class="dot" class:ok={e.ok}></span>
            <div class="cm"><b>{e.name}</b><span>{signing === e.id ? (e.key ? 'Paste your API key…' : 'Waiting for you to sign in in the new tab…') : e.ok ? `Signed in with your ${e.via}` : 'Not signed in'}</span></div>
            {#if confirmOut === e.id}
              <span class="hint">Sign out of {e.name}?</span>
              <button class="pill" onclick={() => (confirmOut = null)}>Cancel</button>
              <button class="pill warn" onclick={() => { e.ok = false; confirmOut = null; }}>Sign out</button>
            {:else if e.ok}
              <button class="pill" onclick={() => (confirmOut = e.id)}>Sign out</button>
            {:else}
              <button class="pill on" disabled={signing === e.id} onclick={() => { signing = e.id; setTimeout(() => { e.ok = true; signing = null; }, 1800); }}>{e.key ? 'Add key' : 'Sign in'}</button>
            {/if}
          </div>
        {/each}
      </div>
      <h4>New chats start on</h4>
      <div class="field"><label for="dm">Model</label>
        <select id="dm" class="dd" value={defModel} onchange={(ev) => pickModel((ev.target as HTMLSelectElement).value)}>{#each usable as m}<option value={m.id}>{m.name}</option>{/each}</select>
        <span></span>
        <span class="lb">Effort</span>
        <div class="seg">{#each cur.efforts as x}<button class:on={defEffort === x} onclick={() => (defEffort = x)}>{x}</button>{/each}</div>
        <span></span>
        <span class="hint">Any chat can still switch model from its message box. Agents with their own model keep it.</span></div>

    {:else if sec === 'connections'}
      <h3>Connections</h3>
      <p class="lead">Services zenbot can use for you (<code>~/.zenbot/mcp.json</code>). Other agents get only the ones you allow them.</p>
      <div class="rows">
        {#each CONNS as c}
          <div class="li"><span class="dot ok"></span>
            <div class="cm"><b>{c.name}</b><span>{c.login ? `Ready · ${c.login}` : 'Ready'}</span></div>
          </div>
        {/each}
        <button class="li add" onclick={() => ask('Connect a new service for me: ')}><span class="plus">+</span><div class="cm"><b>Connect a service</b><span>Opens a chat with zenbot to set it up</span></div></button>
      </div>
      <p class="hint">Plugins bring their own tools too; they're under Plugins.</p>

    {:else if sec === 'plugins'}
      <h3>Plugins</h3>
      <div class="rows">
        {#each plugins as p}
          <div class="li" class:off={!p.on}>
            <div class="cm"><b>{p.name} <small>{p.v}</small></b><span>{p.what}</span>
              <span class="perms">{#if p.kernel.length}{#each p.kernel as x}<i>{PERM[x]}</i>{/each}{:else}<i>No access to your data</i>{/if}</span></div>
            {#if p.next}<button class="pill on" disabled={upd === p.name} onclick={() => { upd = p.name; setTimeout(() => { p.v = p.next!; p.next = undefined; upd = null; }, 1500); }}>{upd === p.name ? 'Updating…' : `Update to ${p.next}`}</button>{/if}
            {@render toggle(p.on, () => (p.on = !p.on), `${p.name} on or off`)}
          </div>
        {/each}
      </div>
      <h4>Marketplace</h4>
      <p class="hint">{plugins.some((p) => p.next) ? 'Updates come from the marketplace; each one asks again if it needs new access.' : 'Everything in the marketplace is installed and up to date.'}</p>

    {:else if sec === 'memory'}
      <h3>Memory</h3>
      <p class="lead">What zenbot keeps in mind between chats. When it fills up, and every night, zenbot sorts it: lasting things move to long-term memory.</p>
      <div class="meter"><div style="width:{(MEM.used / MEM.max) * 100}%"></div></div>
      <div class="mrow"><span>Short-term memory: <b>{MEM.notes} notes</b>, {Math.round((MEM.used / MEM.max) * 100)}% full</span><span class="sp"></span>
        <span class="hint">Last sorted {MEM.sleep} ({MEM.why}): {MEM.promoted} kept for good, {MEM.archived} archived, {MEM.kept} still in mind</span></div>
      <div class="acts">
        <button class="pill" onclick={() => ask('Show me what you remember about me and let me correct it.')}>Review with zenbot</button>
        <button class="pill" onclick={() => openFile?.('/home/exedev/.zenbot/global/MEMORY.md', { open: true })}>Open MEMORY.md</button>
      </div>

    {:else if sec === 'updates'}
      <h3>Updates</h3>
      <div class="card big"><div class="cm">
        {#if VER.behind}<b>{VER.behind} new changes</b><span>Running {VER.running}, latest {VER.latest}.</span><ul class="chg">{#each VER.commits as c}<li>{c}</li>{/each}</ul>
        {:else}<b>Up to date</b><span>Running {VER.running} · checked {checking ? 'now…' : VER.checked}</span>{/if}</div>
        {#if VER.behind}<button class="pill on">Update now</button>{:else}<button class="pill" onclick={() => { checking = true; setTimeout(() => (checking = false), 1500); }}>Check again</button>{/if}</div>
      <h4>What's new</h4>
      {#each NOTES as n, i}
        <div class="note"><div class="nd">{n.day}{#if i === 0}<span class="tag">running</span>{/if}</div><ul class="chg">{#each n.items as x}<li>{x}</li>{/each}</ul></div>
      {/each}
    {/if}
  </div>
</div></div>

<style>
  .set { flex: 1; min-height: 0; display: grid; grid-template-columns: 180px minmax(0, 1fr); gap: 8px; max-width: 920px; width: 100%; margin: 0 auto; padding: 0 16px; }
  .snav { display: flex; flex-direction: column; gap: 2px; padding-top: 4px; }
  .snav button { text-align: left; padding: 7px 12px; border-radius: 10px; font-size: 14px; color: var(--dim); }
  .snav button:hover { color: var(--ink); background: var(--soft); }
  .snav button.on { color: var(--ink); background: var(--soft); font-weight: 600; }
  .sbody { overflow: auto; padding: 0 6px 100px; display: flex; flex-direction: column; gap: 14px; }
  h3 { margin: 6px 0 0; font-family: var(--serif); font-size: 22px; font-weight: 600; }
  h4 { margin: 8px 0 -4px; font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); font-weight: 650; }
  .lead { margin: -6px 0 0; color: var(--dim); font-size: 13.5px; }
  .field { display: grid; grid-template-columns: 150px auto 1fr; align-items: center; gap: 6px 12px; }
  .field label, .lb { font-size: 13.5px; font-weight: 600; }
  .field .hint { grid-column: 2 / -1; }
  .field input { font: inherit; font-size: 14px; padding: 6px 10px; border: 1px solid var(--faint); border-radius: 9px; background: var(--paper); color: var(--ink); outline: none; width: 16rem; }
  .field input:focus { border-color: var(--accent); }
  .hint { font-size: 12.5px; color: var(--dim); }
  .seg { display: inline-flex; background: var(--soft); border-radius: 99px; padding: 2px; justify-self: start; }
  .seg button { padding: 4px 12px; border-radius: 99px; font-size: 13px; color: var(--dim); }
  .seg button.on { background: var(--paper); color: var(--ink); font-weight: 600; box-shadow: 0 1px 3px var(--shadow); }
  .dd { appearance: none; font: inherit; font-size: 13.5px; color: var(--ink); background: var(--paper) url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%23888' stroke-width='2' stroke-linecap='round'%3E%3Cpath d='M7 10l5 5 5-5'/%3E%3C/svg%3E") no-repeat right 8px center / 14px; border: 1px solid var(--faint); border-radius: 9px; padding: 6px 30px 6px 10px; justify-self: start; min-width: 14rem; }
  .cards { display: grid; grid-template-columns: repeat(auto-fill, minmax(260px, 1fr)); gap: 10px; }
  .card { display: flex; align-items: center; gap: 12px; padding: 12px 14px; border: 1px solid var(--faint); border-radius: 14px; background: var(--paper); }
  .card.big { align-items: flex-start; }
  .cm { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
  .cm b { font-size: 14px; font-weight: 600; }
  .cm b small { font-weight: 400; color: var(--dim); font-size: 12px; margin-left: 4px; }
  .cm > span { font-size: 12.5px; color: var(--dim); }
  .cm em { color: var(--bad, #b5543c); font-style: normal; font-weight: 600; }
  .rows { border: 1px solid var(--faint); border-radius: 14px; background: var(--paper); overflow: hidden; }
  .li { display: flex; align-items: center; gap: 12px; padding: 11px 14px; width: 100%; text-align: left; }
  .li + .li { border-top: 1px solid var(--faint); }
  .li.off .cm { opacity: 0.55; }
  .li.add:hover { background: var(--soft); }
  .li.add .plus { width: 10px; color: var(--accent); font-size: 18px; text-align: center; }
  .dimlist .cm { opacity: 0.8; }
  .dot { width: 10px; height: 10px; border-radius: 50%; flex: none; background: var(--faint); }
  .dot.ok { background: var(--accent); }
  .dot.silent { background: color-mix(in srgb, var(--accent) 45%, var(--faint)); }
  .dot.error { background: var(--bad, #b5543c); }
  .tag { font-size: 12px; font-weight: 600; color: var(--accent); }
  .perms { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 4px; }
  .perms i { font-style: normal; font-size: 11.5px; padding: 1px 8px; border-radius: 99px; background: var(--soft); border: 1px solid var(--faint); color: var(--ink); }
  .pill { display: inline-flex; align-items: center; padding: 4px 12px; border-radius: 99px; border: 1px solid var(--faint); font-size: 12.5px; background: var(--paper); color: var(--ink); white-space: nowrap; }
  .pill:hover { border-color: var(--dim); }
  .pill.on { background: var(--accent); border-color: var(--accent); color: var(--on-accent); font-weight: 600; }
  .pill.warn { border-color: var(--bad, #b5543c); color: var(--bad, #b5543c); font-weight: 600; }
  .pill:disabled { opacity: 0.7; }
  .sw { width: 36px; height: 21px; border-radius: 99px; background: var(--faint); position: relative; flex: none; transition: background 0.15s; }
  .sw i { position: absolute; top: 2.5px; left: 2.5px; width: 16px; height: 16px; border-radius: 50%; background: var(--paper); box-shadow: 0 1px 2px var(--shadow); transition: transform 0.15s; }
  .sw.on { background: var(--accent); }
  .sw.on i { transform: translateX(15px); }
  .more { align-self: flex-start; font-size: 13px; color: var(--dim); }
  .more:hover { color: var(--ink); }
  .meter { height: 8px; border-radius: 99px; background: var(--soft); overflow: hidden; }
  .meter div { height: 100%; background: var(--accent); border-radius: 99px; }
  .mrow { display: flex; align-items: baseline; gap: 10px; font-size: 13.5px; flex-wrap: wrap; }
  .sp { flex: 1; }
  .acts { display: flex; gap: 8px; }
  code { font-size: 12px; background: var(--soft); padding: 1px 5px; border-radius: 5px; }
  .dot.none { background: var(--faint); }
  .note { display: grid; grid-template-columns: 110px 1fr; gap: 10px; padding: 2px 4px; }
  .nd { font-size: 13px; font-weight: 600; display: flex; flex-direction: column; gap: 4px; align-items: flex-start; }
  .note .chg { margin: 0; }
  .chg { margin: 6px 0 0; padding-left: 18px; font-size: 13px; }
  /* One column when the panel is narrow: a phone, or Settings beside a chat. */
  .setc { flex: 1; min-height: 0; display: flex; container-type: inline-size; }
  .card { flex-wrap: wrap; }
  .card > .cm { flex: 1 1 200px; }
  @container (max-width: 640px) {
    .set { grid-template-columns: 1fr; grid-template-rows: auto minmax(0, 1fr); padding: 0 10px; }
    .snav { flex-direction: row; align-items: center; overflow-x: auto; gap: 4px; padding-bottom: 4px; scrollbar-width: none; }
    .snav button { white-space: nowrap; }
    .field { grid-template-columns: 1fr; }
    .field .hint { grid-column: 1; }
  }
</style>
