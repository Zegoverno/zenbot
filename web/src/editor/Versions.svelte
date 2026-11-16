<script lang="ts">
  // The note's versions: the open one (closed with a changelog line), and the closed ones,
  // newest first, each opened read-only with a click.
  import { untrack } from 'svelte';
  import type { VersionState } from '../lib/api.ts';

  let { vs, close, view, suggest, sel, ask }: {
    vs: VersionState; close: (log: string) => Promise<string | null>; view: (n: number) => void;
    suggest: () => Promise<string>;
    /** The closed version chosen with Alt+↑↓ (newest first), and Alt+G's count: each one starts closing. */
    sel: number; ask: number;
  } = $props();
  let closing = $state(false);
  let log = $state('');
  let error = $state('');
  const older = $derived([...vs.versions].reverse());
  // zenbot's suggested line, shown faded after what's typed: Tab takes it, Enter closes.
  let suggestion = $state('');
  let asking = $state(false);
  const rest = $derived(suggestion && suggestion.toLowerCase().startsWith(log.toLowerCase()) ? suggestion.slice(log.length) : '');

  $effect(() => {
    if (ask && vs.open && !closing) untrack(start);
  });

  async function start() {
    closing = true;
    suggestion = '';
    asking = true;
    try {
      suggestion = await suggest();
    } catch {
      suggestion = '';
    } finally {
      asking = false;
    }
  }

  function keys(e: KeyboardEvent) {
    if (e.key === 'Tab' && rest) {
      e.preventDefault();
      log = suggestion;
    } else if (e.key === 'Enter') submit();
    else if (e.key === 'Escape') { closing = false; error = ''; }
  }

  async function submit() {
    if (!log.trim()) {
      error = 'Say what changed: it goes in the changelog.';
      return;
    }
    const err = await close(log);
    if (err) error = err;
    else {
      closing = false;
      log = '';
      error = '';
    }
  }
</script>

<section class="versions">
  <div class="panel-head">Version</div>
  <div class="now">
    {#if vs.open}
      <span class="label open">v{vs.current + 1} · open</span>
      {#if !closing}<button class="act" onclick={start}>Close v{vs.current + 1}…</button>{/if}
    {:else}
      <span class="label">v{vs.current} · closed</span>
      <span class="dim small">typing opens v{vs.current + 1}</span>
    {/if}
  </div>
  {#if closing}
    <div class="log-wrap">
      <div class="log-ghost" aria-hidden="true"><span class="typed">{log}</span>{rest}</div>
      <!-- svelte-ignore a11y_autofocus -->
      <input class="log-in" bind:value={log} autofocus placeholder={rest || asking ? '' : `What changed in v${vs.current + 1}?`} onkeydown={keys} />
    </div>
    <div class="dim small hint">{asking ? 'zenbot is reading the changes…' : rest ? 'Tab takes zenbot\'s line, Enter closes' : ''}</div>
    <div class="row">
      <button class="act" onclick={submit}>Close version</button>
      <button class="act dim" onclick={() => { closing = false; error = ''; }}>Cancel</button>
    </div>
    {#if error}<div class="err">{error}</div>{/if}
  {/if}
  {#if older.length}
    <ul>
      {#each older as v (v.n)}
        <li class:sel={v.n === older[sel]?.n}><button onclick={() => view(v.n)} title="Open v{v.n}, read-only">
          <b>v{v.n}</b> · {v.date} · {v.log}{v.by === 'zenbot' ? ' (zenbot)' : ''}
        </button></li>
      {/each}
    </ul>
  {/if}
</section>
