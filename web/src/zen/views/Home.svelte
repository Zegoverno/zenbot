<script lang="ts">
  // A dashboard over the same live board as Sessions. Rows always open real chats.
  import { onMount } from 'svelte';
  import { zen, type BoardSession, type Task } from '../api.ts';
  import { openSession, setView } from '../app.svelte.ts';

  let sessions = $state<BoardSession[]>([]);
  let tasks = $state<Task[]>([]);
  let today = $state('');
  let taskError = $state(false);
  let error = $state('');
  let loaded = $state(false);
  const top = $derived(sessions.filter((s) => !s.parent));
  const waiting = $derived(top.filter((s) => s.waiting && !s.busy));
  const working = $derived(top.filter((s) => s.busy));
  const recent = $derived([...top].sort ( (a, b) => b.updated_at.localeCompare(a.updated_at)).slice(0, 5));
  const due = $derived(tasks.filter((t) => t.overdue || (t.due && t.due <= today)));
  const hour = new Date().getHours();
  const greeting = hour < 12 ? 'morning' : hour < 18 ? 'afternoon' : 'evening';
  const date = new Date().toLocaleDateString(undefined, { weekday: 'long', day: 'numeric', month: 'long' });

  onMount(() => {
    let alive = true;
    const refresh = async () => {
      try {
        const b = await zen.board();
        if (alive) { sessions = b.sessions; error = ''; loaded = true; }
      } catch { if (alive) { error = 'Could not load your sessions.'; loaded = true; } }
      try {
        const t = await zen.tasks();
        if (alive) { tasks = t.projects.flatMap((p) => p.tasks); today = t.today; taskError = false; }
      } catch { if (alive) taskError = true; }
    };
    void refresh();
    const timer = setInterval(refresh, 4000);
    return () => { alive = false; clearInterval(timer); };
  });
</script>

<div class="home">
  <header>
    <div><div class="date">{date}</div><h1>Good {greeting}</h1>
      <p>{waiting.length} need you · {working.length} working · {taskError ? 'tasks unavailable' : `${due.length} tasks due`}</p></div>
    <button class="new" onclick={() => openSession('new')}>＋ New chat</button>
  </header>
  {#if error}<p role="alert">{error}</p>{/if}
  {#if !loaded}<p>Loading your board…</p>{/if}
  <div class="grid">
    <section class="card needs"><div class="card-head"><h2>Needs you</h2><span>{waiting.length}</span></div>
      {#each waiting as s (s.id)}<button class="row" onclick={() => openSession(s.id)}><span class="orb">{(s.agent || 'z').slice(0, 1)}</span><span><b>{s.title || 'Untitled chat'}</b><small>Waiting for your answer</small></span><span class="arrow">›</span></button>{:else}<p class="empty">Nothing waiting on you.</p>{/each}
    </section>
    <section class="card"><div class="card-head"><h2>Working now</h2><span>{working.length}</span></div>
      {#each working as s (s.id)}<button class="row" onclick={() => openSession(s.id)}><span class="pulse">●</span><span><b>{s.title || 'Untitled chat'}</b><small>{s.agent || 'zenbot'} is working</small></span><span class="arrow">›</span></button>{:else}<p class="empty">All quiet for now.</p>{/each}
    </section>
    <section class="card"><div class="card-head"><h2>Tasks</h2><button onclick={() => setView('home', 'tasks')}>View all ›</button></div>
      {#if taskError}<p class="empty">Could not load tasks.</p>{:else}{#each due.slice(0, 5) as t}<button class="row" onclick={() => setView('home', 'tasks')}><span class="check">○</span><span><b>{t.text}</b><small>{t.project}{t.due ? ` · ${t.due}` : ''}</small></span><span class="arrow">›</span></button>{:else}<p class="empty">No tasks due today.</p>{/each}{/if}
    </section>
    <section class="card"><div class="card-head"><h2>Recent chats</h2><button onclick={() => setView('home', 'sessions')}>View all ›</button></div>
      {#each recent as s (s.id)}<button class="row" onclick={() => openSession(s.id)}><span><b>{s.title || 'Untitled chat'}</b><small>{s.agent || 'zenbot'}</small></span><span class="arrow">›</span></button>{:else}<p class="empty">No chats yet.</p>{/each}
    </section>
  </div>
</div>

<style>
  .home { max-width: 1100px; margin: auto; padding: 46px 32px 64px; color: var(--ink); }
  header { display:flex; align-items:center; justify-content:space-between; gap:16px; margin-bottom:32px; }
  .date { color:var(--dim); font-size:12px; letter-spacing:.08em; text-transform:uppercase; }
  h1 { font-family: Georgia, serif; font-size:36px; font-weight:400; margin:8px 0; }
  header p, .empty { color:var(--dim); font-size:13px; }
  button { font:inherit; cursor:pointer; }
  .new { background:var(--accent); color:var(--paper); border:0; padding:11px 16px; border-radius:10px; white-space:nowrap; }
  .grid { display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); gap:16px; }
  .card { border:1px solid var(--faint); border-radius:14px; padding:19px; min-width:0; background:var(--paper); }
  .needs { grid-column:span 2; }
  .card-head { display:flex; align-items:center; justify-content:space-between; gap:10px; margin-bottom:12px; }
  .card-head h2 { margin:0; font-family:Georgia,serif; font-size:19px; font-weight:400; }
  .card-head span, .card-head button { color:var(--dim); font-size:12px; }
  .card-head button { border:0; background:none; }
  .row { display:flex; align-items:center; text-align:left; gap:12px; width:100%; border:0; border-top:1px solid var(--faint); background:none; color:inherit; padding:13px 0; }
  .row:hover { color:var(--accent); }
  .row b { display:block; font-size:13px; font-weight:600; overflow-wrap:anywhere; }
  .row small { display:block; color:var(--dim); margin-top:4px; }
  .orb { display:grid; place-items:center; flex:none; width:32px; height:32px; border-radius:50%; background:var(--faint); color:var(--accent); }
  .arrow { margin-left:auto; color:var(--dim); font-size:20px; }
  .pulse, .check { color:var(--accent); }
  @media (max-width:700px) { .home { padding:28px 18px; } h1 { font-size:29px; } .grid { grid-template-columns:1fr; } .needs { grid-column:auto; } }
</style>
