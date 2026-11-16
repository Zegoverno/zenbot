<script lang="ts">
  // Alt+O: open a note. With nothing typed it shows a folder of the vault: ↑↓ choose, Enter or →
  // goes in (or opens the note), ← or Backspace goes up. Typing searches every note's path.
  // Esc closes. The last row starts a new note in the folder showing.
  import { onMount, untrack } from 'svelte';
  import { api } from '../lib/api.ts';

  let { from = '', open, create, close }: {
    /** The note open now: the finder starts in its folder. */
    from?: string; open: (path: string) => void; create: (dir: string) => void; close?: () => void;
  } = $props();

  type Item = { kind: 'up' | 'dir' | 'file' | 'new'; path: string; label: string };
  let files = $state<string[]>([]);
  let dir = $state(untrack(() => from).split('/').slice(0, -1).join('/'));
  let filter = $state('');
  let sel = $state(0);
  let input: HTMLInputElement;
  let list: HTMLElement;

  onMount(async () => {
    input.focus();
    files = (await api.files()).files;
    // Start on the note that is open.
    const i = items.findIndex((x) => x.kind === 'file' && x.path === from);
    if (i >= 0) sel = i;
  });

  const name = (p: string) => p.split('/').pop()!;
  const items = $derived.by((): Item[] => {
    const q = filter.toLowerCase().split(/\s+/).filter(Boolean);
    if (q.length) {
      return files
        .filter((f) => q.every((w) => f.toLowerCase().includes(w)))
        .sort((a, b) => Number(!name(b).toLowerCase().includes(q[0])) - Number(!name(a).toLowerCase().includes(q[0])) || a.length - b.length)
        .slice(0, 100)
        .map((f) => ({ kind: 'file', path: f, label: f.replace(/\.md$/, '') }));
    }
    const pre = dir ? `${dir}/` : '';
    const inside = files.filter((f) => f.startsWith(pre)).map((f) => f.slice(pre.length));
    const dirs = [...new Set(inside.filter((f) => f.includes('/')).map((f) => f.split('/')[0]))].sort((a, b) => a.localeCompare(b));
    return [
      ...(dir ? [{ kind: 'up' as const, path: dir.split('/').slice(0, -1).join('/'), label: '..' }] : []),
      ...dirs.map((d) => ({ kind: 'dir' as const, path: pre + d, label: `${d}/` })),
      ...inside.filter((f) => !f.includes('/')).map((f) => ({ kind: 'file' as const, path: pre + f, label: f.replace(/\.md$/, '') })),
      { kind: 'new', path: dir, label: `+ New note in ${dir || 'the vault'}/` },
    ];
  });

  $effect(() => {
    if (sel >= items.length) sel = Math.max(0, items.length - 1);
    const el = list?.children[sel];
    if (el) el.scrollIntoView({ block: 'nearest' });
  });

  function enter(dirTo: string) {
    dir = dirTo;
    filter = '';
    sel = 0;
  }

  function act(it: Item | undefined) {
    if (!it) return;
    if (it.kind === 'file') open(it.path);
    else if (it.kind === 'new') create(it.path);
    else enter(it.path);
  }

  function up() {
    if (!dir) return;
    const was = dir;
    enter(dir.split('/').slice(0, -1).join('/'));
    sel = Math.max(0, items.findIndex((x) => x.path === was));
  }

  function keys(e: KeyboardEvent) {
    const empty = filter === '';
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') sel = (sel + (e.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length;
    else if (e.key === 'Enter') act(items[sel]);
    else if (e.key === 'ArrowRight' && empty && items[sel]?.kind === 'dir') act(items[sel]);
    else if ((e.key === 'ArrowLeft' || e.key === 'Backspace') && empty && dir) up();
    else if (e.key === 'Escape' && close) close();
    else return;
    e.preventDefault();
    e.stopPropagation();
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="opener-back" onclick={(e) => e.target === e.currentTarget && close?.()}>
  <div class="opener" role="dialog" aria-label="Open a note">
    <input bind:this={input} bind:value={filter} onkeydown={keys} oninput={() => (sel = 0)} placeholder={`Search notes, or browse ${dir || 'the vault'}/`} aria-label="Search notes" autocomplete="off" spellcheck="false" />
    <ul bind:this={list}>
      {#each items as it, i (it.kind + it.path)}
        <li class:sel={i === sel} class="k-{it.kind}">
          <button onmousedown={(e) => e.preventDefault()} onclick={() => act(it)} onmousemove={() => (sel = i)}>{it.label}</button>
        </li>
      {/each}
    </ul>
    <p class="dim">↑↓ choose · Enter open · ← up · type to search all notes{close ? ' · Esc close' : ''}</p>
  </div>
</div>
