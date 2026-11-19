<script lang="ts" module>
  // Pieces of a task row shared by the Tasks tab and Home's task widgets, so both read the same:
  // the round check (its ring in the priority's colour), the flag for urgent/important, and the
  // labelled facts (calendar = due, folder = project, ◎ = goal).
  import { PRI, days, when, type T } from './taskstore.svelte.ts';
  export { box, flag, facts };
</script>

{#snippet box(t: T, onclick: () => void)}
  <button class="tb-box" role="checkbox" aria-checked={!!t.done} aria-label={t.done ? 'Mark as not done' : 'Mark as done'} {onclick}
    style={t.pri && !t.done ? `--p:${PRI[t.pri].c}` : ''} class:done={t.done}>{#if t.done}✓{/if}</button>
{/snippet}

{#snippet flag(t: T)}
  {#if t.pri === 'urgent' || t.pri === 'important'}
    <svg class="tb-flag" style="color:{PRI[t.pri].c}" viewBox="0 0 24 24" aria-label={PRI[t.pri].l} role="img"><title>{PRI[t.pri].l}: {PRI[t.pri].h}</title><path d="M5 21V4h11l-1.5 4L16 12H5" fill="currentColor" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round"/></svg>
  {/if}
{/snippet}

{#snippet facts(t: T, show: { project?: boolean; goal?: boolean } = {})}
  {#if t.due}
    {@const n = days(t.due)}
    <span class="tb-f" class:late={n < 0 && !t.done} class:today={n === 0 && !t.done} title="Due {t.due}">
      <svg viewBox="0 0 24 24"><rect x="4" y="5" width="16" height="15" rx="3"/><path d="M4 10h16M9 3v4M15 3v4"/></svg>{when(t.due)}</span>
  {/if}
  {#if show.project !== false}
    <span class="tb-f" title="Project: {t.project}"><svg viewBox="0 0 24 24"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/></svg>{t.project}</span>
  {/if}
  {#if t.goal && show.goal !== false}
    <span class="tb-f goal" title="Goal: {t.goal}"><svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="8"/><circle cx="12" cy="12" r="3.5"/></svg>{t.goal}</span>
  {/if}
{/snippet}

<style>
  :global(.tb-box) { --p: var(--dim); flex: none; width: 20px; height: 20px; margin-top: 1px; border-radius: 50%; border: 1.6px solid var(--p); display: grid; place-items: center; font-size: 12px; color: var(--on-accent); transition: background 0.15s; }
  :global(.tb-box:hover) { background: color-mix(in srgb, var(--p) 15%, transparent); }
  :global(.tb-box.done) { background: var(--accent); border-color: var(--accent); }
  :global(.tb-flag) { width: 13px; height: 13px; flex: none; vertical-align: -1px; margin-right: 5px; }
  :global(.tb-f) { display: inline-flex; align-items: center; gap: 4px; min-width: 0; }
  :global(.tb-f svg) { width: 12px; height: 12px; flex: none; fill: none; stroke: currentColor; stroke-width: 1.8; stroke-linecap: round; opacity: 0.8; }
  :global(.tb-f.today) { color: var(--accent); font-weight: 600; }
  :global(.tb-f.late) { color: var(--bad, #b5543c); font-weight: 600; }
  :global(.tb-f.goal) { max-width: 22rem; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
</style>
