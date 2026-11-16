<script lang="ts">
  // A pane's file: read in the reader (its reading aids, shared with the terminal app), or, for a
  // vault note, written in the writer in the same place (`e` or Write; Alt+W reads again).
  // The editor (CodeMirror) loads only when a note is first written.
  import { untrack } from 'svelte';
  import Reader from './Reader.svelte';
  import { app, editNote, notice, readNote, vaultNote, type Pane } from './app.svelte.ts';
  import { expected } from './reader/nav.ts';
  import { api } from '../lib/api.ts';

  let { path, pane, active }: { path: string; pane: Pane; active: boolean } = $props();

  const rel = $derived(vaultNote(path));
  const writing = $derived(app.write[pane] && rel !== null);
  let note = $state<{ path: string; text: string; hash: string | null } | null>(null);

  $effect(() => {
    const r = writing ? rel : null;
    untrack(() => {
      if (r === null) note = null;
      else if (note?.path !== r) void load(r);
    });
  });

  async function load(r: string) {
    if (app.draft?.path === r) {
      note = { path: r, text: app.draft.text, hash: null };
      app.draft = null;
      return;
    }
    try {
      const d = await api.read(r);
      if (vaultNote(path) === r && app.write[pane]) note = { path: r, ...d };
    } catch (e) {
      notice(`can't write ${r}: ${(e as Error).message}`, 4000);
      readNote(pane);
    }
  }

  function read() {
    // The reader comes back to this file keeping its way back.
    expected[pane] = path;
    readNote(pane);
  }
</script>

{#if writing}
  {#if note && note.path === rel}
    {#await import('../editor/Editor.svelte') then { default: Editor }}
      {#key note.path}
        <Editor path={note.path} text={note.text} hash={note.hash} keys={active} {read} open={(p) => editNote(p, pane)} />
      {/key}
    {/await}
  {:else}
    <p class="dim wait">opening…</p>
  {/if}
{:else}
  <Reader {path} {pane} {active} />
{/if}

<style>
  .wait { padding: 12px 16px; margin: 0; font-size: 13px; }
</style>
