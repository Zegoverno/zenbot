<script lang="ts">
  // The Agents view: the owner's agents (the kernel's agents.rs), zenbot first, each with what it
  // may use; one whose agent.toml is broken says why. Choosing one opens its three files in place
  // of the list (as the file viewer opens in place of Files): SOUL.md (who it is), IDENTITY.md (how
  // it works), agent.toml (what it may use). Save sends only the files changed; the kernel checks
  // the settings first and writes nothing when they're refused, so its reason shows here. Delete
  // asks first (never zenbot); the kernel moves the folder to its backups. Below the agents, a
  // read-only Roles section: the subagent profiles of each agent's roles.toml (roles.rs), grouped
  // by agent when more than one has roles.
  import { onMount, tick } from 'svelte';
  import type { Pane } from '../app.svelte.ts';
  import { app, openSession, notice, typing, tilde } from '../app.svelte.ts';
  import { zen, AGENT_NAME, type Agent, type AgentDetail, type AgentFiles } from '../api.ts';
  import { pageRows, follow, plain } from './list.ts';

  let { pane, active }: { pane: Pane; active: boolean } = $props();

  type File = 'soul' | 'identity' | 'settings';
  const FILES: { k: File; name: string; what: string; rows: number }[] = [
    { k: 'soul', name: 'SOUL.md', what: 'who it is', rows: 12 },
    { k: 'identity', name: 'IDENTITY.md', what: 'how it works', rows: 12 },
    { k: 'settings', name: 'agent.toml', what: 'what it may use', rows: 10 },
  ];

  let agents = $state<Agent[] | null>(null);
  let error = $state('');
  let sel = $state<string | null>(null);
  let list: HTMLElement | undefined = $state();

  /** The agent open in the editor, as the kernel last gave it, and the texts being edited. */
  let open = $state<AgentDetail | null>(null);
  let draft = $state<Record<File, string>>({ soul: '', identity: '', settings: '' });
  let busy = $state(false);
  let refused = $state('');
  let confirming = $state(false);
  /** Esc or Back pressed once with unsaved changes: a second one drops them. */
  let leaving = $state(false);

  /** The "New agent" form. */
  let form = $state<{ name: string; description: string } | null>(null);
  let formError = $state('');
  let nameInput: HTMLInputElement | undefined = $state();

  const at = $derived(Math.max(0, agents?.findIndex((a) => a.name === sel) ?? 0));
  /** The agents with roles to show (or a roles file to complain about). */
  const withRoles = $derived((agents ?? []).filter((a) => a.roles && (a.roles.error || a.roles.roles?.length)));
  const changed = $derived(open ? FILES.filter((f) => draft[f.k] !== open![f.k]).map((f) => f.k) : []);

  async function load() {
    try {
      agents = await zen.agents();
      error = '';
      if (agents.length && (!sel || !agents.some((a) => a.name === sel))) sel = agents[0].name;
    } catch (e) {
      error = (e as Error).message;
    }
  }

  onMount(() => {
    load();
  });

  /** "all tools", the tool list, "no MCP" / its servers, the model: what an agent may use. */
  function uses(a: Agent): string {
    const parts = [a.all_tools ? 'all tools' : a.tools?.length ? a.tools.join(', ') : 'no tools'];
    if (Array.isArray(a.mcp)) parts.push(a.mcp.length ? `MCP: ${a.mcp.join(', ')}` : 'no MCP');
    if (a.model) parts.push(a.model);
    return parts.join(' · ');
  }

  function select(i: number) {
    if (!agents?.length) return;
    sel = agents[Math.min(agents.length - 1, Math.max(0, i))].name;
    follow(list);
  }

  function fill(d: AgentDetail) {
    open = d;
    draft = { soul: d.soul, identity: d.identity, settings: d.settings };
  }

  async function edit(name: string | null) {
    if (!name) return;
    sel = name;
    try {
      fill(await zen.agent(name));
      refused = '';
      confirming = leaving = false;
    } catch (e) {
      notice(`can't open @${name}: ${(e as Error).message}`);
    }
  }

  function back() {
    if (changed.length && !leaving) {
      leaving = true;
      notice('unsaved changes: Save, or back again to drop them');
      return;
    }
    open = null;
    refused = '';
    confirming = leaving = false;
    load().then(() => follow(list));
  }

  async function save() {
    if (!open || !changed.length || busy) return;
    const change: AgentFiles = {};
    for (const k of changed) change[k] = draft[k];
    busy = true;
    try {
      const d = await zen.saveAgent(open.name, change);
      // Keep what was typed meanwhile; what was sent is now the kernel's.
      const typed = { ...draft };
      open = d;
      draft = { soul: typed.soul === change.soul ? d.soul : typed.soul, identity: typed.identity === change.identity ? d.identity : typed.identity, settings: typed.settings === change.settings ? d.settings : typed.settings };
      refused = '';
      leaving = false;
      notice(`saved @${d.name}`);
    } catch (e) {
      refused = (e as Error).message;
    } finally {
      busy = false;
    }
  }

  async function remove() {
    if (!open || open.name === 'zenbot' || busy) return;
    if (!confirming) return void (confirming = true);
    busy = true;
    try {
      const r = await zen.removeAgent(open.name);
      notice(`removed @${open.name} · its folder is in ${tilde(r.moved_to)}`);
      open = null;
      confirming = false;
      refused = '';
      sel = null;
      await load();
    } catch (e) {
      refused = (e as Error).message;
      confirming = false;
    } finally {
      busy = false;
    }
  }

  async function startForm() {
    form = { name: '', description: '' };
    formError = '';
    await tick();
    nameInput?.focus();
  }

  async function create() {
    if (!form || busy) return;
    const name = form.name.trim();
    if (!AGENT_NAME.test(name)) return void (formError = 'a name is a lowercase letter, then lowercase letters, digits or hyphens; at most 32');
    busy = true;
    try {
      const a = await zen.newAgent(name, form.description.trim());
      form = null;
      formError = '';
      await load();
      await edit(a.name);
    } catch (e) {
      formError = (e as Error).message;
    } finally {
      busy = false;
    }
  }

  function formKey(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      e.preventDefault();
      create();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      form = null;
    }
  }

  function key(e: KeyboardEvent) {
    if (!active) return;
    if (open && (e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === 's') {
      e.preventDefault();
      save();
      return;
    }
    if (!plain(e) || typing(e) || form) return;
    if (open) {
      if (e.key === 'Escape') {
        e.preventDefault();
        if (confirming) confirming = false;
        else back();
      }
      return;
    }
    const page = pageRows(list, 46);
    const acts: Record<string, () => void> = {
      ArrowUp: () => select(at - 1),
      k: () => select(at - 1),
      ArrowDown: () => select(at + 1),
      j: () => select(at + 1),
      PageUp: () => select(at - page),
      PageDown: () => select(at + page),
      Home: () => select(0),
      End: () => select((agents?.length ?? 1) - 1),
      r: load,
      Enter: () => edit(sel),
      n: () => openSession('new', sel ?? 'zenbot'),
      '+': startForm,
    };
    const act = acts[e.key];
    if (act) {
      e.preventDefault();
      act();
    }
  }
</script>

<svelte:window onkeydown={key} />

<div class="view" bind:this={list}>
  {#if open}
    {@const o = open}
    <div class="tools">
      <button class="chip" onclick={back} title="esc">← agents</button>
      <b>@{o.name}</b>
      <span class="dim">{o.sessions} session{o.sessions === 1 ? '' : 's'} · {tilde(o.dir)}</span>
      <span class="grow"></span>
      <button class="act" onclick={() => openSession('new', o.name)}>New session with this agent</button>
      <button class="act primary" onclick={save} disabled={!changed.length || busy} title="ctrl+s">{busy ? 'saving…' : changed.length ? 'Save' : 'Saved'}</button>
      <button class="act danger" onclick={remove} disabled={o.name === 'zenbot' || busy} title={o.name === 'zenbot' ? 'zenbot is the default agent and stays' : 'delete this agent'}>Delete</button>
    </div>
    {#if confirming}
      <div class="confirm" role="alertdialog" aria-label="confirm delete">
        <span>Delete <b>@{o.name}</b>? Its folder moves to zen's backups; its sessions stay.</span>
        <button class="act danger armed" onclick={remove} disabled={busy}>Yes, delete @{o.name}</button>
        <button class="act" onclick={() => (confirming = false)}>Cancel</button>
      </div>
    {/if}
    {#if refused}<p class="refused" role="alert">Not saved: {refused}</p>{/if}
    {#if o.error}<p class="refused" role="alert">This agent is broken: {o.error}</p>
    {:else}<p class="dim pad">{o.description || 'no description'} · {uses(o)}</p>{/if}
    {#each FILES as f}
      <label class="file">
        <span class="fname"><b>{f.name}</b> <span class="dim">{f.what}</span>{#if changed.includes(f.k)}<span class="accent">{" · changed"}</span>{/if}</span>
        <textarea bind:value={draft[f.k]} rows={f.rows} spellcheck={f.k !== 'settings'} aria-label={f.name} oninput={() => (leaving = false)}></textarea>
      </label>
    {/each}
  {:else}
    <div class="tools">
      {#if error}
        <span class="err">can't read the agents: {error}</span>
      {:else if !agents}
        <span class="dim">loading…</span>
      {:else}
        <span class="dim">{agents.length} agent{agents.length === 1 ? '' : 's'}</span>
      {/if}
      <span class="grow"></span>
      <button class="chip" onclick={load} title="r">↻</button>
      <button class="act" onclick={startForm} title="+">New agent</button>
    </div>
    {#if form}
      <form class="newform" onsubmit={(e) => { e.preventDefault(); create(); }}>
        <input bind:this={nameInput} bind:value={form.name} onkeydown={formKey} placeholder="name (e.g. rex)" aria-label="agent name" maxlength="32" spellcheck="false" autocomplete="off" />
        <input bind:value={form.description} onkeydown={formKey} placeholder="one line: what it's for" aria-label="agent description" autocomplete="off" />
        <button class="act primary" type="submit" disabled={busy}>Create</button>
        <button class="act" type="button" onclick={() => (form = null)}>Cancel</button>
        {#if formError}<p class="refused wide" role="alert">{formError}</p>{/if}
      </form>
    {/if}

    <h3 class="section-title">Agents</h3>
    <p class="section-lead">Who you talk to. Each has its own character, memory and limits.</p>
    <div class="cards" role="listbox" aria-label="agents" tabindex="-1">
      {#each agents ?? [] as a (a.name)}
        {@const on = a.name === sel}
        {@const live = a as Agent & { chats?: number; busy?: { id: string; title: string }[] }}
        <div
          class="row"
          class:sel={on}
          class:active
          data-sel={on ? '1' : '0'}
          role="option"
          tabindex="-1"
          aria-selected={on}
          onclick={() => (on ? edit(a.name) : (sel = a.name))}
          ondblclick={() => edit(a.name)}
          onkeydown={() => {}}
        >
          <div class="line">
            <span class="orb">{a.name[0].toUpperCase()}</span><b>@{a.name}</b>
            {#if a.name === app.agent}<span class="dim small">Talking to</span>{/if}
            {#if a.description}<span class="desc">{a.description}</span>{/if}
            {#if on}<span class="tail"><button class="act" onclick={(e) => { e.stopPropagation(); edit(a.name); }}>edit</button></span>{/if}
          </div>
          {#if a.error}<div class="refused small">⚠ {a.error}</div>
          {:else}<div class="dim small">{uses(a)}</div>{/if}
          <div class="stats"><span>{live.busy ? (live.busy.length ? `Working on ${live.busy.length} job${live.busy.length === 1 ? '' : 's'}` : 'Idle') : 'Status unavailable'}</span>{#if live.chats !== undefined}<span>{live.chats} chats</span>{/if}<span>{a.roles?.roles?.length ?? 0} roles</span></div>
          <button class="act" onclick={(e) => { e.stopPropagation(); openSession('new', a.name); }}>Talk to {a.name}</button>
        </div>
      {/each}
    </div>

    {#if withRoles.length}
      <section class="roles" aria-label="roles">
        <h3>Roles</h3>
        <p class="dim pad">Profiles for subagents (<code>delegate</code> with a role), from each agent's roles.toml.</p>
        {#each withRoles as a (a.name)}
          {@const r = a.roles!}
          {#if withRoles.length > 1}<div class="group dim"><b>@{a.name}</b>{#if r.default_model}{" · default "}{r.default_model}{/if}</div>
          {:else if r.default_model}<div class="group dim">default model {r.default_model}</div>{/if}
          {#if r.error}<div class="refused small">⚠ {r.error}</div>{/if}
          {#each r.roles ?? [] as role (role.name)}
            <div class="role" data-role={role.name}>
              <div class="line">
                <b>{role.name}</b>
                <span class="dim">{role.model ?? 'default model'}{role.effort ? ` · ${role.effort}` : ''}{(role as typeof role & { uses_30d?: number }).uses_30d !== undefined ? ` · ${(role as typeof role & { uses_30d: number }).uses_30d} uses in 30 days` : ''}</span>
              </div>
              {#if role.description}<div class="small">{role.description}</div>{/if}
              <div class="dim small">{role.tools ? `tools: ${role.tools.join(', ') || 'none'}` : 'all tools'}{role.skills.length ? ` · skills: ${role.skills.join(', ')}` : ''}</div>
            </div>
          {/each}
        {/each}
      </section>
    {/if}
  {/if}
</div>

<style>
  .section-title { margin: 20px 20px 2px; font-size: 22px; }
  .section-lead { margin: 0 20px 12px; color: var(--dim); }
  .cards { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, 270px), 1fr)); gap: 10px; margin: 0 14px; }
  .cards .row { border: 1px solid var(--faint); border-radius: 14px; padding: 14px; margin: 0; }
  .cards .line { flex-wrap: wrap; align-items: center; }
  .orb { display: grid; place-items: center; border-radius: 50%; width: 36px; height: 36px; background: var(--accent); color: var(--paper); }
  .stats { display: flex; flex-wrap: wrap; gap: 12px; margin: 10px 0; color: var(--dim); font-size: 12px; }
  .view { container-type: inline-size; padding: 6px 0 24px; font-family: var(--sans); font-size: 14px; }
  .tools { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; padding: 4px 14px 6px; font-size: 13px; }
  .grow { flex: 1; }
  .chip { font-size: 12px; color: var(--dim); border: 1px solid var(--faint); border-radius: 6px; padding: 3px 8px; flex: none; }
  .act { font-size: 12px; color: var(--accent); border: 1px solid color-mix(in srgb, var(--accent) 50%, transparent); border-radius: 6px; padding: 3px 8px; flex: none; }
  .act.primary { background: var(--accent); color: var(--paper); }
  .act.danger { color: var(--err); border-color: color-mix(in srgb, var(--err) 50%, transparent); }
  .act.danger.armed { background: var(--err); color: var(--paper); }
  .act:disabled { opacity: 0.5; cursor: default; }
  .pad { padding: 2px 14px 8px; margin: 0; font-size: 13px; }
  .row { box-sizing: border-box; padding: 5px 14px; cursor: pointer; border-radius: 6px; margin: 0 6px; outline-offset: -1px; }
  .row:hover { background: color-mix(in srgb, var(--ink) 4%, transparent); }
  .row.sel { outline: 1px solid var(--faint); }
  .row.sel.active { outline: none; background: color-mix(in srgb, var(--accent) 14%, transparent); }
  .line { display: flex; align-items: baseline; gap: 10px; min-height: 22px; }
  .desc { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .tail { margin-left: auto; flex: none; }
  .small { font-size: 12.5px; margin-top: 1px; overflow-wrap: anywhere; }
  .refused { color: var(--err); background: color-mix(in srgb, var(--err) 9%, transparent); border-radius: 6px; padding: 6px 10px; margin: 4px 14px; font-size: 13px; white-space: pre-wrap; overflow-wrap: anywhere; }
  .refused.small { margin: 3px 0 0; padding: 3px 8px; font-size: 12.5px; }
  .confirm { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; margin: 4px 14px; padding: 8px 10px; border: 1px solid var(--err); border-radius: 6px; font-size: 13px; }
  .newform { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; padding: 4px 14px 10px; }
  .newform input { font: inherit; font-size: 13px; color: var(--ink); background: transparent; border: 1px solid var(--faint); border-radius: 6px; padding: 4px 8px; min-width: 0; }
  .newform input:first-child { width: 12em; }
  .newform input:nth-child(2) { flex: 1; min-width: 12em; }
  .newform input:focus, .file textarea:focus { outline: none; border-color: var(--accent); }
  .wide { flex: 1 1 100%; margin: 0; }
  .file { display: block; padding: 6px 14px; }
  .fname { display: block; font-size: 13px; margin-bottom: 4px; }
  .file textarea { display: block; box-sizing: border-box; width: 100%; resize: vertical; font-family: var(--mono); font-size: 13px; line-height: 1.45; color: var(--ink); background: transparent; border: 1px solid var(--faint); border-radius: 6px; padding: 8px 10px; }
  .roles { margin-top: 14px; }
  .roles h3 { margin: 0; padding: 4px 14px; font-size: 14px; }
  .group { padding: 6px 14px 2px; font-size: 13px; }
  .role { padding: 5px 14px; margin: 0 6px; }
  .dim { color: var(--dim); }
  .err { color: var(--err); }
  .accent { color: var(--accent); }
  @media (pointer: coarse) { .row { padding-top: 8px; padding-bottom: 8px; } .act { padding: 6px 10px; } }
</style>
