<script lang="ts">
  // Agents and their subagent roles. Changes are requested in a new zenbot chat.
  import { onMount } from 'svelte';
  import { app, openSession } from '../app.svelte.ts';
  import type { Pane } from '../app.svelte.ts';
  import { zen, type Agent, type Role } from '../api.ts';

  let { pane, active }: { pane: Pane; active: boolean } = $props();
  type LiveAgent = Agent & { chats?: number; busy?: { id: string; title: string }[] };
  type LiveRole = Role & { uses_30d?: number };
  let agents = $state<LiveAgent[] | null>(null);
  let error = $state('');
  let selected = $state('');
  let open = $state<string | null>(null);
  const owner = $derived(agents?.find(a => a.name === selected));
  const roles = $derived((owner?.roles?.roles ?? []) as LiveRole[]);
  async function load() {
    try {
      agents = await zen.agents();
      selected = agents.some(a => a.name === app.agent) ? app.agent : agents[0]?.name ?? '';
      error = '';
    } catch (e) { error = (e as Error).message; }
  }
  onMount(load);
  function ask(text: string) { app.chatDraft = text; openSession('new'); }
  function select(name: string) { selected = name; open = null; }
  function uses(a: Agent) {
    const parts = [a.all_tools ? 'all tools' : a.tools?.length ? a.tools.join(', ') : 'no tools'];
    if (Array.isArray(a.mcp)) parts.push(a.mcp.length ? `MCP: ${a.mcp.join(', ')}` : 'no MCP');
    return parts.join(' · ');
  }
</script>

<div class="awrap">
  <section>
    <div class="sh"><h3>Agents</h3><span class="sd">Who you talk to. Each has its own character, memory and limits.</span></div>
    {#if error}<p role="alert">Can't read the agents: {error} <button class="link" onclick={load}>Retry</button></p>
    {:else if !agents}<p class="dim">Loading…</p>{/if}
    <div class="agrid">
      {#each agents ?? [] as a (a.name)}
        {@const n = a.roles?.roles?.length ?? 0}
        <article class="ag" class:sel={a.name === selected}>
          <button class="top" onclick={() => select(a.name)} aria-pressed={a.name === selected} aria-label={`Show ${a.name} roles`}>
            <span class="orb" class:busy={!!a.busy?.length} style="--a:var(--accent); --b:var(--accent)">{a.name[0].toUpperCase()}</span>
            <div class="nm"><b>{a.name}</b></div>
            {#if a.name === selected}<span class="cur">Talking to</span>{/if}
          </button>
          {#if a.error}<p role="alert">{a.error}</p>{:else if a.description}<p>{a.description}</p>{/if}
          <dl>
            <div><dt>Now</dt><dd class:live={!!a.busy?.length}>{a.busy ? (a.busy.length ? `Working on ${a.busy.length} job${a.busy.length === 1 ? '' : 's'}` : 'Idle') : 'Unavailable'}</dd></div>
            <div><dt>Model</dt><dd>{a.model ?? 'Default'}</dd></div>
            <div><dt>Can use</dt><dd>{uses(a)}</dd></div>
            <div><dt>Chats</dt><dd>{a.chats ?? 'Unavailable'}</dd></div>
          </dl>
          <div class="acts">
            <button class="pill on" onclick={() => openSession('new', a.name)}>Talk to {a.name}</button>
            <span class="tag" class:none={!n} title="Helpers {a.name} can hand work to">{a.roles?.error ? 'Roles unavailable' : n ? `${n} role${n === 1 ? '' : 's'}` : 'No roles'}</span>
          </div>
        </article>
      {/each}
      <button class="ag new" onclick={() => ask('Create a new agent for me:')}><span class="plus">+</span><b>New agent</b><span>Opens a chat with zenbot: tell it who you need and it sets the agent up.</span></button>
    </div>
  </section>
  {#if owner}
    <section>
      <div class="sh"><h3>{owner.name}'s roles <span class="n">{roles.length}</span></h3><span class="sd">Helpers {owner.name} hands parts of a job to. {owner.name} tunes them as it learns.</span></div>
      {#if owner.roles?.error}<div class="empty" role="alert">Can't read roles: {owner.roles.error}</div>
      {:else if roles.length}
        <div class="rlist">
          {#each roles as r (r.name)}
            <article class="role" class:open={open === r.name} data-role={r.name}>
              <button class="rh" aria-expanded={open === r.name} onclick={() => (open = open === r.name ? null : r.name)}>
                <span class="rn">{r.name}</span><span class="ra">{r.description}</span>
                <span class="chip">{r.model ?? 'Default model'}{r.effort ? ` · ${r.effort}` : ''}</span>
                <span class="used" title="Times {owner.name} used this role in the last 30 days">{r.uses_30d === undefined ? 'Unavailable' : `${r.uses_30d} ${r.uses_30d === 1 ? 'use' : 'uses'}`}</span>
              </button>
              {#if open === r.name}
                <div class="rb">
                  <div class="kv"><span class="k">Tools</span><span>{#if r.tools === null}Same as {owner.name}'s{:else if r.tools.length}{#each r.tools as t}<code>{t}</code>{/each}{:else}None{/if}</span></div>
                  <div class="kv"><span class="k">Skills</span><span>{#if r.skills.length}{#each r.skills as skill}<code>{skill}</code>{/each}{:else}None attached{/if}</span></div>
                  <div class="kv"><span class="sp"></span><button class="pill" onclick={() => ask(`Change ${owner.name}'s ${r.name} role:`)}>Ask zenbot to change it</button></div>
                </div>
              {/if}
            </article>
          {/each}
          <button class="newr" onclick={() => ask(`Create a new role for ${owner.name}:`)}><span class="plus">+</span> New role <span class="dim">· opens a chat with zenbot to set it up</span></button>
        </div>
        <p class="foot">Uses are counted over the last 30 days. Without a role, a helper uses <b>{owner.roles?.default_model ?? 'the default model'}</b>. Roles live in <code>agents/{owner.name}/roles.toml</code>.</p>
      {:else}
        <div class="empty">{owner.name} has no roles yet: it does everything itself. <button class="link" onclick={() => ask(`Create a new role for ${owner.name}:`)}>+ New role</button></div>
      {/if}
    </section>
  {/if}
</div>

<style>
  .awrap { flex: 1; overflow: auto; padding: 4px 22px 100px; display: flex; flex-direction: column; gap: 26px; max-width: 980px; width: 100%; margin: 0 auto; }
  .sh { display: flex; align-items: baseline; gap: 10px; flex-wrap: wrap; margin-bottom: 10px; }
  .sh h3 { margin: 0; font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); font-weight: 650; }
  .sh h3 .n { font-weight: 500; letter-spacing: 0; opacity: 0.75; margin-left: 4px; }
  .sd { font-size: 13px; color: var(--dim); }
  .sp { flex: 1; }
  .agrid { display: grid; grid-template-columns: repeat(auto-fill, minmax(260px, 1fr)); gap: 12px; }
  .ag { border: 1px solid var(--faint); border-radius: 16px; padding: 14px; display: flex; flex-direction: column; gap: 10px; text-align: left; background: var(--paper); }
  .ag.sel { border-color: color-mix(in srgb, var(--accent) 50%, var(--faint)); box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 10%, transparent); }
  .orb { width: 38px; height: 38px; border-radius: 50%; display: grid; place-items: center; flex: none; font-weight: 700; font-size: 16px; color: #fbfaf6;
    background: radial-gradient(circle at 32% 28%, var(--b), var(--a) 70%); box-shadow: 0 0 0 3px color-mix(in srgb, var(--a) 18%, transparent); }
  .orb.busy { animation: breathe 1.8s ease-in-out infinite; }
  @keyframes breathe { 50% { box-shadow: 0 0 0 7px color-mix(in srgb, var(--a) 10%, transparent); } }
  .top { display: flex; align-items: center; gap: 10px; width: 100%; text-align: left; }
  .nm { display: flex; flex-direction: column; flex: 1; }
  .nm b { font-size: 15px; }
  .cur { font-size: 11px; font-weight: 650; color: var(--accent); background: color-mix(in srgb, var(--accent) 12%, transparent); padding: 2px 8px; border-radius: 99px; }
  dl { margin: 0; display: grid; grid-template-columns: 1fr 1fr; gap: 6px 12px; }
  dt { font-size: 10.5px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); font-weight: 650; }
  dd { margin: 0; font-size: 13px; }
  dd.live { color: var(--accent); font-weight: 600; }
  .tag { display: inline-flex; align-items: center; padding: 3px 10px; border-radius: 99px; font-size: 12px; font-weight: 600; background: var(--soft); color: var(--ink); }
  .tag.none { color: var(--dim); font-weight: 400; }
  .acts { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; margin-top: auto; }
  .ag.new { border-style: dashed; align-items: flex-start; justify-content: center; color: var(--dim); gap: 4px; }
  .ag.new:hover { border-color: var(--accent); color: var(--ink); }
  .ag.new .plus { font-size: 22px; color: var(--accent); }
  .ag.new b { color: var(--ink); }
  .ag.new span:last-child { font-size: 13px; }
  .pill { display: inline-flex; align-items: center; gap: 5px; padding: 4px 11px; border-radius: 99px; border: 1px solid var(--faint); font-size: 12.5px; background: var(--paper); color: var(--ink); }
  .pill:hover { border-color: var(--dim); }
  .pill.on { background: var(--accent); border-color: var(--accent); color: var(--on-accent); font-weight: 600; }
  .rlist { border: 1px solid var(--faint); border-radius: 16px; overflow: hidden; background: var(--paper); }
  .role + .role { border-top: 1px solid var(--faint); }
  .rh { width: 100%; display: grid; grid-template-columns: 110px 1fr auto 56px; align-items: center; gap: 12px; padding: 11px 14px; text-align: left; }
  .rh:hover, .role.open .rh { background: var(--soft); }
  .rn { font-weight: 650; font-size: 14px; }
  .ra { font-size: 13px; color: var(--dim); overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .role.open .ra { white-space: normal; color: var(--ink); }
  .chip { font-size: 12px; padding: 2px 9px; border-radius: 99px; background: var(--soft); white-space: nowrap; border: 1px solid var(--faint); }
  .used { font-size: 12px; color: var(--dim); text-align: right; }
  .rb { padding: 4px 14px 12px 136px; display: flex; flex-direction: column; gap: 7px; background: var(--soft); }
  .kv { display: flex; gap: 10px; align-items: baseline; font-size: 13px; }
  .kv .k { width: 52px; flex: none; font-size: 10.5px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); font-weight: 650; }
  .kv span:last-child { display: flex; flex-wrap: wrap; gap: 4px; }
  code { font-size: 12px; background: var(--paper); border: 1px solid var(--faint); padding: 1px 6px; border-radius: 6px; }
  .newr { display: flex; align-items: center; gap: 6px; width: 100%; padding: 10px 14px; font-size: 13.5px; font-weight: 600; text-align: left; color: var(--ink); border-top: 1px solid var(--faint); }
  .newr:hover { background: var(--soft); }
  .newr .plus { color: var(--accent); font-size: 17px; }
  .newr .dim { font-weight: 400; }
  .foot { font-size: 12.5px; color: var(--dim); margin: 8px 4px 0; }
  .empty { border: 1px dashed var(--faint); border-radius: 16px; padding: 18px; color: var(--dim); font-size: 13.5px; }
  .link { color: var(--accent); font-weight: 600; }
  .dim { color: var(--dim); }
  @media (max-width: 900px) { .awrap { padding: 4px 12px 100px; } .rh { grid-template-columns: 1fr auto; } .ra { grid-column: 1 / -1; grid-row: 2; } .used { display: none; } .rb { padding-left: 14px; } }
</style>
