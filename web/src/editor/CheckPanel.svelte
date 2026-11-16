<script lang="ts">
  // The AI's proposals of one family, in text order, the one selected on the page first:
  // Check's fixes, the marks, or Shrink's cuts. Alt+↑↓ move, Alt+Enter takes, Alt+Del rejects.
  import type { Snippet } from 'svelte';
  import type { Family, Fix } from './fixes.ts';

  let { family, title, fixes, current, busy, error, go, accept, asAlternative, reject, again, clear, children }: {
    family: Family; title: string; fixes: Array<Fix & { text: string }>; current: string | null; busy: boolean; error: string;
    go: (id: string) => void; accept: (id: string) => void; asAlternative: (id: string) => void; reject: (id: string) => void;
    again: () => void; clear: () => void; children?: Snippet;
  } = $props();
  const noun = $derived(family === 'shrink' ? 'cuts' : family === 'marks' ? 'marks' : 'fixes');
  const words = (t: string) => t.split(/\s+/).filter((w) => /[\p{L}\p{N}]/u.test(w)).length;
  const saves = $derived(fixes.reduce((n, f) => n + words(f.text) - words(f.replace), 0));
  const counts = $derived(['spelling', 'punctuation', 'grammar', 'clarity', 'cut', 'weak', 'long', 'filler']
    .map((k) => [k, fixes.filter((f) => f.kind === k).length] as const).filter(([, n]) => n));
  const at = $derived(fixes.findIndex((f) => f.id === current));
  // Keep the selected fix in sight in the list.
  $effect(() => {
    if (current) setTimeout(() => document.querySelector('.checkpanel li.sel')?.scrollIntoView({ block: 'nearest' }));
  });
</script>

<div class="checkpanel">
  <div class="panel-head">{title}{#if fixes.length} · {at >= 0 ? `${at + 1} of ${fixes.length}` : `${fixes.length} ${noun}`}{/if}</div>
  {@render children?.()}
  {#if busy}
    <p class="dim">Reading the piece…</p>
  {:else if !fixes.length}
    <p class="dim">{error ? '' : `No ${noun} left.`}</p>
  {:else}
    <div class="counts">{#each counts as [k, n]}<span class="kind-{k}">{n} {k}{k === 'cut' ? `s, −${saves} words` : ''}</span>{/each}</div>
    <ol>
      {#each fixes as f (f.id)}
        <li class:sel={f.id === current}>
          <button class="fix" onclick={() => go(f.id)}>
            <span class="kind kind-{f.kind}">{f.kind}</span>
            {#if family === 'marks'}
              <span class="change">{f.text}</span>
              <span class="why">{f.why}</span>
            {:else}
              <span class="change"><s>{f.text}</s>{#if f.replace} → <b>{f.replace}</b>{/if}</span>
              {#if f.id === current}<span class="why">{f.why}</span>{/if}
            {/if}
          </button>
          {#if f.id === current}
            <div class="acts">
              {#if family === 'marks'}
                <button onclick={() => accept(f.id)}>Done <kbd>Alt</kbd>+<kbd>↵</kbd></button>
              {:else}
                <button onclick={() => accept(f.id)}>{family === 'shrink' ? 'Cut' : 'Accept'} <kbd>Alt</kbd>+<kbd>↵</kbd></button>
                <button onclick={() => reject(f.id)}>{family === 'shrink' ? 'Keep' : 'Reject'} <kbd>Alt</kbd>+<kbd>Del</kbd></button>
                {#if family === 'check'}
                  <button onclick={() => asAlternative(f.id)} title="The fix shows; your words stay as the other version">As alternative</button>
                {/if}
              {/if}
            </div>
          {/if}
        </li>
      {/each}
    </ol>
  {/if}
  {#if error}<p class="err">{error}</p>{/if}
  <div class="foot">
    <span class="dim"><kbd>Alt</kbd>+<kbd>↑↓</kbd> move</span>
    <button onclick={again} disabled={busy}>Read again</button>
    <button onclick={clear}>Clear</button>
  </div>
</div>
