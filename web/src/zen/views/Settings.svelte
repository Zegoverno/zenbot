<script lang="ts">
  // Settings (a board page off the top bar). Plugins: the installed ones (version, permissions,
  // enable or disable, remove; for one granted `ai`, its AI switch and last 30 days' AI calls) and the default opener per file pattern and mode. Marketplace: the
  // kernel's marketplace entries, with Install / Update; installing first asks the kernel what the
  // plugin needs and shows those permissions in plain words before anything is installed
  // (ADR 0001, Host API).
  import { onMount } from 'svelte';
  import SettingsSections from './SettingsSections.svelte';
  let section = $state('you');
  const sections = [['you','You'], ['appearance','Appearance'], ['models','Models'], ['connections','Connections'], ['plugins','Plugins'], ['memory','Memory'], ['updates','Updates']];
  import { pluginApi, type MarketEntry, type Plugin } from '../api.ts';
  import { app, notice, settings } from '../app.svelte.ts';
  import { loadPlugins, permissionWords, plugins } from '../plugins.svelte.ts';

  const tab = $derived(app.settings);

  // ---- installed ----

  let removing = $state<string | null>(null);

  async function toggle(p: Plugin) {
    try {
      await pluginApi.setEnabled(p.name, !p.enabled);
      p.enabled = !p.enabled;
    } catch (e) {
      notice(`couldn't ${p.enabled ? 'disable' : 'enable'} ${p.title || p.name}: ${(e as Error).message}`);
    }
    await loadPlugins();
  }

  async function toggleAi(p: Plugin) {
    const on = p.ai_enabled !== false;
    try {
      await pluginApi.setAi(p.name, !on);
      p.ai_enabled = !on;
    } catch (e) {
      notice(`couldn't turn AI ${on ? 'off' : 'on'} for ${p.title || p.name}: ${(e as Error).message}`);
    }
    await loadPlugins();
  }

  async function remove(p: Plugin) {
    removing = null;
    try {
      await pluginApi.remove(p.name);
      notice(`removed ${p.title || p.name}; its data stays`, 3000);
    } catch (e) {
      notice(`couldn't remove ${p.title || p.name}: ${(e as Error).message}`);
    }
    await loadPlugins();
    market = null;
  }

  /** Every pattern and mode an enabled plugin opens, with who can open it. */
  const slots = $derived.by(() => {
    const m = new Map<string, { key: string; pattern: string; mode: string; options: { value: string; label: string }[] }>();
    for (const p of plugins.list.filter((x) => x.enabled))
      for (const o of p.contributes.openers ?? [])
        for (const pattern of o.patterns) {
          const key = `${pattern}:${o.mode}`;
          if (!m.has(key)) m.set(key, { key, pattern, mode: o.mode, options: [{ value: 'builtin', label: o.mode === 'edit' ? 'Writer (built-in)' : 'Reader (built-in)' }] });
          m.get(key)!.options.push({ value: `${p.name}/${o.id}`, label: `${o.title} · ${p.title || p.name}` });
        }
    return [...m.values()];
  });

  async function setDefault(key: string, value: string) {
    try {
      await pluginApi.setDefault(key, value);
      plugins.defaults[key] = value;
    } catch (e) {
      notice(`couldn't set the default: ${(e as Error).message}`);
    }
  }

  // ---- marketplace ----

  let market = $state<MarketEntry[] | null>(null);
  let marketError = $state('');
  let asking = $state<{ entry: MarketEntry; permissions: string[] } | null>(null);
  let busy = $state<string | null>(null);

  async function loadMarket() {
    try {
      market = await pluginApi.marketplace();
      marketError = '';
    } catch (e) {
      marketError = (e as Error).message;
    }
  }

  $effect(() => {
    if (tab === 'marketplace' && market === null) void loadMarket();
  });

  /** Asks the kernel what the plugin needs; nothing is installed until the owner approves. */
  async function install(entry: MarketEntry) {
    busy = entry.name;
    try {
      const r = await pluginApi.install(entry.name);
      if ('needs_approval' in r && r.needs_approval) asking = { entry, permissions: r.permissions ?? [] };
      else await installed(entry);
    } catch (e) {
      notice(`couldn't install ${entry.title || entry.name}: ${(e as Error).message}`);
    } finally {
      busy = null;
    }
  }

  async function approve() {
    if (!asking) return;
    const { entry, permissions } = asking;
    asking = null;
    busy = entry.name;
    try {
      const r = await pluginApi.install(entry.name, permissions);
      // The release may have changed since the dialog opened. Never report success when the
      // kernel asks for a fresh approval (it checks the exact permission set).
      if ('needs_approval' in r && r.needs_approval) asking = { entry, permissions: r.permissions ?? [] };
      else await installed(entry);
    } catch (e) {
      notice(`couldn't install ${entry.title || entry.name}: ${(e as Error).message}`);
    } finally {
      busy = null;
    }
  }

  async function installed(entry: MarketEntry) {
    notice(`${entry.installed ? 'updated' : 'installed'} ${entry.title || entry.name} ${entry.version}`, 3000);
    await Promise.all([loadPlugins(), loadMarket()]);
  }

  onMount(() => {
    void loadPlugins();
  });
</script>

<div class="settings-container"><div class="view settings"><h2>Settings</h2>
  <nav class="section-nav" aria-label="Settings sections">{#each sections as [key, label]}<button class:on={section === key} aria-current={section === key ? 'page' : undefined} onclick={() => section = key}>{label}</button>{/each}</nav>
  <div class="settings-content">{#if section !== 'plugins'}<SettingsSections {section} />{:else}
  <div class="ztabs sub" role="tablist" aria-label="settings">
    <button role="tab" class:on={tab === 'plugins'} aria-selected={tab === 'plugins'} onclick={() => settings('plugins')}>Plugins</button>
    <button role="tab" class:on={tab === 'marketplace'} aria-selected={tab === 'marketplace'} onclick={() => settings('marketplace')}>Marketplace</button>
  </div>

  {#if tab === 'plugins'}
    {#if plugins.error}<p class="dim note">⚠ {plugins.error}</p>{/if}
    {#if plugins.loaded && !plugins.list.length && !plugins.error}
      <p class="dim note">No plugins yet. <button class="link" onclick={() => settings('marketplace')}>Browse the marketplace</button></p>
    {/if}
    {#each plugins.list as p (p.name)}
      <section class="plugin" aria-label={p.title || p.name} class:off={!p.enabled}>
        <div class="line">
          <span class="title">{p.title || p.name}</span>
          <span class="dim">{p.version}</span>
          <span class="grow"></span>
          <label class="dim switch"><input type="checkbox" checked={p.enabled} onchange={() => toggle(p)} aria-label={`enable ${p.title || p.name}`} /> enabled</label>
          <button class="dim act" onclick={() => (removing = p.name)}>remove</button>
        </div>
        {#if p.description}<p class="dim desc">{p.description}</p>{/if}
        <p class="dim perms">{p.permissions.length ? p.permissions.map(permissionWords).join(' · ') : 'No permissions beyond its own data'}</p>
        {#if p.permissions.includes('ai')}
          <p class="dim perms ai">
            <label class="switch"><input type="checkbox" checked={p.ai_enabled !== false} onchange={() => toggleAi(p)} aria-label={`AI for ${p.title || p.name}`} /> AI</label>
            <span>{p.ai_calls_30d ?? 0} AI call{(p.ai_calls_30d ?? 0) === 1 ? '' : 's'} in the last 30 days</span>
          </p>
        {/if}
        {#if removing === p.name}
          <p class="confirm">
            Remove {p.title || p.name}? Its data stays.
            <button class="warn" onclick={() => remove(p)}>Yes, remove {p.title || p.name}</button>
            <button class="dim" onclick={() => (removing = null)}>Cancel</button>
          </p>
        {/if}
      </section>
    {/each}
    {#if slots.length}
      <h3 class="head dim">DEFAULT OPENERS</h3>
      {#each slots as s (s.key)}
        <label class="slot">
          <span class="pat">{s.pattern}</span>
          <span class="dim">{s.mode === 'edit' ? 'write' : 'read'}</span>
          <select aria-label={`default for ${s.pattern} ${s.mode}`} value={plugins.defaults[s.key] ?? 'builtin'} onchange={(e) => setDefault(s.key, (e.currentTarget as HTMLSelectElement).value)}>
            {#each s.options as o}<option value={o.value}>{o.label}</option>{/each}
          </select>
        </label>
      {/each}
    {/if}
  {:else}
    {#if marketError}<p class="dim note">⚠ {marketError}</p>{/if}
    {#if market === null && !marketError}<p class="dim note">…</p>{/if}
    {#if market && !market.length}<p class="dim note">The marketplace is empty.</p>{/if}
    {#each market ?? [] as m (m.name)}
      <section class="plugin" aria-label={m.title || m.name}>
        <div class="line">
          <span class="title">{m.title || m.name}</span>
          <span class="dim">{m.version}{m.author ? ` · ${m.author}` : ''}</span>
          <span class="grow"></span>
          {#if m.installed === m.version}
            <span class="dim">Installed</span>
          {:else}
            <button class="act primary" disabled={busy === m.name} onclick={() => install(m)}>{m.installed ? `Update from ${m.installed}` : 'Install'}</button>
          {/if}
        </div>
        {#if m.description}<p class="dim desc">{m.description}</p>{/if}
        <p class="dim perms">
          {(m.kernel ?? []).length ? (m.kernel ?? []).map(permissionWords).join(' · ') : 'No permissions beyond its own data'}
          {#if m.repository}· <a href={m.repository} target="_blank" rel="noopener noreferrer">source</a>{/if}
        </p>
      </section>
    {/each}
  {/if}
  {/if}
</div></div></div>

{#if asking}
  <div class="scrim">
    <div class="dialog" role="dialog" aria-modal="true" aria-label={`Install ${asking.entry.title || asking.entry.name}`}>
      <h3>Install {asking.entry.title || asking.entry.name} {asking.entry.version}?</h3>
      <p>New plugins become the default reader or editor for their supported file types. Updates preserve your choices.</p>
      {#if asking.permissions.length}
        <p class="dim">It asks to:</p>
        <ul>{#each asking.permissions as p}<li>{permissionWords(p)}</li>{/each}</ul>
      {:else}
        <p class="dim">It asks for nothing beyond its own data.</p>
      {/if}
      <p class="dim small">It runs sandboxed: it reaches only what's listed here.</p>
      <div class="buttons">
        <button class="dim" onclick={() => (asking = null)}>Cancel</button>
        <button class="act primary" onclick={approve}>Install</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .settings-container { container-type:inline-size; }
  .settings h2 { grid-column:1/-1; font-family:var(--serif); margin:0 0 8px; font-size:22px; }
  .settings-content { min-width:0; }
  .section-nav { display:flex; flex-direction:column; gap:4px; align-self:start; }
  .section-nav button { text-align:left; }
  @container (max-width:680px) { .settings { grid-template-columns:1fr !important; padding:18px !important; } .section-nav { flex-direction:row; flex-wrap:wrap; } }
  .section-nav button { padding:7px 12px; border-radius:10px; color:var(--dim); }
  .section-nav button.on { background:color-mix(in srgb, var(--ink) 8%, transparent); color:var(--ink); font-weight:600; }
  .settings { display:grid; grid-template-columns:180px minmax(0,1fr); gap:18px 32px; padding:24px 30px 40px; max-width:1100px; margin:0 auto; }
  .sub { display: flex; gap: 4px; margin: 4px 0 14px; }
  .sub button { padding: 4px 10px; border-radius: 6px; color: var(--dim); }
  .sub button.on { color: var(--ink); background: color-mix(in srgb, var(--ink) 8%, transparent); }
  .note { margin: 8px 0; }
  .link { color: var(--accent); }
  .plugin { padding: 10px 0; border-bottom: 1px solid var(--faint); }
  .plugin.off .title { color: var(--dim); }
  .line { display: flex; align-items: baseline; gap: 10px; }
  .title { font-weight: 600; }
  .grow { flex: 1; }
  .desc, .perms { margin: 4px 0 0; font-size: 13px; }
  .ai { display: flex; gap: 12px; align-items: center; }
  .switch { display: inline-flex; align-items: center; gap: 4px; font-size: 13px; cursor: pointer; }
  .act { font-size: 13px; }
  .act:hover { color: var(--ink); }
  .act.primary { color: var(--accent); }
  .act:disabled { opacity: 0.5; cursor: default; }
  .confirm { margin: 8px 0 0; font-size: 13px; display: flex; gap: 12px; align-items: baseline; }
  .warn { color: var(--err); }
  .head { font-size: 12px; letter-spacing: 0.06em; font-weight: 500; margin: 22px 0 6px; }
  .slot { display: flex; align-items: baseline; gap: 10px; padding: 4px 0; font-size: 14px; }
  .pat { font-family: var(--mono); font-size: 13px; min-width: 90px; }
  select { font: inherit; color: inherit; background: var(--paper); border: 1px solid var(--faint); border-radius: 6px; padding: 2px 6px; margin-left: auto; }
  .scrim { position: fixed; inset: 0; background: rgb(0 0 0 / 0.25); display: grid; place-items: center; z-index: 60; }
  .dialog { background: var(--paper); border: 1px solid var(--faint); border-radius: 10px; padding: 18px 20px; width: min(440px, 92vw); box-shadow: 0 8px 30px rgb(0 0 0 / 0.15); }
  .dialog h3 { margin: 0 0 10px; font-size: 16px; }
  .dialog ul { margin: 6px 0 10px; padding-left: 20px; }
  .dialog p { margin: 6px 0; }
  .small { font-size: 12px; }
  .buttons { display: flex; justify-content: flex-end; gap: 16px; margin-top: 14px; }
</style>
