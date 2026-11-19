<script lang="ts">
  // Files tab (mock): opens in the shared vault; ↑ and the path climb up to /, the Vault
  // button comes straight back. Folders first, dotfiles dimmed; search looks under this folder.
  import { fsRoot, NOTES, PROPS, VAULT, type FsNode } from './data.ts';
  import { render } from '../src/zen/md.ts';

  let { ask }: { ask?: (path: string) => void } = $props();

  const KEY = 'zen.mock.files';
  const saved = (() => { try { const v = JSON.parse(sessionStorage.getItem(KEY) ?? 'null'); return typeof v === 'object' ? v : null; } catch { return null; } })();
  let cwd = $state<string>(saved?.cwd ?? VAULT);
  // The open file, if any: it replaces the list; ↑ or its folder's crumb goes back.
  let file = $state<string | null>(saved?.file ?? null);
  $effect(() => sessionStorage.setItem(KEY, JSON.stringify({ cwd, file, flt })));
  let q = $state('');
  // Filters by the notes' properties (type, status, tags): OR within one, AND across them.
  type Flt = Record<string, string[]>;
  let flt = $state<Flt>(saved?.flt ?? {});
  let fltOpen = $state(false);
  const FACETS = ['type', 'status', 'tags'];
  const active = $derived(Object.values(flt).reduce((n, v) => n + v.length, 0));
  function toggleF(k: string, v: string) {
    const cur = flt[k] ?? [];
    flt[k] = cur.includes(v) ? cur.filter((x) => x !== v) : [...cur, v];
  }
  function frontOf(text: string): Flt {
    const m = /^---\n([\s\S]*?)\n---/.exec(text);
    const out: Flt = {};
    for (const l of (m?.[1] ?? '').split('\n')) {
      const r = /^(\w+):\s*(.+)$/.exec(l);
      if (r) out[r[1]] = r[2].replace(/^\[|\]$/g, '').split(/,\s*/).map((x) => x.trim());
    }
    return out;
  }
  /** A note's properties: its text's front matter, or the stand-in table. */
  function propsOf(p: string): Flt {
    const r = p.startsWith(VAULT + '/') ? p.slice(VAULT.length + 1) : p;
    const t = edits[p] ?? NOTES[r];
    return { ...(PROPS[r] ?? {}), ...(t ? frontOf(t) : {}) };
  }
  const matches = (p: string) => FACETS.every((k) => !flt[k]?.length || flt[k].some((v) => propsOf(p)[k]?.includes(v)));
  /** Every note under this folder, for the filter's values and counts. */
  const notesHere = $derived.by(() => {
    const out: string[] = [];
    const walk = (n: FsNode, p: string) => { for (const k of n.kids ?? []) { const kp = join(p, k.name); if (k.kids) walk(k, kp); else if (k.name.endsWith('.md')) out.push(kp); } };
    walk(here, cwd);
    return out;
  });
  const facets = $derived(FACETS.map((k) => {
    const c = new Map<string, number>();
    for (const p of notesHere) for (const v of propsOf(p)[k] ?? []) c.set(v, (c.get(v) ?? 0) + 1);
    for (const v of flt[k] ?? []) if (!c.has(v)) c.set(v, 0);
    return [k, [...c].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))] as [string, [string, number][]];
  }).filter(([, vs]) => vs.length));
  let sel = $state<string | null>(null);
  // Long paths keep the current folder in view: the crumbs scroll to their end.
  let crumbsEl: HTMLElement | undefined = $state();
  $effect(() => { void cwd; void file; queueMicrotask(() => crumbsEl && (crumbsEl.scrollLeft = crumbsEl.scrollWidth)); });

  const parts = (p: string) => p.split('/').filter(Boolean);
  function nodeAt(p: string): FsNode | null {
    let n: FsNode | undefined = fsRoot;
    for (const x of parts(p)) n = n?.kids?.find((k) => k.name === x);
    return n?.kids ? n : null;
  }
  const join = (a: string, b: string) => (a === '/' ? '' : a) + '/' + b;
  const parent = (p: string) => '/' + parts(p).slice(0, -1).join('/');
  const inVault = $derived(cwd === VAULT || cwd.startsWith(VAULT + '/'));
  const here = $derived(nodeAt(cwd) ?? fsRoot);
  const sorted = (ks: FsNode[]) => [...ks].sort((a, b) => (+!a.kids - +!b.kids) || a.name.localeCompare(b.name));
  const entries = $derived(sorted(here.kids ?? []));

  /** Breadcrumbs: inside the vault they start at "Vault"; outside, at "/". */
  const crumbs = $derived.by(() => {
    const c = folderCrumbs();
    return file ? [...c, { label: file.split('/').pop()!, path: file }] : c;
  });
  function folderCrumbs() {
    if (inVault) {
      const rest = parts(cwd.slice(VAULT.length));
      return [{ label: 'Vault', path: VAULT }, ...rest.map((x, i) => ({ label: x, path: VAULT + '/' + rest.slice(0, i + 1).join('/') }))];
    }
    const ps = parts(cwd);
    return [{ label: '/', path: '/' }, ...ps.map((x, i) => ({ label: x, path: '/' + ps.slice(0, i + 1).join('/') }))];
  }

  /** Search: names under the current folder, with where they are. */
  const found = $derived.by(() => {
    const t = q.trim().toLowerCase();
    if (!t && !active) return [];
    const out: { path: string; node: FsNode }[] = [];
    const walk = (n: FsNode, p: string) => {
      for (const k of n.kids ?? []) {
        const kp = join(p, k.name);
        if ((!t || k.name.toLowerCase().includes(t)) && (!active || (!k.kids && k.name.endsWith('.md') && matches(kp)))) out.push({ path: kp, node: k });
        walk(k, kp);
      }
    };
    walk(here, cwd);
    return out.slice(0, 40);
  });
  const shortPath = (p: string) => (p.startsWith(VAULT + '/') ? p.slice(VAULT.length + 1) : p.replace('/home/exedev', '~'));

  function go(p: string) { if (p === file) return; const from = file; cwd = p; q = ''; file = null; editing = false; sel = from; }
  function up() { if (file) go(cwd); else go(parent(cwd)); }
  function openNode(p: string, n: FsNode) {
    if (n.kids) return go(p);
    cwd = parent(p); q = ''; sel = p; file = p; editing = false;
  }

  // ---- the open note ----
  // A simple built-in reader (with a plain-text Edit) stands in for now; the zenread plugin
  // will own reading later (Ze, 2026-11-19).
  let edits = $state<Record<string, string>>(JSON.parse(sessionStorage.getItem(KEY + '.edits') ?? '{}'));
  $effect(() => sessionStorage.setItem(KEY + '.edits', JSON.stringify(edits)));
  const rel = (p: string) => (p.startsWith(VAULT + '/') ? p.slice(VAULT.length + 1) : p);
  const fileName = $derived(file ? file.split('/').pop()! : '');
  const isNote = $derived(/\.md$/i.test(fileName));
  const title = $derived(fileName.replace(/\.md$/i, ''));
  const text = $derived(file ? (edits[file] ?? NOTES[rel(file)] ?? `# ${title}\n\nA note in ${shortPath(parent(file))}.\n`) : '');
  const node = $derived(file ? findNode(file) : null);
  function findNode(p: string) { const n = nodeAt(parent(p)); return n?.kids?.find((k) => k.name === p.split('/').pop()) ?? null; }
  const doc = $derived(isNote ? render(text) : null);
  const props = $derived((doc?.front ?? '').split('\n').map((l) => /^(\w+):\s*(.+)$/.exec(l)).filter((m): m is RegExpExecArray => !!m)
    .map((m) => [m[1], m[2].replace(/^\[|\]$/g, '').split(/,\s*/)] as [string, string[]]));
  /** Notes that link here, by [[title]]. */
  const backlinks = $derived(file ? Object.keys(NOTES).filter((k) => VAULT + '/' + k !== file && (edits[VAULT + '/' + k] ?? NOTES[k]).includes(`[[${title}]]`)) : []);
  /** Paths of every vault note, to follow [[links]] by title. */
  function allNotes(n: FsNode = nodeAt(VAULT)!, p = VAULT, out: string[] = []) {
    for (const k of n.kids ?? []) { const kp = p + '/' + k.name; if (k.kids) allNotes(k, kp, out); else if (k.name.endsWith('.md')) out.push(kp); }
    return out;
  }
  function follow(name: string) {
    const p = allNotes().find((x) => x.split('/').pop() === name + '.md');
    if (p) openNode(p, { name });
  }
  /** Mount the rendered note; its checkboxes tick (and write the text), [[links]] open their note. */
  function mount(host: HTMLElement) {
    $effect(() => {
      if (!doc) return;
      host.replaceChildren(doc.el);
      host.parentElement?.scrollTo(0, 0);
      host.querySelectorAll<HTMLInputElement>('input[type=checkbox]').forEach((b, i) => {
        b.disabled = false;
        b.onchange = () => toggleTask(i, b.checked);
      });
    });
    const click = (e: MouseEvent) => {
      const a = (e.target as HTMLElement).closest('a');
      if (!a) return;
      if (a.dataset.wiki) { e.preventDefault(); follow(a.dataset.wiki); }
      else if (a.getAttribute('href')?.startsWith('http')) { a.target = '_blank'; a.rel = 'noopener'; }
    };
    host.addEventListener('click', click);
    return () => host.removeEventListener('click', click);
  }
  function toggleTask(i: number, on: boolean) {
    let n = -1;
    edits[file!] = text.replace(/^(\s*[-*] \[)( |x|X)(\])/gm, (m, a, _x, c) => (++n === i ? a + (on ? 'x' : ' ') + c : m));
  }
  // Between files: the other files of this folder, in list order; ‹ › and ← → step through them.
  const siblings = $derived(file ? entries.filter((n) => !n.kids).map((n) => join(cwd, n.name)) : []);
  const at = $derived(file ? siblings.indexOf(file) : -1);
  let sibOpen = $state(false);
  function step(d: number) {
    if (at < 0 || !siblings.length || editing) return;
    const p = siblings[(at + d + siblings.length) % siblings.length];
    openNode(p, { name: p.split('/').pop()! }); sibOpen = false;
  }
  function keys(e: KeyboardEvent) {
    if (!file || editing || e.metaKey || e.ctrlKey || e.altKey) return;
    const t = e.target as HTMLElement;
    if (t.closest('input, textarea, [contenteditable]')) return;
    if (e.key === 'ArrowLeft') { e.preventDefault(); step(-1); }
    else if (e.key === 'ArrowRight') { e.preventDefault(); step(1); }
    else if (e.key === 'Escape' && sibOpen) { e.stopPropagation(); sibOpen = false; }
  }
  const nameOf = (p: string) => p.split('/').pop()!;
  let editing = $state(false);
  let draft = $state('');
  function edit() { draft = text; editing = true; }
  function save() { edits[file!] = draft; editing = false; }
  const count = (n: FsNode) => { const c = n.kids?.length ?? 0; return c === 0 ? 'empty' : `${c} item${c === 1 ? '' : 's'}`; };
  const kind = (n: FsNode) => (n.kids ? 'dir' : /\.(png|jpe?g|gif|webp)$/i.test(n.name) ? 'img' : /\.md$/i.test(n.name) ? 'note' : 'file');
  const ICON: Record<string, string> = {
    dir: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
    note: '<path d="M6 3h9l4 4v14H6z"/><path d="M9 12h7M9 16h5"/>',
    img: '<rect x="4" y="4" width="16" height="16" rx="2"/><circle cx="9" cy="9" r="1.6"/><path d="M20 16l-5-5-9 9"/>',
    file: '<path d="M6 3h9l4 4v14H6z"/>',
    up: '<path d="M12 19V5M6 11l6-6 6 6"/>',
    vault: '<path d="M4 6a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v13H6a2 2 0 0 1-2-2z"/><path d="M8 4v15"/>',
    filter: '<path d="M4 6h16M7 12h10M10 18h4"/>',
    search: '<circle cx="11" cy="11" r="6"/><path d="M20 20l-4.5-4.5"/>',
  };
</script>

<svelte:window onkeydown={keys} />

{#snippet ic(name: string)}<svg class="fi" viewBox="0 0 24 24" aria-hidden="true">{@html ICON[name]}</svg>{/snippet}

<div class="fwrap">
<div class="bar">
  <button class="ib" title={file ? 'Back to the folder' : 'Up one folder'} aria-label={file ? 'Back to the folder' : 'Up one folder'} disabled={cwd === '/' && !file} onclick={up}>{@render ic('up')}</button>
  <nav class="crumbs" aria-label="Path" bind:this={crumbsEl}>
    {#each crumbs as c, i}
      {#if i > 0}<span class="sep">›</span>{/if}
      <button class="cr" class:last={i === crumbs.length - 1} class:vault={c.label === 'Vault'} onclick={() => go(c.path)} title={c.path}>{c.label}</button>
    {/each}
  </nav>
  {#if cwd !== VAULT || file}
    <button class="home" title="Back to the vault" onclick={() => go(VAULT)}>{@render ic('vault')}<span class="hl">Vault</span></button>
  {/if}
</div>

{#if file}
  <div class="reader">
    <div class="rbar">
      {#each props as [k, vs]}
        {#if k === 'tags'}{#each vs as v}<span class="chip tag">#{v}</span>{/each}
        {:else}<span class="chip" title={k}>{#if k !== 'type'}<em>{k}</em>{/if}{vs.join(', ')}</span>{/if}
      {/each}
      <span class="edited">Edited {node?.when ?? 'today'}</span>
      <span class="sp"></span>
      {#if siblings.length > 1 && !editing}
        <div class="sib">
          <button class="nb" title={`Previous: ${nameOf(siblings[(at - 1 + siblings.length) % siblings.length])} (←)`} aria-label="Previous file" onclick={() => step(-1)}>‹</button>
          <button class="cnt" title="Files in this folder" aria-haspopup="listbox" aria-expanded={sibOpen} onclick={() => (sibOpen = !sibOpen)}>{at + 1} of {siblings.length}</button>
          <button class="nb" title={`Next: ${nameOf(siblings[(at + 1) % siblings.length])} (→)`} aria-label="Next file" onclick={() => step(1)}>›</button>
          {#if sibOpen}
            <div class="sibs" role="listbox">
              {#each siblings as sp, i}
                <button role="option" aria-selected={i === at} class:on={i === at} onclick={() => { openNode(sp, { name: nameOf(sp) }); sibOpen = false; }}>
                  {@render ic(kind({ name: nameOf(sp) }))}<span>{nameOf(sp)}</span>
                </button>
              {/each}
            </div>
          {/if}
        </div>
      {/if}
      {#if editing}
        <button class="rb" onclick={() => (editing = false)}>Cancel</button>
        <button class="rb pri" onclick={save}>Save</button>
      {:else}
        {#if isNote}<button class="rb" onclick={edit} title="Edit the text (E)">Edit</button>{/if}
        {#if ask}<button class="rb pri" onclick={() => ask?.(shortPath(file!))} title="Ask zenbot about this note">Ask zenbot</button>{/if}
      {/if}
    </div>
    <div class="rscroll">
      {#if editing}
        <!-- svelte-ignore a11y_autofocus -->
        <textarea class="raw" bind:value={draft} autofocus spellcheck="true" onkeydown={(e) => { if ((e.metaKey || e.ctrlKey) && e.key === 's') { e.preventDefault(); save(); } if (e.key === 'Escape') { e.stopPropagation(); editing = false; } }}></textarea>
      {:else if isNote}
        <article class="note" {@attach mount}></article>
        {#if backlinks.length}
          <div class="back">
            <div class="bh">Linked from</div>
            {#each backlinks as b}<button class="bl" onclick={() => follow(b.split('/').pop()!.replace(/\.md$/, ''))}>{@render ic('note')}<span>{b.split('/').pop()!.replace(/\.md$/, '')}</span><small>{parent('/' + b).slice(1) || 'Vault'}</small></button>{/each}
          </div>
        {/if}
      {:else if kind(node ?? { name: fileName }) === 'img'}
        <div class="img"><div class="ph">{@render ic('img')}<span>{fileName}</span></div></div>
      {:else}
        <div class="none">No preview for this kind of file. Ask zenbot to open it for you.</div>
      {/if}
    </div>
  </div>
{:else}
<div class="fsearch">{@render ic('search')}<input placeholder={`Search in ${crumbs[crumbs.length - 1].label === '/' ? 'everything' : crumbs[crumbs.length - 1].label}`} bind:value={q} />
  <button class="fb" class:on={fltOpen || active > 0} aria-expanded={fltOpen} title="Filter by properties" onclick={() => (fltOpen = !fltOpen)}>{@render ic('filter')}<span class="hl">Filter</span>{#if active}<b>{active}</b>{/if}</button>
</div>
{#if fltOpen}
  <div class="facets">
    {#each facets as [k, vs]}
      <div class="fg"><span class="fk">{k}</span>
        {#each vs as [v, n]}
          <button class="fc" class:tag={k === 'tags'} class:on={flt[k]?.includes(v)} aria-pressed={flt[k]?.includes(v) ?? false} onclick={() => toggleF(k, v)}>{k === 'tags' ? '#' : ''}{v}<small>{n}</small></button>
        {/each}
      </div>
    {:else}
      <div class="fk none2">No notes with properties here.</div>
    {/each}
    {#if active}<button class="clr" onclick={() => (flt = {})}>Clear filters</button>{/if}
  </div>
{:else if active}
  <div class="facets act">
    {#each FACETS as k}{#each flt[k] ?? [] as v}
      <button class="fc on" class:tag={k === 'tags'} title="Remove this filter" onclick={() => toggleF(k, v)}>{#if k !== 'tags'}<em>{k}</em>{/if}{k === 'tags' ? '#' : ''}{v} ×</button>
    {/each}{/each}
    <button class="clr" onclick={() => (flt = {})}>Clear</button>
  </div>
{/if}

<div class="list">
  {#if q.trim() || active}
    {#if active}<div class="rc">{found.length} note{found.length === 1 ? '' : 's'}</div>{/if}
    {#each found as r (r.path)}
      {@const pr = active ? propsOf(r.path) : {}}
      <button class="row" class:on={sel === r.path} onclick={() => openNode(r.path, r.node)}>
        <span class="k {kind(r.node)}">{@render ic(kind(r.node))}</span>
        <span class="nm"><span>{r.node.name.replace(/\.md$/, '')}</span><small>{shortPath(parent(r.path))}</small></span>
        {#if active}<span class="pc">{#if pr.status}<span class="chip">{pr.status[0]}</span>{/if}{#each (pr.tags ?? []).slice(0, 2) as t}<span class="chip tag">#{t}</span>{/each}</span>{/if}
      </button>
    {:else}
      <div class="none">{q.trim() ? `Nothing called “${q.trim()}”` : 'No notes'}{active ? ' with these filters' : ''} here.</div>
    {/each}
  {:else}
    {#each entries as n (n.name)}
      {@const p = join(cwd, n.name)}
      <button class="row" class:on={sel === p} class:hid={n.name.startsWith('.')} class:isvault={p === VAULT} onclick={() => openNode(p, n)}>
        <span class="k {kind(n)}">{@render ic(p === VAULT ? 'vault' : kind(n))}</span>
        <span class="nm"><span>{n.name}</span>{#if p === VAULT}<small>your shared vault</small>{/if}</span>
        <span class="when">{n.kids ? (n.when ?? count(n)) : n.when}</span>
      </button>
    {:else}
      <div class="none">This folder is empty.</div>
    {/each}
  {/if}
</div>
{/if}
</div>

<style>
  .fwrap { flex: 1; min-height: 0; display: flex; flex-direction: column; width: 100%; max-width: 860px; margin: 0 auto; }
  .bar { display: flex; align-items: center; gap: 6px; padding: 2px 16px 6px 14px; }
  .fi { width: 17px; height: 17px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; flex: none; }
  .ib { width: 30px; height: 30px; border-radius: 9px; display: grid; place-items: center; color: var(--dim); flex: none; }
  .ib:hover:not(:disabled) { background: var(--soft); color: var(--ink); }
  .ib:disabled { opacity: 0.35; cursor: default; }
  .crumbs { flex: 1; min-width: 0; display: flex; align-items: center; gap: 2px; overflow-x: auto; scrollbar-width: none; white-space: nowrap; font-size: 14px; }
  .cr { padding: 4px 6px; border-radius: 7px; color: var(--dim); }
  .cr:hover { background: var(--soft); color: var(--ink); }
  .cr.last { color: var(--ink); font-weight: 600; }
  .cr.vault { color: var(--accent); font-weight: 600; }
  .sep { color: var(--faint); font-size: 13px; }
  .home { display: inline-flex; align-items: center; gap: 6px; padding: 5px 11px 5px 9px; border-radius: 99px; font-size: 13px; font-weight: 600; flex: none;
    color: var(--accent); border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--faint)); background: color-mix(in srgb, var(--accent) 8%, var(--paper)); }
  .home:hover { background: color-mix(in srgb, var(--accent) 16%, var(--paper)); }
  .home .fi { width: 15px; height: 15px; }
  .fsearch { display: flex; align-items: center; gap: 8px; margin: 2px 16px 6px; padding: 8px 12px; border-radius: 12px; background: var(--soft); color: var(--dim); }
  .fsearch input { flex: 1; min-width: 0; border: 0; outline: none; background: transparent; font-size: 14px; color: var(--ink); }
  .list { flex: 1; overflow: auto; padding: 0 10px 80px; display: flex; flex-direction: column; gap: 1px; }
  .row { display: flex; align-items: center; gap: 11px; text-align: left; padding: 9px 12px; border-radius: 12px; font-size: 14px; }
  .row:hover { background: var(--soft); }
  .row.on { background: color-mix(in srgb, var(--accent) 14%, var(--paper)); }
  .row.hid { color: var(--dim); }
  .k { color: var(--dim); display: grid; }
  .k.dir { color: var(--accent); }
  .row.isvault .nm > span { font-weight: 600; }
  .nm { flex: 1; min-width: 0; display: flex; flex-direction: column; }
  .nm > span { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .nm small { font-size: 12px; color: var(--dim); overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .when { font-size: 12px; color: var(--dim); white-space: nowrap; }
  @media (max-width: 900px) { .fb .hl { display: none; } .pc { display: none; } .fk { width: 100%; } .home { padding: 6px; } .home .hl { display: none; } .bl small { display: none; } .rscroll { padding: 16px 16px 120px; } }
  .fb { display: inline-flex; align-items: center; gap: 5px; padding: 3px 9px; margin: -3px -6px -3px 0; border-radius: 99px; font-size: 13px; color: var(--dim); }
  .fb:hover, .fb.on { color: var(--accent); background: color-mix(in srgb, var(--accent) 10%, transparent); }
  .fb b { min-width: 17px; height: 17px; border-radius: 99px; font-size: 11px; display: grid; place-items: center; background: var(--accent); color: var(--on-accent); }
  .facets { display: flex; flex-direction: column; gap: 8px; margin: 0 16px 8px; padding: 10px 12px; border: 1px solid var(--faint); border-radius: 12px; }
  .facets.act { flex-direction: row; flex-wrap: wrap; align-items: center; gap: 6px; border: 0; padding: 0 2px; }
  .fg { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; }
  .fk { font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); font-weight: 650; width: 56px; }
  .fk.none2 { width: auto; text-transform: none; letter-spacing: 0; font-weight: 400; font-size: 13px; }
  .fc { display: inline-flex; align-items: center; gap: 5px; font-size: 13px; padding: 3px 10px; border-radius: 99px; border: 1px solid var(--faint); background: var(--paper); }
  .fc:hover { border-color: var(--dim); }
  .fc.tag { color: var(--accent); }
  .fc small { font-size: 11px; color: var(--dim); }
  .fc em { font-style: normal; opacity: 0.75; margin-right: 2px; }
  .fc.on { background: var(--accent); border-color: var(--accent); color: var(--on-accent); }
  .fc.on small { color: inherit; opacity: 0.8; }
  .clr { align-self: flex-start; font-size: 13px; color: var(--accent); font-weight: 600; padding: 2px 4px; }
  .rc { font-size: 12px; color: var(--dim); padding: 4px 12px 2px; }
  .pc { display: flex; gap: 4px; flex: none; }
  .reader { flex: 1; min-height: 0; display: flex; flex-direction: column; }
  .rbar { display: flex; align-items: center; flex-wrap: wrap; gap: 6px; padding: 4px 16px 10px 20px; border-bottom: 1px solid var(--faint); }
  .chip { font-size: 12px; padding: 2px 9px; border-radius: 99px; background: var(--soft); color: var(--ink); white-space: nowrap; }
  .chip em { font-style: normal; color: var(--dim); margin-right: 5px; }
  .chip.tag { color: var(--accent); background: color-mix(in srgb, var(--accent) 10%, transparent); }
  .edited { font-size: 12px; color: var(--dim); margin-left: 4px; }
  .sp { flex: 1; }
  .rb { padding: 5px 12px; border-radius: 99px; font-size: 13px; font-weight: 600; border: 1px solid var(--faint); }
  .rb:hover { border-color: var(--dim); }
  .rb.pri { background: var(--accent); border-color: var(--accent); color: var(--on-accent); }
  .sib { position: relative; display: flex; align-items: center; margin-right: 4px; border: 1px solid var(--faint); border-radius: 99px; }
  .nb { width: 28px; height: 26px; font-size: 18px; line-height: 1; color: var(--dim); border-radius: 99px; }
  .nb:hover { color: var(--ink); background: var(--soft); }
  .cnt { font-size: 12px; color: var(--dim); padding: 0 4px; white-space: nowrap; font-variant-numeric: tabular-nums; }
  .cnt:hover { color: var(--ink); }
  .sibs { position: absolute; top: 34px; right: 0; z-index: 12; min-width: 240px; max-width: 320px; max-height: 50vh; overflow: auto; padding: 6px; display: flex; flex-direction: column;
    background: var(--paper); border: 1px solid var(--faint); border-radius: 14px; box-shadow: 0 14px 40px var(--shadow); }
  .sibs button { display: flex; align-items: center; gap: 9px; padding: 7px 10px; border-radius: 8px; font-size: 14px; text-align: left; color: var(--dim); }
  .sibs button span { color: var(--ink); overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .sibs button:hover { background: var(--soft); }
  .sibs button.on span { color: var(--accent); font-weight: 600; }
  .rscroll { flex: 1; overflow: auto; padding: 18px 22px 90px; }
  .raw { width: 100%; min-height: 60vh; resize: vertical; border: 1px solid var(--faint); border-radius: 12px; padding: 16px; background: var(--paper); color: var(--ink); font: 14px/1.6 var(--mono); outline: none; }
  .raw:focus { border-color: var(--accent); }
  .note { max-width: 680px; margin: 0 auto; font-size: 15.5px; line-height: 1.65; }
  .note :global(h1) { font-family: var(--serif); font-size: 30px; line-height: 1.2; font-weight: 600; margin: 4px 0 14px; }
  .note :global(h2) { font-size: 17px; font-weight: 650; margin: 26px 0 8px; }
  .note :global(h3) { font-size: 15.5px; font-weight: 650; margin: 20px 0 6px; }
  .note :global(p) { margin: 0 0 12px; }
  .note :global(ul), .note :global(ol) { margin: 0 0 12px; padding-left: 22px; }
  .note :global(li) { margin: 2px 0; }
  .note :global(li p) { margin: 0; }
  .note :global(li.md-task) { list-style: none; margin-left: -22px; }
  .note :global(li.md-task input) { accent-color: var(--accent); width: 16px; height: 16px; vertical-align: -3px; margin: 0 8px 0 0; cursor: pointer; }
  .note :global(li.md-done) { color: var(--dim); text-decoration: line-through; }
  .note :global(a) { color: var(--accent); text-decoration: underline; text-decoration-color: color-mix(in srgb, var(--accent) 35%, transparent); text-underline-offset: 3px; }
  .note :global(a.md-wiki) { text-decoration-style: dotted; font-weight: 550; }
  .note :global(blockquote) { margin: 16px 0; padding: 10px 16px; border-left: 3px solid var(--accent); background: var(--soft); border-radius: 0 10px 10px 0; color: var(--ink); }
  .note :global(blockquote p) { margin: 0; }
  .note :global(table) { border-collapse: collapse; margin: 6px 0 16px; font-size: 14.5px; width: 100%; }
  .note :global(th), .note :global(td) { padding: 7px 10px; border-bottom: 1px solid var(--faint); text-align: left; }
  .note :global(th) { font-weight: 650; color: var(--dim); font-size: 12.5px; text-transform: uppercase; letter-spacing: 0.04em; }
  .note :global(code) { font-family: var(--mono); font-size: 0.9em; background: var(--soft); padding: 1px 5px; border-radius: 5px; }
  .back { max-width: 680px; margin: 28px auto 0; padding-top: 14px; border-top: 1px solid var(--faint); }
  .bh { font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); font-weight: 650; margin-bottom: 6px; }
  .bl { display: flex; align-items: center; gap: 9px; width: 100%; padding: 7px 8px; border-radius: 10px; font-size: 14px; text-align: left; color: var(--dim); }
  .bl span { color: var(--ink); }
  .bl small { margin-left: auto; font-size: 12px; }
  .bl:hover { background: var(--soft); }
  .img { display: grid; place-items: center; padding: 20px; }
  .ph { width: min(100%, 560px); aspect-ratio: 4 / 3; border-radius: 14px; display: grid; place-content: center; justify-items: center; gap: 10px; color: var(--dim); font-size: 13px;
    background: linear-gradient(135deg, color-mix(in srgb, var(--accent) 18%, var(--soft)), var(--soft)); }
  .ph .fi { width: 40px; height: 40px; }
  .none { padding: 20px 12px; color: var(--dim); font-size: 14px; }
</style>
