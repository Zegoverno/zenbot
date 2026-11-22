<script lang="ts">
  // The `?` overlay: the shortcuts that work right now, grouped as the focused panel's, then the
  // global ones. Each row shows the key as the owner has it (rebinding included); a command with no
  // key shows what reaches it (the go-to's `g f`, or the ring). Esc or `?` closes it.
  import { ui, active, focused, kindName, keyOf, clashed, changed } from './keys.svelte.ts';
  import { keyLabel } from './keys.ts';
  import { app } from './app.svelte.ts';

  const FIXED_ROWS: [string, string][] = [
    ['Ctrl+K', 'Command list'], ['Ctrl+.', 'Quick-control ring: then a letter or 1–6'], ['Ctrl+\\', 'Switch focus between panels'],
    ['Ctrl+Shift+\\', 'Close the side panel, or reopen it'], ['?', 'This list'], ['g, then h s f t r a m p', 'Go to Home, Sessions, Files, Tasks, Routines, Agents, Games, Preview'],
    ['1–9', 'Pick a tab'], ['Esc', 'Back one level'],
  ];
  const keysOfCmd = (c: ReturnType<typeof active>[number]) => [c.hint, ...[keyOf(c), ...(c.alt ?? [])].filter(Boolean).map((k) => keyLabel(k))].filter(Boolean).join(' · ');
  const own = $derived(active().filter((c) => c.scope !== 'global'));
  const global = $derived(active().filter((c) => c.scope === 'global' && !c.reserved));
</script>

{#if ui.help}
  <button class="scrim" aria-label="Close" onclick={() => (ui.help = false)}></button>
  <div class="help" role="dialog" aria-label="Keyboard shortcuts" aria-modal="true">
    <header><h2>Shortcuts</h2><span class="sub">{kindName(focused())}{app.narrow ? '' : ' is focused'} · <kbd>?</kbd> or <kbd>Esc</kbd> closes</span></header>
    <div class="cols">
      <section>
        <h3>{kindName(focused())}</h3>
        {#each own as c (c.id)}
          <div class="r"><span>{c.title}{#if clashed(c)} <em>clash</em>{/if}</span><kbd class:ch={changed(c)}>{keysOfCmd(c) || '—'}</kbd></div>
        {:else}<div class="r dim">Nothing of its own: the global keys work.</div>{/each}
      </section>
      <section>
        <h3>Everywhere</h3>
        {#each FIXED_ROWS as [k, t]}<div class="r"><span>{t}</span><kbd>{k}</kbd></div>{/each}
        {#each global as c (c.id)}
          <div class="r"><span>{c.title}</span><kbd class:ch={changed(c)}>{keysOfCmd(c) || '—'}</kbd></div>
        {/each}
      </section>
    </div>
  </div>
{/if}

<style>
  .scrim { position: fixed; inset: 0; z-index: 40; background: color-mix(in srgb, var(--bg) 45%, transparent); backdrop-filter: blur(3px); cursor: default; }
  .help { position: fixed; z-index: 41; left: 50%; top: 10vh; transform: translateX(-50%); width: min(760px, calc(100vw - 24px)); max-height: 78vh; overflow: auto;
    background: var(--paper); border: 1px solid var(--faint); border-radius: 18px; box-shadow: 0 24px 70px var(--shadow); padding: 18px 22px 22px; }
  header { display: flex; align-items: baseline; gap: 12px; margin-bottom: 8px; }
  h2 { margin: 0; font-family: var(--serif); font-size: 21px; font-weight: 600; }
  .sub { color: var(--dim); font-size: 13px; }
  .cols { display: grid; grid-template-columns: 1fr 1fr; gap: 10px 28px; }
  h3 { margin: 12px 0 6px; font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); font-weight: 650; }
  .r { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 4px 0; font-size: 14px; border-bottom: 1px dashed var(--faint); }
  .r.dim { color: var(--dim); }
  em { font-style: normal; font-size: 11px; font-weight: 650; color: var(--warn); }
  kbd { font-family: var(--sans); font-size: 12px; border: 1px solid var(--faint); border-radius: 6px; padding: 1px 7px; color: var(--ink); background: var(--paper); white-space: nowrap; }
  kbd.ch { color: var(--accent); border-color: color-mix(in srgb, var(--accent) 45%, var(--faint)); }
  @media (max-width: 700px) { .cols { grid-template-columns: 1fr; } }
</style>
