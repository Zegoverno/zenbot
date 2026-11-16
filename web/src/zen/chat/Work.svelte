<script lang="ts">
  // A run of tool calls between pieces of text: one line (the latest step, how many, how many
  // failed) until opened; open, every step with the head of its result (a click shows all of it).
  import { toolSummary } from '../api.ts';
  import { clean, type Step, type Result } from './model.ts';

  let { steps, open, ontoggle }: { steps: Step[]; open: boolean; ontoggle: () => void } = $props();
  let full = $state<Record<number, boolean>>({});

  const failed = $derived(steps.filter((s) => s.result?.error).length);
  const count = $derived(`${steps.length} step${steps.length === 1 ? '' : 's'}`);
  const latest = $derived(steps[steps.length - 1]);

  const detail = (s: Step) => clean(toolSummary(s.name, s.args).slice(s.name.length).trim());
  const shown = (r: Result, i: number) => (full[i] ? r.lines : r.lines.slice(0, 3));

  function toggle(e: MouseEvent) {
    if (getSelection()?.toString()) return; // selecting text, not folding
    e.stopPropagation();
    ontoggle();
  }
</script>

{#snippet result(r: Result | undefined, i: number)}
  {#if r}
    <div class="result" class:wk-bad={r.error}>
      {#if r.lines.length === 0}<div class="dim">└ (no output)</div>{/if}
      {#each shown(r, i) as l, j}<div class="rl"><span class="dim">{j === 0 ? '└ ' : '  '}</span>{l}</div>{/each}
      {#if r.lines.length > 3}
        <button class="more" onclick={(e) => { e.stopPropagation(); full[i] = !full[i]; }}>
          {full[i] ? '  fold' : `  … ${r.lines.length - 3} more lines`}
        </button>
      {/if}
    </div>
  {/if}
{/snippet}

<div class="work" class:wk-open={open}>
  {#if open}
    <button class="head" onclick={toggle} title="ctrl+o folds every run"><span class="acc">▾</span> <span class="dim">{count}{failed ? ` · ${failed} failed` : ''} · fold</span></button>
    {#each steps as s, i}
      <div class="step"><span class="acc">•</span> <b>{s.name}</b> <span class="dim">{detail(s)}</span></div>
      {@render result(s.result, i)}
    {/each}
  {:else}
    <button class="head line" onclick={toggle} title="click or ctrl+o to show the steps">
      <span class="acc">▸</span>
      <span class="sum">{clean(toolSummary(latest.name, latest.args))}</span>
      <span class:dim={!failed} class:errc={failed}>· {count}{failed ? ` · ${failed} failed` : ''}</span>
    </button>
  {/if}
</div>

<style>
  .work { font-family: var(--mono); font-size: 12.5px; line-height: 1.5; margin: 2px 0 10px; }
  .head { display: block; width: 100%; text-align: left; padding: 1px 0; color: var(--ink); font: inherit; cursor: pointer; }
  .head.line { display: flex; gap: 6px; min-width: 0; }
  .sum { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; flex: 0 1 auto; }
  .head.line > span:last-child { flex: none; white-space: nowrap; }
  .head:hover .sum, .head:hover .dim { color: var(--ink); }
  .acc { color: var(--accent); }
  .dim { color: var(--dim); }
  .errc { color: var(--err); }
  .step { padding-left: 2px; overflow-wrap: anywhere; white-space: pre-wrap; }
  .step b { font-weight: 600; }
  .result { color: var(--dim); padding-left: 14px; margin-bottom: 4px; }
  .result.wk-bad { color: var(--err); }
  .rl { white-space: pre-wrap; overflow-wrap: anywhere; }
  .more { color: var(--dim); font: inherit; cursor: pointer; padding: 0; }
  .more:hover { color: var(--ink); }
</style>
