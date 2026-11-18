<script lang="ts">
  // The Files view (the TUI's files.rs): a folder tree from the pane's root, read lazily per open
  // folder, dotfiles off until `.`; a file opens through its opener (Opened.svelte: the built-in
  // reader or a plugin's) in place of the tree, and the tree comes back as it was when the file
  // closes. Secrets are never listed (the server leaves them out). Expansion and selection are kept
  // per pane for the page's life (tree.svelte.ts).
  // `a` makes a file (a name ending in `/` a folder), `A` a folder and `R` renames the selected row,
  // asked with a small input over the tree; the server never overwrites (server/home.ts).
  // The search field (`/`) replaces the tree with results: names under the root at once (the
  // server's `find`, no secrets, dot entries or symlinks), then, after a pause (3+ characters) or
  // Enter, the kernel's search of the vault's notes (words and meaning) with a snippet each. A result
  // opens through the normal opener (plugins included); Esc clears.
  import { onMount } from 'svelte';
  import type { Pane } from '../app.svelte.ts';
  import { app, openFile, openDir, tilde, typing, notice } from '../app.svelte.ts';
  import { files, zen } from '../api.ts';
  import type { Found, SearchHit } from '../api.ts';
  import Opened from '../Opened.svelte';
  import { trees } from './tree.svelte.ts';
  import { pageRows, follow, plain, human } from './list.ts';

  let { pane, active }: { pane: Pane; active: boolean } = $props();

  /** Never listed: huge, generated, or noise (as the terminal app). */
  const SKIP = new Set(['.git', 'target', 'node_modules', '.venv', '__pycache__']);
  /** Most rows shown, so a huge folder can't stall the page. */
  const MAX_ROWS = 5000;

  const t = $derived(trees[pane]);
  let list: HTMLElement | undefined = $state();
  const pending = new Set<string>();

  const parentOf = (p: string) => (p.lastIndexOf('/') > 0 ? p.slice(0, p.lastIndexOf('/')) : '/');

  async function read(dir: string) {
    if (pending.has(dir)) return;
    pending.add(dir);
    const tree = trees[pane];
    try {
      const r = await files.list(dir);
      tree.listing[dir] = r.entries;
      delete tree.failed[dir];
    } catch (e) {
      tree.failed[dir] = (e as Error).message;
    } finally {
      pending.delete(dir);
    }
  }

  /** Read every open folder again (`r`; `forget`: drop what was listed first). */
  function reload(forget: boolean) {
    const tree = trees[pane];
    if (forget) {
      tree.listing = {};
      tree.failed = {};
    }
    for (const r of rows) if (r.dir && tree.open[r.path]) read(r.path);
    read(tree.root);
  }

  // Follow the pane's root: a new root opens; going up selects the folder we came from.
  $effect(() => {
    const root = app.root[pane];
    const tree = trees[pane];
    if (!root || root === tree.root) return;
    const from = tree.root;
    tree.root = root;
    tree.open[root] = true;
    if (from && from.startsWith(root + '/')) {
      // Open the folders down to where we were, and select it.
      for (let p = parentOf(from); p.length > root.length; p = parentOf(p)) tree.open[p] = true;
      tree.sel = from;
    } else tree.sel = null;
  });

  interface Row {
    path: string;
    name: string;
    dir: boolean;
    depth: number;
    size: number;
    up?: boolean;
  }

  const rows = $derived.by((): Row[] => {
    const out: Row[] = [];
    if (!t.root) return out;
    if (t.root !== '/' && t.root !== app.home && t.root.startsWith(app.home + '/')) out.push({ path: '..', name: '..', dir: true, depth: 0, size: 0, up: true });
    const walk = (dir: string, depth: number) => {
      for (const e of t.listing[dir] ?? []) {
        if (out.length >= MAX_ROWS) return;
        if (SKIP.has(e.name) || (e.hidden && !t.hidden)) continue;
        out.push({ path: e.path, name: e.name, dir: e.dir, depth, size: e.size });
        if (e.dir && t.open[e.path]) walk(e.path, depth + 1);
      }
    };
    walk(t.root, 0);
    return out;
  });

  // Read open folders that aren't listed yet.
  $effect(() => {
    if (!t.root) return;
    if (!(t.root in t.listing) && !(t.root in t.failed)) read(t.root);
    for (const r of rows) if (r.dir && !r.up && t.open[r.path] && !(r.path in t.listing) && !(r.path in t.failed)) read(r.path);
  });

  const at = $derived(Math.max(0, rows.findIndex((r) => r.path === t.sel)));

  onMount(() => {
    if (t.root) reload(false);
    follow(list);
  });

  // The tree comes back from the reader: show the selected row.
  $effect(() => {
    if (!app.file[pane]) follow(list);
  });

  function select(i: number) {
    if (!rows.length) return;
    t.sel = rows[Math.min(rows.length - 1, Math.max(0, i))].path;
    follow(list);
  }

  /** Expand the selected folder; true when it was a closed folder. */
  function expand(r: Row | undefined): boolean {
    if (!r || !r.dir || r.up || t.open[r.path]) return false;
    t.open[r.path] = true;
    return true;
  }

  function foldOrParent(r: Row | undefined) {
    if (!r || r.up) return;
    if (r.dir && t.open[r.path]) {
      t.open[r.path] = false;
      return;
    }
    const parent = parentOf(r.path);
    const i = rows.findIndex((x) => x.path === parent);
    if (i >= 0) select(i);
    else if (parent === t.root && rows[0]?.up) select(0);
  }

  function activate(r: Row | undefined) {
    if (!r) return;
    if (r.up) openDir(parentOf(t.root), pane);
    else if (r.dir) {
      if (!expand(r)) foldOrParent(r);
    } else openFile(r.path, pane);
  }

  // ---- search ----

  interface Result {
    key: string;
    path: string;
    name: string;
    sub: string;
    dir: boolean;
    snippet?: string;
  }

  let q = $state('');
  let input: HTMLInputElement | undefined = $state();
  let names = $state<Found[]>([]);
  let hits = $state<SearchHit[]>([]);
  let nameBusy = $state(false);
  let contentBusy = $state(false);
  let contentErr = $state('');
  let hit = $state(-1);
  let nameSeq = 0;
  let contentSeq = 0;
  let nameTimer: ReturnType<typeof setTimeout> | undefined;
  let contentTimer: ReturnType<typeof setTimeout> | undefined;
  const searching = $derived(q.trim() !== '');

  const base = (p: string) => p.slice(p.lastIndexOf('/') + 1);
  /** A path shown shortly: in the vault, from it; else `~/…`. */
  const shown = (p: string) => (app.vault && p.startsWith(app.vault + '/') ? p.slice(app.vault.length + 1) : tilde(p));

  /** Names first, then content matches (their snippet), each file once. */
  const results = $derived.by((): Result[] => {
    const out: Result[] = names.map((f) => ({ key: 'n:' + f.path, path: f.path, name: f.name, sub: shown(f.path), dir: f.dir }));
    const seen = new Set(out.map((r) => r.path));
    for (const h of hits) {
      if (!h.path || seen.has(h.path)) continue;
      seen.add(h.path);
      out.push({ key: 'c:' + h.path, path: h.path, name: h.title || base(h.path), sub: shown(h.path), dir: false, snippet: h.snippet });
    }
    return out;
  });

  function clearSearch() {
    clearTimeout(nameTimer);
    clearTimeout(contentTimer);
    nameSeq++;
    contentSeq++;
    q = '';
    names = [];
    hits = [];
    nameBusy = contentBusy = false;
    contentErr = '';
    hit = -1;
  }

  async function findNames(text: string, root: string) {
    const seq = ++nameSeq;
    nameBusy = true;
    try {
      const r = await files.find(root, text);
      if (seq === nameSeq) names = r.results;
    } catch {
      if (seq === nameSeq) names = [];
    } finally {
      if (seq === nameSeq) nameBusy = false;
    }
  }

  async function findContent(text: string) {
    const seq = ++contentSeq;
    contentBusy = true;
    contentErr = '';
    try {
      const r = await zen.search(text, 'notes', 10);
      if (seq === contentSeq) hits = r.results;
    } catch (e) {
      if (seq === contentSeq) {
        hits = [];
        contentErr = (e as Error).message;
      }
    } finally {
      if (seq === contentSeq) contentBusy = false;
    }
  }

  // Typing: names after a moment, content after a longer one (3+ characters); a new text drops what an earlier one found.
  $effect(() => {
    const text = q.trim();
    const root = t.root;
    clearTimeout(nameTimer);
    clearTimeout(contentTimer);
    hit = -1;
    if (!text) return;
    nameTimer = setTimeout(() => findNames(text, root), 120);
    if (text.length >= 3) contentTimer = setTimeout(() => findContent(text), 600);
    else {
      contentSeq++;
      hits = [];
      contentBusy = false;
    }
    return () => {
      clearTimeout(nameTimer);
      clearTimeout(contentTimer);
    };
  });

  function searchNow() {
    const text = q.trim();
    if (!text) return;
    clearTimeout(nameTimer);
    clearTimeout(contentTimer);
    findNames(text, t.root);
    findContent(text);
  }

  function pick(i: number) {
    if (!results.length) return;
    hit = Math.min(results.length - 1, Math.max(0, i));
    follow(list);
  }

  function open(r: Result | undefined) {
    if (!r) return;
    if (r.dir) {
      clearSearch();
      openDir(r.path, pane);
    } else openFile(r.path, pane);
  }

  function searchKey(e: KeyboardEvent) {
    if (e.isComposing) return;
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      if (q) clearSearch();
      else input?.blur();
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      pick(hit < 0 ? 0 : hit + (e.key === 'ArrowDown' ? 1 : -1));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      e.stopPropagation();
      if (hit >= 0 && results[hit]) open(results[hit]);
      else searchNow();
    }
  }

  // ---- new file, new folder, rename ----

  type Ask = { kind: 'file' | 'folder' | 'rename'; dir: string; from?: string; value: string };
  let ask = $state<Ask | null>(null);
  let askInput: HTMLInputElement | undefined = $state();
  let busy = false;

  /** The selected row, unless it is the `..` one or nothing was selected. */
  const chosen = $derived(t.sel && t.sel !== '..' ? rows.find((r) => r.path === t.sel) : undefined);

  /** Where a new entry goes: inside the selected folder, else beside the selected file, else the root. */
  function targetDir(): string {
    const r = chosen;
    if (!r) return t.root;
    return r.dir ? r.path : parentOf(r.path);
  }

  function begin(kind: Ask['kind']) {
    if (kind === 'rename') {
      const r = chosen;
      if (!r) return notice('select a file or folder to rename');
      ask = { kind, dir: parentOf(r.path), from: r.path, value: r.name };
    } else {
      const dir = targetDir();
      trees[pane].open[dir] = true;
      ask = { kind, dir, value: '' };
    }
  }

  // Focus the input as it appears; a rename starts with the name's stem selected.
  $effect(() => {
    if (!askInput || !ask) return;
    askInput.focus();
    const dot = ask.value.lastIndexOf('.');
    if (ask.kind === 'rename') askInput.setSelectionRange(0, dot > 0 ? dot : ask.value.length);
  });

  /** Read a folder again now (not skipped while another read is under way). */
  async function refresh(dir: string) {
    const tree = trees[pane];
    try {
      tree.listing[dir] = (await files.list(dir)).entries;
      delete tree.failed[dir];
    } catch (e) {
      tree.failed[dir] = (e as Error).message;
    }
  }

  async function submit() {
    const a = ask;
    if (!a || busy) return;
    let name = a.value.trim();
    if (!name) return;
    busy = true;
    const tree = trees[pane];
    try {
      if (a.kind === 'rename') {
        const from = a.from!;
        if (name === from.slice(from.lastIndexOf('/') + 1)) {
          ask = null;
          return;
        }
        const { path } = await files.rename(from, name);
        // What was listed or open under the old path now lives under the new one.
        for (const k of Object.keys(tree.open)) if (k === from || k.startsWith(from + '/')) {
          tree.open[path + k.slice(from.length)] = tree.open[k];
          delete tree.open[k];
        }
        for (const k of Object.keys(tree.listing)) if (k === from || k.startsWith(from + '/')) delete tree.listing[k];
        for (const k of Object.keys(tree.failed)) if (k === from || k.startsWith(from + '/')) delete tree.failed[k];
        if (name.startsWith('.')) tree.hidden = true;
        await refresh(a.dir);
        tree.sel = path;
        ask = null;
        follow(list);
      } else {
        const folder = a.kind === 'folder' || name.endsWith('/');
        name = name.replace(/\/+$/, '');
        const { path } = folder ? await files.mkdir(a.dir, name) : await files.create(a.dir, name);
        if (name.startsWith('.')) tree.hidden = true;
        tree.open[a.dir] = true;
        await refresh(a.dir);
        tree.sel = path;
        ask = null;
        follow(list);
        if (!folder) openFile(path, pane);
      }
    } catch (e) {
      notice(`couldn't ${a.kind === 'rename' ? 'rename' : 'create'} ${name}: ${(e as Error).message}`);
    } finally {
      busy = false;
    }
  }

  function askKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      ask = null;
    } else if (e.key === 'Enter' && !e.isComposing) {
      e.preventDefault();
      e.stopPropagation();
      submit();
    }
  }

  function key(e: KeyboardEvent) {
    if (!active || app.file[pane] || !plain(e) || typing(e)) return;
    if (e.key === '/') {
      e.preventDefault();
      input?.focus();
      return;
    }
    if (searching) {
      const move: Record<string, () => void> = {
        ArrowUp: () => pick(hit - 1),
        k: () => pick(hit - 1),
        ArrowDown: () => pick(hit + 1),
        j: () => pick(hit + 1),
        Enter: () => open(results[hit]),
        Escape: clearSearch,
      };
      if (move[e.key]) {
        e.preventDefault();
        move[e.key]();
      }
      return;
    }
    const r = rows[at];
    const page = pageRows(list);
    const acts: Record<string, () => void> = {
      ArrowUp: () => select(at - 1),
      k: () => select(at - 1),
      ArrowDown: () => select(at + 1),
      j: () => select(at + 1),
      PageUp: () => select(at - page),
      PageDown: () => select(at + page),
      Home: () => select(0),
      End: () => select(rows.length - 1),
      ArrowRight: () => {
        if (!expand(r) && r && !r.dir) activate(r);
      },
      Enter: () => activate(r),
      ArrowLeft: () => foldOrParent(r),
      '.': () => (t.hidden = !t.hidden),
      r: () => reload(true),
      a: () => begin('file'),
      A: () => begin('folder'),
      R: () => begin('rename'),
    };
    const act = acts[e.key];
    if (act) {
      e.preventDefault();
      act();
    }
  }

  /** The root as clickable parts: `~ / projects / src`. */
  const crumbs = $derived.by(() => {
    const shown = tilde(t.root);
    if (!shown) return [];
    const out: { name: string; path: string }[] = [];
    if (shown.startsWith('~')) {
      out.push({ name: '~', path: app.home });
      let p = app.home;
      for (const part of shown.slice(1).split('/').filter(Boolean)) out.push({ name: part, path: (p = `${p}/${part}`) });
    } else {
      out.push({ name: '/', path: '/' });
      let p = '';
      for (const part of shown.split('/').filter(Boolean)) out.push({ name: part, path: (p = `${p}/${part}`) });
    }
    return out;
  });
</script>

<svelte:window onkeydown={key} />

{#if app.file[pane]}
  <Opened path={app.file[pane]} {pane} {active} />
{:else}
  <div class="view" bind:this={list}>
    <div class="searchbar">
      <input
        bind:this={input}
        bind:value={q}
        onkeydown={searchKey}
        type="search"
        placeholder="/ search files and notes"
        aria-label="search files"
        spellcheck="false"
        autocomplete="off"
        autocapitalize="off"
      />
      {#if app.vault && t.root !== app.vault}<button class="chip" onclick={() => { clearSearch(); openDir(app.vault, pane); }} title="the vault">vault</button>{/if}
      {#if app.home && t.root !== app.home}<button class="chip" onclick={() => { clearSearch(); openDir(app.home, pane); }} title="your home folder">home</button>{/if}
    </div>
    {#if searching}
      <div class="results" role="listbox" aria-label="search results">
        {#each results as r, i (r.key)}
          <div
            class="result"
            class:sel={i === hit}
            class:active
            data-sel={i === hit ? '1' : '0'}
            role="option"
            tabindex="-1"
            aria-selected={i === hit}
            onclick={() => {
              pick(i);
              open(r);
            }}
            onkeydown={() => {}}
          >
            <span class="rname">{r.name}{r.dir ? '/' : ''}</span>
            <span class="rsub">{r.sub}</span>
            {#if r.snippet}<span class="snip">{r.snippet}</span>{/if}
          </div>
        {/each}
        {#if nameBusy || contentBusy}<p class="dim pad">searching…</p>{/if}
        {#if contentErr}<p class="dim pad">notes search unavailable: {contentErr}</p>{/if}
        {#if !results.length && !nameBusy && !contentBusy}<p class="dim pad">nothing found · Enter searches the notes' words and meaning</p>{/if}
      </div>
    {:else}
    <div class="tools">
      <nav class="crumbs" aria-label="folder">
        {#each crumbs as c, i}
          {#if i > 0 && crumbs[i - 1].name !== '/'}<span class="sep">/</span>{/if}
          <button class:here={i === crumbs.length - 1} onclick={() => openDir(c.path, pane)}>{c.name}</button>
        {/each}
      </nav>
      <span class="grow"></span>
      <button class="chip" onclick={() => begin('file')} title="a">+ file</button>
      <button class="chip" onclick={() => begin('folder')} title="A">+ folder</button>
      <button class="chip" disabled={!chosen || chosen.dir} onclick={() => chosen && !chosen.dir && openFile(chosen.path, pane, true)}>Edit selected file</button>
      <button class="chip" onclick={() => begin('rename')} disabled={!chosen} title="R">rename</button>
      <button class="chip" class:on={t.hidden} onclick={() => (t.hidden = !t.hidden)} title=".">hidden</button>
      <button class="chip" onclick={() => reload(true)} title="r">↻</button>
    </div>
    {#if ask}
      <div class="ask">
        <label for="files-ask" class="dim">{ask.kind === 'rename' ? 'rename to' : ask.kind === 'folder' ? 'new folder in' : 'new file in'} {ask.kind === 'rename' ? '' : tilde(ask.dir) + '/'}</label>
        <input
          id="files-ask"
          bind:this={askInput}
          bind:value={ask.value}
          onkeydown={askKey}
          placeholder={ask.kind === 'file' ? 'name (a trailing / makes a folder)' : 'name'}
          aria-label={ask.kind === 'rename' ? 'new name' : 'name'}
          spellcheck="false"
          autocomplete="off"
          autocapitalize="off"
        />
        <button class="chip" onclick={submit}>{ask.kind === 'rename' ? 'rename' : 'create'}</button>
        <button class="chip" onclick={() => (ask = null)} title="Esc">cancel</button>
      </div>
    {/if}
    {#if t.failed[t.root]}<p class="err">can't list {tilde(t.root)}: {t.failed[t.root]}</p>{/if}
    {#if t.root && !(t.root in t.listing) && !t.failed[t.root]}<p class="dim pad">loading…</p>{/if}
    {#if t.root in t.listing && rows.filter((r) => !r.up).length === 0}<p class="dim pad">empty{t.hidden ? '' : ' · . shows hidden files'}</p>{/if}
    <div role="tree" aria-label="files" tabindex="-1">
      {#each rows as r, i (r.path)}
        <div
          class="row"
          class:sel={i === at}
          class:active
          class:file={!r.dir}
          data-sel={i === at ? '1' : '0'}
          role="treeitem"
          tabindex="-1"
          aria-selected={i === at}
          aria-expanded={r.dir && !r.up ? !!t.open[r.path] : undefined}
          style:padding-left="{14 + r.depth * 16}px"
          onclick={() => {
            select(i);
            activate(r);
          }}
          onkeydown={() => {}}
        >
          <span class="twist">{r.up ? '↰' : !r.dir ? '' : t.open[r.path] ? '▾' : '▸'}</span>
          <span class="name">{r.name}{r.dir && !r.up ? '/' : ''}</span>
          {#if r.dir && t.failed[r.path]}<span class="err small">can't read</span>{/if}
          {#if !r.dir}<span class="size">{human(r.size)}</span>{/if}
        </div>
      {/each}
    </div>
    {/if}
  </div>
{/if}

<style>
  .view { container-type: inline-size; padding: 6px 0 24px; font-family: var(--sans); font-size: 14px; }
  .searchbar { display: flex; gap: 8px; align-items: center; padding: 2px 14px 6px; }
  .searchbar input { flex: 1; max-width: 420px; min-width: 0; font: inherit; font-size: 13px; color: var(--ink); background: transparent; border: 1px solid var(--faint); border-radius: 6px; padding: 4px 8px; }
  .searchbar input:focus { outline: none; border-color: var(--accent); }
  .result { display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px 10px; padding: 6px 14px; margin: 0 6px; border-radius: 6px; cursor: pointer; outline-offset: -1px; }
  .result:hover { background: color-mix(in srgb, var(--ink) 4%, transparent); }
  .result.sel { outline: 1px solid var(--faint); }
  .result.sel.active { outline: none; background: color-mix(in srgb, var(--accent) 14%, transparent); }
  .rname { min-width: 0; overflow-wrap: anywhere; }
  .rsub { color: var(--dim); font-size: 12px; min-width: 0; overflow-wrap: anywhere; }
  .snip { flex-basis: 100%; color: var(--dim); font-size: 12px; line-height: 1.4; overflow: hidden; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; }
  .tools { display: flex; gap: 8px; align-items: center; padding: 4px 14px 8px; }
  .crumbs { display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px; min-width: 0; font-size: 13px; }
  .crumbs button { color: var(--dim); padding: 2px 3px; border-radius: 4px; }
  .crumbs button:hover { color: var(--ink); background: color-mix(in srgb, var(--ink) 6%, transparent); }
  .crumbs button.here { color: var(--ink); }
  .sep { color: var(--faint); }
  .grow { flex: 1; }
  .chip { font-size: 12px; color: var(--dim); border: 1px solid var(--faint); border-radius: 6px; padding: 3px 8px; flex: none; }
  .chip.on { color: var(--accent); border-color: var(--accent); }
  .chip:disabled { opacity: 0.45; cursor: default; }
  .ask { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; padding: 2px 14px 8px; font-size: 13px; }
  .ask label { min-width: 0; overflow-wrap: anywhere; }
  .ask input { flex: 1; min-width: 8em; max-width: 320px; font: inherit; color: var(--ink); background: transparent; border: 1px solid var(--faint); border-radius: 6px; padding: 4px 8px; }
  .ask input:focus { outline: none; border-color: var(--accent); }
  .pad { padding: 2px 14px; margin: 0; font-size: 13px; }
  .err { color: var(--err); padding: 4px 14px; margin: 0; font-size: 13px; }
  .err.small { padding: 0; font-size: 12px; }
  .row { display: flex; align-items: baseline; gap: 6px; min-height: 28px; box-sizing: border-box; padding: 4px 14px; cursor: pointer; white-space: nowrap; overflow: hidden; border-radius: 6px; margin: 0 6px; outline-offset: -1px; }
  .row:hover { background: color-mix(in srgb, var(--ink) 4%, transparent); }
  .row.sel { outline: 1px solid var(--faint); }
  .row.sel.active { outline: none; background: color-mix(in srgb, var(--accent) 14%, transparent); }
  .twist { width: 1em; flex: none; color: var(--dim); text-align: center; }
  .name { overflow: hidden; text-overflow: ellipsis; min-width: 0; }
  .file .name { color: color-mix(in srgb, var(--ink) 82%, var(--paper)); }
  .size { margin-left: auto; padding-left: 12px; color: var(--dim); font-size: 12px; flex: none; }
  .dim { color: var(--dim); }
  @media (pointer: coarse) { .row { min-height: 40px; padding-top: 8px; padding-bottom: 8px; } }
</style>
