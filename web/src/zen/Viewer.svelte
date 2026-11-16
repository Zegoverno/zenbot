<script lang="ts">
  // Core Files fallback. Rich reading/writing belongs to optional plugins.
  // Markdown uses the chat's text-node renderer; other files remain escaped plain text.
  import { untrack } from 'svelte';
  import { files, type FileRead } from './api.ts';
  import { app, closeFile, editNote, readNote, vaultNote, typing, type Pane } from './app.svelte.ts';
  import { api, Conflict } from '../lib/api.ts';
  import { render } from './md.ts';

  let { path, pane, active }: { path: string; pane: Pane; active: boolean } = $props();
  const rel = $derived(vaultNote(path));
  const writing = $derived(app.write[pane] && rel !== null);
  let doc = $state<FileRead | null>(null);
  let text = $state('');
  let saved = $state('');
  let hash = $state<string | null>(null);
  let error = $state('');
  let saving = $state(false);
  let loaded = $state(false);
  let host = $state<HTMLDivElement>();
  let generation = 0;

  $effect(() => {
    const p = path, r = rel, w = writing;
    untrack(() => { void load(p, r, w); });
  });
  async function load(p: string, r: string | null, w: boolean) {
    const g = ++generation;
    loaded = false; error = ''; doc = null;
    try {
      if (w && r !== null) {
        const draft = app.draft?.path === r ? app.draft : null;
        const d = draft ? { text: draft.text, hash: null } : await api.read(r);
        if (g !== generation) return;
        if (draft) app.draft = null;
        text = saved = d.text; hash = d.hash;
      } else {
        const d = await files.read(p);
        if (g !== generation) return;
        doc = d;
      }
      loaded = true;
    } catch (e) { if (g === generation) error = (e as Error).message; }
  }
  $effect(() => {
    if (host && doc?.text !== undefined && /\.md$/i.test(path)) {
      host.replaceChildren(render(doc.text, { keepFront: true }).el);
    }
  });
  async function save() {
    if (rel === null || saving || !loaded) return;
    const g = generation, value = text, r = rel;
    saving = true; error = '';
    try {
      const out = await api.write(r, value, hash);
      if (g === generation) { hash = out.hash; saved = value; }
    } catch (e) {
      if (g === generation) error = e instanceof Conflict
        ? 'Changed on disk. Your edits are kept here; copy them before reloading.'
        : (e as Error).message;
    } finally { saving = false; }
  }
  function read() {
    if (saving || ((text !== saved || hash === null) && !confirm('Discard unsaved edits?'))) return;
    readNote(pane);
  }
  function key(e: KeyboardEvent) {
    if (!active) return;
    if (writing && (e.ctrlKey || e.metaKey) && e.key === 's') { e.preventDefault(); void save(); }
    else if (writing && e.altKey && e.key.toLowerCase() === 'w') { e.preventDefault(); read(); }
    else if (!typing(e) && !writing && e.key === 'e' && rel !== null) { e.preventDefault(); editNote(rel, pane); }
    else if (!typing(e) && !writing && e.key === 'Escape') { e.preventDefault(); closeFile(pane); }
  }
</script>

<svelte:window onkeydown={key} />
<section class="reader" aria-label="File">
  <header>
    <span>{path.split('/').pop()}</span>
    {#if writing}
      <button onclick={read} disabled={saving}>Read</button>
      <button onclick={save} disabled={!loaded || saving || text === saved && hash !== null}>Save</button>
      <span role="status">{saving ? 'Saving…' : text !== saved || hash === null ? 'Unsaved' : 'Saved'}</span>
    {:else if rel !== null}
      <button title="write (e)" onclick={() => editNote(rel!, pane)}>Edit</button>
    {/if}
    <button onclick={() => writing ? read() : closeFile(pane)}>{writing ? 'Cancel' : 'Close'}</button>
  </header>
  {#if error}<p role="alert">{error}</p>{/if}
  {#if !loaded && !error}<p>Opening…</p>
  {:else if writing && loaded}
    <textarea aria-label="Markdown source" bind:value={text} spellcheck="false"></textarea>
  {:else if doc?.text !== undefined}
    {#if /\.md$/i.test(path)}<div class="scroll" bind:this={host}></div>
    {:else}<pre>{doc.text}</pre>{/if}
  {:else if doc}<p>{doc.binary ? 'Binary file' : doc.tooBig ? 'File too large to preview' : 'No preview available'}</p>{/if}
</section>

<style>
  .reader { display: flex; flex-direction: column; height: 100%; min-height: 0; }
  header { display: flex; gap: 10px; align-items: center; padding: 10px 16px; border-bottom: 1px solid var(--faint); }
  header span:first-child { margin-right: auto; overflow-wrap: anywhere; }
  .scroll, pre { overflow: auto; padding: 16px; margin: 0; }
  .scroll { line-height: 1.6; overflow-wrap: anywhere; }
  .scroll :global(pre) { overflow: auto; }
  .scroll :global(table) { border-collapse: collapse; }
  .scroll :global(th), .scroll :global(td) { border: 1px solid var(--faint); padding: 4px 8px; }
  .scroll :global(a) { color: var(--accent); }
  textarea { flex: 1; min-height: 240px; resize: none; padding: 16px; color: inherit; background: transparent; font: 14px/1.6 monospace; border: 0; }
  p { padding: 8px 16px; }
</style>
