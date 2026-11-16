<script lang="ts">
  // The zen app (zen/Shell.svelte): board, sessions, files, tasks. A vault note is read and
  // written in a pane's viewer; the editor's old addresses land there:
  //   ?path=wiki/Note.md   the board, writing that note (the zen TUI's `e` opens this)
  //   ?notes               the board's Files, at the vault
  //   ?new=wiki            a new note in that folder: its file name first, then its H1 (the TUI's
  //                        `n`); then the board, writing it
  import { onMount } from 'svelte';
  import Shell from './zen/Shell.svelte';
  import { app } from './zen/app.svelte.ts';
  import { api } from './lib/api.ts';
  import { cleanFileName } from './doc/note.ts';

  const params = new URLSearchParams(location.search);
  let mode = $state<'name' | 'title' | 'zen'>(params.has('new') && !params.has('path') ? 'name' : 'zen');
  let path = $state('');
  let dir = $state((params.get('new') ?? '').replace(/^\/+|\/+$/g, ''));
  let name = $state('');
  let title = $state('');
  let field = $state<HTMLInputElement>();

  /** On to the board, writing the note (Shell reads `?path=` once it knows the vault). */
  function write(p: string) {
    history.replaceState(null, '', `/?path=${encodeURIComponent(p)}`);
    mode = 'zen';
  }

  async function nameDone() {
    const file = cleanFileName(name);
    if (!file) return;
    const p = dir ? `${dir}/${file}` : file;
    // An existing note opens; nothing is ever overwritten.
    if ((await api.exists(p)).exists) return write(p);
    path = p;
    title = file.replace(/\.md$/, '');
    mode = 'title';
  }

  function titleDone() {
    const t = title.trim();
    if (!t) return;
    // Written only once it has something past its H1 (the editor decides).
    app.draft = { path, text: `# ${t}\n\n` };
    write(path);
  }

  onMount(() => {
    if (mode === 'name') field?.focus();
  });
  $effect(() => {
    if (mode === 'name' || mode === 'title') field?.focus();
  });
</script>

{#if mode === 'name' || mode === 'title'}
  <form class="ask" onsubmit={(e) => { e.preventDefault(); mode === 'name' ? nameDone() : titleDone(); }}>
    {#if mode === 'name'}
      <label for="f">File name <span class="dim">in {dir || 'the vault'}/</span></label>
      <input id="f" bind:this={field} bind:value={name} autocomplete="off" placeholder="My idea" />
    {:else}
      <label for="f">Title <span class="dim">{path}</span></label>
      <div class="h1"><span class="dim">#</span> <input id="f" bind:this={field} bind:value={title} autocomplete="off" /></div>
    {/if}
    <p class="dim">Enter to continue</p>
  </form>
{:else}
  <Shell />
{/if}
