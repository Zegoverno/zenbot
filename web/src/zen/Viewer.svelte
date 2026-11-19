<script lang="ts">
  // Core Files fallback. Rich reading/writing belongs to optional plugins.
  // Markdown uses the chat's text-node renderer; other files remain escaped plain text.
  import { untrack } from 'svelte';
  import { files, type FileRead } from './api.ts';
  import { app, closeFile, editNote, openFile, readNote, vaultNote, typing, type Pane } from './app.svelte.ts';
  import { api, Conflict } from '../lib/api.ts';
  import { render } from './md.ts';

  let { path, pane, active }: { path: string; pane: Pane; active: boolean } = $props();
  const rel = $derived(vaultNote(path));
  /** Pictures show as themselves (the server's image types; no SVG). */
  const picture = $derived(/\.(png|jpe?g|gif|webp|avif|bmp|ico)$/i.test(path));
  const writing = $derived(app.write[pane] && rel !== null);
  let doc = $state<FileRead | null>(null);
  let text = $state('');
  let saved = $state('');
  let hash = $state<string | null>(null);
  let error = $state('');
  let saving = $state(false);
  let loaded = $state(false);
  let host = $state<HTMLDivElement>();
  let links = $state<{ path: string; title: string }[]>([]);
  let linksTruncated = $state(false);
  let linksError = $state('');
  let linkGeneration = 0;
  $effect(() => {
    const p = path, r = rel, w = writing;
    const g = ++linkGeneration;
    links = []; linksError = ''; linksTruncated = false;
    if (r === null || w || !/\.md$/i.test(p)) return;
    void fetch(`/api/fs/backlinks?path=${encodeURIComponent(p)}`).then(async response => {
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      return response.json() as Promise<{ references: { path: string; title: string }[]; truncated: boolean }>;
    }).then(result => { if (g === linkGeneration) { links = result.references; linksTruncated = result.truncated; } })
      .catch(e => { if (g === linkGeneration) linksError = (e as Error).message; });
  });
  let generation = 0;
  let pendingLeave = $state<(() => void) | null>(null);
  const dirty = $derived(writing && loaded && (text !== saved || hash === null));
  const draftKey = (p: string) => `zen.files.draft:${p}`;
  function getDraft(p: string): { text: string; saved: string; hash: string | null } | null {
    try {
      const d = JSON.parse(sessionStorage.getItem(draftKey(p)) ?? 'null');
      return d && typeof d.text === 'string' && typeof d.saved === 'string' && (typeof d.hash === 'string' || d.hash === null) ? d : null;
    } catch { return null; }
  }
  function persist(p: string, value: string, baseline: string, version: string | null) {
    try {
      if (value !== baseline || version === null) sessionStorage.setItem(draftKey(p), JSON.stringify({ text: value, saved: baseline, hash: version }));
      else sessionStorage.removeItem(draftKey(p));
    } catch { /* Storage may be unavailable; the editor still keeps its in-memory copy. */ }
  }
  function dropDraft(p: string) { try { sessionStorage.removeItem(draftKey(p)); } catch { /* unavailable */ } }
  function input(e: Event) {
    text = (e.currentTarget as HTMLTextAreaElement).value;
    persist(path, text, saved, hash);
  }

  $effect(() => {
    const p = path, r = rel, w = writing;
    untrack(() => { void load(p, r, w); });
  });
  async function load(p: string, r: string | null, w: boolean) {
    const g = ++generation;
    loaded = false; error = ''; doc = null;
    try {
      if (w && r !== null) {
        const stored = getDraft(p);
        const draft = app.draft?.path === r ? app.draft : null;
        const d = stored ? null : draft ? { text: draft.text, hash: null } : await api.read(r);
        if (g !== generation) return;
        if (draft) app.draft = null;
        if (stored) { text = stored.text; saved = stored.saved; hash = stored.hash; }
        else if (d) { text = saved = d.text; hash = d.hash; }
        persist(p, text, saved, hash);
      } else {
        if (r !== null && getDraft(p)) { openFile(p, pane, true, 'builtin'); return; }
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
  async function save(): Promise<boolean> {
    if (rel === null || saving || !loaded) return false;
    const g = generation, value = text, r = rel, p = path;
    saving = true; error = '';
    try {
      const out = await api.write(r, value, hash);
      if (g === generation) { hash = out.hash; saved = value; persist(p, text, saved, hash); }
      return g === generation && text === value;
    } catch (e) {
      if (g === generation) error = e instanceof Conflict
        ? 'Changed on disk. Your edits are kept here; copy them before reloading.'
        : (e as Error).message;
      return false;
    } finally { saving = false; }
  }
  export function leave(next: () => void) {
    if (saving) return;
    if (dirty) pendingLeave = next;
    else next();
  }
  async function saveAndLeave() {
    if (await save()) { const next = pendingLeave; pendingLeave = null; next?.(); }
  }
  function discard() {
    const next = pendingLeave;
    dropDraft(path); pendingLeave = null; next?.();
  }
  function read() { leave(() => readNote(pane)); }
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
      <button onclick={save} disabled={!loaded || saving || text === saved && hash !== null}>Save</button>
      <span role="status">{saving ? 'Saving…' : text !== saved || hash === null ? 'Unsaved' : 'Saved'}</span>
    {/if}
    <button onclick={() => writing ? read() : closeFile(pane)}>{writing ? 'Cancel' : 'Close'}</button>
  </header>
  {#if pendingLeave}
    <div class="guard" role="alert">
      <span>You have unsaved changes.</span>
      <button onclick={() => (pendingLeave = null)} disabled={saving}>Keep editing</button>
      <button onclick={saveAndLeave} disabled={saving}>Save and leave</button>
      <button onclick={discard} disabled={saving}>Discard</button>
    </div>
  {/if}
  {#if error}<p role="alert">{error}</p>{/if}
  {#if !loaded && !error}<p>Opening…</p>
  {:else if writing && loaded}
    <textarea aria-label="Markdown source" value={text} oninput={input} spellcheck="false"></textarea>
  {:else if picture && doc}
    <div class="scroll picture"><img src={`/api/fs/image?path=${encodeURIComponent(path)}&v=${doc.mtime}`} alt={path.split('/').pop()} /></div>
  {:else if doc?.text !== undefined}
    {#if /\.md$/i.test(path)}<div class="scroll"><div bind:this={host}></div>
      {#if links.length || linksTruncated || linksError}<section aria-label="Linked from"><h3>Linked from</h3>
        {#each links as link}<button onclick={() => openFile(link.path, pane)}>{link.title}</button>{/each}
        {#if linksTruncated}<p>Backlinks incomplete: vault scan limit reached.</p>{/if}
        {#if linksError}<p>Backlinks unavailable: {linksError}</p>{/if}
      </section>{/if}</div>
    {:else}<pre>{doc.text}</pre>{/if}
  {:else if doc}<p>{doc.binary ? 'Binary file' : doc.tooBig ? 'File too large to preview' : 'No preview available'}</p>{/if}
</section>

<style>
  .guard { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; padding: 10px 16px; border-bottom: 1px solid var(--faint); }
  .guard span { margin-right: auto; }
  .picture { display: flex; align-items: flex-start; justify-content: center; padding: 12px; }
  .picture img { max-width: 100%; height: auto; image-rendering: auto; background: repeating-conic-gradient(#8881 0 25%, transparent 0 50%) 0 0 / 16px 16px; }
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
