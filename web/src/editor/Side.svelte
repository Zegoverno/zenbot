<script lang="ts" generics="T extends 'notes' | 'versions' | 'properties'">
  // The right-hand panel (Alt+R), about the note: its notes, its versions (with the changelog,
  // written by zenbot, never edited on the page) and its properties, one tab at a time.
  import { changelogEntries } from '../doc/note.ts';
  import Props from './Props.svelte';
  import Versions from './Versions.svelte';
  import Tabs from './Tabs.svelte';
  import type { NoteProposal, VersionState } from '../lib/api.ts';

  let { tab, tabs, pickTab, vsel, ask, changelog, notes, edit, suggest, find, findNotes, keep, drop, pick, frontmatter, setFrontmatter, vs, closeVersion, viewVersion }: {
    tab: T; tabs: readonly T[]; pickTab: (t: T) => void; vsel: number; ask: number;
    changelog: string; notes: string; edit: (text: string) => void; suggest: () => Promise<string>;
    find: { busy: boolean; error: string; proposals: NoteProposal[]; picked: number | null }; findNotes: () => void;
    keep: (p: NoteProposal) => void; drop: (p: NoteProposal) => void; pick: (i: number) => void;
    frontmatter: string; setFrontmatter: (fm: string) => void;
    vs: VersionState | null; closeVersion: (log: string) => Promise<string | null>; viewVersion: (n: number) => void;
  } = $props();
  const entries = $derived(changelogEntries(changelog));
  // Keep the selected proposal in sight.
  $effect(() => {
    if (find.picked !== null) setTimeout(() => document.querySelector('.proposals li.sel')?.scrollIntoView({ block: 'nearest' }));
  });
</script>

<aside class="side">
  <Tabs {tabs} {tab} pick={pickTab} keyHint="Alt+R: next tab" />
  {#if tab === 'versions'}
    {#if vs}<Versions {vs} close={closeVersion} view={viewVersion} {suggest} sel={vsel} {ask} />{:else}<p class="dim">A new note gets versions once it is saved.</p>{/if}
    {#if entries.length}
      <section class="log">
        <div class="panel-head">Changelog</div>
        <ul>
          {#each entries as e}<li>{e}</li>{/each}
        </ul>
      </section>
    {/if}
  {:else if tab === 'properties'}
    <Props {frontmatter} change={setFrontmatter} />
  {:else}
    <section class="notes">
      <div class="panel-head notes-head">Notes
        <button class="find" onclick={findNotes} disabled={find.busy} title="zenbot searches the wiki for what helps this piece (Alt+G)">{find.busy ? 'Searching the wiki…' : 'Find in wiki'}</button>
      </div>
      {#if find.proposals.length}
        <ul class="proposals">
          {#each find.proposals as p, i (p.page + p.text)}
            <li class:sel={i === find.picked}>
              <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
              <span class="ptext" onclick={() => pick(i)}>{p.text} <span class="plink">[[{p.page}]]</span>{#if p.quote}<span class="pquote">“{p.quote}”</span>{/if}<span class="by">zenbot</span></span>
              <span class="pacts"><button title="Keep it in Notes" onclick={() => keep(p)}>✓</button><button title="Dismiss" onclick={() => drop(p)}>✕</button></span>
            </li>
          {/each}
        </ul>
      {/if}
      {#if find.error}<p class="find-err">{find.error}</p>{/if}
      <textarea value={notes} oninput={(e) => edit(e.currentTarget.value)} placeholder="Notes for this piece: ideas, lines that don't fit yet, things to check." spellcheck="true"></textarea>
    </section>
  {/if}
</aside>
