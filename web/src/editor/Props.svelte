<script lang="ts">
  // The note's properties (its YAML front matter), one row each, as Obsidian shows them.
  // A value is saved when you leave it or press Enter; lists are comma separated.
  import { NOTE_TYPES, readProps, removeProp, setProp } from '../doc/props.ts';

  let { frontmatter, change }: { frontmatter: string; change: (fm: string) => void } = $props();
  const rows = $derived(readProps(frontmatter));
  let adding = $state(false);
  let newKey = $state('');

  function commit(key: string, list: boolean, value: string) {
    const next = setProp(frontmatter, key, value, list);
    if (next !== frontmatter) change(next);
  }

  function add() {
    const key = newKey.trim().replace(/:$/, '');
    adding = false;
    newKey = '';
    if (!key || rows?.some((p) => p.key === key)) return;
    const list = ['aliases', 'tags', 'areas'].includes(key);
    change(setProp(frontmatter, key, '', list));
    setTimeout(() => (document.querySelector(`.props [data-key="${CSS.escape(key)}"]`) as HTMLInputElement | null)?.focus());
  }

  const keys = (e: KeyboardEvent) => {
    if (e.key === 'Enter') (e.currentTarget as HTMLInputElement).blur();
  };
</script>

<section class="props">
  <div class="panel-head">Properties</div>
  {#if rows === null}
    <p class="dim">The properties aren't valid YAML; fix them in Obsidian.</p>
  {:else}
    <datalist id="zw-types">{#each NOTE_TYPES as t}<option value={t}></option>{/each}</datalist>
    {#each rows as p (p.key)}
      <div class="prop">
        <span class="k">{p.key}</span>
        {#if p.fixed}
          <span class="v fixed" title="Edit this one in Obsidian">{p.value}</span>
        {:else}
          <input class="v" data-key={p.key} value={p.value} list={p.key === 'type' ? 'zw-types' : undefined}
            placeholder={p.list ? 'a, b, c' : 'empty'} spellcheck="false"
            onchange={(e) => commit(p.key, p.list, e.currentTarget.value)} onkeydown={keys} />
        {/if}
        <button class="x" title="Remove {p.key}" onclick={() => change(removeProp(frontmatter, p.key))}>×</button>
      </div>
    {/each}
    {#if adding}
      <!-- svelte-ignore a11y_autofocus -->
      <input class="newkey" bind:value={newKey} placeholder="property name" autofocus spellcheck="false"
        onkeydown={(e) => { if (e.key === 'Enter') add(); if (e.key === 'Escape') { adding = false; newKey = ''; } }}
        onblur={add} />
    {:else}
      <button class="add" onclick={() => (adding = true)}>+ property</button>
    {/if}
  {/if}
</section>
