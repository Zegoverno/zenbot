<script lang="ts">
  // Agents tab: two sections. Agents: who you can talk to (each its own soul, memory and limits;
  // agents/<name>/). Roles: the helpers an agent hands parts of a job to (subagents), from its
  // roles.toml: a kind of work, a model, an effort, a narrower tool list and attached skills. Roles
  // are the agent's to tune; here you read them, and ask the agent to change one. (The redesign
  // mock's Agents, on GET /api/agents.)
  import { models, modelOf, type A } from '../live.svelte.ts';
  let { agents, current, pick, ask }: { agents: A[]; current: A; pick: (a: A) => void; ask: (text: string) => void } = $props();

  const short = (id: string | null | undefined) => (id ? modelOf(id).name : '');
  const infoOf = (a: A) => {
    const n = (a.info.busy ?? []).length;
    return {
      about: a.info.description || (a.id === 'zenbot' ? 'Your chief of staff: carries jobs end to end, keeps your vault, tasks and routines.' : ''),
      model: short(a.info.model) || short(models.default) || 'Default',
      state: n ? `Working on ${n} job${n > 1 ? 's' : ''}` : 'Idle',
      busy: n > 0,
      chats: a.info.chats ?? 0,
      limits: a.info.can_use ?? (a.info.all_tools ? 'All tools' : (a.info.tools ?? []).join(', ') || 'Its own tools'),
    };
  };
  type Role = { name: string; about: string; model: string; vendor: 'claude' | 'openai'; effort?: string; tools: string[] | null; skills: string[]; used: number; note?: string };
  const rolesOf = (a: A): { fallback: string; roles: Role[] } => {
    const r = a.info.roles;
    return {
      fallback: short(r?.default_model) || 'its own model',
      roles: (r?.roles ?? []).map((x) => ({
        name: x.name, about: x.description, model: short(x.model) || short(r?.default_model) || 'Default',
        vendor: (x.model ?? '').startsWith('codex/') ? 'openai' : 'claude', effort: x.effort ?? undefined,
        tools: x.tools, skills: x.skills ?? [], used: (x as { uses_30d?: number }).uses_30d ?? 0,
      })),
    };
  };
  // Roles follow the agent picked at the top (the one you're talking to): no switch here.
  let open = $state<string | null>(null);
  const rs = $derived(rolesOf(current));
  const owner = $derived(current);
</script>

<div class="awrap">
  <section>
    <div class="sh"><h3>Agents</h3><span class="sd">Who you talk to. Each has its own character, memory and limits.</span></div>
    <div class="agrid">
      {#each agents as a (a.id)}
        {@const i = infoOf(a)}
        {@const n = a.info.roles?.roles?.length ?? 0}
        <article class="ag" class:sel={a.id === current.id}>
          <div class="top">
            <span class="orb" class:busy={i.busy} style="--a:{a.a}; --b:{a.b}">{a.name[0].toUpperCase()}</span>
            <div class="nm"><b>{a.name}</b><span>{a.role}</span></div>
            {#if a.id === current.id}<span class="cur">Talking to</span>{/if}
          </div>
          <p>{i.about}</p>
          <dl>
            <div><dt>Now</dt><dd class:live={i.busy}>{i.state}</dd></div>
            <div><dt>Model</dt><dd>{i.model}</dd></div>
            <div><dt>Can use</dt><dd>{i.limits}</dd></div>
            <div><dt>Chats</dt><dd>{i.chats}</dd></div>
          </dl>
          <div class="acts">
            {#if a.id !== current.id}<button class="pill on" onclick={() => pick(a)}>Talk to {a.name}</button>{/if}
            <span class="tag" class:none={!n} title="Helpers {a.name} can hand work to">{n ? `${n} role${n > 1 ? 's' : ''}` : 'No roles'}</span>
          </div>
        </article>
      {/each}
      <button class="ag new" onclick={() => ask('Create a new agent for me: ')}><span class="plus">+</span><b>New agent</b><span>Opens a chat with zenbot: tell it who you need and it sets the agent up.</span></button>
    </div>
  </section>

  <section>
    <div class="sh">
      <h3>{owner.name}'s roles <span class="n">{rs.roles.length}</span></h3>
      <span class="sd">Helpers {owner.name} hands parts of a job to. {owner.name} tunes them as it learns.</span>
    </div>
    {#if rs.roles.length}
      <div class="rlist">
        {#each rs.roles as r (r.name)}
          <article class="role" class:open={open === r.name}>
            <button class="rh" aria-expanded={open === r.name} onclick={() => (open = open === r.name ? null : r.name)}>
              <span class="rn">{r.name}</span>
              <span class="ra">{r.about}</span>
              <span class="chip {r.vendor}">{r.model}{#if r.effort}{' · '}{r.effort}{/if}</span>
              <span class="used" title="Times {owner.name} used this role in the last 30 days">{r.used} {r.used === 1 ? 'use' : 'uses'}</span>
            </button>
            {#if open === r.name}
              <div class="rb">
                <div class="kv"><span class="k">Tools</span><span>{#if r.tools}{#each r.tools as t}<code>{t}</code>{/each}{:else}Same as {owner.name}'s{/if}</span></div>
                <div class="kv"><span class="k">Skills</span><span>{#if r.skills.length}{#each r.skills as s}<code>{s}</code>{/each}{:else}None attached{/if}</span></div>
                {#if r.note}<div class="kv"><span class="k">Note</span><span class="dim">{r.note}</span></div>{/if}
                <div class="kv end"><span class="sp"></span><button class="pill" onclick={() => ask(`Change ${owner.name}'s ${r.name} role: `)}>Ask zenbot to change it</button></div>
              </div>
            {/if}
          </article>
        {/each}
        <button class="role newr" onclick={() => ask(`Create a new role for ${owner.name}: `)}><span class="plus">+</span> New role <span class="dim">· opens a chat with zenbot to set it up</span></button>
      </div>
      <p class="foot">Uses are counted over the last 30 days. Without a role, a helper uses <b>{rs.fallback}</b>. Roles live in <code>agents/{owner.id}/roles.toml</code>.</p>
    {:else}
      <div class="empty">{owner.name} has no roles yet: it does everything itself. <button class="link" onclick={() => ask(`Create a new role for ${owner.name}: `)}>+ New role</button></div>
    {/if}
  </section>
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
  .top { display: flex; align-items: center; gap: 10px; }
  .nm { display: flex; flex-direction: column; flex: 1; }
  .nm b { font-size: 15px; }
  .nm span { font-size: 12.5px; color: var(--dim); }
  .cur { font-size: 11px; font-weight: 650; color: var(--accent); background: color-mix(in srgb, var(--accent) 12%, transparent); padding: 2px 8px; border-radius: 99px; }
  .ag p { margin: 0; font-size: 13.5px; line-height: 1.45; }
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
  .pill.sel2 { border-color: var(--ink); }
  .seg { display: flex; background: var(--soft); border-radius: 99px; padding: 2px; }
  .seg button { padding: 3px 11px; border-radius: 99px; font-size: 12.5px; color: var(--dim); }
  .seg button.on { background: var(--paper); color: var(--ink); font-weight: 600; box-shadow: 0 1px 3px var(--shadow); }
  .rlist { border: 1px solid var(--faint); border-radius: 16px; overflow: hidden; background: var(--paper); }
  .role + .role { border-top: 1px solid var(--faint); }
  .rh { width: 100%; display: grid; grid-template-columns: 110px 1fr auto 56px; align-items: center; gap: 12px; padding: 11px 14px; text-align: left; }
  .rh:hover, .role.open .rh { background: var(--soft); }
  .rn { font-weight: 650; font-size: 14px; }
  .ra { font-size: 13px; color: var(--dim); overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .role.open .ra { white-space: normal; color: var(--ink); }
  .chip { font-size: 12px; padding: 2px 9px; border-radius: 99px; background: var(--soft); white-space: nowrap; border: 1px solid var(--faint); }
  .chip.openai { border-color: color-mix(in srgb, var(--ink) 25%, var(--faint)); }
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
