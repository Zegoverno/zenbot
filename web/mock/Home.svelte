<script lang="ts">
  // Home: the board you land on, one panel, no chat on it. A grid of widgets the owner arranges:
  // "Customize" lets you drag to reorder, size each one (S/M/L: 1–3 columns), hide it, or add one
  // back from the catalog; the layout is kept in localStorage. A widget's rows open the chat they
  // belong to (onopen); nothing is answered here. Fake data, like the rest of the mock.
  type Size = 1 | 2 | 3;
  type Slot = { id: string; size: Size };
  let { narrow, onopen }: { narrow: boolean; onopen: (session: string) => void } = $props();

  const CATALOG: Record<string, { title: string; hint: string; size: Size }> = {
    needs: { title: 'Needs you', hint: 'Questions waiting for your answer', size: 2 },
    working: { title: 'Working now', hint: 'What your agents are doing', size: 1 },
    today: { title: 'Today', hint: 'Tasks and reminders due', size: 1 },
    recent: { title: 'Recent chats', hint: 'Pick up where you left off', size: 1 },
    gospel: { title: 'Gospel of the day', hint: 'From the Liturgia plugin', size: 1 },
    reports: { title: 'From your routines', hint: 'Results of scheduled jobs', size: 1 },
    agents: { title: 'Agents', hint: 'Who is on duty', size: 1 },
    files: { title: 'Recent files', hint: 'Notes and files touched lately', size: 1 },
  };
  const DEFAULT: Slot[] = [
    { id: 'needs', size: 2 }, { id: 'working', size: 1 }, { id: 'today', size: 1 },
    { id: 'recent', size: 1 }, { id: 'gospel', size: 1 }, { id: 'reports', size: 1 },
  ];
  const KEY = 'zen.mock.home';
  function load(): Slot[] {
    try {
      const v = JSON.parse(localStorage.getItem(KEY) ?? '');
      if (Array.isArray(v) && v.every((s) => CATALOG[s?.id] && [1, 2, 3].includes(s.size))) return v;
    } catch {}
    return DEFAULT.map((s) => ({ ...s }));
  }
  let layout = $state<Slot[]>(load());
  const keep = () => localStorage.setItem(KEY, JSON.stringify(layout));
  let edit = $state(false);
  let dragId = $state<string | null>(null);
  const hidden = $derived(Object.keys(CATALOG).filter((id) => !layout.some((s) => s.id === id)));

  function moveTo(id: string, to: number) {
    const from = layout.findIndex((s) => s.id === id);
    if (from < 0 || to < 0 || to >= layout.length || from === to) return;
    const [s] = layout.splice(from, 1);
    layout.splice(to, 0, s);
    keep();
  }
  function over(e: DragEvent, id: string) {
    if (!dragId || dragId === id) return;
    e.preventDefault();
    moveTo(dragId, layout.findIndex((s) => s.id === id));
  }
  const resize = (s: Slot, size: Size) => { s.size = size; keep(); };
  const remove = (id: string) => { layout = layout.filter((s) => s.id !== id); keep(); };
  const add = (id: string) => { layout.push({ id, size: CATALOG[id].size }); keep(); };
  const reset = () => { layout = DEFAULT.map((s) => ({ ...s })); keep(); };

  const NEEDS = [
    { id: 'budget', title: 'Family budget for December', q: 'Should I move R$ 400 from savings to cover the December gap?', when: '8:40', agent: 'z' },
    { id: 'gift', title: 'Christmas gift ideas for the parents', q: 'Which of these three would your mother like most?', when: '7:05', agent: 'z' },
  ];
  const WORKING = [
    { id: 'trip', title: 'Plan a weekend in Campos do Jordão', step: 'Checking availability for Fri 21 – Sun 23…', since: '1 min' },
    { id: 'brief', title: 'Morning brief', step: 'Reading today’s calendar…', since: 'now' },
  ];
  const TODAY = [
    { t: 'Pay the electricity bill', at: '', done: true },
    { t: 'Call Mom — her birthday', at: '12:00', done: false },
    { t: 'Book Pousada Villa Bella', at: 'by Fri', done: false },
    { t: 'Pick up the dry cleaning', at: '18:00', done: false },
  ];
  const RECENT = [
    { id: 'recipe', t: 'Baby-friendly dinner recipes', w: 'Wed' },
    { id: 'doctor', t: 'Paediatrician appointment reminders', w: 'Tue' },
    { id: 'mass', t: 'Sunday Mass times near home', w: 'Mon' },
    { id: 'car', t: 'Car insurance renewal', w: 'Nov 12' },
  ];
  const REPORTS = [
    { id: 'brief', t: 'Morning brief', d: '3 things for today, no clashes', w: '6:00' },
    { id: 'budgetwk', t: 'Weekly budget check', d: 'On track; groceries 8% over', w: 'Mon' },
  ];
  const now = new Date(2026, 10, 19, 9, 20);
  const date = now.toLocaleDateString('en-GB', { weekday: 'long', day: 'numeric', month: 'long' });
</script>

<div class="home">
  <header class="hh">
    <div>
      <div class="date">{date}</div>
      <h1>Good morning, Adriane</h1>
      <p class="sum"><b class="w">2 things need you</b> · 2 jobs running · 3 tasks today</p>
    </div>
    <span class="sp"></span>
    {#if edit}
      <button class="ghost" onclick={reset}>Reset</button>
      <button class="solid" onclick={() => (edit = false)}>Done</button>
    {:else}
      <button class="ghost" onclick={() => (edit = true)}>Customize</button>
    {/if}
  </header>

  <div class="grid" class:edit>
    {#each layout as s, i (s.id)}
      {@const w = CATALOG[s.id]}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <article class="wd" style="grid-column: span {narrow ? 1 : s.size}" class:dragging={dragId === s.id}
        draggable={edit} ondragstart={(e) => { dragId = s.id; e.dataTransfer?.setData('text/plain', s.id); }}
        ondragover={(e) => over(e, s.id)} ondrop={(e) => e.preventDefault()} ondragend={() => (dragId = null)}>
        <header class="wh">
          {#if edit}<span class="grip" title="Drag to move">⠿</span>{/if}
          <h2>{w.title}</h2>
          {#if s.id === 'needs' && !edit}<span class="count">{NEEDS.length}</span>{/if}
          <span class="sp"></span>
          {#if edit}
            <div class="ctl">
              <button title="Move earlier" disabled={i === 0} onclick={() => moveTo(s.id, i - 1)}>‹</button>
              <button title="Move later" disabled={i === layout.length - 1} onclick={() => moveTo(s.id, i + 1)}>›</button>
              {#if !narrow}
                <span class="sizes">{#each [1, 2, 3] as n}<button class:on={s.size === n} onclick={() => resize(s, n as Size)}>{['S', 'M', 'L'][n - 1]}</button>{/each}</span>
              {/if}
              <button class="x" title="Hide this widget" onclick={() => remove(s.id)}>×</button>
            </div>
          {/if}
        </header>

        <div class="wb" inert={edit}>
          {#if s.id === 'needs'}
            <div class="needs" class:wide={s.size > 1 && !narrow}>
              {#each NEEDS as n}
                <button class="ncard" onclick={() => onopen(n.id)}>
                  <span class="nt">{n.title}<span class="when">{n.when}</span></span>
                  <span class="nq">{n.q}</span>
                  <span class="go">Open chat →</span>
                </button>
              {/each}
            </div>
          {:else if s.id === 'working'}
            {#each WORKING as r}
              <button class="row" onclick={() => onopen(r.id)}>
                <span class="orb busy"></span>
                <span class="col"><span class="t">{r.title}</span><span class="d">{r.step}</span></span>
                <span class="when">{r.since}</span>
              </button>
            {/each}
          {:else if s.id === 'today'}
            {#each TODAY as r}
              <label class="row task" class:done={r.done}><input type="checkbox" checked={r.done} /><span class="t">{r.t}</span><span class="when">{r.at}</span></label>
            {/each}
          {:else if s.id === 'recent'}
            {#each RECENT as r}
              <button class="row" onclick={() => onopen(r.id)}><span class="t">{r.t}</span><span class="when">{r.w}</span></button>
            {/each}
          {:else if s.id === 'gospel'}
            <div class="gospel">
              <div class="ref">Thursday, 33rd week in Ordinary Time · Lk 19:41–44</div>
              <blockquote>“If this day you only knew what makes for peace.”</blockquote>
              <button class="link">Read the readings →</button>
            </div>
          {:else if s.id === 'reports'}
            {#each REPORTS as r}
              <button class="row" onclick={() => onopen(r.id)}><span class="col"><span class="t">{r.t}</span><span class="d">{r.d}</span></span><span class="when">{r.w}</span></button>
            {/each}
          {:else if s.id === 'agents'}
            <div class="row"><span class="orb busy"></span><span class="col"><span class="t">zenbot</span><span class="d">Working on 2 jobs</span></span></div>
            <div class="row"><span class="orb g"></span><span class="col"><span class="t">Garry</span><span class="d">Idle</span></span></div>
          {:else if s.id === 'files'}
            {#each [['Family budget — December.md', '8:41'], ['Campos do Jordão trip.md', '9:16'], ['baby-room-ideas.png', 'Wed']] as [f, w]}
              <div class="row"><span class="t">{f}</span><span class="when">{w}</span></div>
            {/each}
          {/if}
        </div>
      </article>
    {/each}

    {#if edit}
      <div class="add" style="grid-column: span {narrow ? 1 : 3}">
        <div class="addh">Add a widget</div>
        {#if hidden.length}
          <div class="addl">
            {#each hidden as id}
              <button onclick={() => add(id)}><b>＋ {CATALOG[id].title}</b><span>{CATALOG[id].hint}</span></button>
            {/each}
          </div>
        {:else}
          <div class="dim">Every widget is on the board. Plugins can add more.</div>
        {/if}
      </div>
    {/if}
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
  .ghost, .solid { font-size: 13px; padding: 7px 14px; border-radius: 99px; border: 1px solid var(--faint); }
  .ghost:hover { border-color: var(--dim); }
  .solid { background: var(--accent); border-color: var(--accent); color: var(--on-accent); font-weight: 600; }

  .grid { max-width: 72rem; margin: 0 auto; display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); grid-auto-flow: row dense; gap: 14px; align-items: start; }
  .wd { background: color-mix(in srgb, var(--soft) 55%, var(--paper)); border: 1px solid var(--faint); border-radius: 18px; padding: 14px 14px 10px; min-width: 0; transition: box-shadow 0.15s, opacity 0.15s; }
  .edit .wd { border-style: dashed; cursor: grab; }
  .edit .wd:hover { box-shadow: 0 8px 24px var(--shadow); }
  .wd.dragging { opacity: 0.4; }
  .wh { display: flex; align-items: center; gap: 8px; padding: 0 4px 8px; min-height: 30px; }
  .wh h2 { min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; margin: 0; font-size: 12px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); font-weight: 650; }
  .grip { color: var(--dim); font-size: 14px; }
  .count { min-width: 18px; height: 18px; padding: 0 5px; border-radius: 99px; font-size: 11px; font-weight: 700; display: grid; place-items: center; background: var(--warn); color: var(--on-accent); }
  .ctl { display: flex; align-items: center; gap: 2px; }
  .ctl button { min-width: 24px; height: 24px; border-radius: 7px; font-size: 13px; color: var(--dim); }
  .ctl button:hover:not(:disabled) { background: var(--soft); color: var(--ink); }
  .ctl button:disabled { opacity: 0.35; cursor: default; }
  .sizes { display: inline-flex; border: 1px solid var(--faint); border-radius: 8px; padding: 1px; margin: 0 4px; }
  .sizes button { font-size: 11px; font-weight: 650; min-width: 22px; height: 20px; }
  .sizes button.on { background: var(--accent); color: var(--on-accent); }
  .x { font-size: 16px !important; }
  .edit .wb { opacity: 0.6; }

  .row { display: flex; align-items: center; gap: 10px; width: 100%; text-align: left; padding: 8px 6px; border-radius: 10px; font-size: 14px; }
  button.row:hover { background: var(--soft); }
  .col { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 1px; }
  .t { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .d { font-size: 12.5px; color: var(--dim); overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .when { font-size: 12px; color: var(--dim); white-space: nowrap; margin-left: auto; }
  .task input { accent-color: var(--accent); width: 16px; height: 16px; margin: 0; }
  .task.done .t { text-decoration: line-through; color: var(--dim); }

  .orb { width: 12px; height: 12px; border-radius: 50%; flex: none; background: radial-gradient(circle at 32% 28%, #8fb39f, #4f7d67 70%); }
  .orb.g { background: radial-gradient(circle at 32% 28%, #b3a3cf, #7a6a9b 70%); }
  .orb.busy { animation: breathe 1.8s ease-in-out infinite; }
  @keyframes breathe { 50% { box-shadow: 0 0 0 5px color-mix(in srgb, #4f7d67 18%, transparent); } }

  .needs { display: grid; gap: 10px; }
  .needs.wide { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .ncard { display: flex; flex-direction: column; gap: 6px; text-align: left; padding: 14px; border-radius: 14px; background: color-mix(in srgb, var(--warn) 7%, var(--paper)); border: 1px solid color-mix(in srgb, var(--warn) 35%, var(--faint)); }
  .ncard:hover { border-color: var(--warn); }
  .nt { display: flex; gap: 8px; font-size: 12.5px; color: var(--dim); }
  .nq { font-weight: 600; line-height: 1.4; }
  .go { font-size: 13px; color: var(--accent); font-weight: 600; margin-top: 2px; }

  .gospel { padding: 2px 6px 6px; }
  .ref { font-size: 12.5px; color: var(--dim); }
  blockquote { margin: 10px 0 12px; font-family: var(--serif); font-size: 19px; line-height: 1.4; }
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
</style>
