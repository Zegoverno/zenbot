<script lang="ts">
  // Files tab (mock): opens in the shared vault; ↑ and the path climb up to /, the Vault
  // button comes straight back. Folders first, dotfiles dimmed; search looks under this folder.
  import { fsRoot, NOTES, VAULT, type FsNode } from './data.ts';
  import { render } from '../src/zen/md.ts';

  let { ask }: { ask?: (name: string) => void } = $props();

  const KEY = 'zen.mock.files';
  const saved = (() => { try { const v = JSON.parse(sessionStorage.getItem(KEY) ?? 'null'); return typeof v === 'object' ? v : null; } catch { return null; } })();
  let cwd = $state<string>(saved?.cwd ?? VAULT);
  // The open file, if any: it replaces the list; ↑ or its folder's crumb goes back.
  let file = $state<string | null>(saved?.file ?? null);
  $effect(() => sessionStorage.setItem(KEY, JSON.stringify({ cwd, file })));
  let q = $state('');
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
    if (!t) return [];
    const out: { path: string; node: FsNode }[] = [];
    const walk = (n: FsNode, p: string) => {
      for (const k of n.kids ?? []) {
        const kp = join(p, k.name);
        if (k.name.toLowerCase().includes(t)) out.push({ path: kp, node: k });
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
    search: '<circle cx="11" cy="11" r="6"/><path d="M20 20l-4.5-4.5"/>',
  };
</script>

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
      {#if editing}
        <button class="rb" onclick={() => (editing = false)}>Cancel</button>
        <button class="rb pri" onclick={save}>Save</button>
      {:else}
        {#if isNote}<button class="rb" onclick={edit} title="Edit the text (E)">Edit</button>{/if}
        {#if ask}<button class="rb pri" onclick={() => ask?.(title)} title="Ask zenbot about this note">Ask zenbot</button>{/if}
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
<div class="fsearch">{@render ic('search')}<input placeholder={`Search in ${crumbs[crumbs.length - 1].label === '/' ? 'everything' : crumbs[crumbs.length - 1].label}`} bind:value={q} /></div>

<div class="list">
  {#if q.trim()}
    {#each found as r (r.path)}
      <button class="row" class:on={sel === r.path} onclick={() => openNode(r.path, r.node)}>
        <span class="k {kind(r.node)}">{@render ic(kind(r.node))}</span>
        <span class="nm"><span>{r.node.name}</span><small>{shortPath(parent(r.path))}</small></span>
      </button>
    {:else}
      <div class="none">Nothing called “{q.trim()}” here.</div>
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
  @media (max-width: 900px) { .home { padding: 6px; } .home .hl { display: none; } .bl small { display: none; } .rscroll { padding: 16px 16px 120px; } }
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
