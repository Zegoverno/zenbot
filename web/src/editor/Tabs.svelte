<script lang="ts" generics="T extends string">
  // A panel's tabs: Alt+L (left) or Alt+R (right) moves to the next one, a click picks one.
  let { tabs, tab, pick, keyHint }: { tabs: readonly T[]; tab: T; pick: (t: T) => void; keyHint: string } = $props();
</script>

<nav class="tabs" title={keyHint}>
  {#each tabs as t}<button class:on={t === tab} onclick={() => pick(t)}>{t}</button>{/each}
</nav>
