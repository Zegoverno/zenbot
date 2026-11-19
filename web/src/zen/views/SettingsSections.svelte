<script lang="ts">
  import { onMount } from 'svelte';
  import { zen, type Models } from '../api.ts';
  let { section }: { section: string } = $props();
  type Prefs = { name?: string; default_model?: string; default_effort?: string };
  type Notes = { running: string; available?: boolean; behind?: number; error?: string; notes?: {version: string; date: string; items: string[]}[] };
  let prefs = $state<Prefs>({}), name = $state(''), user = $state(''), savedUser = $state(''), limit = $state(0);
  let models = $state<Models | null>(null), version = $state<Notes | null>(null);
  let mcp = $state<{ servers: Record<string, number>; login: Record<string, string>; problems: string[] } | null>(null);
  let memory = $state<{ memories: unknown[]; size: number; last_sleep: unknown } | null>(null);
  let userLoaded = $state(false);
  let error = $state(''), prefError = $state(''), userError = $state(''), busy = $state(false);
  type Engine = { id: string; signed_in: boolean; login: {status: string; url?: string; code?: string; error?: string}; runtime_note?: string | null };
  let engines = $state<Engine[] | null>(null), engineError = $state(''), modelsError = $state(''), engineBusy = $state(false), instructions = $state(''), key = $state(''), keyMessage = $state('');
  let timer: ReturnType<typeof setTimeout> | undefined;
  let alive = false;
  let availableModels = $derived(models?.models.filter(m => models!.authenticated[m.id.split('/')[0]] === true) ?? []);
  async function refreshModels() { try { models = await zen.models(); modelsError = ''; } catch (e) { models = null; modelsError = (e as Error).message; } }
  async function refreshEngines() {
    try {
      engines = (await call<{engines: Engine[]}>('engines')).engines; engineError = '';
      const status = engines.find(e => e.id === 'codex')?.login.status;
      if (status === 'starting' || status === 'pending') { if (alive) timer = setTimeout(refreshEngines, 2000); }
      else if (status === 'complete') await refreshModels();
    } catch (e) { engineError = (e as Error).message; }
  }
  async function login(id: string) {
    engineBusy = true; engineError = ''; instructions = '';
    try { const result = await call<{instructions?: string}>(`engines/${id}/login`, 'POST'); instructions = result.instructions ?? ''; await refreshEngines(); }
    catch (e) { engineError = (e as Error).message; } finally { engineBusy = false; }
  }
  async function logout(id: string) {
    if (!window.confirm(`Sign out of ${id}?`)) return;
    engineBusy = true; engineError = '';
    try { await call(`engines/${id}/logout`, 'POST'); await refreshEngines(); await refreshModels(); }
    catch (e) { engineError = (e as Error).message; } finally { engineBusy = false; }
  }
  async function saveKey() {
    engineBusy = true; engineError = ''; keyMessage = '';
    try { const result = await call<{message: string}>('engines/openrouter/key', 'PUT', {key}); key = ''; keyMessage = result.message; await refreshEngines(); }
    catch (e) { engineError = (e as Error).message; } finally { engineBusy = false; }
  }
  let theme = $state<'auto'|'light'|'dark'>('auto');
  async function call<T>(path: string, method = 'GET', body?: unknown): Promise<T> {
    const r = await fetch(`/api/zen/${path}`, { method, headers: method === 'GET' ? {} : { 'content-type': 'application/json', 'x-zen-web': '1' }, body: body === undefined ? undefined : JSON.stringify(body) });
    const data = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(data.error || `HTTP ${r.status}`);
    return data as T;
  }
  async function patch(p: Record<string, unknown>) { try { prefs = await call<Prefs>('prefs', 'PATCH', p); name = prefs.name ?? ''; prefError = ''; } catch (e) { prefError = (e as Error).message; } }
  async function saveUser() { busy = true; try { const r = await call<{text:string}>('prompt-files/USER.md', 'PUT', {text:user}); savedUser = r.text; userError = ''; } catch(e) { userError = (e as Error).message; } finally { busy = false; } }
  async function refreshVersion() { try { version = await call<Notes>('version?refresh=true'); } catch(e) { error = (e as Error).message; } }
  function setTheme(t: typeof theme) { theme = t; document.documentElement.dataset.theme = t; localStorage.setItem('zen.theme', t); window.dispatchEvent(new CustomEvent('zen-theme', {detail:t})); }
  onMount(() => {
    const t = localStorage.getItem('zen.theme'); if (t === 'light' || t === 'dark') theme = t;
    call<Prefs>('prefs').then(p => { prefs = p; name = p.name ?? ''; }).catch(e => prefError = e.message);
    call<{text:string;limit:number}>('prompt-files/USER.md').then(p => { user = savedUser = p.text; limit = p.limit; userLoaded = true; }).catch(e => userError = e.message);
    alive = true; refreshModels(); refreshEngines();
    call<typeof mcp>('mcp').then(v => mcp = v).catch(e => error = e.message);
    call<typeof memory>('memory').then(v => memory = v).catch(e => error = e.message);
    call<Notes>('version').then(v => version = v).catch(e => error = e.message);
    return () => { alive = false; clearTimeout(timer); };
  });
</script>
<div class="body">
{#if section === 'you'}
  <h3>You</h3><p>What your agents know about you, in USER.md.</p>
  <label>Your name <input maxlength="100" bind:value={name} /></label><button onclick={() => patch({name: name.trim() || null})}>Save name</button>
  {#if prefError}<p role="alert">Could not load or save preferences: {prefError}</p>{/if}
  <label class="file">USER.md <textarea rows="16" bind:value={user}></textarea></label>
  <p>A backup is made before saving. {#if limit}Sessions read at most {limit} characters.{/if}</p><button disabled={busy || user === savedUser || !userLoaded} onclick={saveUser}>Save USER.md</button>
  {#if userError}<p role="alert">Could not load or save USER.md: {userError}</p>{/if}
{:else if section === 'appearance'}
  <h3>Appearance</h3><p>Choose a theme for this browser.</p><div class="choices">{#each ['auto','light','dark'] as t}<button class:on={theme === t} onclick={() => setTheme(t as typeof theme)}>{t}</button>{/each}</div>
{:else if section === 'models'}
  <h3>Models</h3><p>Default for new sessions.</p>
  {#if modelsError}<p role="alert">Could not load models: {modelsError}</p>
  {:else if models}
    {#if availableModels.length}
      <label>Model <select value={availableModels.some(m => m.id === (prefs.default_model ?? models!.default)) ? (prefs.default_model ?? models.default) : ''} onchange={(e) => patch({default_model:e.currentTarget.value, default_effort:null})}><option value="" disabled>Select an authenticated model</option>{#each availableModels as m}<option value={m.id}>{m.name || m.id}</option>{/each}</select></label>
      {@const model = availableModels.find(m => m.id === (prefs.default_model ?? models!.default))}
      <label>Effort <select value={prefs.default_effort ?? ''} onchange={(e) => patch({default_effort:e.currentTarget.value || null})}><option value="">Model default</option>{#each model?.efforts ?? [] as effort}<option value={effort}>{effort}</option>{/each}</select></label>
    {:else}<p>No authenticated models available.</p>{/if}
  {:else}<p>Loading models…</p>{/if}
  <h3>Engines</h3>
  {#if engineError}<p role="alert">Engine error: {engineError}</p>{/if}
  {#if engines}
    {#each engines as engine}
      <div class="card"><b>{engine.id}</b><span>{engine.signed_in ? 'Signed in' : 'Not signed in'}</span>
        {#if engine.id !== 'openrouter'}
          {#if engine.signed_in}<button disabled={engineBusy || (engine.id === 'codex' && ['starting','pending'].includes(engine.login.status))} onclick={() => logout(engine.id)}>Sign out</button>
          {:else}<button disabled={engineBusy || (engine.id === 'codex' && ['starting','pending'].includes(engine.login.status))} onclick={() => login(engine.id)}>Sign in {engine.id === 'codex' ? 'Codex' : 'Claude'}</button>{/if}
        {/if}
      </div>
      {#if engine.id === 'codex'}
        {#if engine.login.status === 'starting'}<p>Starting Codex sign-in…</p>
        {:else if engine.login.status === 'pending'}<p>Complete sign-in at <a href={engine.login.url} target="_blank" rel="noopener noreferrer">{engine.login.url}</a> using code <strong>{engine.login.code}</strong>. Waiting for authorization…</p>
        {:else if engine.login.status === 'complete'}<p>Codex login completed. {engine.signed_in ? 'Signed in.' : 'Authentication status has not updated yet.'}</p>
        {:else if engine.login.status === 'error'}<p role="alert">{engine.login.error || 'Codex login failed.'}</p>
        {:else if engine.login.status === 'expired'}<p role="alert">Codex login expired. Try again.</p>{/if}
      {/if}
      {#if engine.id === 'openrouter'}
        <p>Saving a key requires a kernel upgrade or service restart before it takes effect. {engine.runtime_note ?? ''}</p>
        <label>OpenRouter API key <input type="password" autocomplete="off" bind:value={key} /></label><button disabled={engineBusy || !key.trim()} onclick={saveKey}>Save key</button>
        {#if keyMessage}<p>{keyMessage}</p>{/if}
      {/if}
    {/each}
  {:else if !engineError}<p>Loading engines…</p>{/if}
  {#if instructions}<p>{instructions}</p>{/if}
  {#if prefError}<p role="alert">Preferences unavailable: {prefError}</p>{/if}
{:else if section === 'connections'}
  <h3>Connections</h3><p>MCP servers configured for zenbot.</p>
  {#if mcp}{#each Object.entries(mcp.servers) as [server, count]}<div class="card"><b>{server}</b><span>{count} cached tools{mcp.login[server] ? ` · ${mcp.login[server]}` : ''}</span></div>{:else}<p>No connections configured.</p>{/each}{#each mcp.problems as problem}<p role="alert">{problem}</p>{/each}{:else}<p>Loading connections…</p>{/if}
  <p>To sign in to a server, use <code>/mcp login &lt;server&gt;</code> in chat.</p>
{:else if section === 'memory'}
  <h3>Memory</h3><p>Short-term notes available to zenbot.</p>{#if memory}<div class="card"><b>{memory.memories.length} notes</b><span>Target: {memory.size} characters</span></div>{#if memory.last_sleep}<pre>{JSON.stringify(memory.last_sleep, null, 2)}</pre>{/if}{:else}<p>Loading memory…</p>{/if}
{:else if section === 'updates'}
  <h3>Updates</h3>{#if version}<div class="card"><b>Running {version.running}</b><span>{version.available ? `${version.behind ?? ''} commits available` : 'No update reported'}</span></div>{#if version.error}<p role="alert">{version.error}</p>{/if}{#each version.notes ?? [] as note}<article><h4>{note.version} · {note.date}</h4><ul>{#each note.items as item}<li>{item}</li>{/each}</ul></article>{/each}{:else}<p>Loading update information…</p>{/if}<button onclick={refreshVersion}>Check for updates</button><p>To apply an update, use <code>/upgrade</code> in chat.</p>
{/if}
{#if error}<p role="alert">{error}</p>{/if}
</div>
<style>
.body { display:flex; flex-direction:column; gap:12px; min-width:0; padding:4px 12px 40px; }
h3 {font-family:var(--serif);font-size:22px;margin:0} p {color:var(--dim);margin:0} label {display:flex;gap:12px;align-items:center;flex-wrap:wrap} .file {align-items:stretch;flex-direction:column} textarea {width:100%;box-sizing:border-box;resize:vertical;font:inherit} input,textarea,select {background:var(--paper);color:var(--ink);border:1px solid var(--faint);border-radius:8px;padding:8px} button {align-self:flex-start;border:1px solid var(--faint);border-radius:20px;padding:5px 14px} button.on {background:var(--accent);color:var(--paper)} .choices {display:flex;gap:5px} .card {display:flex;gap:12px;align-items:center;justify-content:space-between;border:1px solid var(--faint);border-radius:14px;padding:14px;flex-wrap:wrap} .card span {color:var(--dim)} pre {white-space:pre-wrap;overflow-wrap:anywhere} article h4 {margin-bottom:4px} article ul {margin-top:0}
</style>
