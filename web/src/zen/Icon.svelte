<script lang="ts">
  // One of zenbot's line icons (icons.ts), 24×24, stroked with the text colour.
  import { ICONS } from './icons.ts';
  let { name, size = 18 }: { name: string; size?: number } = $props();
</script>

<svg class="ico" viewBox="0 0 24 24" width={size} height={size} aria-hidden="true">{@html ICONS[name] ?? ICONS.plugin}</svg>

<style>
  .ico { fill: none; stroke: currentColor; stroke-width: 1.6; stroke-linecap: round; stroke-linejoin: round; flex: none; }
</style>
