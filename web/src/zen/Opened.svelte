<script lang="ts">
  // A pane's open file through its opener: the default for the file's pattern and mode (the
  // installed reader and writer, or the built-ins as fallback), or what "Open with" picked.
  // "Open with" lists the built-ins and every enabled plugin opener for the file, and makes one the
  // default. Plugins see vault files only, so files elsewhere always open built-in.
  import Viewer from './Viewer.svelte';
  import PluginFrame from './PluginFrame.svelte';
  import { app, closeFile, openFile, notice, vaultNote, type Pane } from './app.svelte.ts';
  import { defaultOpener, inVault, openersFor, plugins, setDefault, type Mode } from './plugins.svelte.ts';

  let { path, pane, active }: { path: string; pane: Pane; active: boolean } = $props();

  const name = $derived(path.split('/').pop() ?? '');
  const rel = $derived(inVault(path));
  const mode = $derived<Mode>(app.write[pane] ? 'edit' : 'read');
  const options = $derived(rel !== null ? openersFor(name) : []);
  const chosen = $derived.by(() => {
    const pick = app.opener[pane];
    if (pick === 'builtin') return null;
    if (pick) return options.find((o) => o.key === pick) ?? null;
    return defaultOpener(name, mode);
  });
  const titleOf = (plugin: string) => plugins.list.find((p) => p.name === plugin)?.title || plugin;

  interface Choice { key: string; mode: Mode; label: string; isDefault: boolean }
  const choices = $derived.by((): Choice[] => {
    const out: Choice[] = [{ key: 'builtin', mode: 'read', label: 'Reader (built-in)', isDefault: defaultOpener(name, 'read') === null }];
    if (vaultNote(path)) out.push({ key: 'builtin', mode: 'edit', label: 'Writer (built-in)', isDefault: defaultOpener(name, 'edit') === null });
    for (const o of options) out.push({ key: o.key, mode: o.mode, label: `${o.title} · ${titleOf(o.plugin)}`, isDefault: defaultOpener(name, o.mode)?.key === o.key });
    return out;
  });
  const current = (c: Choice) => (chosen ? c.key === chosen.key : c.key === 'builtin' && c.mode === mode);

  let menu = $state(false);
  let viewer: Viewer | undefined = $state();
  const leave = (next: () => void) => viewer ? viewer.leave(next) : next();

  function open(c: Choice) {
    leave(() => {
      menu = false;
      openFile(path, pane, c.mode === 'edit', c.key);
    });
  }

  async function makeDefault(c: Choice) {
    try {
      await setDefault(name, c.mode, c.key);
      notice(`${name.includes('.') ? `*.${name.split('.').pop()}` : name} files ${c.mode === 'edit' ? 'write' : 'open'} with ${c.label} from now on`, 3000);
    } catch (e) {
      notice(`couldn't set the default: ${(e as Error).message}`);
    }
  }
</script>

<div class="opened">
    <div class="with" role="toolbar" aria-label="file actions">
      <button class="dim" onclick={() => leave(() => closeFile(pane))} title="back to the folder">Back</button>
      <button class="dim" aria-pressed={mode === 'read'} onclick={() => leave(() => openFile(path, pane))}>Read</button>
      <button class="dim" aria-pressed={mode === 'edit'} disabled={!vaultNote(path) && !options.some((o) => o.mode === 'edit')} onclick={() => leave(() => openFile(path, pane, true))}>Edit</button>
      <span class="dim name">{name}{chosen ? ` · ${chosen.title}` : ''}</span>
      <span class="grow"></span>
      <div class="menu-at">
        <button class="dim" aria-haspopup="menu" aria-expanded={menu} onclick={() => (menu = !menu)}>Open with…</button>
        {#if menu}
          <div class="menu" role="menu" aria-label="open with">
            {#each choices as c}
              <div class="item" class:on={current(c)}>
                <button role="menuitem" onclick={() => open(c)}>{c.label}</button>
                {#if c.isDefault}<span class="dim tag">default</span>
                {:else}<button class="dim tag" onclick={() => makeDefault(c)}>make default</button>{/if}
              </div>
            {/each}
          </div>
        {/if}
      </div>
    </div>
  <div class="body">
    {#if chosen && rel !== null}
      {#key `${chosen.key}:${rel}`}
        <PluginFrame plugin={chosen.plugin} ui={chosen.ui} {pane} path={rel} mode={chosen.mode} slot="opener" title={chosen.title} />
      {/key}
    {:else}
      <Viewer bind:this={viewer} {path} {pane} {active} />
    {/if}
  </div>
</div>

<style>
  .opened { height: 100%; min-height: 0; display: flex; flex-direction: column; }
  .body { flex: 1; min-height: 0; position: relative; }
  .with { display: flex; align-items: center; gap: 10px; padding: 4px 16px; font-size: 13px; flex: none; }
  .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .grow { flex: 1; }
  .menu-at { position: relative; }
  .menu { position: absolute; right: 0; top: calc(100% + 4px); z-index: 30; min-width: 260px; background: var(--paper); border: 1px solid var(--faint); border-radius: 8px; padding: 4px; box-shadow: 0 4px 18px rgb(0 0 0 / 0.08); }
  .item { display: flex; align-items: center; gap: 10px; padding: 4px 8px; border-radius: 6px; }
  .item.on { background: color-mix(in srgb, var(--ink) 6%, transparent); }
  .item > button:first-child { flex: 1; text-align: left; }
  .tag { font-size: 12px; }
  button.tag:hover, .with button:hover { color: var(--ink); }
</style>
