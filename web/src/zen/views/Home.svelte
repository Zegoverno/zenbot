<script lang="ts">
  import { onMount } from 'svelte';
  import { zen, routines, files, ago, type BoardSession, type Agent, type Routine, type RoutineRun, type Entry } from '../api.ts';
  import { openSession, setView, openFile } from '../app.svelte.ts';
  import { catalog, defaults, layoutOf, selectTasks, slot, type Kind, type Slot, type LiveTask } from './home.ts';
  type Board = BoardSession & { question?: string | null };
  let sessions = $state<Board[]>([]), tasks = $state<LiveTask[]>([]), agents = $state<Agent[]>([]), jobs = $state<Routine[]>([]), runs = $state<RoutineRun[]>([]), recentFiles = $state<Entry[]>([]);
  let today = $state(''), name = $state(''), error = $state(''), taskError = $state(''), layoutError = $state(''), loaded = $state(false), edit = $state(false), cfg = $state(''), drag = $state('');
  let layout = $state<Slot[]>(defaults());
  let now = $state(new Date());
  let saving = Promise.resolve();
  const top = $derived(sessions.filter(s => !s.parent));
  const waiting = $derived(top.filter(s => s.waiting));
  const working = $derived(top.filter(s => s.busy));
  const recent = $derived([...top].sort((a,b) => b.updated_at.localeCompare(a.updated_at)).slice(0,5));
  const projects = $derived([...new Set(tasks.map(t => t.project))].sort());
  const goals = $derived([...new Set(tasks.flatMap(t => t.goals))].sort());
  const hidden = $derived((Object.keys(catalog) as Kind[]).filter(k => k === 'today' || !layout.some(s => s.kind === k)));
  const due = $derived(tasks.filter(t => !t.done && t.due && t.due <= today).length);
  const date = $derived(now.toLocaleDateString(undefined,{weekday:'long',day:'numeric',month:'long'}));
  const greeting = $derived(now.getHours() < 12 ? 'morning' : now.getHours() < 18 ? 'afternoon' : 'evening');
  async function request<T>(url: string, method = 'GET', body?: unknown): Promise<T> {
    const res = await fetch(url,{method,headers:method === 'GET' ? {} : {'content-type':'application/json','x-zen-web':'1'},body:body === undefined ? undefined : JSON.stringify(body)});
    const data = await res.json().catch(() => ({})); if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`); return data as T;
  }
  function keep() {
    const snapshot = JSON.parse(JSON.stringify(layout));
    saving = saving.catch(() => {}).then(async () => { try { await request('/api/zen/prefs','PATCH',{home_layout:snapshot}); layoutError = ''; } catch (e) { layoutError = `Could not save layout: ${(e as Error).message}`; } });
  }
  function move(id:string, offset:number) { const i=layout.findIndex(s=>s.id===id), j=i+offset; if(i<0||j<0||j>=layout.length)return; const a=[...layout]; const [s]=a.splice(i,1); a.splice(j,0,s); layout=a; keep(); }
  function add(k:Kind) { const id = k === 'today' && layout.some(s=>s.kind===k) ? `today-${crypto.randomUUID()}` : k; const s=slot(k,id); if(k==='today' && layout.some(x=>x.kind===k)) s.filter.show='all'; layout=[...layout,s]; cfg=k==='today'?id:''; keep(); }
  async function toggle(t:LiveTask) { if(!t.id || !t.rev) { taskError='Task cannot be updated without an id and revision.'; return; } try { await request(`/api/zen/tasks/${encodeURIComponent(t.id)}`,'PATCH',{done:!t.done,rev:t.rev}); await loadTasks(); } catch(e) { taskError=`Could not update task: ${(e as Error).message}`; await loadTasks(false); } }
  async function loadTasks(clear = true) { try { const t=await zen.tasks(); tasks=t.projects.flatMap(p=>p.tasks) as LiveTask[]; today=t.today; if(clear) taskError=''; } catch { taskError='Could not load tasks.'; } }
  onMount(() => {
    let alive=true;
    void request<{name?:string;home_layout?:unknown}>('/api/zen/prefs').then(p=>{ if(alive){name=p.name||'';layout=layoutOf(p.home_layout);} }).catch(()=>{layoutError='Could not load Home preferences.'});
    const refresh=async()=>{
      now=new Date();
      const results=await Promise.allSettled([zen.board(),zen.agents(),routines.list(),routines.runs(),files.home().then(h=>files.list(h.vault))]);
      if(!alive)return;
      if(results[0].status==='fulfilled'){sessions=results[0].value.sessions;error='';}else error='Could not load your sessions.';
      if(results[1].status==='fulfilled')agents=results[1].value;
      if(results[2].status==='fulfilled')jobs=results[2].value.jobs;
      if(results[3].status==='fulfilled')runs=results[3].value;
      if(results[4].status==='fulfilled')recentFiles=results[4].value.entries.filter(e=>!e.dir&&!e.hidden).sort((a,b)=>b.mtime-a.mtime).slice(0,5);
      loaded=true; void loadTasks();
    };
    void refresh(); const timer=setInterval(refresh,5000); return ()=>{alive=false;clearInterval(timer)};
  });
</script>
<div class="home">
  <header class="hh"><div><div class="date">{date}</div><h1>Good {greeting}{name ? `, ${name}` : ''}</h1><p class="sum"><b class="w">{waiting.length} things need you</b> · {working.length} jobs running · {due} tasks due today{#if jobs.some(j=>j.failures>0)} · <button class="link" onclick={()=>setView('home','routines')}>{jobs.filter(j=>j.failures>0).length} routine failed</button>{/if}</p></div><span class="sp"></span>{#if edit}<button class="ghost" onclick={()=>{layout=defaults();keep()}}>Reset</button><button class="solid" onclick={()=>edit=false}>Done</button>{:else}<button class="ghost" onclick={()=>edit=true}>Customize</button><button class="solid" onclick={()=>openSession('new')}>＋ New chat</button>{/if}</header>
  {#if error}<p role="alert">{error}</p>{/if}{#if taskError}<p role="alert">{taskError}</p>{/if}{#if layoutError}<p role="alert">{layoutError}</p>{/if}{#if !loaded}<p class="dim">Loading your board…</p>{/if}
  <div class:edit class="grid">
    {#each layout as s, i (s.id)}
      <article class="wd" style={`--span:${s.size}`} draggable={edit} ondragstart={()=>drag=s.id} ondragend={()=>drag=''} ondragover={(e)=>{if(edit && drag && drag!==s.id)e.preventDefault()}} ondrop={(e)=>{e.preventDefault();const from=layout.findIndex(x=>x.id===drag); if(from>=0)move(drag,i-from);drag=''}}>
        <header class="wh">{#if edit}<span class="grip">⠿</span>{/if}<h2>{catalog[s.kind].title}</h2>{#if s.kind==='needs' && waiting.length}<span class="count">{waiting.length}</span>{/if}<span class="sp"></span>{#if edit}<div class="ctl"><button aria-label="Move left" disabled={i===0} onclick={()=>move(s.id,-1)}>←</button><button aria-label="Move right" disabled={i===layout.length-1} onclick={()=>move(s.id,1)}>→</button><span class="sizes">{#each [1,2,3] as n}<button class:on={s.size===n} aria-label={`Size ${n}`} onclick={()=>{s.size=n as 1|2|3;keep()}}>{['S','M','L'][n-1]}</button>{/each}</span>{#if s.kind==='today'}<button aria-label="Configure tasks" onclick={()=>cfg=cfg===s.id?'':s.id}>⚙</button>{/if}<button aria-label="Remove widget" onclick={()=>{layout=layout.filter(x=>x.id!==s.id);keep()}}>×</button></div>{/if}</header>
        {#if edit && cfg===s.id && s.kind==='today'}<div class="cfg">
          <label>Show <select bind:value={s.filter.show} onchange={keep}><option value="today">Overdue & today</option><option value="week">Next 7 days</option><option value="all">All open</option></select></label>
          <label>Order <select bind:value={s.filter.order} onchange={keep}><option value="when">By due date</option><option value="priority">By priority</option></select></label>
          <label>Project <select bind:value={s.filter.project} onchange={keep}><option value="">All projects</option>{#each projects as p}<option value={p}>{p}</option>{/each}</select></label>
          <label>Goal <select bind:value={s.filter.goal} onchange={keep}><option value="">Any goal</option><option value="-">No goal</option>{#each goals as g}<option value={g}>{g}</option>{/each}</select></label>
          <label>Priority <select bind:value={s.filter.priority} onchange={keep}><option value="">All priorities</option>{#each ['urgent','important','medium','not_urgent','parking_lot','none'] as p}<option value={p}>{p.replaceAll('_',' ')}</option>{/each}</select></label>
          <label>Rows <select bind:value={s.filter.limit} onchange={keep}>{#each [5,8,12] as n}<option value={n}>{n}</option>{/each}</select></label></div>{/if}
        <div class="wb" inert={edit}>
        {#if s.kind==='needs'}<div class:wide={s.size>1} class="needs">{#each waiting as x}<button class="ncard" onclick={()=>openSession(x.id)}><span class="nt">{x.title||'Untitled chat'}<span class="when">{ago(x.updated_at)}</span></span><span class="nq">{x.question || 'Waiting for your answer'}</span><span class="go">Open chat →</span></button>{:else}<p class="dim">Nothing waiting on you.</p>{/each}</div>
        {:else if s.kind==='working'}{#each working as x}<button class="row" onclick={()=>openSession(x.id)}><span class="orb busy"></span><span class="col"><span class="t">{x.title||'Untitled chat'}</span><span class="d">{x.agent||'zenbot'} is working</span></span><span class="when">{ago(x.updated_at)}</span></button>{:else}<p class="dim">All quiet for now.</p>{/each}
        {:else if s.kind==='today'}{@const list=selectTasks(tasks,s.filter,today)}{#each list.slice(0,s.filter.limit) as t (t.id)}<div class="row task"><input type="checkbox" checked={t.done} aria-label={`Complete ${t.text}`} onchange={()=>toggle(t)} /><button class="col" onclick={()=>setView('home','tasks')}><span class="t">{t.text}</span><span class="d">{t.project}{t.due?` · ${t.due}`:''}</span></button></div>{:else}<p class="dim">{s.filter.show==='today'?'Nothing due today.':'Nothing here.'}</p>{/each}<button class="link" onclick={()=>setView('home','tasks')}>{list.length>s.filter.limit?`${list.length-s.filter.limit} more in Tasks →`:'All tasks →'}</button>
        {:else if s.kind==='recent'}{#each recent as x}<button class="row" onclick={()=>openSession(x.id)}><span class="t">{x.title||'Untitled chat'}</span><span class="when">{ago(x.updated_at)}</span></button>{:else}<p class="dim">No chats yet.</p>{/each}
        {:else if s.kind==='reports'}{#each runs.slice(0,5) as r}<button class="row" onclick={()=>r.session?openSession(r.session):setView('home','routines')}><span class="col"><span class="t">{r.job}</span><span class="d">{r.error || r.status}</span></span><span class="when">{ago(r.started_at)}</span></button>{:else}<p class="dim">No routine runs yet.</p>{/each}
        {:else if s.kind==='agents'}{#each agents as a}<button class="row" onclick={()=>setView('home','agents')}><span class="orb" class:busy={working.some(x=>x.agent===a.name)}></span><span class="col"><span class="t">{a.name}</span><span class="d">{a.error || (working.some(x=>x.agent===a.name)?'Working':'Idle')}</span></span></button>{:else}<p class="dim">No agents found.</p>{/each}
        {:else}<p class="d">Recently changed in the vault root</p>{#each recentFiles as f}<button class="row" onclick={()=>openFile(f.path)}><span class="t">{f.name}</span><span class="when">{ago(new Date(f.mtime).toISOString())}</span></button>{:else}<p class="dim">No recent files in the vault root.</p>{/each}{/if}
        </div>
      </article>
    {/each}
    {#if edit}<div class="add"><div class="addh">Add a widget</div><div class="addl">{#each hidden as k}<button onclick={()=>add(k)}><b>＋ {catalog[k].title}</b><span>{catalog[k].hint}</span></button>{/each}</div></div>{/if}
  </div>
</div>
<style>
  .home { flex: 1; overflow: auto; padding: 28px 32px 96px; }
  .hh { display: flex; align-items: flex-end; gap: 8px; max-width: 72rem; margin: 0 auto 22px; }
  .date { font-size: 13px; color: var(--dim); margin-bottom: 4px; }
  h1 { font-family: var(--serif); font-size: 32px; font-weight: 600; margin: 0; letter-spacing: -0.01em; }
  .sum { margin: 6px 0 0; color: var(--dim); font-size: 14px; }
  .sum .w { color: var(--warn); font-weight: 600; }
  .sp { flex: 1; }
  .cfg { display: flex; flex-direction: column; gap: 7px; margin: 0 0 10px; padding: 10px 12px; border: 1px solid var(--faint); border-radius: 12px; background: var(--paper); }
  .ghost, .solid { font-size: 13px; padding: 7px 14px; border-radius: 99px; border: 1px solid var(--faint); }
  .ghost:hover { border-color: var(--dim); }
  .solid { background: var(--accent); border-color: var(--accent); color: var(--on-accent); font-weight: 600; }

  .grid { max-width: 72rem; margin: 0 auto; display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); grid-auto-flow: row dense; gap: 14px; align-items: stretch; }
  .wd { background: color-mix(in srgb, var(--soft) 55%, var(--paper)); border: 1px solid var(--faint); border-radius: 18px; padding: 14px 14px 10px; min-width: 0; transition: box-shadow 0.15s, opacity 0.15s; }
  .edit .wd { border-style: dashed; cursor: grab; }
  .edit .wd:hover { box-shadow: 0 8px 24px var(--shadow); }
  .wh { display: flex; align-items: center; gap: 8px; padding: 0 4px 8px; min-height: 30px; }
  .wh h2 { min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; margin: 0; font-size: 12px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); font-weight: 650; }
  .grip { color: var(--dim); font-size: 14px; }
  .count { min-width: 18px; height: 18px; padding: 0 5px; border-radius: 99px; font-size: 11px; font-weight: 700; display: grid; place-items: center; background: var(--warn); color: var(--on-accent); }
  .ctl { display: flex; align-items: center; gap: 2px; }
  .ctl button { min-width: 24px; height: 24px; border-radius: 7px; font-size: 13px; color: var(--dim); }
  .ctl button:hover:not(:disabled) { background: var(--soft); color: var(--ink); }
  .ctl button.on { background: var(--soft); color: var(--ink); }
  .ctl button:disabled { opacity: 0.35; cursor: default; }
  .sizes { display: inline-flex; border: 1px solid var(--faint); border-radius: 8px; padding: 1px; margin: 0 4px; }
  .sizes button { font-size: 11px; font-weight: 650; min-width: 22px; height: 20px; }
  .sizes button.on { background: var(--accent); color: var(--on-accent); }
  .edit .wb { opacity: 0.6; }

  .row { display: flex; align-items: center; gap: 10px; width: 100%; text-align: left; padding: 8px 6px; border-radius: 10px; font-size: 14px; }
  button.row:hover { background: var(--soft); }
  .col { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 1px; }
  .t { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .d { font-size: 12.5px; color: var(--dim); overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .when { font-size: 12px; color: var(--dim); white-space: nowrap; margin-left: auto; }
  .task input { accent-color: var(--accent); width: 16px; height: 16px; margin: 0; }

  .orb { width: 12px; height: 12px; border-radius: 50%; flex: none; background: radial-gradient(circle at 32% 28%, #8fb39f, #4f7d67 70%); }
  .orb.busy { animation: breathe 1.8s ease-in-out infinite; }
  @keyframes breathe { 50% { box-shadow: 0 0 0 5px color-mix(in srgb, #4f7d67 18%, transparent); } }

  .needs { display: grid; gap: 10px; }
  .needs.wide { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .ncard { display: flex; flex-direction: column; gap: 6px; text-align: left; padding: 14px; border-radius: 14px; background: color-mix(in srgb, var(--warn) 7%, var(--paper)); border: 1px solid color-mix(in srgb, var(--warn) 35%, var(--faint)); }
  .ncard:hover { border-color: var(--warn); }
  .nt { display: flex; gap: 8px; font-size: 12.5px; color: var(--dim); }
  .nq { font-weight: 600; line-height: 1.4; }
  .go { font-size: 13px; color: var(--accent); font-weight: 600; margin-top: 2px; }

  .link { font-size: 13px; color: var(--accent); font-weight: 600; }

  .add { border: 1px dashed var(--faint); border-radius: 18px; padding: 14px 18px; }
  .addh { font-size: 12px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); font-weight: 650; margin-bottom: 10px; }
  .addl { display: flex; flex-wrap: wrap; gap: 8px; }
  .addl button { display: flex; flex-direction: column; align-items: flex-start; gap: 2px; padding: 10px 14px; border-radius: 14px; border: 1px solid var(--faint); text-align: left; }
  .addl button:hover { border-color: var(--accent); }
  .addl b { font-size: 14px; font-weight: 600; }
  .addl span { font-size: 12px; color: var(--dim); }
  .dim { color: var(--dim); font-size: 14px; }

  @media (max-width: 900px) {
    .home { padding: 18px 14px 110px; }
    h1 { font-size: 26px; }
    .grid { grid-template-columns: minmax(0, 1fr); }
    .hh { flex-wrap: wrap; }
  }

  button { font: inherit; color: inherit; cursor: pointer; border: 0; background: transparent; }
  .wd { grid-column: span var(--span); }
  .add { grid-column: 1 / -1; }
  .cfg label { display: flex; align-items: center; justify-content: space-between; gap: 8px; color: var(--dim); font-size: 12px; }
  .cfg select { max-width: 65%; font: inherit; color: var(--ink); background: var(--paper); border: 1px solid var(--faint); border-radius: 8px; padding: 4px; }
  .task .col { text-align: left; }
  @media (max-width: 900px) { .wd { grid-column: auto; } .needs.wide { grid-template-columns: 1fr; } }
</style>
