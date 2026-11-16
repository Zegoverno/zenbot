<script lang="ts">
  // The alternatives tab's editor (Alt+Enter over a selection): the versions of one word, sentence or
  // paragraph, oldest first. ↑↓ selects a version and shows it in the page; Enter makes it the
  // default (the big dot, what stays in the text); Del deletes it; typing + Enter adds yours;
  // Alt+G asks the AI for three; Alt+Delete removes them all, keeping the default. Esc closes and the page shows the default again.
  import { onMount, untrack } from 'svelte';
  import type { Group } from '../doc/note.ts';

  let { group, busy, error, preview, mark, add, remove, removeAll, ai, close }: {
    group: Group; busy: boolean; error: string;
    preview: (i: number) => void; mark: (def: number | undefined) => void; add: (text: string) => void; remove: (i: number) => number | undefined; removeAll: (def: number) => void; ai: () => void;
    close: (def: number) => void;
  } = $props();

  let input: HTMLInputElement;
  let draft = $state('');
  // The page shows the selected version while the panel is open; the default is what stays.
  // The panel is made once per group (keyed on its id): these start from the group, then live on.
  let def = $state(untrack(() => group.def ?? group.active));
  let sel = $state(untrack(() => group.active));
  let count = $state(untrack(() => group.variants.length));
  const label = $derived({ word: 'Word', sentence: 'Sentence', paragraph: 'Paragraph' }[group.scope]);

  onMount(() => input.focus());
  // The input is disabled while the AI thinks; give the keyboard back when it answers.
  $effect(() => {
    if (!busy) input?.focus();
  });
  // New versions (yours or the AI's) arrive at the end: select the first of them.
  $effect(() => {
    const n = group.variants.length;
    if (n > count) select(count);
    count = n;
  });

  // The page draws the default apart from the version previewed.
  $effect(() => {
    mark(def);
  });

  function select(i: number) {
    if (i < 0 || i >= group.variants.length) return;
    sel = i;
    preview(i);
  }

  function makeDefault(i: number) {
    def = i;
    select(i);
  }

  function del(i: number) {
    const n = group.variants.length;
    if (n < 2) return;
    count = n - 1;
    def = remove(i) ?? def;
    sel = Math.min(i, n - 2);
    preview(sel);
  }

  function onkey(e: KeyboardEvent) {
    if (e.altKey && e.code === 'KeyG') {
      // e.code, not e.key: on a Mac, Option+G types "©".
      e.preventDefault();
      ai();
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      select(sel + (e.key === 'ArrowDown' ? 1 : -1));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      const t = draft.trim();
      if (t) {
        draft = '';
        add(t);
      } else {
        makeDefault(sel);
      }
    } else if (e.key === 'Escape') {
      e.preventDefault();
      close(def);
    } else if (e.altKey && (e.key === 'Delete' || e.key === 'Backspace') && draft === '') {
      // Alt+Delete (Option+Delete on a Mac): all of them go, the default stays in the text.
      e.preventDefault();
      removeAll(def);
    } else if ((e.key === 'Delete' || e.key === 'Backspace') && draft === '') {
      e.preventDefault();
      del(sel);
    }
  }
</script>

<div class="alts" role="group" aria-label="Alternatives">
  <div class="panel-head">{label} · {group.variants.length} {group.variants.length === 1 ? 'version' : 'versions'}</div>
  <ol>
    {#each group.variants as v, i (i + v.text)}
      <li class:sel={i === sel} class:def={i === def}>
        <button
          onmousedown={(e) => { e.preventDefault(); select(i); input.focus(); }}
          ondblclick={() => makeDefault(i)}
          title="Click to show it, double-click to make it the default"
        >
          <span class="text">{v.text}</span>
          {#if i === def}<span class="badge" title="Default: what stays in the text">D</span>{/if}
          <span class="who" title={v.by === 'ai' ? (v.model ?? 'AI') : v.by === 'zenbot' ? 'zenbot' : 'You'}>{v.by === 'ai' ? '🤖' : v.by === 'zenbot' ? 'zb' : '✍️'}</span>
        </button>
        {#if group.variants.length > 1}
          <button class="x" title="Delete" aria-label="Delete" onmousedown={(e) => { e.preventDefault(); del(i); input.focus(); }}>×</button>
        {/if}
      </li>
    {/each}
  </ol>
  <input
    bind:this={input}
    bind:value={draft}
    onkeydown={onkey}
    placeholder={busy ? '🤖 thinking…' : 'Another way to say it…'}
    disabled={busy}
    aria-label="New alternative"
  />
  {#if error}<p class="err">{error}</p>{/if}
  <p class="hint">↑↓ select · Enter default · Del delete · <kbd>Alt</kbd>+<kbd>Del</kbd> remove all · <kbd>Alt</kbd>+<kbd>G</kbd> 🤖 three more · Esc close</p>
</div>
