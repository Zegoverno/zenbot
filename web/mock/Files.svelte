<script lang="ts">
  // Files tab (mock): opens in the shared vault; ↑ and the path climb up to /, the Vault
  // button comes straight back. Folders first, dotfiles dimmed; search looks under this folder.
  import { fsRoot, VAULT, type FsNode } from './data.ts';

  const KEY = 'zen.mock.files';
  let cwd = $state(sessionStorage.getItem(KEY) ?? VAULT);
  $effect(() => sessionStorage.setItem(KEY, cwd));
  let q = $state('');
  let sel = $state<string | null>(null);
  // Long paths keep the current folder in view: the crumbs scroll to their end.
  let crumbsEl: HTMLElement | undefined = $state();
  $effect(() => { void cwd; queueMicrotask(() => crumbsEl && (crumbsEl.scrollLeft = crumbsEl.scrollWidth)); });

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
    if (inVault) {
      const rest = parts(cwd.slice(VAULT.length));
      return [{ label: 'Vault', path: VAULT }, ...rest.map((x, i) => ({ label: x, path: VAULT + '/' + rest.slice(0, i + 1).join('/') }))];
    }
    const ps = parts(cwd);
    return [{ label: '/', path: '/' }, ...ps.map((x, i) => ({ label: x, path: '/' + ps.slice(0, i + 1).join('/') }))];
  });

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

  function go(p: string) { cwd = p; q = ''; sel = null; }
  function openNode(p: string, n: FsNode) { if (n.kids) go(p); else sel = p; }
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
  <button class="ib" title="Up one folder" aria-label="Up one folder" disabled={cwd === '/'} onclick={() => go(parent(cwd))}>{@render ic('up')}</button>
  <nav class="crumbs" aria-label="Path" bind:this={crumbsEl}>
    {#each crumbs as c, i}
      {#if i > 0}<span class="sep">›</span>{/if}
      <button class="cr" class:last={i === crumbs.length - 1} class:vault={c.label === 'Vault'} onclick={() => go(c.path)} title={c.path}>{c.label}</button>
    {/each}
  </nav>
  {#if cwd !== VAULT}
    <button class="home" title="Back to the vault" onclick={() => go(VAULT)}>{@render ic('vault')}<span class="hl">Vault</span></button>
  {/if}
</div>

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
  @media (max-width: 900px) { .home { padding: 6px; } .home .hl { display: none; } }
  .none { padding: 20px 12px; color: var(--dim); font-size: 14px; }
</style>
