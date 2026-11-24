<script lang="ts">
  // Settings: You (name, USER.md), Appearance (this browser's theme), Models (engines' sign-in, the
  // default model and effort for new chats), Connections (MCP servers), Plugins (installed ones:
  // on/off, AI, updates, remove, default openers; the marketplace with installs that show what a
  // plugin needs before anything is installed), Memory and Updates (version, release notes, the
  // upgrade). Adding things (a connection, a routine) opens a chat with zenbot, which knows how.
  // (The redesign mock's Settings, on the kernel's API.)
  import { onMount, untrack } from 'svelte';
  import { onTopic } from '../events.svelte.ts';
  import { zen, pluginApi, type MarketEntry, type Plugin } from '../api.ts';
  import { me, setName, patchPrefs, models, modelOf, loadModels, agents, EFFORT } from '../live.svelte.ts';
  import { loadPlugins, permissionWords, plugins as host } from '../plugins.svelte.ts';
  import { app, notice } from '../app.svelte.ts';
  let { theme, setTheme, ask, openFile }: { theme: 'auto' | 'light' | 'dark'; setTheme: (t: 'auto' | 'light' | 'dark') => void; ask: (text: string) => void; openFile?: (path: string, how?: { open?: boolean; edit?: boolean }) => void } = $props();

  async function call<T>(path: string, method = 'GET', body?: unknown): Promise<T> {
    const r = await fetch(`/api/zen/${path}`, { method, headers: method === 'GET' ? {} : { 'content-type': 'application/json', 'x-zen-web': '1' }, body: body === undefined ? undefined : JSON.stringify(body) });
    const data = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(data.error || `HTTP ${r.status}`);
    return data as T;
  }

  // ---- You ----
  // USER.md is edited here, through the kernel (GET/PUT /prompt-files/USER.md: backed up, size-checked).
  let userEdit = $state<{ text: string; limit: number; saving: boolean; error: string } | null>(null);
  async function editUser() {
    try {
      const f = await call<{ text: string; limit: number }>('prompt-files/USER.md');
      userEdit = { text: f.text, limit: f.limit, saving: false, error: '' };
    } catch (e) { notice(`couldn't open USER.md: ${(e as Error).message}`); }
  }
  async function saveUser() {
    if (!userEdit) return;
    userEdit.saving = true;
    userEdit.error = '';
    try {
      await call('prompt-files/USER.md', 'PUT', { text: userEdit.text });
      userEdit = null;
      userUpdated = new Date().toLocaleDateString('en', { month: 'short', day: 'numeric' });
      notice('USER.md saved');
    } catch (e) { if (userEdit) { userEdit.saving = false; userEdit.error = (e as Error).message; } }
  }
  let nameDraft = $state(me.full);
  $effect(() => { if (me.loaded && !nameDraft) nameDraft = me.full; });
  let nameSaved = $state(false);
  async function saveName() {
    if (nameDraft.trim() === me.full) return;
    try {
      if (await setName(nameDraft)) { nameSaved = true; setTimeout(() => (nameSaved = false), 1500); } else nameDraft = me.full;
    } catch (e) { notice(`couldn't save your name: ${(e as Error).message}`); nameDraft = me.full; }
  }
  const readers = $derived(agents.list.map((a) => a.name).join(' and '));
  let userUpdated = $state('');

  const SECTIONS = [
    ['you', 'You'], ['appearance', 'Appearance'], ['models', 'Models'], ['connections', 'Connections'],
    ['plugins', 'Plugins'], ['memory', 'Memory'], ['updates', 'Updates'],
  ] as const;
  type Sec = (typeof SECTIONS)[number][0];
  const saved = sessionStorage.getItem('zen.settings') as Sec | null;
  let sec = $state<Sec>(app.settings === 'marketplace' ? 'plugins' : SECTIONS.some(([k]) => k === app.settings) ? (app.settings as Sec) : saved && SECTIONS.some(([k]) => k === saved) ? saved : 'you');
  $effect(() => sessionStorage.setItem('zen.settings', sec));

  // ---- Models: the engines' sign-in, and what new chats start on (prefs) ----
  type Engine = { id: string; signed_in: boolean; login: { status: string; url?: string; code?: string; error?: string }; runtime_note?: string | null };
  const ENGINE: Record<string, { name: string; via: string; key: boolean }> = {
    claude: { name: 'Claude', via: 'claude.ai account', key: false },
    codex: { name: 'Codex (OpenAI)', via: 'ChatGPT account', key: false },
    openrouter: { name: 'OpenRouter', via: 'API key', key: true },
  };
  let engines = $state<Engine[]>([]);
  let engineError = $state('');
  let confirmOut = $state<string | null>(null);
  let signing = $state<string | null>(null);
  let instructions = $state('');
  let keyDraft = $state('');
  let alive = true;
  let timer: ReturnType<typeof setTimeout> | undefined;
  async function refreshEngines() {
    try {
      engines = (await call<{ engines: Engine[] }>('engines')).engines;
      engineError = '';
      const pending = engines.find((e) => ['starting', 'pending'].includes(e.login.status));
      if (pending && alive) timer = setTimeout(refreshEngines, 2000);
      else if (signing && engines.find((e) => e.id === signing)?.signed_in) { signing = null; void loadModels(); }
    } catch (e) { engineError = (e as Error).message; }
  }
  async function signIn(id: string) {
    signing = id; instructions = ''; engineError = '';
    if (ENGINE[id]?.key) return;
    try {
      const r = await call<{ instructions?: string; url?: string }>(`engines/${id}/login`, 'POST');
      instructions = r.instructions ?? '';
      if (r.url) window.open(r.url, '_blank', 'noopener,noreferrer');
      await refreshEngines();
    } catch (e) { engineError = (e as Error).message; signing = null; }
  }
  async function signOut(id: string) {
    confirmOut = null;
    try { await call(`engines/${id}/logout`, 'POST'); await refreshEngines(); await loadModels(); }
    catch (e) { engineError = (e as Error).message; }
  }
  async function saveKey() {
    try { const r = await call<{ message?: string }>('engines/openrouter/key', 'PUT', { key: keyDraft }); keyDraft = ''; signing = null; notice(r.message ?? 'Key saved', 4000); await refreshEngines(); }
    catch (e) { engineError = (e as Error).message; }
  }
  const login = (id: string) => engines.find((e) => e.id === id)?.login;
  let defModel = $state('');
  let defEffort = $state('');
  const usable = $derived(models.list.filter((m) => models.raw?.authenticated[m.id.split('/')[0]] !== false));
  const cur = $derived(modelOf(defModel || models.default));
  async function pickModel(id: string) {
    defModel = id;
    const m = modelOf(id);
    if (!m.efforts.includes(defEffort)) defEffort = m.def;
    await savePrefs();
  }
  async function pickEffort(x: string) { defEffort = x; await savePrefs(); }
  async function savePrefs() {
    try { await patchPrefs({ default_model: defModel, default_effort: defEffort || null }); me.defaultModel = defModel; me.defaultEffort = defEffort || null; }
    catch (e) { notice(`couldn't save the default model: ${(e as Error).message}`); }
  }

  // ---- Connections (GET /api/mcp: the global servers, sign-in state, problems) ----
  let mcp = $state<{ servers: Record<string, number>; login: Record<string, string>; problems: string[] } | null>(null);
  const CONNS = $derived(Object.keys(mcp?.servers ?? {}).filter((n) => !n.startsWith('plugin-')).map((name) => ({ name, login: mcp?.login[name] ?? null })));

  // ---- Plugins (installed, the marketplace) ----
  const PERM: Record<string, string> = { 'files:read': 'Read your files', 'files:write': 'Write your files', ai: 'Use AI' };
  const perm = (x: string) => PERM[x] ?? permissionWords(x);
  let market = $state<MarketEntry[] | null>(null);
  let marketError = $state('');
  let upd = $state<string | null>(null);
  let removing = $state<string | null>(null);
  let asking = $state<{ entry: MarketEntry; permissions: string[] } | null>(null);
  const nextOf = (p: Plugin) => { const m = market?.find((x) => x.name === p.name); return m?.update_available && m.installed === p.version ? m.version : null; };
  const fresh = $derived((market ?? []).filter((m) => !m.installed));
  async function loadMarket() {
    try { market = await pluginApi.marketplace(); marketError = ''; } catch (e) { marketError = (e as Error).message; }
  }
  async function flip(p: Plugin) {
    try { await pluginApi.setEnabled(p.name, !p.enabled); p.enabled = !p.enabled; } catch (e) { notice(`couldn't switch ${p.title || p.name}: ${(e as Error).message}`); }
    await loadPlugins();
  }
  async function flipAi(p: Plugin) {
    const on = p.ai_enabled !== false;
    try { await pluginApi.setAi(p.name, !on); p.ai_enabled = !on; } catch (e) { notice(`couldn't switch AI for ${p.title || p.name}: ${(e as Error).message}`); }
    await loadPlugins();
  }
  async function remove(p: Plugin) {
    removing = null;
    try { await pluginApi.remove(p.name); notice(`Removed ${p.title || p.name}; its data stays`, 3000); } catch (e) { notice(`couldn't remove ${p.title || p.name}: ${(e as Error).message}`); }
    await Promise.all([loadPlugins(), loadMarket()]);
  }
  /** Asks the kernel what the plugin needs; nothing is installed until the owner approves. */
  async function install(entry: MarketEntry, approve?: string[]) {
    upd = entry.name; asking = null;
    try {
      const r = await pluginApi.install(entry.name, approve);
      // The release may have changed since the question: never report success when the kernel asks again.
      if ('needs_approval' in r && r.needs_approval) asking = { entry, permissions: r.permissions ?? [] };
      else { notice(`${entry.installed ? 'Updated' : 'Installed'} ${entry.title || entry.name} ${entry.version}`, 3000); await Promise.all([loadPlugins(), loadMarket()]); }
    } catch (e) { notice(`couldn't install ${entry.title || entry.name}: ${(e as Error).message}`); }
    upd = null;
  }
  /** Every pattern and mode an enabled plugin opens, with who can open it. */
  const slots = $derived.by(() => {
    const m = new Map<string, { key: string; pattern: string; mode: string; options: { value: string; label: string }[] }>();
    for (const p of host.list.filter((x) => x.enabled))
      for (const o of p.contributes.openers ?? [])
        for (const pattern of o.patterns) {
          const key = `${pattern}:${o.mode}`;
          if (!m.has(key)) m.set(key, { key, pattern, mode: o.mode, options: [{ value: 'builtin', label: o.mode === 'edit' ? 'Built-in editor' : 'Built-in reader' }] });
          m.get(key)!.options.push({ value: `${p.name}/${o.id}`, label: `${o.title} · ${p.title || p.name}` });
        }
    return [...m.values()];
  });
  async function setDefault(key: string, value: string) {
    try { await pluginApi.setDefault(key, value); host.defaults[key] = value; } catch (e) { notice(`couldn't set the default: ${(e as Error).message}`); }
  }

  // ---- Memory (GET /api/memory: short-term notes, the size limit, the last sleep) ----
  type Sleep = { ended_at?: string; trigger?: string; promoted?: number; dropped?: number; kept?: number; error?: string | null };
  let mem = $state<{ memories: { text: string }[]; size?: number; limit?: number; last_sleep?: Sleep | null } | null>(null);
  const used = $derived(mem ? mem.size ?? mem.memories.reduce((n, m) => n + m.text.length, 0) : 0);
  const max = $derived(mem?.limit ?? 4000);
  const sleepAt = (s: Sleep | null | undefined) => { if (!s?.ended_at) return 'not yet'; const d = new Date(s.ended_at); const today = d.toDateString() === new Date().toDateString(); return `${today ? 'Today' : d.toLocaleDateString('en', { month: 'short', day: 'numeric' })} ${d.toLocaleTimeString('en-GB', { hour: 'numeric', minute: '2-digit' })}`; };
  const why = (t?: string) => (t === 'size' ? 'memory was full' : t === 'schedule' || t === 'nightly' ? 'nightly' : t ?? '');

  // ---- Updates (GET /api/version: the running commit, the latest on main, release notes) ----
  type Ver = { running: string; latest?: string; behind?: number; available?: boolean; commits?: string[]; error?: string; checked_at?: string; notes?: { version: string; date: string; items: string[] }[] };
  let ver = $state<Ver | null>(null);
  let checking = $state(false);
  let upgrading = $state(false);
  async function check(refresh = false) {
    checking = true;
    try { ver = await zen.version(refresh) as Ver; } catch (e) { notice(`couldn't check for updates: ${(e as Error).message}`); }
    checking = false;
  }
  async function upgrade() {
    upgrading = true;
    try { await zen.upgrade(); notice('Updating: zenbot restarts once no session is working', 8000); } catch (e) { notice(`couldn't start the update: ${(e as Error).message}`); upgrading = false; }
  }
  const at = (iso?: string) => (iso ? new Date(iso).toLocaleTimeString('en-GB', { hour: 'numeric', minute: '2-digit' }) : '');
  const dayOf = (d: string) => { const x = new Date(d + 'T12:00:00'); return isNaN(+x) ? d : x.toLocaleDateString('en', { month: 'short', day: 'numeric' }); };

  function userStat() {
    fetch(`/api/fs/stat?path=${encodeURIComponent('~/.zenbot/USER.md')}`).then((r) => r.json()).then((s: { mtime?: number }) => { if (s.mtime) userUpdated = new Date(s.mtime).toLocaleDateString('en', { month: 'short', day: 'numeric' }); }).catch(() => {});
  }
  // Preferences changed elsewhere (another tab, an agent) show here, unless this field was being
  // changed: a draft that differs from what was last loaded is kept.
  let lastFull = me.full;
  $effect(() => {
    const f = me.full;
    untrack(() => { if (nameDraft === lastFull || !nameDraft) nameDraft = f; lastFull = f; });
  });
  $effect(() => {
    const m = me.defaultModel, e = me.defaultEffort;
    untrack(() => { if (m) defModel = m; defEffort = e ?? ''; });
  });
  onMount(() => {
    defModel = me.defaultModel || models.default;
    defEffort = me.defaultEffort ?? '';
    void refreshEngines();
    void loadPlugins(); void loadMarket();
    call<typeof mcp>('mcp').then((v) => (mcp = v)).catch(() => {});
    call<typeof mem>('memory').then((v) => (mem = v)).catch(() => {});
    void check();
    userStat();
    // Live: each section refetches when its topic changes (an open USER.md edit is left alone).
    const offs = [
      onTopic('models', refreshEngines),
      onTopic('mcp', () => call<typeof mcp>('mcp').then((v) => (mcp = v)).catch(() => {})),
      onTopic('memory', () => call<typeof mem>('memory').then((v) => (mem = v)).catch(() => {})),
      onTopic('version', () => check()),
      onTopic('prefs', userStat),
    ];
    return () => { alive = false; clearTimeout(timer); offs.forEach((off) => off()); };
  });
  $effect(() => { if (!defModel && models.default) defModel = me.defaultModel || models.default; });
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
      <div class="card big"><div class="cm"><b>USER.md</b><span>{userUpdated ? `Updated ${userUpdated} · ` : ''}read by {readers}</span></div>
        {#if !userEdit}
          <div class="acts">
            <button class="pill on" onclick={editUser}>Edit</button>
            <button class="pill" onclick={() => ask('Update what you know about me: ')}>Ask zenbot to update it</button>
          </div>
        {/if}</div>
      {#if userEdit}
        <div class="uedit">
          <textarea aria-label="USER.md" bind:value={userEdit.text} spellcheck="true" onkeydown={(e) => { if (e.key === 's' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); void saveUser(); } if (e.key === 'Escape') userEdit = null; }}></textarea>
          <div class="acts"><span class="hint" class:err={userEdit.text.length > userEdit.limit || !!userEdit.error}>{userEdit.error || `${userEdit.text.length.toLocaleString()} / ${userEdit.limit.toLocaleString()} characters`}</span><span class="sp"></span>
            <button class="pill" onclick={() => (userEdit = null)}>Cancel</button>
            <button class="pill on" disabled={userEdit.saving || userEdit.text.length > userEdit.limit} onclick={saveUser}>{userEdit.saving ? 'Saving…' : 'Save'}</button></div>
        </div>
      {/if}

    {:else if sec === 'appearance'}
      <h3>Appearance</h3>
      <div class="field"><span class="lb">Theme</span>
        <div class="seg">{#each [['auto', 'Auto'], ['light', 'Light'], ['dark', 'Dark']] as [k, l]}<button class:on={theme === k} onclick={() => setTheme(k as 'auto')}>{l}</button>{/each}</div>
        <span class="hint">Auto follows your device. Kept in this browser.</span></div>

    {:else if sec === 'models'}
      <h3>Models</h3>
      <p class="lead">The engines zenbot runs on. Signing out stops every model of that engine until you sign in again.</p>
      {#if engineError}<p class="hint err" role="alert">{engineError}</p>{/if}
      <div class="rows">
        {#each engines as en (en.id)}
          {@const e = { id: en.id, ok: en.signed_in, ...(ENGINE[en.id] ?? { name: en.id, via: 'account', key: false }) }}
          {@const l = login(en.id)}
          <div class="li"><span class="dot" class:ok={e.ok}></span>
            <div class="cm"><b>{e.name}</b><span>{signing === e.id ? (e.key ? 'Paste your API key…' : l?.code ? `Enter the code ${l.code} on the sign-in page` : instructions || 'Waiting for you to sign in in the new tab…') : e.ok ? `Signed in with your ${e.via}` : 'Not signed in'}</span>
              {#if signing === e.id && l?.url}<span><a href={l.url} target="_blank" rel="noopener noreferrer">Open the sign-in page ↗</a></span>{/if}
              {#if l?.error}<span class="err">{l.error}</span>{/if}
              {#if en.runtime_note && signing === e.id}<span>{en.runtime_note}</span>{/if}</div>
            {#if signing === e.id && e.key}
              <input class="key" type="password" placeholder="sk-or-…" bind:value={keyDraft} aria-label="OpenRouter API key" onkeydown={(ev) => ev.key === 'Enter' && keyDraft.trim() && saveKey()} />
              <button class="pill" onclick={() => (signing = null)}>Cancel</button>
              <button class="pill on" disabled={!keyDraft.trim()} onclick={saveKey}>Save key</button>
            {:else if confirmOut === e.id}
              <span class="hint">Sign out of {e.name}?</span>
              <button class="pill" onclick={() => (confirmOut = null)}>Cancel</button>
              <button class="pill warn" onclick={() => signOut(e.id)}>Sign out</button>
            {:else if e.ok}
              <button class="pill" onclick={() => (confirmOut = e.id)}>Sign out</button>
            {:else}
              <button class="pill on" disabled={signing === e.id} onclick={() => signIn(e.id)}>{e.key ? 'Add key' : 'Sign in'}</button>
            {/if}
          </div>
        {:else}
          <div class="li"><div class="cm"><span>Loading…</span></div></div>
        {/each}
      </div>
      <h4>New chats start on</h4>
      <div class="field"><label for="dm">Model</label>
        <select id="dm" class="dd" value={defModel} onchange={(ev) => pickModel((ev.target as HTMLSelectElement).value)}>{#each usable as m}<option value={m.id}>{m.group === 'Claude' ? 'Claude ' : m.group === 'OpenAI' ? 'Codex ' : ''}{m.name}</option>{/each}</select>
        <span></span>
        <span class="lb">Effort</span>
        <div class="seg">{#each cur.efforts as x}<button class:on={(defEffort || cur.def) === x} title={EFFORT[x]?.[1]} onclick={() => pickEffort(x)}>{x}</button>{/each}</div>
        <span></span>
        <span class="hint">Any chat can still switch model from its message box. Agents with their own model keep it.</span></div>

    {:else if sec === 'connections'}
      <h3>Connections</h3>
      <p class="lead">Services zenbot can use for you (<code>~/.zenbot/mcp.json</code>). Other agents get only the ones you allow them.</p>
      <div class="rows">
        {#each CONNS as c (c.name)}
          <div class="li"><span class="dot ok"></span>
            <div class="cm"><b>{c.name}</b><span>{c.login ? `Ready · ${c.login}` : 'Ready'}</span></div>
          </div>
        {/each}
        {#each mcp?.problems ?? [] as pr}
          <div class="li"><span class="dot error"></span><div class="cm"><b>Problem</b><span>{pr}</span></div></div>
        {/each}
        <button class="li add" onclick={() => ask('Connect a new service for me: ')}><span class="plus">+</span><div class="cm"><b>Connect a service</b><span>Opens a chat with zenbot to set it up</span></div></button>
      </div>
      <p class="hint">Plugins bring their own tools too; they're under Plugins.</p>

    {:else if sec === 'plugins'}
      <h3>Plugins</h3>
      {#if host.error}<p class="hint err" role="alert">{host.error}</p>{/if}
      <div class="rows">
        {#each host.list as p (p.name)}
          {@const next = nextOf(p)}
          <div class="li" class:off={!p.enabled} aria-label={p.title || p.name}>
            <div class="cm"><b>{p.name} <small>{p.version}</small></b><span>{p.description}</span>
              <span class="perms">{#if p.permissions.length}{#each p.permissions as x}<i>{perm(x)}</i>{/each}{:else}<i>No access to your data</i>{/if}
                {#if p.permissions.includes('ai')}<button class="aisw" role="switch" aria-checked={p.ai_enabled !== false} aria-label="AI for {p.title || p.name}" onclick={() => flipAi(p)}>AI {p.ai_enabled !== false ? 'on' : 'off'} · {p.ai_calls_30d ?? 0} call{(p.ai_calls_30d ?? 0) === 1 ? '' : 's'} in 30 days</button>{/if}</span>
              {#if removing === p.name}<span class="confirm">Remove {p.title || p.name}? Its data stays. <button class="pill warn" onclick={() => remove(p)}>Remove</button> <button class="pill" onclick={() => (removing = null)}>Cancel</button></span>{/if}</div>
            {#if next}{@const m = market?.find((x) => x.name === p.name)}<button class="pill on" disabled={upd === p.name} onclick={() => m && install(m)}>{upd === p.name ? 'Updating…' : `Update to ${next}`}</button>{/if}
            <button class="pill rm" title="Remove {p.title || p.name}" onclick={() => (removing = p.name)}>Remove</button>
            {@render toggle(p.enabled, () => flip(p), `${p.name} on or off`)}
          </div>
        {:else}
          <div class="li"><div class="cm"><span>{host.loaded ? 'No plugins yet.' : 'Loading…'}</span></div></div>
        {/each}
      </div>
      {#if slots.length}
        <h4>Open files with</h4>
        <div class="rows">
          {#each slots as s (s.key)}
            <div class="li"><div class="cm"><b>{s.pattern}</b><span>{s.mode === 'edit' ? 'Editing' : 'Reading'}</span></div>
              <select class="dd" aria-label="Default for {s.pattern} {s.mode}" value={host.defaults[s.key] ?? 'builtin'} onchange={(e) => setDefault(s.key, (e.currentTarget as HTMLSelectElement).value)}>
                {#each s.options as o}<option value={o.value}>{o.label}</option>{/each}
              </select></div>
          {/each}
        </div>
      {/if}
      <h4>Marketplace</h4>
      {#if marketError}<p class="hint err">{marketError}</p>{/if}
      {#if asking}
        <div class="card big" role="alertdialog" aria-label="Install {asking.entry.title || asking.entry.name}"><div class="cm"><b>Install {asking.entry.title || asking.entry.name} {asking.entry.version}?</b><span>It asks to:</span>
          <span class="perms">{#each asking.permissions as x}<i>{perm(x)}</i>{:else}<i>No access to your data</i>{/each}</span></div>
          <div class="acts"><button class="pill" onclick={() => (asking = null)}>Cancel</button><button class="pill on" onclick={() => asking && install(asking.entry, asking.permissions)}>Allow and install</button></div></div>
      {/if}
      {#if fresh.length}
        <div class="rows">
          {#each fresh as m (m.name)}
            <div class="li" aria-label={m.title || m.name}><div class="cm"><b>{m.title || m.name} <small>{m.version}{m.author ? ` · ${m.author}` : ''}</small></b><span>{m.description ?? ''}</span>
              <span class="perms">{#each m.kernel ?? [] as x}<i>{perm(x)}</i>{:else}<i>No access to your data</i>{/each}</span></div>
              <button class="pill on" disabled={upd === m.name} onclick={() => install(m)}>{upd === m.name ? 'Installing…' : 'Install'}</button></div>
          {/each}
        </div>
      {/if}
      <p class="hint">{market === null ? 'Loading the marketplace…' : host.list.some((p) => nextOf(p)) ? 'Updates come from the marketplace; each one asks again if it needs new access.' : fresh.length ? 'Each plugin says what it needs before anything is installed.' : 'Everything in the marketplace is installed and up to date.'}</p>

    {:else if sec === 'memory'}
      <h3>Memory</h3>
      <p class="lead">What zenbot keeps in mind between chats. When it fills up, and every night, zenbot sorts it: lasting things move to long-term memory.</p>
      <div class="meter"><div style="width:{Math.min(100, (used / max) * 100)}%"></div></div>
      <div class="mrow"><span>Short-term memory: <b>{mem?.memories.length ?? 0} notes</b>, {Math.round((used / max) * 100)}% full</span><span class="sp"></span>
        {#if mem?.last_sleep}<span class="hint">Last sorted {sleepAt(mem.last_sleep)}{why(mem.last_sleep.trigger) ? ` (${why(mem.last_sleep.trigger)})` : ''}: {mem.last_sleep.promoted ?? 0} kept for good, {mem.last_sleep.dropped ?? 0} archived, {mem.last_sleep.kept ?? 0} still in mind</span>{/if}</div>
      <div class="acts">
        <button class="pill" onclick={() => ask('Show me what you remember about me and let me correct it.')}>Review with zenbot</button>
        <button class="pill" title="Read-only: zenbot keeps it; ask it to change what it remembers" onclick={() => openFile?.(`${app.vault}/MEMORY.md`, { open: true })}>View MEMORY.md <small class="ro">read-only</small></button>
      </div>

    {:else if sec === 'updates'}
      <h3>Updates</h3>
      <div class="card big"><div class="cm">
        {#if !ver}<b>Checking…</b>
        {:else if ver.error}<b>Couldn't check</b><span>{ver.error}</span>
        {:else if ver.behind}<b>{ver.behind} new changes</b><span>Running {ver.running}, latest {ver.latest}.</span><ul class="chg">{#each ver.commits ?? [] as c}<li>{c}</li>{/each}</ul>
        {:else}<b>Up to date</b><span>Running {ver.running} · checked {checking ? 'now…' : at(ver.checked_at) || 'just now'}</span>{/if}</div>
        {#if ver?.behind}<button class="pill on" disabled={upgrading} onclick={upgrade}>{upgrading ? 'Updating…' : 'Update now'}</button>{:else}<button class="pill" disabled={checking} onclick={() => check(true)}>Check again</button>{/if}</div>
      <h4>What's new</h4>
      {#each ver?.notes ?? [] as n, i}
        <div class="note"><div class="nd">{dayOf(n.date)}{#if i === 0}<span class="tag">running</span>{/if}</div><ul class="chg">{#each n.items as x}<li>{x}</li>{/each}</ul></div>
      {:else}
        <p class="hint">No release notes yet.</p>
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
  .hint.err { color: var(--err, #b5543c); }
  .seg { display: inline-flex; background: var(--soft); border-radius: 99px; padding: 2px; justify-self: start; }
  .seg button { padding: 4px 12px; border-radius: 99px; font-size: 13px; color: var(--dim); }
  .seg button.on { background: var(--paper); color: var(--ink); font-weight: 600; box-shadow: 0 1px 3px var(--shadow); }
  .dd { appearance: none; font: inherit; font-size: 13.5px; color: var(--ink); background: var(--paper) url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%23888' stroke-width='2' stroke-linecap='round'%3E%3Cpath d='M7 10l5 5 5-5'/%3E%3C/svg%3E") no-repeat right 8px center / 14px; border: 1px solid var(--faint); border-radius: 9px; padding: 6px 30px 6px 10px; justify-self: start; min-width: 14rem; }
  .card { display: flex; align-items: center; gap: 12px; padding: 12px 14px; border: 1px solid var(--faint); border-radius: 14px; background: var(--paper); }
  .card.big { align-items: flex-start; }
  .uedit { display: flex; flex-direction: column; gap: 8px; }
  .uedit textarea { min-height: 18rem; resize: vertical; padding: 12px 14px; border: 1px solid var(--faint); border-radius: 14px; background: var(--paper); color: var(--ink); font-family: var(--mono); font-size: 13px; line-height: 1.55; outline: none; }
  .uedit textarea:focus { border-color: color-mix(in srgb, var(--accent) 45%, var(--faint)); }
  .uedit .acts { display: flex; align-items: center; gap: 8px; }
  .uedit .sp { flex: 1; }
  .ro { font-size: 11px; color: var(--dim); margin-left: 6px; }
  .cm { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
  .cm b { font-size: 14px; font-weight: 600; }
  .cm b small { font-weight: 400; color: var(--dim); font-size: 12px; margin-left: 4px; }
  .cm > span { font-size: 12.5px; color: var(--dim); }
  .rows { border: 1px solid var(--faint); border-radius: 14px; background: var(--paper); overflow: hidden; }
  .li { display: flex; align-items: center; gap: 12px; padding: 11px 14px; width: 100%; text-align: left; }
  .li + .li { border-top: 1px solid var(--faint); }
  .li.off .cm { opacity: 0.55; }
  .li.add:hover { background: var(--soft); }
  .li.add .plus { width: 10px; color: var(--accent); font-size: 18px; text-align: center; }
  .dot { width: 10px; height: 10px; border-radius: 50%; flex: none; background: var(--faint); }
  .dot.ok { background: var(--accent); }
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
  .meter { height: 8px; border-radius: 99px; background: var(--soft); overflow: hidden; }
  .meter div { height: 100%; background: var(--accent); border-radius: 99px; }
  .mrow { display: flex; align-items: baseline; gap: 10px; font-size: 13.5px; flex-wrap: wrap; }
  .sp { flex: 1; }
  .acts { display: flex; gap: 8px; }
  code { font-size: 12px; background: var(--soft); padding: 1px 5px; border-radius: 5px; }
  .err { color: var(--err); }
  .key { font: inherit; font-size: 13px; padding: 4px 10px; border: 1px solid var(--faint); border-radius: 9px; background: var(--paper); color: var(--ink); width: 12rem; }
  .aisw { font-size: 11.5px; padding: 1px 8px; border-radius: 99px; border: 1px solid var(--faint); color: var(--accent); }
  .aisw[aria-checked='false'] { color: var(--dim); }
  .pill.rm { color: var(--dim); }
  .confirm { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; margin-top: 6px; font-size: 12.5px; color: var(--ink) !important; }
  .cm a { color: var(--accent); font-weight: 600; }
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
