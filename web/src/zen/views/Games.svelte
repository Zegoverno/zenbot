<script lang="ts">
  // The Games view (built in, ADR 0001): a picker over every enabled plugin's `games`; the last one
  // played opens when the view does, and "← games" goes back to the picker. With none installed it
  // points to the marketplace.
  import PluginFrame from '../PluginFrame.svelte';
  import { settings, type Pane } from '../app.svelte.ts';
  import { games, plugins, type Game } from '../plugins.svelte.ts';

  let { pane }: { pane: Pane; active: boolean } = $props();

  const LAST = 'zen.games.last';
  const list = $derived(games());
  let playing = $state<string | null>(localStorage.getItem(LAST));
  const game = $derived(list.find((g) => g.key === playing) ?? null);
  const titleOf = (plugin: string) => plugins.list.find((p) => p.name === plugin)?.title || plugin;

  function play(g: Game) {
    playing = g.key;
    localStorage.setItem(LAST, g.key);
  }
</script>

{#if game}
  <div class="playing">
    <div class="tools">
      <button class="dim" onclick={() => (playing = null)}>← games</button>
      <span class="dim">{game.title} · {titleOf(game.plugin)}</span>
    </div>
    {#key game.key}
      <PluginFrame plugin={game.plugin} ui={game.ui} {pane} slot="game" title={game.title} />
    {/key}
  </div>
{:else}
  <div class="view" aria-label="games">
    {#if !plugins.loaded}
      <p class="dim empty">…</p>
    {:else if !list.length}
      <p class="dim empty">No games yet. <button class="link" onclick={() => settings('marketplace')}>Find one in the marketplace</button></p>
    {:else}
      {#each list as g (g.key)}
        <button class="row" onclick={() => play(g)}>
          <span class="title">{g.title}</span>
          <span class="dim">{g.description ?? ''}</span>
          <span class="dim from">{titleOf(g.plugin)}</span><span class="play">Play →</span>
        </button>
      {/each}
    {/if}
  </div>
{/if}

<style>
  .playing { height: 100%; min-height: 0; display: flex; flex-direction: column; }
  .tools { display: flex; gap: 12px; align-items: center; padding: 6px 16px; font-size: 13px; flex: none; }
  .tools button:hover { color: var(--ink); }
  .view { padding: 8px 0; }
  .empty { padding: 12px 16px; margin: 0; }
  .link { color: var(--accent); }
  .row { display: flex; gap: 12px; align-items: baseline; width: 100%; text-align: left; padding: 8px 16px; }
  .row:hover { background: color-mix(in srgb, var(--ink) 5%, transparent); }
  .play { opacity:0; color:var(--accent); border:1px solid var(--faint); border-radius:99px; padding:3px 12px; }
  .row:hover .play, .row:focus-visible .play { opacity:1; }
  @media (hover:none) { .play { opacity:1; } }
  .title { font-weight: 550; }
  .from { margin-left: auto; font-size: 13px; }
</style>
