<script lang="ts">
  // The quick-control ring (after Logitech's Actions Ring), bottom right. One button that shows the
  // focused panel's icon; open (click, or Ctrl+.), it fans out that panel's commands on a quarter
  // circle (at most six), plus "Focus ⟷" bearing the other panel's icon when the page is split. Each
  // action shows its number (and its letter when its key is one), and Ctrl+. then a letter or 1–6
  // runs it; arrows and Enter work too (the key handling is in keys.svelte.ts). The commands are
  // the focused panel's own (views register them): the ring just draws what the registry holds.
  import Icon from './Icon.svelte';
  import { ui, ringItems, focused, kindIcon, toggleRing } from './keys.svelte.ts';
  import { keyLabel } from './keys.ts';
  import { app } from './app.svelte.ts';

  // Read when the ring opens too: a command's title can depend on the row under the cursor.
  const items = $derived.by(() => { void ui.ring; return ringItems(); });
  const icon = $derived(kindIcon(focused()));
  const at = (i: number, n: number) => {
    const R = n > 6 ? 188 : 156;
    const a = Math.PI + (n < 2 ? Math.PI / 4 : (i * (Math.PI / 2)) / (n - 1));
    return `translate(${Math.cos(a) * R}px, ${Math.sin(a) * R}px)`;
  };
  const letter = (k: string | undefined) => (k && /^[a-z]$/.test(k) ? k.toUpperCase() : '');
  let hover = $state('');
  const label = $derived(hover || (ui.ring ? items[ui.ringAt]?.title : '') || '');
</script>

{#if ui.ring}<button class="scrim" aria-label="Close" onclick={() => (ui.ring = false)}></button>{/if}
<div class="qc" class:open={ui.ring} class:hide={app.narrow && app.screen === 'chat' && !ui.ring} data-ring>
  {#each ui.ring ? items : [] as a, i (a.id)}
    <button class="qa" class:cur={i === ui.ringAt} style="--to: {at(i, items.length)}; animation-delay: {i * 18}ms"
      title="{a.title}{a.key ? ` (${keyLabel(a.key)})` : ''}" aria-label={a.title} data-ring-item={a.id}
      onmouseenter={() => (hover = a.title)} onmouseleave={() => (hover = '')} onclick={() => { ui.ring = false; hover = ''; a.run(); }}>
      <Icon name={a.icon} size={20} />
      <span class="qn">{letter(a.key) || i + 1}</span>
    </button>
  {/each}
  {#if ui.ring && label}<span class="qlabel">{label}</span>{/if}
  <button class="qbtn" onclick={toggleRing} aria-expanded={ui.ring} title="Quick actions (Ctrl+.)" aria-label="Quick actions" data-ring-icon={icon}>
    <Icon name={icon} size={22} />
  </button>
</div>

<style>
  .scrim { position: fixed; inset: 0; z-index: 14; background: color-mix(in srgb, var(--bg) 35%, transparent); backdrop-filter: blur(2px); cursor: default; }
  .qc.hide { display: none; }
  .qc { position: fixed; right: 26px; bottom: 26px; width: 56px; height: 56px; z-index: 15; }
  .qbtn { position: absolute; inset: 0; border-radius: 50%; display: grid; place-items: center; color: var(--on-accent); background: radial-gradient(circle at 32% 28%, color-mix(in srgb, var(--accent) 70%, white), var(--accent) 72%);
    box-shadow: 0 8px 24px var(--shadow), 0 0 0 4px color-mix(in srgb, var(--accent) 16%, transparent); transition: transform 0.2s; }
  .qc.open .qbtn { transform: scale(0.92); }
  .qa { position: absolute; left: 4px; top: 4px; width: 48px; height: 48px; border-radius: 50%; display: grid; place-items: center; color: var(--ink);
    background: var(--paper); border: 1px solid var(--faint); box-shadow: 0 6px 18px var(--shadow); transform: var(--to);
    animation: fan 0.22s cubic-bezier(.2,.9,.3,1.2) both; }
  @keyframes fan { from { transform: translate(0, 0); opacity: 0; } }
  .qa:hover, .qa:focus-visible, .qa.cur { color: var(--on-accent); background: var(--accent); border-color: var(--accent); outline: none; }
  .qn { position: absolute; right: -3px; top: -3px; min-width: 17px; height: 17px; padding: 0 4px; border-radius: 99px; display: grid; place-items: center; font-size: 10.5px; font-weight: 700;
    background: var(--ink); color: var(--paper); }
  .qlabel { position: absolute; right: 200px; bottom: 14px; white-space: nowrap; font-size: 13px; font-weight: 600; padding: 5px 12px; border-radius: 99px; background: var(--ink); color: var(--paper); pointer-events: none; }
  @media (max-width: 900px) { .qc { right: 16px; bottom: 96px; } .qlabel { right: 180px; } }
</style>
