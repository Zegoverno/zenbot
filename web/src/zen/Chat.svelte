<script lang="ts">
  // A session's chat, as the redesign draws it (web/mock/ChatMock2.svelte's chat pane) on the
  // terminal's logic (crates/zen/src/tui: events.rs, transcript.rs, input.rs, pickers.rs).
  // The header: the title (double-click or ⋯ › Rename), working / needs-you, the ⋯ details (model,
  // effort, context, session; rename; archive with how it ended) and × to close. The conversation:
  // prompts as bubbles, Markdown replies streamed as they come, each run of tool calls folded into
  // one "N steps" line, questions as a card with their options (one click answers), and while the
  // agent works one live line that opens into every step of the turn and its subagents (each with
  // its steps, and its session beside the chat). The message box: attachments (sent to ~/inbox at
  // once, named in the prompt), the model and effort pickers, send or stop; `/` commands with their
  // pickers. 'new' is a chat whose session starts with its first prompt: the greeting, who to talk
  // to, and three ideas that load without blocking; that prompt is handed to the chat re-created
  // with the session's id (Shell keys Chat on the session).
  // Plugins: a plugin's slash command calls its tool and shows its UI in the right panel (typed with
  // no argument and with declared `choices`, it opens a picker of them first: a choice runs it, one
  // with `then` goes to the composer to finish); a plugin UI's `sendMessage` queues an
  // owner-reviewed draft here (`chat.queue`), and what plugin UIs put in the model context shows
  // as a removable chip above the box and goes with the next prompt (plugins.svelte.ts).
  import { onMount, tick } from 'svelte';
  import { zen, files, commands, upload, short, readOnly, AGENT_NAME, pluginApi, type Command, type Message, type Outcome, type SessionDetail, type ZenEvent } from './api.ts';
  import { chat, contexts, contextSaving, dropContext, plugins, resultText, showPanel, takeContext, toolArgs, uiOf } from './plugins.svelte.ts';
  import { app, board as toBoard, openSession, sessionStarted, openFile, openDir, pickTab, openTab, settings, notice, typing, tilde, closeChat, goBack, openSub, showFile } from './app.svelte.ts';
  import { me, board, agents, agentOf, models, modelOf, kids, loadKid, loadIdeas, stepOf, clock, EFFORT, effortName, type Sugg } from './live.svelte.ts';
  import { render as mdRender } from './md.ts';
  import * as M from './chat/model.ts';

  let { id, split }: { id: string; split: boolean } = $props();

  const READ_ONLY = 'Read-only: zenbot drives this session';
  const PAGE = 40;

  // ---- the session ----
  // svelte-ignore state_referenced_locally
  const sid: string | null = id === 'new' ? null : id;
  let title = $state('');
  let model = $state('');
  let effort = $state<string | null>(null);
  let kind = $state<string | null>(null);
  /** Who the session is: a fresh chat starts as the agent picked at the top (the picker changes it). */
  // svelte-ignore state_referenced_locally
  let agent = $state(sid ? 'zenbot' : app.agent);
  let cmds = $state<Command[]>([]);
  let loaded = $state(false);
  let failed = $state('');
  /** The kernel's message count, and how many we hold (for "show N earlier"). */
  let total = $state(0);
  let held = $state(0);
  /** How many of the kernel's messages we've accounted for (shown, or older than the first shown). */
  let known = 0;
  let last = PAGE;
  let firstSeq = Infinity;
  let modelPinned = false;
  const ro = $derived(readOnly({ kind }));
  const who = $derived(agentOf(agent));

  // ---- the conversation ----
  let entries = $state<M.Entry[]>([]);
  let expandAll = $state(false);
  let stream = '';
  let streamShown = $state('');
  let streamTimer: ReturnType<typeof setTimeout> | null = null;
  let lastDraw = 0;
  /** Prompts sent here that the kernel hasn't echoed yet. */
  let pending: string[] = [];
  /** Folded runs opened by a click, and steps whose output shows. */
  let open = $state<Record<number, boolean>>({});
  let outOpen = $state<Record<string, boolean>>({});

  // ---- the turn ----
  let busy = $state(false);
  let status = $state('Working');
  let turnStarted = $state(Date.now());
  let toolSince = $state<number | null>(null);
  let turnTokens = 0;
  let turnModel = '';
  let turnEffort: string | null = null;
  let aborting = false;
  let sessionTokens = $state(0);
  let suggestion = $state<{ id: number; text: string; taken: boolean } | null>(null);
  let lastFile: string | null = null;
  let now = $state(Date.now());
  /** When each tool call of the turn started (ms into the turn), by call id. */
  let stepAt = $state<Record<string, number>>({});
  let liveOpen = $state(false);
  let hOpen = $state<Record<string, boolean>>({});

  // ---- the connection ----
  let conn: ReturnType<typeof zen.socket> | null = null;
  let gen = 0;
  let connected = $state(true);
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  let retries = 0;
  let alive = true;
  let upgrading = false;
  let mcpFlow = $state<{ server: string; flow: string } | null>(null);

  // ---- the header's menus ----
  let details = $state(false);
  let renaming = $state(false);
  let nameDraft = $state('');
  const OUTCOMES = [
    { v: 'done', l: 'Done', h: 'The job is done' },
    { v: 'partial', l: 'Part done', h: 'Continue in a new chat that picks up from here' },
    { v: 'not-done', l: 'Not done', h: 'Stop here' },
    { v: '-', l: 'Just archive', h: 'Don’t say how it ended' },
  ];
  let outcome = $state('done');
  let outcomeOpen = $state(false);
  let pop = $state<'model' | 'effort' | null>(null);
  let moreModels = $state(false);

  // ---- the composer ----
  let input = $state('');
  let ta = $state<HTMLTextAreaElement>();
  let root = $state<HTMLElement>();
  let scroller = $state<HTMLElement>();
  let content = $state<HTMLElement>();
  let menuSel = $state(0);
  let creating = $state(false);
  // `then` (plugin-choice only): the args hint of a choice that needs more input.
  type Pick = { title: string; items: { label: string; value: string; then?: string }[]; sel: number; kind: 'session' | 'model' | 'effort' | 'outcome' | 'agent' | 'plugin-choice'; command?: Command };
  let picker = $state<Pick | null>(null);
  let hist: string[] = [];
  let hi = $state(-1);
  let draft = '';
  let pinned = $state(true);
  let unseen = $state(0);

  // ---- new chat: three ideas from the assist model, never blocking the box ----
  let sugg = $state<Sugg[] | null>(null);
  let suggFailed = $state(false);

  // ---- attachments: sent to ~/inbox at once, named in the prompt on send ----
  type Attachment = { id: number; name: string; size: number; progress: number; path?: string; error?: string; cancel: () => void };
  let attachments = $state<Attachment[]>([]);
  let filePick = $state<HTMLInputElement>();
  let dragging = $state(false);
  let nextAtt = 0;
  const uploading = $derived(attachments.some((a) => !a.path && !a.error));
  const ready = $derived(attachments.filter((a) => a.path));

  function attach(list: FileList | File[] | null | undefined) {
    if (ro || !list) return;
    for (const file of Array.from(list)) {
      const id = nextAtt++;
      const job = upload(file, (f) => setAtt(id, { progress: f }));
      attachments.push({ id, name: file.name || 'pasted', size: file.size, progress: 0, cancel: job.cancel });
      job.done.then(
        (r) => setAtt(id, { path: r.path, progress: 1 }),
        (e) => setAtt(id, { error: String((e as Error).message ?? e) }),
      );
    }
    ta?.focus();
  }
  function setAtt(id: number, patch: Partial<Attachment>) {
    const a = attachments.find((x) => x.id === id);
    if (a) Object.assign(a, patch);
  }
  function unattach(id: number) {
    const a = attachments.find((x) => x.id === id);
    if (a && !a.path && !a.error) a.cancel();
    attachments = attachments.filter((x) => x.id !== id);
  }
  function onPaste(e: ClipboardEvent) {
    const fs_ = e.clipboardData?.files;
    if (fs_ && fs_.length) {
      e.preventDefault();
      attach(fs_);
    }
  }
  function onDrop(e: DragEvent) {
    dragging = false;
    if (!e.dataTransfer?.files.length) return;
    e.preventDefault();
    attach(e.dataTransfer.files);
  }
  const sizeText = (n: number) => (n < 1024 ? `${n} B` : n < 1024 ** 2 ? `${(n / 1024).toFixed(0)} KB` : n < 1024 ** 3 ? `${(n / 1024 ** 2).toFixed(1)} MB` : `${(n / 1024 ** 3).toFixed(2)} GB`);

  const menu = $derived(input.startsWith('/') && !/\s/.test(input) ? cmds.filter((c) => c.name.startsWith(input.slice(1))) : []);
  const info = $derived(modelOf(model || me.defaultModel || models.default));
  const levels = $derived(info.efforts);
  const shownEffort = $derived(levels.length ? (effort ?? info.def ?? null) : null);
  const earlier = $derived(Math.max(0, total - held));

  // ---- the turn's steps and subagents ----
  /** Where the current turn starts in the transcript: after the last prompt. */
  const turnAt = $derived.by(() => {
    for (let i = entries.length - 1; i >= 0; i--) if (entries[i].k === 'user') return i;
    return -1;
  });
  /** The current turn's tool calls, in plain words. */
  const live = $derived(busy ? entries.slice(turnAt + 1).flatMap((e) => (e.k === 'work' ? e.steps : [])) : []);
  const liveSteps = $derived(live.map((s) => ({ ...stepOf(s.name, s.args), t: s.id && stepAt[s.id] !== undefined ? clock(stepAt[s.id]) : '', failed: s.result?.error === true })));
  /** This turn's subagents: the session's children started since the turn began. */
  const subs = $derived(busy && sid ? (board.children[sid] ?? []).filter((c) => Date.parse(c.created_at) >= turnStarted - 2000 || c.busy) : []);
  $effect(() => {
    if (!liveOpen || !subs.length) return;
    const ids = subs.map((s) => s.id);
    for (const k of ids) void loadKid(k);
    const t = setInterval(() => ids.forEach((k) => void loadKid(k)), 3000);
    return () => clearInterval(t);
  });
  const subName = (c: { id: string; title: string | null; role?: string | null }) => kids[c.id]?.name || (c.title ?? '').replace(/^subagent:\s*/i, '') || c.role || 'Subagent';
  /** The question card waiting on an answer (the last thing said, nothing sent since). */
  const asking = $derived(!busy && entries.length > 0 && entries[entries.length - 1].k === 'questions');
  const shown = $derived(busy ? 'working' : asking ? 'waiting' : 'chat');
  const placeholder = $derived(ro ? READ_ONLY : suggestion && !busy && !input ? suggestion.text : asking ? `Answer ${who.name}…` : `Message ${who.name}`);
  const closeTitle = $derived(split && app.view !== 'subagent' ? `Close the chat, keep ${app.view}` : 'Close the chat, back to Sessions');

  // ---- entries ----

  function push(...list: M.New[]) {
    for (const e of list) M.absorb(entries, e);
    if (!pinned) unseen += list.length;
  }
  const line = (text: string, sty: M.Sty = 'dim') => push({ k: 'line', text, sty });

  /** Add messages we don't hold yet (the live ones carry no seq, so we count). */
  function take(messages: Message[]) {
    for (const m of messages) {
      held++;
      known++;
      if (m.role === 'user' && !m.kernel) {
        const i = pending.findIndex((p) => M.echoes(m, p));
        if (i >= 0) {
          pending.splice(i, 1);
          continue;
        }
      }
      push(...M.messageEntries(m));
    }
  }

  function apply(d: SessionDetail) {
    title = d.title ?? '';
    model = d.model;
    effort = d.effort;
    kind = d.kind;
    agent = d.agent ?? 'zenbot';
    total = d.message_count;
  }

  // ---- loading and the socket ----

  async function load() {
    try {
      const d = await zen.session(id, last);
      if (!alive) return;
      apply(d);
      sessionTokens = d.tokens;
      entries = M.build(d.messages);
      held = d.messages.length;
      known = d.message_count;
      for (const m of d.messages) firstSeq = Math.min(firstSeq, m.seq);
      if (d.busy) {
        beginTurn();
        const u = [...d.messages].reverse().find((m) => m.role === 'user');
        if (u?.timestamp) turnStarted = u.timestamp;
      } else if (d.suggestion) suggestion = { id: d.suggestion.id, text: d.suggestion.text.trim(), taken: false };
      loaded = true;
      const h = M.takeHandoff(id);
      if (h) {
        if (!title) title = h.text.slice(0, 40).trim();
        push({ k: 'user', text: h.text, kernel: false });
        pending.push(h.text);
        beginTurn();
      }
      connect();
      if (h && !conn?.send(h.message)) {
        busy = false;
        notice("couldn't send to zenbot; press ↑ to get the prompt back and send it again");
      }
      await bottom();
    } catch (e) {
      failed = String((e as Error).message ?? e);
      loaded = true;
    }
  }

  function connect() {
    if (!sid || !alive) return;
    const g = ++gen;
    conn = zen.socket(
      sid,
      (e) => g === gen && onEvent(e),
      () => g === gen && lost(),
    );
    connected = true;
  }

  function lost() {
    conn = null;
    connected = false;
    if (!upgrading) notice('lost connection to zenbot; reconnecting…');
    retry(retries === 0 ? 300 : Math.min(10_000, 1000 * 2 ** Math.min(retries, 4)));
  }

  function retry(ms: number) {
    clearTimeout(retryTimer);
    retryTimer = setTimeout(reconnect, ms);
  }

  /** Back on the kernel: what happened meanwhile, then the live events. */
  async function reconnect() {
    if (!alive || !sid || conn) return;
    retries++;
    try {
      await catchUp();
      connect();
      retries = 0;
      if (!upgrading) notice('reconnected', 2000);
    } catch {
      retry(Math.min(10_000, 1000 * 2 ** Math.min(retries, 4)));
    }
  }

  /** Fetch the session and add the messages we don't hold; the turn's state follows the kernel's. */
  async function catchUp() {
    if (!sid) return;
    let d = await zen.session(sid, PAGE);
    let missing = d.message_count - known;
    if (missing > d.messages.length) {
      d = await zen.session(sid, Math.min(missing, 1000));
      missing = d.message_count - known;
    }
    if (!alive) return;
    apply(d);
    if (missing > 0) {
      if (missing > d.messages.length) line(`${missing - d.messages.length} messages from meanwhile aren't shown; reload the page for them`, 'warn');
      take(d.messages.slice(-missing));
    }
    known = Math.max(known, d.message_count);
    // Deltas missed meanwhile can't be pieced together: the reply shows whole when it ends.
    resetStream();
    if (d.busy && !busy) beginTurn();
    if (!d.busy) busy = false;
  }

  async function showEarlier() {
    if (!sid || !scroller) return;
    last += PAGE;
    try {
      const d = await zen.session(sid, last);
      const older = d.messages.filter((m) => m.seq < firstSeq);
      if (!older.length) return;
      const keep = scroller.scrollHeight - scroller.scrollTop;
      const before = M.build(older);
      entries = [...before, ...entries];
      held += older.length;
      total = d.message_count;
      firstSeq = Math.min(...older.map((m) => m.seq));
      await tick();
      scroller.scrollTop = scroller.scrollHeight - keep;
    } catch (e) {
      notice(String((e as Error).message ?? e));
    }
  }

  // ---- events (events.rs) ----

  function beginTurn() {
    busy = true;
    status = 'Working';
    turnStarted = Date.now();
    turnTokens = 0;
    toolSince = null;
    stepAt = {};
  }

  function resetStream() {
    stream = '';
    streamShown = '';
    if (streamTimer) clearTimeout(streamTimer);
    streamTimer = null;
  }

  function drawStream() {
    if (streamTimer) return;
    const wait = Math.max(0, 33 - (performance.now() - lastDraw));
    streamTimer = setTimeout(() => {
      streamTimer = null;
      lastDraw = performance.now();
      streamShown = stream;
    }, wait);
  }

  const base = (name: string) => name.split('__').pop() ?? name;

  function onEvent(e: ZenEvent) {
    switch (e.type) {
      case 'message': {
        const m = e.message;
        held++;
        known++;
        total = Math.max(total, known);
        if (m.role === 'user' && !m.kernel) {
          const i = pending.findIndex((p) => M.echoes(m, p));
          if (i >= 0) {
            pending.splice(i, 1);
            return;
          }
        }
        if (m.role === 'assistant') {
          push(...M.messageEntries(m, { aborting, streamed: stream }));
          resetStream();
          turnTokens += M.usageTotal(m.usage);
          if (m.model) turnModel = m.model;
        } else push(...M.messageEntries(m));
        break;
      }
      case 'delta':
        stream += e.delta;
        status = 'Writing';
        toolSince = null;
        drawStream();
        break;
      case 'busy':
        if (e.busy === false) break;
        suggestion = null;
        if (!busy) beginTurn();
        turnEffort = e.effort ?? null;
        if (e.model) turnModel = e.model.split('/').pop() ?? e.model;
        break;
      case 'questions':
        push({ k: 'questions', questions: e.questions.map((q) => ({ question: M.clean(q.question), options: q.options.map(M.clean) })) });
        break;
      case 'title':
        title = M.clean(e.title);
        break;
      case 'suggestion':
        if (!busy) suggestion = { id: e.id, text: e.text.trim(), taken: false };
        break;
      case 'model':
        if (e.model) {
          model = e.model;
          effort = e.effort;
        }
        break;
      case 'status':
        status = M.clean(e.text || 'Working');
        break;
      case 'child_end':
        sessionTokens += M.recordTotal(e.turn);
        break;
      case 'idle':
        busy = false;
        break;
      case 'thinking':
        status = 'Thinking';
        break;
      case 'tool_start': {
        status = 'Working';
        toolSince = Date.now();
        stepAt[e.call_id] = Date.now() - turnStarted;
        const path = typeof e.args?.path === 'string' ? e.args.path : null;
        const b = base(e.name);
        if (path && (b === 'read' || b === 'write' || b === 'edit')) lastFile = path;
        break;
      }
      case 'tool_end':
        status = 'Working';
        toolSince = null;
        break;
      case 'resync':
        line(`missed ${e.skipped} updates from zenbot (the page fell behind)`, 'warn');
        catchUp().catch(() => {});
        if (!e.busy && busy) end(null, undefined, false);
        break;
      case 'end':
        if (busy) end(e.error, e.turn, e.next === true);
        break;
      case 'error':
        busy = false;
        line(M.clean(e.error), 'err');
        break;
    }
  }

  function end(error: string | null, turn: Parameters<typeof M.recordTotal>[0], next: boolean) {
    toolSince = null;
    if (aborting) {
      if (stream.trim()) push({ k: 'md', text: stream });
      line('interrupted', 'warn');
    } else if (error) line(M.clean(error), 'err');
    aborting = false;
    resetStream();
    if (turn && typeof turn === 'object') turnTokens = M.recordTotal(turn);
    sessionTokens += turnTokens;
    turnEffort = null;
    void turnModel;
    busy = next;
    if (busy) status = 'Continuing';
  }

  // ---- sending ----

  function abort() {
    if (!busy || ro) return;
    conn?.send({ type: 'abort' });
    status = 'Stopping';
    aborting = true;
  }

  // What plugin UIs on screen want to go with the next prompt: one chip each, removable.
  const pluginContext = $derived(Object.entries(contexts[sid ?? 'new'] ?? {}));

  let pluginDrafts = $state<{ plugin: string; text: string }[]>([]);

  function sendPluginDraft(d: { plugin: string; text: string }) {
    if (ro || creating || uploading || mcpFlow || ready.length) return;
    const ownDraft = input;
    input = `(from the ${d.plugin} plugin) ${d.text}`;
    void submit().finally(() => { if (!input) input = ownDraft; });
    pluginDrafts = pluginDrafts.filter((x) => x !== d);
  }

  async function submit() {
    const raw = input.trim();
    if ((!raw && !ready.length) || creating) return;
    if (uploading) return notice('still sending the attachments; send when they show ✓');
    if (contextSaving(sid ?? 'new')) return notice("still saving a plugin's images; send when they show ✓");
    const steer = raw.startsWith('/steer ');
    let text = steer ? raw.slice(7).trim() : raw;
    if (!text && !ready.length) return;
    // Attachments travel as paths in ~/inbox: zenbot reads them with its own tools.
    const sent = ready.map((a) => a.id);
    if (ready.length && !text.startsWith('/')) {
      const list = ready.map((a) => `(attached ${tilde(a.path!)})`).join('\n');
      text = text ? `${text}\n\n${list}` : list;
    }
    // The file open beside the chat travels with the prompt, so "this file" needs no path.
    const viewing = split && app.view === 'files' ? app.viewing : null;
    if (viewing && !text.startsWith('/')) text += `\n\n(viewing ${tilde(viewing)})`;
    if (raw) M.saveHistory(raw);
    hi = -1;
    if (mcpFlow) {
      const f = mcpFlow;
      mcpFlow = null;
      input = '';
      if (text.startsWith('/')) {
        notice(`sign-in to ${f.server} cancelled`);
        return command(text);
      }
      try {
        const done = await zen.mcpCallback(f.flow, text);
        line(`Signed in to ${done.server || f.server}.`, 'accent');
      } catch (e) {
        line(String((e as Error).message ?? e), 'err');
      }
      return;
    }
    if (text.startsWith('/') && !steer) {
      input = '';
      return command(text);
    }
    if (ro) return notice('this session is read-only: zenbot drives it');
    // What plugin UIs on screen told the chat: no context field on the kernel's prompt yet, so it
    // leads the prompt in one delimited block.
    text = takeContext(sid ?? 'new') + text;
    const message: Record<string, unknown> = { type: 'prompt', text };
    if (suggestion) message.suggestion = { id: suggestion.id, taken: suggestion.taken };
    if (steer) message.mode = 'steer';
    if (!sid) {
      creating = true;
      try {
        const s = await zen.create({ ...(modelPinned ? { model } : {}), ...(effort ? { effort } : {}), ...(agent !== 'zenbot' ? { agent } : {}) });
        M.handOver({ id: s.id, text, message });
        input = '';
        attachments = attachments.filter((a) => !sent.includes(a.id));
        sessionStarted(s.id);
      } catch (e) {
        notice(`couldn't start the session: ${(e as Error).message ?? e}`);
      } finally {
        creating = false;
      }
      return;
    }
    if (!conn || !connected) {
      notice('not connected to zenbot; reconnecting… (your prompt stays here)');
      retry(0);
      return;
    }
    const wasBusy = busy;
    if (!wasBusy) {
      push({ k: 'user', text, kernel: false });
      pending.push(text);
      beginTurn();
      aborting = false;
      resetStream();
    } else notice(steer ? 'steering: zenbot reads it at the next tool boundary' : 'queued: zenbot reads it when this turn ends', 3000);
    if (!title) title = text.slice(0, 40).trim();
    input = '';
    attachments = attachments.filter((a) => !sent.includes(a.id));
    suggestion = null;
    pinned = true;
    unseen = 0;
    if (!conn.send(message)) {
      if (!wasBusy) {
        busy = false;
        pending.pop();
      }
      notice("couldn't send to zenbot; press ↑ to get the prompt back and send it again");
    }
    await bottom();
  }

  // ---- commands (input.rs `command`) ----

  function resolve(word: string): string | null {
    const names = cmds.map((c) => c.name);
    if (names.includes(word)) return word;
    const m = names.filter((n) => n.startsWith(word));
    return m.length === 1 ? m[0] : null;
  }

  const KERNEL = new Set(['jobs', 'skills', 'memory', 'mcp', 'models', 'suggestions', 'status', 'upgrade']);

  async function command(text: string) {
    const sp = text.indexOf(' ');
    const word = (sp < 0 ? text : text.slice(0, sp)).slice(1);
    const arg = sp < 0 ? '' : text.slice(sp + 1).trim();
    if (!cmds.length) {
      try {
        cmds = (await commands.list()).commands;
      } catch {}
    }
    const name = resolve(word);
    const plugged = cmds.find((c) => c.name === name && c.plugin && c.tool);
    if (plugged) return arg || !plugged.choices?.length ? await pluginCommand(plugged, arg) : choicePicker(plugged);
    const needSession = () => {
      if (sid) return false;
      notice('send a message first: this works on the session');
      return true;
    };
    try {
      switch (name) {
        case 'board':
        case 'exit':
          return toBoard();
        case 'new': {
          const as = arg.startsWith('@') ? arg.slice(1).split(/\s+/)[0].toLowerCase() : 'zenbot';
          if (!AGENT_NAME.test(as)) return notice('usage: /new [@agent] [title]');
          if (!sid) agent = as;
          return openSession('new', as);
        }
        case 'agents':
          return await agentsCommand(arg);
        case 'resume':
          return await sessionPicker();
        case 'model':
          return arg ? await setModel(arg) : modelPicker();
        case 'effort':
          return arg ? await setEffort(arg) : effortPicker();
        case 'rename':
          if (!arg) return notice('usage: /rename <title>');
          if (needSession()) return;
          await zen.update(sid!, { title: arg });
          title = arg;
          return notice('renamed');
        case 'archive': {
          if (!sid) return notice('nothing to archive yet');
          const [o, ...note] = arg.split(' ');
          if (!o) return void (picker = { title: 'How did it end?', kind: 'outcome', sel: 0, items: M.OUTCOMES.map(([v, h]) => ({ label: `${v.padEnd(9)} ${h}`, value: v })) });
          if (!M.OUTCOMES.some(([v]) => v === o)) return notice('usage: /archive [done|partial|not-done [note]]');
          return await archive(o, note.join(' ').trim());
        }
        case 'steer':
          return notice('usage: /steer <message>');
        case 'files':
          if (!arg) return pickTab('files');
          return await locateAnd(arg, true, true);
        case 'open':
          if (arg) return await locateAnd(arg, true);
          if (lastFile) return await locateAnd(lastFile, true);
          return notice('no file yet: /open <path>');
        case 'close':
          if (!split) return notice('no side panel is open');
          app.view = null;
          return;
        case 'mouse':
          return notice('the mouse always works here');
        case 'tasks':
          return pickTab('tasks');
        case 'restart':
          return location.reload();
        case 'help':
          return help();
        case 'mcp':
          if (arg.startsWith('login')) return await mcpLogin(arg.split(/\s+/)[1]);
          return await kernel('mcp', arg);
        case 'upgrade':
          if (!arg) return await upgrade();
          return await kernel('upgrade', arg);
        case null:
          return notice(`unknown command ${text}; try /help`);
        default:
          if (KERNEL.has(name)) return await kernel(name, arg);
          return notice(`unknown command ${text}; try /help`);
      }
    } catch (e) {
      notice(String((e as Error).message ?? e));
    }
  }

  function help() {
    push({ k: 'help', rows: cmds.map((c) => [`/${c.name}`, `${c.help}${c.args ? `: /${c.name} ${c.args}` : ''}`]), keys: M.KEYS });
  }

  /** A kernel command's answer, in the transcript (never sent to the agent). */
  async function kernel(name: string, arg: string) {
    const head = `/${name}${arg ? ' ' + arg : ''}`;
    push({ k: 'mono', text: '…', title: head });
    const e = entries[entries.length - 1] as Extract<M.Entry, { k: 'mono' }>;
    try {
      const r = await commands.run(name, arg ? arg.split(/\s+/) : []);
      e.text = M.clean(r.output);
    } catch (err) {
      e.text = String((err as Error).message ?? err);
    }
    await bottom(false);
  }

  // The shared command table changes when plugins are installed or switched on/off.
  $effect(() => {
    void plugins.revision;
    let gone = false;
    commands.list().then((c) => { if (!gone) cmds = c.commands; }).catch(() => {});
    return () => { gone = true; };
  });

  /** A plugin's command: calls its tool; its UI opens in the right panel, or its text shows here. */
  async function pluginCommand(c: Command, arg: string) {
    const head = `/${c.name}${arg ? ' ' + arg : ''}`;
    push({ k: 'mono', text: '…', title: head });
    const e = entries[entries.length - 1] as Extract<M.Entry, { k: 'mono' }>;
    const args = toolArgs(c.args, arg);
    try {
      const r = await pluginApi.call(c.plugin!, c.tool!, args, sid);
      const ui = r.isError ? null : (c.ui ?? uiOf(r));
      const t = plugins.list.find((p) => p.name === c.plugin)?.title || c.plugin!;
      if (ui) {
        showPanel(c.plugin!, ui, t, args, r);
        e.text = `opened in the side panel (${t})`;
      } else e.text = M.clean(resultText(r) || (r.isError ? 'failed' : '(nothing)'));
    } catch (err) {
      e.text = String((err as Error).message ?? err);
    }
    await bottom(false);
  }

  async function mcpLogin(server: string | undefined) {
    if (!server) return notice('usage: /mcp [login <server>]');
    const r = (await zen.mcpLogin(server)) as { flow: string; authorize_url: string; auth_server?: string; redirect?: string };
    mcpFlow = { server, flow: r.flow };
    line(`Sign-in to ${server}${r.auth_server ? ` at ${r.auth_server}` : ''}:`, 'plain');
    push({ k: 'link', text: r.authorize_url, url: r.authorize_url });
    line(`Open the link and approve. Your browser then fails to load ${r.redirect ?? '127.0.0.1'}: that's expected. Copy that page's whole address and paste it here as your next message (or type a /command to cancel).`, 'plain');
  }

  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

  /** `/upgrade` (client.rs `upgrade`): update the kernel, follow its log, wait for the restart. */
  async function upgrade() {
    if (upgrading) return notice('an upgrade is already running');
    upgrading = true;
    line('checking for updates…');
    try {
      const v = await zen.version(true);
      if (v.error) throw new Error(`could not check for updates: ${v.error}`);
      if (!v.available) return line(`zenbot ${v.running} is up to date`, 'accent');
      line(`zenbot update available: ${v.running} → ${v.latest} (${v.behind} new commit${v.behind === 1 ? '' : 's'})`, 'warn');
      for (const c of v.commits ?? []) line(`  · ${c}`);
      const target = v.latest ?? '';
      const started = (await zen.upgrade()).started_at ?? '';
      let shownLines = 0;
      for (;;) {
        await sleep(1000);
        let st;
        try {
          st = await zen.upgradeState();
        } catch {
          break;
        }
        const log = (st.job?.log ?? '').split('\n').filter((l, i, a) => i < a.length - 1 || l);
        for (const l of log.slice(shownLines)) line(M.clean(l));
        shownLines = log.length;
        const s = st.job?.status;
        if (s === 'running') continue;
        if (s === 'scheduled' || s === undefined) break;
        throw new Error('upgrade failed; nothing was changed');
      }
      line('restarting zenbot as soon as no session is working…');
      const deadline = Date.now() + 30 * 60_000;
      while (Date.now() < deadline && alive) {
        await sleep(2000);
        let h;
        try {
          h = await zen.health();
        } catch {
          continue;
        }
        if (target && h.ok && (h.commit ?? '').startsWith(target)) {
          line(`zenbot upgraded to ${target}`, 'accent');
          if (sid) {
            conn?.close();
            conn = null;
            await catchUp().catch(() => {});
            connect();
          }
          return;
        }
        try {
          const st = await zen.upgradeState();
          const lr = st.last_result ?? '';
          if (lr.slice(0, 20) >= started && (lr.includes('FAILED') || lr.includes('rolled back'))) throw new Error(`upgrade rolled back: ${lr} (details in ~/.zenbot/upgrade.log)`);
        } catch (e) {
          if (String(e).includes('rolled back')) throw e;
        }
      }
      throw new Error("zenbot hasn't restarted after 30 minutes (a session may still be working); check ~/.zenbot/upgrade.log");
    } catch (e) {
      line(String((e as Error).message ?? e), 'err');
    } finally {
      upgrading = false;
    }
  }

  /** `/agents` opens the Agents view beside the chat; `new <name> [description]` makes one there. */
  async function agentsCommand(arg: string) {
    const [sub, name, ...rest] = arg.split(/\s+/).filter(Boolean);
    if (!sub || sub === 'list') return openTab('agents');
    if (sub === 'new' && name) {
      const a = await zen.newAgent(name.toLowerCase(), rest.join(' '));
      notice(`made @${a.name}: its files are in the Agents view`);
      return openTab('agents');
    }
    if (sub === 'rm' && name) {
      notice('remove an agent in the Agents view: it asks first');
      return openTab('agents');
    }
    notice('usage: /agents [new <name> [description] | rm <name>]');
  }

  // ---- pickers (pickers.rs) ----

  async function sessionPicker() {
    const list = await zen.sessions();
    const items = list.map((s) => ({ label: `${s.updated_at.slice(5, 16).replace('T', ' ')}  ${M.clean(s.title || '(untitled)')}`, value: s.id }));
    if (!items.length) return notice('no other sessions');
    picker = { title: 'Resume a session', kind: 'session', sel: 0, items };
  }

  function modelPicker() {
    const ids = models.list.map((m) => m.id);
    if (!ids.length) return notice('no models yet');
    picker = { title: 'Choose a model', kind: 'model', sel: Math.max(0, ids.indexOf(model)), items: ids.map((m) => ({ label: `${m}${m === model ? '  (current)' : ''}`, value: m })) };
  }

  function effortPicker() {
    if (!levels.length) return notice(`${model} has no thinking levels to choose from`);
    const cur = (v: string | null) => (effort === v ? '  (current)' : '');
    const def = info.def ? ` (${info.def})` : '';
    const items = [{ label: `default${def}${cur(null)}`, value: 'default' }, ...levels.map((l) => ({ label: `${l}${cur(l)}`, value: l }))];
    picker = { title: 'Choose a thinking level', kind: 'effort', sel: effort ? levels.indexOf(effort) + 1 : 0, items };
  }

  /** A plugin command typed with no argument and with declared choices: pick the first argument. */
  function choicePicker(c: Command) {
    const w = Math.max(...c.choices!.map((x) => x.value.length));
    picker = {
      title: c.description || `/${c.name}`,
      kind: 'plugin-choice',
      sel: 0,
      command: c,
      items: c.choices!.map((x) => ({ label: `${x.value.padEnd(w)}  ${x.description}${x.then ? `  ${x.then}` : ''}`, value: x.value, then: x.then })),
    };
  }

  async function pick(i = picker?.sel ?? 0) {
    const p = picker;
    picker = null;
    const v = p?.items[i]?.value;
    if (!p || !v) return;
    try {
      if (p.kind === 'plugin-choice') {
        // A choice that needs more input goes to the composer for the owner to finish; the rest runs.
        if (p.items[i].then) await setInput(`/${p.command!.name} ${v} `);
        else await pluginCommand(p.command!, v);
        ta?.focus();
        return;
      }
      if (p.kind === 'session') openSession(v);
      else if (p.kind === 'model') await setModel(v);
      else if (p.kind === 'effort') await setEffort(v);
      else if (p.kind === 'agent') agent = v;
      else await archive(v, '');
    } catch (e) {
      notice(String((e as Error).message ?? e));
    }
    ta?.focus();
  }

  async function setModel(value: string) {
    if (models.list.length && !models.list.some((m) => m.id === value)) {
      const m = models.list.filter((x) => x.id.endsWith('/' + value) || x.id.includes(value));
      if (m.length !== 1) return notice(`unknown model ${value}; /model lists them`);
      value = m[0].id;
    }
    const lv = modelOf(value).efforts;
    const reset = !!effort && !lv.includes(effort);
    if (sid) {
      const s = await zen.update(sid, { model: value });
      model = s.model;
      effort = s.effort;
    } else {
      model = value;
      if (reset) effort = null;
    }
    modelPinned = true;
  }

  async function setEffort(value: string) {
    if (value !== 'default' && levels.length && !levels.includes(value)) return notice(`${model} takes: default, ${levels.join(', ')}`);
    if (sid) {
      const s = await zen.update(sid, { effort: value });
      effort = s.effort;
    } else effort = value === 'default' ? null : value;
  }

  /** Archive the session with how it ended (`-`: unsaid); after partial, open the session that continues it. Undo brings it back. */
  async function archive(o: string, note: string) {
    if (!sid) return notice('nothing to archive yet');
    const out = o === '-' ? undefined : (o as Outcome);
    const s = await zen.update(sid, { archived: true, ...(out ? { outcome: out } : {}), ...(note ? { note } : {}) });
    const me_ = sid;
    const undo = () => { void zen.update(me_, { archived: false }).then(() => openSession(me_)).catch((e) => notice(String((e as Error).message ?? e))); };
    const label = OUTCOMES.find((x) => x.v === o)?.l.toLowerCase();
    if (o === 'partial') {
      openSession(s.continued_by ?? 'new');
      notice('Archived as part done · a new chat continues it', 6000, undo);
    } else {
      closeChat();
      notice(o === '-' ? 'Archived' : `Archived as ${label}`, 6000, undo);
    }
  }
  async function archiveChat() {
    details = false;
    outcomeOpen = false;
    const o = outcome;
    outcome = 'done';
    try {
      await archive(o, '');
    } catch (e) {
      notice(String((e as Error).message ?? e));
    }
  }

  // ---- rename ----
  function startRename() {
    if (!sid) return;
    nameDraft = title || '';
    renaming = true;
    details = false;
  }
  async function saveRename() {
    if (!renaming) return;
    renaming = false;
    const t = nameDraft.trim();
    if (!t || t === title || !sid) return;
    try {
      await zen.update(sid, { title: t });
      title = t;
    } catch (e) {
      notice(String((e as Error).message ?? e));
    }
  }

  // ---- links ----

  async function locateAnd(ref: string, loud: boolean, dirOnly = false) {
    try {
      const r = await files.locate(ref);
      if (!r.found) return loud && notice(`can't find ${ref}`);
      if (r.found.dir) openDir(r.found.path);
      else if (dirOnly) notice(`${ref} is a file`);
      else openFile(r.found.path);
    } catch (e) {
      if (loud) notice(String((e as Error).message ?? e));
    }
  }

  function onClick(e: MouseEvent) {
    const t = e.target as HTMLElement;
    const a = t.closest<HTMLElement>('a[data-wiki], a[data-href]');
    if (a) {
      e.preventDefault();
      locateAnd(a.dataset.wiki !== undefined ? `[[${a.dataset.wiki}]]` : (a.dataset.href ?? ''), true);
      return;
    }
    const code = t.closest('code');
    if (code && !code.closest('pre') && code.closest('.prose') && !getSelection()?.toString()) {
      const ref = (code.textContent ?? '').trim();
      if (ref && ref.length < 300 && !/\s/.test(ref)) locateAnd(ref, /[/~]/.test(ref));
    }
  }

  function markdown(node: HTMLElement, text: string) {
    const draw = (t: string) => node.replaceChildren(mdRender(t, { keepFront: true }).el);
    draw(text);
    return { update: draw };
  }

  /** An option of a question card: one question answers at once; several fill the box, one line each. */
  function answer(qs: { question: string }[], qi: number, option: string) {
    if (qs.length <= 1) {
      input = option;
      void submit();
      return;
    }
    const rows = input.split('\n').filter((l) => l.trim());
    const tag = `${qi + 1}.`;
    const at = rows.findIndex((l) => l.startsWith(tag + ' '));
    const row = `${tag} ${option}`;
    if (at >= 0) rows[at] = row;
    else rows.push(row);
    input = rows.sort((x, y) => parseInt(x) - parseInt(y)).join('\n');
    ta?.focus();
  }
  /** A question card's answer: the prompt sent after it, if any. */
  function answerOf(i: number): string | null {
    for (let j = i + 1; j < entries.length; j++) {
      const e = entries[j];
      if (e.k === 'user' && !e.kernel) return e.text;
    }
    return null;
  }

  // ---- scrolling ----

  function atBottom() {
    return !scroller || scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 40;
  }

  function onScroll() {
    pinned = atBottom();
    if (pinned) unseen = 0;
  }

  async function bottom(force = true) {
    await tick();
    if (!scroller || (!force && !pinned)) return;
    scroller.scrollTop = scroller.scrollHeight;
    pinned = true;
    unseen = 0;
  }

  // ---- keys ----

  function caretLine(where: 'first' | 'last') {
    if (!ta) return true;
    const v = ta.value;
    if (where === 'first') return !v.slice(0, ta.selectionStart).includes('\n');
    return !v.slice(ta.selectionEnd).includes('\n');
  }

  async function setInput(v: string) {
    input = v;
    await tick();
    ta?.focus();
    ta?.setSelectionRange(v.length, v.length);
  }

  function key(e: KeyboardEvent) {
    const inTa = e.target === ta;
    if (e.defaultPrevented || e.isComposing) return;
    // Keys belong to the chat from its box, or from anywhere that isn't another field or the right panel.
    const t = e.target as HTMLElement | null;
    const mine = inTa || !t || t === document.body || !!root?.contains(t);
    if (!mine || (!inTa && typing(e))) return;
    const ctrl = e.ctrlKey && !e.altKey && !e.metaKey;
    const plain = !e.ctrlKey && !e.altKey && !e.metaKey && !e.shiftKey;

    if (picker && plain) {
      const n = picker.items.length;
      if (e.key === 'ArrowUp') picker.sel = (picker.sel - 1 + n) % n;
      else if (e.key === 'ArrowDown') picker.sel = (picker.sel + 1) % n;
      else if (e.key === 'Enter') pick();
      else if (e.key === 'Escape') picker = null;
      else return;
      e.preventDefault();
      return;
    }
    if (ctrl && e.key.toLowerCase() === 'o') {
      e.preventDefault();
      expandAll = !expandAll;
      open = {};
      return;
    }
    if ((e.key === 'PageUp' || e.key === 'PageDown') && !e.altKey && scroller) {
      e.preventDefault();
      scroller.scrollBy({ top: (e.key === 'PageUp' ? -1 : 1) * Math.max(40, scroller.clientHeight - 60) });
      return;
    }
    if (ctrl && e.key.toLowerCase() === 'c') {
      const sel = getSelection()?.toString() || (ta && ta.selectionStart !== ta.selectionEnd);
      if (busy && !ro && !sel) {
        e.preventDefault();
        abort();
      }
      return;
    }
    if (e.key === 'Escape' && plain) {
      if (details || pop || outcomeOpen) { details = false; pop = null; outcomeOpen = false; return; }
      e.preventDefault();
      if (busy && !ro) abort();
      else if (input.startsWith('/')) input = '';
      return;
    }
    if (!inTa) {
      // Typing anywhere in the chat goes to the composer.
      if (!ro && ta && ((e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) || (e.key === 'Enter' && plain))) ta.focus();
      return;
    }
    // The / menu: ↑↓ move, Tab completes, Enter runs (or starts a command that takes text).
    if (menu.length && plain && hi < 0) {
      const sel = Math.min(menuSel, menu.length - 1);
      if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
        e.preventDefault();
        menuSel = (sel + (e.key === 'ArrowUp' ? menu.length - 1 : 1)) % menu.length;
        return;
      }
      if (e.key === 'Tab') {
        e.preventDefault();
        setInput(`/${menu[sel].name}`);
        menuSel = 0;
        return;
      }
      if (e.key === 'Enter') {
        e.preventDefault();
        runMenu(sel);
        return;
      }
    }
    if (e.key === 'Tab' && plain && !input && suggestion && !busy) {
      e.preventDefault();
      takeSuggestion();
      return;
    }
    if (e.key === 'Enter') {
      if (e.shiftKey) return; // the textarea's own new line
      e.preventDefault();
      if (e.altKey || e.ctrlKey || e.metaKey) {
        const [a, b] = [ta!.selectionStart, ta!.selectionEnd];
        input = input.slice(0, a) + '\n' + input.slice(b);
        tick().then(() => ta?.setSelectionRange(a + 1, a + 1));
        return;
      }
      submit();
      return;
    }
    if (plain && e.key === 'ArrowUp' && caretLine('first')) {
      if (hi < 0) {
        hist = M.loadHistory();
        draft = input;
        hi = hist.length;
      }
      if (hi > 0) {
        e.preventDefault();
        hi--;
        setInput(hist[hi]);
      }
      return;
    }
    if (plain && e.key === 'ArrowDown' && hi >= 0 && caretLine('last')) {
      e.preventDefault();
      hi++;
      if (hi >= hist.length) {
        hi = -1;
        setInput(draft);
      } else setInput(hist[hi]);
    }
  }

  function runMenu(i: number) {
    const c = menu[i];
    if (!c) return;
    menuSel = 0;
    // A command that takes text waits for it; one with declared choices runs, to open its picker.
    if (c.args.startsWith('<') && !c.choices?.length) setInput(`/${c.name} `);
    else {
      input = `/${c.name}`;
      submit();
    }
  }

  function takeSuggestion() {
    if (!suggestion) return;
    suggestion.taken = true;
    setInput(suggestion.text);
  }

  function onInput() {
    hi = -1;
    menuSel = 0;
  }

  /** Who a new chat talks to: the ideas follow. */
  function chooseTalk(a: string) {
    agent = a;
    ta?.focus();
  }

  // The clock while a turn runs.
  $effect(() => {
    if (!busy) return;
    const t = setInterval(() => (now = Date.now()), 500);
    return () => clearInterval(t);
  });
  $effect(() => {
    app.working = busy;
  });

  // Keep the latest in view while it grows (streaming, math loading), unless scrolled up.
  $effect(() => {
    if (!content || !scroller) return;
    const obs = new ResizeObserver(() => {
      if (pinned && scroller) scroller.scrollTop = scroller.scrollHeight;
    });
    obs.observe(content);
    return () => obs.disconnect();
  });

  $effect(() => {
    if (loaded && matchMedia('(pointer: fine)').matches) ta?.focus();
  });

  // A new chat's ideas: they load after the page and the box show; changing agent drops an answer in flight.
  $effect(() => {
    if (sid) return;
    const a = agent;
    const ctl = new AbortController();
    sugg = null;
    suggFailed = false;
    loadIdeas(a, ctl.signal).then((l) => { if (!ctl.signal.aborted) sugg = l; }, () => { if (!ctl.signal.aborted) suggFailed = true; });
    return () => ctl.abort();
  });
  // The agent picked at the top changes who a new chat talks to.
  $effect(() => {
    const a = app.agent;
    if (!sid && !entries.length) agent = a;
  });
  // Files' "Ask zenbot" beside the chat.
  let insertSeen = app.chatInsert.n;
  $effect(() => {
    const ins = app.chatInsert;
    if (ins.n === insertSeen) return;
    insertSeen = ins.n;
    void setInput(ins.text);
  });

  onMount(() => {
    if (id === 'new') { input = app.chatDraft; app.chatDraft = ''; }
    if (app.attachOnOpen) { app.attachOnOpen = false; setTimeout(() => filePick?.click(), 0); }
    if (sid) load();
    else loaded = true;
    const online = () => !conn && sid && retry(0);
    addEventListener('online', online);
    // Untrusted iframe messages never enter submit() without an owner's Send click.
    const queue = (plugin: string, text: string) => {
      if (ro || pluginDrafts.length >= 20) return false;
      pluginDrafts = [...pluginDrafts, { plugin, text }];
      return true;
    };
    chat.queue = queue;
    const focusBox = () => { if (!ro) ta?.focus(); };
    chat.focus = focusBox;
    return () => {
      if (chat.queue === queue) chat.queue = null;
      if (chat.focus === focusBox) chat.focus = null;
      alive = false;
      gen++;
      conn?.close();
      clearTimeout(retryTimer);
      if (streamTimer) clearTimeout(streamTimer);
      removeEventListener('online', online);
      app.working = false;
    };
  });

  const hour = new Date().getHours();
  const greeting = hour < 12 ? 'Good morning' : hour < 18 ? 'Good afternoon' : 'Good evening';
  const groups = $derived([...new Set(models.list.map((m) => m.group))]);
  const curModel = $derived(model || me.defaultModel || models.default);
  /** A folded run's line: its latest step, in plain words. */
  const summary = (steps: M.Step[]) => { const s = stepOf(steps[steps.length - 1].name, steps[steps.length - 1].args); return `${s.label}${s.detail ? ` · ${s.detail}` : ''}`; };
  /** Runs of the current turn show in the live line, not in the flow. */
  const inTurn = (i: number) => busy && i > turnAt;
</script>

<svelte:window onkeydown={key} />

{#snippet ico(name: 'clip' | 'stop' | 'split')}
  <svg class="ico" viewBox="0 0 24 24" aria-hidden="true">
    {#if name === 'clip'}<path d="M20 11.5l-8 8a5 5 0 0 1-7-7l8.5-8.5a3.3 3.3 0 0 1 4.7 4.7L9.7 17.2a1.7 1.7 0 0 1-2.4-2.4l7.7-7.7"/>
    {:else if name === 'stop'}<rect x="6" y="6" width="12" height="12" rx="2"/>
    {:else}<rect x="3" y="5" width="18" height="14" rx="3"/><path d="M12 5v14"/>{/if}
  </svg>
{/snippet}

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="chatroot" bind:this={root}
  ondragover={(e) => { if (!ro && e.dataTransfer?.types.includes('Files')) { e.preventDefault(); dragging = true; } }}
  ondragleave={(e) => { if (!root?.contains(e.relatedTarget as Node)) dragging = false; }}
  ondrop={onDrop}>
  <header class="hd">
    {#if app.narrow}<button class="icon back" title="Back" aria-label="Back" onclick={goBack}>‹</button>{/if}
    {#if renaming}
      <!-- svelte-ignore a11y_autofocus -->
      <input class="rename" bind:value={nameDraft} autofocus aria-label="Chat name"
        onkeydown={(e) => { if (e.key === 'Enter') saveRename(); if (e.key === 'Escape') { e.stopPropagation(); renaming = false; } }} onblur={saveRename} />
    {:else}
      <h1 ondblclick={() => sid && startRename()} title={sid ? 'Double-click to rename' : undefined}>{!sid ? 'New chat' : title || 'New chat'}</h1>
    {/if}
    {#if shown === 'working'}<span class="tag work"><span class="dot work"></span>working</span>
    {:else if shown === 'waiting'}<span class="tag wait"><span class="dot wait"></span>needs you</span>{/if}
    {#if ro}<span class="tag ro">read-only</span>{/if}
    <span class="sp"></span>
    {#if sid}
      <div class="menu-wrap">
        <button class="icon" title="Details" aria-label="Details" aria-expanded={details} onclick={() => (details = !details)}>⋯</button>
        {#if details}
          <div class="menu right">
            <div class="row"><span>Model</span><b>{model.split('/').pop() || '…'}</b></div>
            <div class="row"><span>Effort</span><b>{shownEffort ?? '—'}</b></div>
            <div class="row"><span>Context</span><b>{M.fmtTokens(sessionTokens)} tokens</b></div>
            <div class="row"><span>Session</span><b>{short(sid)}</b></div>
            {#if who.id !== 'zenbot'}<div class="row"><span>Agent</span><b>@{who.id}</b></div>{/if}
            <hr />
            <button onclick={startRename}>Rename</button>
            <button onclick={() => { details = false; help(); }}>Commands and keys</button>
            <div class="archrow">
              <span>Archive as</span>
              <button class="oc" aria-haspopup="listbox" aria-expanded={outcomeOpen} onclick={() => (outcomeOpen = !outcomeOpen)}>{OUTCOMES.find((x) => x.v === outcome)?.l} <span class="car">▾</span></button>
              <button class="archgo" onclick={archiveChat}>Archive</button>
            </div>
            {#if outcomeOpen}
              <div class="ocs" role="listbox">
                {#each OUTCOMES as o}
                  <button class="mopt" role="option" aria-selected={o.v === outcome} class:sel={o.v === outcome} onclick={() => { outcome = o.v; outcomeOpen = false; }}><b>{o.l}</b><span>{o.h}</span></button>
                {/each}
              </div>
            {/if}
          </div>
        {/if}
      </div>
    {/if}
    {#if !app.narrow}<button class="icon close" title={closeTitle} aria-label={closeTitle} onclick={() => closeChat()}>×</button>{/if}
  </header>

  <!-- Clicks on links and paths in the messages (each has a keyboard way: /open). -->
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
  <div class="scroll" bind:this={scroller} onscroll={onScroll} onclick={onClick} role="log">
    {#if !sid && !entries.length}
      <div class="hello">
        <h2>{greeting}{me.name ? `, ${me.name}` : ''}</h2>
        <p>Who do you want to talk to?</p>
        <div class="who" role="radiogroup" aria-label="Agent">
          {#each agents.list as a (a.id)}
            <button class="pick" class:sel={a.id === agent} role="radio" aria-checked={a.id === agent} onclick={() => chooseTalk(a.id)}>
              <span class="orb" style="--a:{a.a}; --b:{a.b}">{a.name[0].toUpperCase()}</span>
              <span class="pn"><b>{a.name}</b><span>{a.role}</span></span>
            </button>
          {/each}
        </div>
        {#if !suggFailed && (!sugg || sugg.length)}
          <div class="sgh">
            Ideas for you
            {#if !sugg}<span class="think" aria-live="polite"><i></i><i></i><i></i> {who.name} is thinking of a few</span>{/if}
          </div>
          <div class="cards">
            {#if sugg}
              {#each sugg as g, i (g.title)}
                <button class="idea" style="animation-delay:{i * 70}ms" onclick={() => setInput(g.prompt)}>
                  <b>{g.title}</b>{#if g.why}<span>{g.why}</span>{/if}
                </button>
              {/each}
            {:else}
              {#each [0, 1, 2] as i (i)}<div class="idea skel" aria-hidden="true"><i></i><i></i></div>{/each}
            {/if}
          </div>
        {/if}
      </div>
    {:else}
      <div class="body" bind:this={content}>
        {#if earlier > 0}
          <button class="earlier" onclick={showEarlier}>Show {Math.min(PAGE, earlier)} earlier · {earlier} not shown</button>
        {/if}
        {#if failed}<div class="note err">Couldn't load the session: {failed}</div>{/if}
        {#each entries as e, i (e.key)}
          {#if e.k === 'user'}
            {#if e.kernel}
              <div class="note kline">zen › {e.text}</div>
            {:else}
              <div class="u"><div class="bubble">{e.text}</div></div>
            {/if}
          {:else if e.k === 'md'}
            <div class="prose" use:markdown={e.text}></div>
          {:else if e.k === 'work'}
            {#if !inTurn(i)}
              {@const bad = e.steps.some((s) => s.result?.error)}
              {@const isOpen = open[e.key] ?? expandAll}
              <div class="w">
                <button class="wline" aria-expanded={isOpen} onclick={() => { if (!getSelection()?.toString()) open[e.key] = !isOpen; }}>
                  <span class="chk" class:bad>{bad ? '!' : '✓'}</span>
                  <span class="ws">{summary(e.steps)}</span><span class="n">· {e.steps.length} step{e.steps.length === 1 ? '' : 's'}</span><span class="car">{isOpen ? '▾' : '▸'}</span>
                </button>
                {#if isOpen}
                  <ol class="steps">
                    {#each e.steps as s, j (j)}
                      {@const p = stepOf(s.name, s.args)}
                      {@const k = `${e.key}:${j}`}
                      <li class:bad={s.result?.error}>
                        <button class="sline" onclick={() => (outOpen[k] = !outOpen[k])} title={s.result ? 'Show what it returned' : undefined}><b>{p.label}</b> <span>{p.detail}</span>{#if s.result?.error} · didn't work{/if}</button>
                        {#if outOpen[k] && s.result}<pre class="out">{s.result.lines.join('\n') || '(no output)'}</pre>{/if}
                      </li>
                    {/each}
                  </ol>
                {/if}
              </div>
            {/if}
          {:else if e.k === 'result'}
            <pre class="out" class:bad={e.result.error}>{e.result.lines.slice(0, 3).join('\n')}{e.result.lines.length > 3 ? `\n… ${e.result.lines.length - 3} more lines` : ''}</pre>
          {:else if e.k === 'line'}
            <div class="note {e.sty}">{e.text}</div>
          {:else if e.k === 'mono'}
            <div class="mono">{#if e.title}<div class="mt">{e.title}</div>{/if}<pre>{e.text}</pre></div>
          {:else if e.k === 'link'}
            <div class="note"><a href={e.url} target="_blank" rel="noopener noreferrer">{e.text}</a></div>
          {:else if e.k === 'questions'}
            {@const ans = answerOf(i)}
            {#if ans !== null}
              <div class="askq done">
                <span class="q">{e.questions.map((q) => q.question).join(' · ')}</span>
                <span class="ans">✓ {ans.split('\n')[0]}</span>
              </div>
            {:else}
              <div class="askq">
                {#each e.questions as q, qi (qi)}
                  <div class="q">{e.questions.length > 1 ? `${qi + 1}. ` : ''}{q.question}</div>
                  <div class="opts">{#each q.options as o, j (j)}<button class:primary={j === 0} disabled={ro} onclick={() => answer(e.questions, qi, o)}>{o}</button>{/each}</div>
                {/each}
                <div class="or">or type your own answer below</div>
              </div>
            {/if}
          {:else if e.k === 'help'}
            <div class="help">
              <b>Commands</b>
              {#each e.rows as [k, h] (k)}<div class="hrow"><button class="acc" onclick={() => setInput(k + ' ')}>{k}</button><span>{h}</span></div>{/each}
              <b>Keys</b>
              {#each e.keys as [k, h] (k)}<div class="hrow"><span class="acc">{k}</span><span>{h}</span></div>{/each}
            </div>
          {/if}
        {/each}
        {#if streamShown}<div class="prose" use:markdown={streamShown}></div>{/if}
        {#if busy}
          {@const cur = liveSteps[liveSteps.length - 1]}
          <div class="livew" aria-live="polite">
            <button class="lrow lhead" aria-expanded={liveOpen} onclick={() => (liveOpen = !liveOpen)} title={liveOpen ? 'Hide the steps' : 'See every step and subagent'}>
              <span class="orb xs busy" style="--a:{who.a}; --b:{who.b}"></span>
              {#if cur && (toolSince !== null || status === 'Working')}
                <span class="lt"><b>{cur.label}</b>{" "}<span class="ld">{cur.detail}</span>…</span>
              {:else}
                <span class="lt"><b>{status}</b>…</span>
              {/if}
              <span class="lm">{liveSteps.length} step{liveSteps.length === 1 ? '' : 's'}{#if subs.length}<span class="wide">{" · "}{subs.length} subagent{subs.length === 1 ? '' : 's'}</span>{/if} · {clock(now - turnStarted)} {liveOpen ? '▾' : '▸'}</span>
            </button>
            {#if liveOpen}
              <div class="lbody">
                <ol class="slist">
                  {#each liveSteps as st, j (j)}
                    {@const isNow = j === liveSteps.length - 1 && toolSince !== null}
                    <li class:bad={st.failed && !isNow} class:now={isNow}><span class="g">{#if isNow}<i class="pulse"></i>{:else}{st.failed ? '!' : '✓'}{/if}</span><span class="lt">{st.label}{" "}<span class="ld">{st.detail}{#if st.failed && !isNow}{" · didn’t work"}{/if}</span></span><span class="st">{st.t}</span></li>
                  {/each}
                  {#if !liveSteps.length}<li class="now"><span class="g"><i class="pulse"></i></span><span class="lt">{status}</span></li>{/if}
                </ol>
                {#each subs as h (h.id)}
                  {@const k = kids[h.id]}
                  {@const done = k ? k.done : !h.busy}
                  {@const last = k?.steps[k.steps.length - 1]}
                  <div class="sarow" class:on={app.view === 'subagent' && app.subagent === h.id && (split || app.screen === 'view')}>
                    <button class="lrow sa" class:done aria-expanded={!!hOpen[h.id]} onclick={() => (hOpen[h.id] = !hOpen[h.id])}>
                      {#if done}<span class="g">✓</span>{:else}<span class="orb xs busy" style="--a:{who.a}; --b:{who.b}"></span>{/if}
                      <span class="lt"><b>{subName(h)}</b>{" "}<span class="ld">{done ? (k?.result?.split('\n')[0] ?? 'Done') : last ? `${last.label} ${last.detail}…` : (h.task ?? '')}</span></span>
                      <span class="lm"><span class="role">{h.role ?? h.kind ?? 'subagent'}</span><span class="wide">{" · "}{k?.steps.length ?? 0} steps</span> {hOpen[h.id] ? '▾' : '▸'}</span>
                    </button>
                    <button class="sopen" onclick={() => openSub(h.id)} title="Open this subagent's session beside the chat" aria-label="Open {subName(h)}'s session">{@render ico('split')}</button>
                  </div>
                  {#if hOpen[h.id] && k}
                    <ol class="slist sub">{#each k.steps as s, q (q)}<li class:now={!done && q === k.steps.length - 1}><span class="g">{#if !done && q === k.steps.length - 1}<i class="pulse"></i>{:else}✓{/if}</span><span class="lt">{s.label}{" "}<span class="ld">{s.detail}</span></span><span class="st">{s.t}</span></li>{/each}</ol>
                  {/if}
                {/each}
              </div>
            {/if}
          </div>
        {/if}
      </div>
    {/if}
  </div>

  <footer class="dock">
    {#if !pinned && !picker && !(menu.length && hi < 0) && entries.length}
      <button class="jump" onclick={() => bottom()}>↓ {unseen ? `${unseen} more · jump` : 'Jump to the latest'}</button>
    {/if}
    {#if !connected && sid}
      <div class="conn">Connection lost · reconnecting… <button onclick={() => retry(0)}>Retry now</button></div>
    {/if}
    {#each pluginDrafts as d (d)}
      <section class="pdraft" aria-label="plugin draft">
        <div class="pk">Draft from {d.plugin}</div>
        <pre>{d.text}</pre>
        <div class="pb"><button class="archgo" onclick={() => sendPluginDraft(d)} disabled={ro || creating || uploading || !!mcpFlow || !!ready.length}>Send plugin draft</button>
          <button class="oc" onclick={() => (pluginDrafts = pluginDrafts.filter((x) => x !== d))}>Dismiss</button></div>
      </section>
    {/each}
    <div class="compose" class:drag={dragging} class:ro>
      {#if picker}
        <div class="menu up pop" role="listbox" aria-label={picker.title}>
          <div class="mg">{picker.title} · ↑↓ enter · esc</div>
          {#each picker.items as it, i (i)}
            <button class="mopt item" class:on={i === picker.sel} role="option" aria-selected={i === picker.sel} onclick={() => pick(i)} onpointermove={(e) => { if (picker && (e.movementX || e.movementY)) picker.sel = i; }}>{it.label}</button>
          {/each}
        </div>
      {:else if menu.length && hi < 0}
        <div class="menu up pop" role="listbox" aria-label="commands">
          {#each menu as c, i (c.name)}
            <button class="mopt item cmd" class:on={i === Math.min(menuSel, menu.length - 1)} onclick={() => runMenu(i)} onmouseenter={() => (menuSel = i)}>
              <b>/{c.name}{#if c.args}<em>{c.args}</em>{/if}</b><span>{c.help}</span>
            </button>
          {/each}
        </div>
      {/if}
      {#if attachments.length || pluginContext.length}
        <div class="atts" aria-label="attachments">
          {#each attachments as a (a.id)}
            <span class="att" class:bad={a.error} title={a.path ? `Saved to ${tilde(a.path)}` : a.error ?? 'sending…'}>
              <button class="atn" onclick={() => a.path && showFile(a.path)}>{@render ico('clip')}{a.name}</button>
              <span class="ap">{a.error ? a.error : a.path ? `${sizeText(a.size)} ✓` : `${Math.round(a.progress * 100)}%`}</span>
              <button aria-label="Remove {a.name}" title="Remove" onclick={() => unattach(a.id)}>×</button>
            </span>
          {/each}
          {#each pluginContext as [name, c] (name)}
            <span class="att ctx" title={c.text} aria-label={`context from ${name}`}>
              {#each c.images as im (im.name)}
                <img class="cimg" class:bad={im.error} src={im.thumb} alt={`image from ${name}`} title={im.error ?? (im.path ? tilde(im.path) : 'saving…')} />
              {/each}
              <span class="atn">{name}: {c.text.split('\n')[0] || `${c.images.length} image${c.images.length === 1 ? '' : 's'}`}</span>
              {#if c.images.some((im) => !im.path && !im.error)}<span class="ap">saving…</span>{/if}
              <button onclick={() => dropContext(sid ?? 'new', name)} aria-label={`remove context from ${name}`} title="Don't send this">×</button>
            </span>
          {/each}
        </div>
      {/if}
      <textarea rows="1" {placeholder} bind:value={input} bind:this={ta} oninput={onInput} onpaste={onPaste} disabled={ro || creating} aria-label="message" spellcheck="true"></textarea>
      <div class="bar">
        {#if !ro}
          <input bind:this={filePick} type="file" multiple hidden onchange={(e) => { attach((e.currentTarget as HTMLInputElement).files); (e.currentTarget as HTMLInputElement).value = ''; }} />
          <button class="icon" title="Attach a file (or paste, or drop it here)" aria-label="attach files" onclick={() => filePick?.click()}>{@render ico('clip')}</button>
        {/if}
        <div class="menu-wrap">
          <button class="mode" class:open={pop === 'model'} aria-haspopup="menu" aria-expanded={pop === 'model'} title="Model" disabled={ro}
            onclick={() => (pop = pop === 'model' ? null : 'model')}>{info.name || '…'} <span class="car">▾</span></button>
          {#if pop === 'model'}
            <div class="menu up mmenu" role="menu">
              {#each groups as g (g)}
                <div class="mg">{g}</div>
                {#each models.list.filter((m) => m.group === g && (!m.more || moreModels || m.id === curModel)) as m (m.id)}
                  <button class="mopt" role="menuitemradio" aria-checked={m.id === curModel} class:sel={m.id === curModel} onclick={() => { pop = null; void setModel(m.id).catch((e) => notice(String(e.message ?? e))); }}>
                    <b>{m.name}{#if m.id === models.default}<em>default</em>{/if}</b>{#if m.hint}<span>{m.hint}</span>{/if}
                  </button>
                {/each}
              {/each}
              {#if !moreModels && models.list.some((m) => m.more)}<button class="mopt more" onclick={() => (moreModels = true)}>More models…</button>{/if}
            </div>
          {/if}
        </div>
        {#if levels.length && shownEffort}
          <div class="menu-wrap">
            <button class="mode" class:open={pop === 'effort'} aria-haspopup="menu" aria-expanded={pop === 'effort'} title="Effort: how hard it thinks" disabled={ro}
              onclick={() => (pop = pop === 'effort' ? null : 'effort')}>
              <span class="bars" aria-hidden="true">{#each [0, 1, 2] as i (i)}<i class:on={levels.indexOf(shownEffort) >= i * 2}></i>{/each}</span>
              {effortName(shownEffort)} <span class="car">▾</span></button>
            {#if pop === 'effort'}
              <div class="menu up eff" role="menu">
                <div class="mg">Effort · how hard it thinks</div>
                <button class="mopt" role="menuitemradio" aria-checked={effort === null} class:sel={effort === null} onclick={() => { pop = null; void setEffort('default'); }}>
                  <b>Default<em>{effortName(info.def)}</em></b><span>What {info.name} uses unless told</span>
                </button>
                {#each levels as l (l)}
                  <button class="mopt" role="menuitemradio" aria-checked={effort === l} class:sel={effort === l} onclick={() => { pop = null; void setEffort(l); }}>
                    <b>{effortName(l)}</b><span>{EFFORT[l]?.[1] ?? ''}</span>
                  </button>
                {/each}
              </div>
            {/if}
          </div>
        {/if}
        <span class="sp"></span>
        {#if suggestion && !input && !busy && !ro}<button class="hint" onclick={takeSuggestion}>Tab: use suggestion</button>{/if}
        {#if busy && !input.trim() && !ro}<button class="send stop" title="Stop (esc)" aria-label="interrupt" onclick={abort}>{@render ico('stop')}</button>
        {:else if !ro}<button class="send" disabled={(!input.trim() && !ready.length) || uploading || creating} title="Send (enter)" aria-label="send" onclick={submit}>↑</button>{/if}
      </div>
    </div>
  </footer>
</div>

<style>
  .chatroot { flex: 1; min-height: 0; display: flex; flex-direction: column; position: relative; }
  .ico { width: 18px; height: 18px; fill: none; stroke: currentColor; stroke-width: 1.6; stroke-linecap: round; stroke-linejoin: round; flex: none; }
  .sp { flex: 1; }
  .icon { width: 32px; height: 32px; border-radius: 9px; display: grid; place-items: center; color: var(--dim); font-size: 16px; }
  .icon:hover { background: var(--soft); color: var(--ink); }
  .orb { width: 38px; height: 38px; border-radius: 50%; display: grid; place-items: center; flex: none; font-weight: 700; font-size: 16px; color: #fbfaf6;
    background: radial-gradient(circle at 32% 28%, var(--b), var(--a) 70%); box-shadow: 0 0 0 3px color-mix(in srgb, var(--a) 18%, transparent); }
  .orb.xs { width: 14px; height: 14px; box-shadow: none; }
  .orb.busy { animation: breathe 1.8s ease-in-out infinite; }
  @keyframes breathe { 50% { box-shadow: 0 0 0 7px color-mix(in srgb, var(--a) 8%, transparent); transform: scale(0.96); } }

  /* chat */
  .hd { display: flex; align-items: center; gap: 10px; padding: 14px 18px 10px 22px; }
  .hd h1 { font-size: 16px; font-weight: 650; margin: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .scroll { flex: 1; overflow: auto; overflow-x: hidden; }
  .body { max-width: 44rem; margin: 0 auto; padding: 14px 26px 12px; display: flex; flex-direction: column; gap: 18px; }
  .u { display: flex; justify-content: flex-end; }
  .bubble { max-width: 82%; background: var(--soft); padding: 10px 15px; border-radius: 18px 18px 5px 18px; line-height: 1.5; white-space: pre-wrap; overflow-wrap: anywhere; }
  .prose { line-height: 1.65; overflow-wrap: anywhere; }
  .prose :global(p) { margin: 0 0 10px; }
  .prose :global(p:last-child) { margin-bottom: 0; }
  .prose :global(.md > :first-child) { margin-top: 0; }
  .prose :global(.md > :last-child) { margin-bottom: 0; }
  .prose :global(ul), .prose :global(ol), .prose :global(blockquote), .prose :global(table), .prose :global(pre) { margin: 0.5em 0; }
  .prose :global(ul), .prose :global(ol) { padding-left: 1.4em; }
  .prose :global(h1), .prose :global(h2), .prose :global(h3), .prose :global(h4) { font-size: 1.05em; margin: 0.9em 0 0.4em; font-weight: 650; }
  .prose :global(h1) { font-size: 1.2em; }
  .prose :global(code) { font-family: var(--mono); font-size: 0.88em; background: var(--soft); padding: 0.05em 0.3em; border-radius: 4px; cursor: pointer; }
  .prose :global(pre) { background: var(--soft); padding: 8px 10px; border-radius: 8px; overflow-x: auto; }
  .prose :global(pre code) { background: none; padding: 0; cursor: text; }
  .prose :global(table) { border-collapse: collapse; display: block; overflow-x: auto; font-size: 0.93em; }
  .prose :global(th), .prose :global(td) { border: 1px solid var(--faint); padding: 3px 9px; text-align: left; }
  .prose :global(blockquote) { border-left: 3px solid var(--faint); padding-left: 10px; color: var(--dim); }
  .prose :global(a) { color: var(--accent); text-decoration: none; cursor: pointer; }
  .prose :global(a:hover) { text-decoration: underline; }
  .prose :global(hr) { border: 0; border-top: 1px solid var(--faint); }
  .prose :global(img) { max-width: 100%; }
  .prose :global(.md-display) { overflow-x: auto; }
  .livew { border: 1px solid var(--faint); border-radius: 14px; background: color-mix(in srgb, var(--soft) 40%, var(--paper)); padding: 4px; }
  .lrow { display: flex; align-items: center; gap: 10px; width: 100%; text-align: left; padding: 7px 10px; border-radius: 10px; font-size: 14px; }
  .lrow:hover { background: var(--soft); }
  .lt { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .lt b { font-weight: 550; }
  .ld { color: var(--dim); }
  .lm { font-size: 12px; color: var(--dim); white-space: nowrap; font-variant-numeric: tabular-nums; }
  @media (max-width: 900px) { .lm .wide { display: none; } }
  .role { display: inline-block; padding: 1px 7px; border-radius: 999px; background: var(--soft); color: var(--ink); font-size: 11.5px; }
  .sarow { display: flex; align-items: center; border-radius: 10px; }
  .sarow .sa { flex: 1; min-width: 0; }
  .sarow.on { background: var(--soft); }
  .sopen { flex: none; width: 30px; height: 30px; display: grid; place-items: center; border-radius: 8px; color: var(--dim); }
  .sopen:hover, .sarow.on .sopen { color: var(--accent); background: var(--soft); }
  .sopen .ico { width: 16px; height: 16px; }
  .sa { font-size: 13.5px; }
  .sa.done { opacity: 0.7; }
  .g { width: 14px; flex: none; text-align: center; font-size: 11px; font-weight: 700; color: var(--accent); }
  .lbody { border-top: 1px solid var(--faint); margin: 2px 6px 0; padding-top: 4px; }
  .pulse { display: inline-block; width: 7px; height: 7px; border-radius: 50%; background: var(--accent); animation: pulse 1.4s ease-in-out infinite; }
  @keyframes pulse { 50% { opacity: 0.3; transform: scale(0.75); } }
  .slist li.now { font-weight: 550; }
  .slist { list-style: none; margin: 4px 0; padding: 0 10px; }
  .slist li { display: flex; align-items: center; gap: 10px; padding: 3px 0; font-size: 13px; animation: rise 0.25s both; }
  .slist li.bad .g, .slist li.bad .ld { color: var(--err); }
  .slist.sub { padding-left: 34px; }
  .st { font-size: 11.5px; color: var(--dim); font-variant-numeric: tabular-nums; }
  .w { min-width: 0; }
  .wline { text-align: left; max-width: 100%; display: inline-flex; align-items: center; gap: 8px; font-size: 13px; color: var(--dim); padding: 5px 11px 5px 6px; border-radius: 99px; border: 1px solid var(--faint); }
  .wline:hover { color: var(--ink); border-color: var(--dim); }
  .ws { min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .n { white-space: nowrap; }
  .chk { width: 18px; height: 18px; border-radius: 50%; display: grid; place-items: center; font-size: 11px; background: color-mix(in srgb, var(--ok) 16%, transparent); color: var(--ok); flex: none; }
  .chk.bad { background: color-mix(in srgb, var(--warn) 18%, transparent); color: var(--warn); }
  .car { font-size: 10px; opacity: 0.7; }
  .steps { margin: 8px 0 0; padding: 0 0 0 26px; font-size: 13px; color: var(--dim); display: flex; flex-direction: column; gap: 4px; }
  .steps b { color: var(--ink); font-weight: 500; }
  .steps .bad, .steps .bad b { color: var(--err); }
  .sline { text-align: left; overflow-wrap: anywhere; color: inherit; }
  .sline:hover b { text-decoration: underline; }
  .out { font-family: var(--mono); font-size: 12px; white-space: pre-wrap; overflow-wrap: anywhere; color: var(--dim); background: var(--soft); border-radius: 8px; padding: 6px 9px; margin: 4px 0 2px; max-height: 18rem; overflow: auto; }
  .out.bad { color: var(--err); }
  .askq { border: 1px solid color-mix(in srgb, var(--warn) 45%, var(--faint)); background: color-mix(in srgb, var(--warn) 6%, var(--paper)); border-radius: 16px; padding: 16px; display: flex; flex-direction: column; gap: 12px; }
  .q { font-weight: 600; }
  .opts { display: flex; flex-wrap: wrap; gap: 8px; }
  .opts button { padding: 8px 14px; border-radius: 99px; border: 1px solid var(--faint); background: var(--paper); font-size: 14px; text-align: left; }
  .opts button:hover { border-color: var(--dim); }
  .opts .primary { background: var(--accent); border-color: var(--accent); color: var(--on-accent); }
  .askq.done { flex-direction: row; flex-wrap: wrap; align-items: baseline; gap: 4px 12px; padding: 10px 14px; background: transparent; border-color: var(--faint); }
  .askq.done .q { font-weight: 500; color: var(--dim); flex: 1; min-width: 12rem; }
  .ans { font-size: 14px; font-weight: 600; color: var(--accent); overflow-wrap: anywhere; }
  .or { font-size: 12px; color: var(--dim); }
  .hello { max-width: 44rem; margin: 10vh auto 0; padding: 0 24px; text-align: center; }
  .hello h2 { font-family: var(--serif); font-size: 30px; font-weight: 600; margin: 0 0 6px; }
  .hello p { color: var(--dim); margin: 0 0 24px; }
  .who { display: flex; flex-wrap: wrap; gap: 10px; justify-content: center; margin-bottom: 34px; }
  .pick { display: flex; align-items: center; gap: 10px; padding: 8px 16px 8px 8px; border-radius: 99px; border: 1px solid var(--faint); text-align: left; transition: border-color 0.15s, background 0.15s; }
  .pick:hover { border-color: var(--dim); }
  .pick.sel { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 9%, var(--paper)); }
  .pick .orb { width: 32px; height: 32px; font-size: 14px; }
  .pn { display: flex; flex-direction: column; line-height: 1.2; }
  .pn b { font-size: 14px; font-weight: 600; }
  .pn span { font-size: 12px; color: var(--dim); }
  .sgh { display: flex; align-items: center; justify-content: center; gap: 10px; font-size: 12px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); font-weight: 650; margin-bottom: 12px; min-height: 18px; }
  .think { text-transform: none; letter-spacing: 0; font-weight: 400; display: inline-flex; align-items: center; gap: 3px; }
  .think i { width: 4px; height: 4px; border-radius: 50%; background: var(--accent); animation: blink 1.2s infinite; }
  .think i:nth-child(2) { animation-delay: 0.2s; } .think i:nth-child(3) { animation-delay: 0.4s; margin-right: 4px; }
  @keyframes blink { 50% { opacity: 0.2; } }
  .cards { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10px; text-align: left; }
  .idea { display: flex; flex-direction: column; gap: 6px; padding: 14px; min-height: 92px; border-radius: 14px; border: 1px solid var(--faint); background: color-mix(in srgb, var(--soft) 45%, var(--paper)); text-align: left; animation: rise 0.3s both; }
  button.idea:hover { border-color: var(--accent); }
  .idea b { font-size: 14px; font-weight: 600; line-height: 1.3; }
  .idea span { font-size: 12.5px; color: var(--dim); line-height: 1.35; }
  @keyframes rise { from { opacity: 0; transform: translateY(6px); } }
  .idea.skel i { display: block; height: 11px; border-radius: 6px; background: linear-gradient(90deg, var(--soft), var(--faint), var(--soft)); background-size: 200% 100%; animation: shimmer 1.4s infinite linear; }
  .idea.skel i:first-child { width: 80%; height: 13px; } .idea.skel i:last-child { width: 55%; margin-top: 4px; }
  @keyframes shimmer { to { background-position: -200% 0; } }
  @media (max-width: 900px) { .cards { grid-template-columns: minmax(0, 1fr); } .idea { min-height: 0; } .hello { margin-top: 4vh; } }
  .dock { padding: 6px 16px 16px; position: relative; }
  .compose { max-width: 44rem; margin: 0 auto; border: 1px solid var(--faint); border-radius: 20px; background: var(--paper); padding: 10px 10px 8px 16px; box-shadow: 0 4px 16px var(--shadow); position: relative; }
  .compose:focus-within { border-color: var(--accent); }
  .compose.drag { border-color: var(--accent); border-style: dashed; }
  .compose.ro { opacity: 0.7; }
  .compose textarea { width: 100%; border: 0; outline: none; background: transparent; resize: none; font: inherit; color: inherit; padding: 4px 0 8px; field-sizing: content; max-height: 40vh; }
  .compose textarea::placeholder { color: var(--dim); }
  .bar { display: flex; align-items: center; gap: 4px; margin-left: -8px; }
  .mode { font-size: 13px; color: var(--dim); padding: 5px 10px; border-radius: 99px; }
  .mode:hover { background: var(--soft); color: var(--ink); }
  .send { width: 34px; height: 34px; border-radius: 50%; background: var(--accent); color: var(--on-accent); font-size: 17px; font-weight: 700; display: grid; place-items: center; flex: none; }
  .send:disabled { background: var(--soft); color: var(--dim); cursor: default; }
  .send.stop { background: var(--soft); color: var(--ink); border: 1px solid var(--faint); }
  .send.stop .ico { width: 14px; height: 14px; }
  .hint { font-size: 12px; color: var(--dim); padding: 4px 8px; border-radius: 99px; }
  .hint:hover { color: var(--ink); background: var(--soft); }

  /* menus */
  .menu-wrap { position: relative; }
  .menu { position: absolute; z-index: 12; min-width: 240px; background: var(--paper); border: 1px solid var(--faint); border-radius: 14px; padding: 6px; box-shadow: 0 14px 40px var(--shadow); display: flex; flex-direction: column; }
  .menu.right { right: 0; top: 38px; }
  .menu.up { bottom: 40px; left: 0; }
  .rename { flex: 0 1 26rem; min-width: 8rem; font: inherit; font-size: 16px; font-weight: 650; color: inherit; background: var(--paper); border: 1px solid var(--accent); border-radius: 8px; padding: 3px 8px; outline: none; }
  .hd .close { font-size: 20px; }
  .archrow { display: flex; align-items: center; gap: 6px; padding: 4px 4px 4px 10px; font-size: 14px; }
  .archrow > span { flex: 1; }
  .oc { padding: 4px 10px; border-radius: 8px; border: 1px solid var(--faint); font-size: 13px; background: var(--paper); }
  .oc:hover { border-color: var(--dim); }
  .archgo { padding: 4px 12px; border-radius: 8px; font-size: 13px; font-weight: 600; color: var(--on-accent); background: var(--accent); }
  .archgo:disabled { opacity: 0.5; }
  .ocs { display: flex; flex-direction: column; padding: 4px 0 2px; }
  .ocs .mopt { text-align: left; padding: 6px 10px 6px 18px; border-radius: 8px; }
  .ocs .mopt:hover { background: var(--soft); }
  .menu.right { min-width: 280px; }
  .menu .row { display: flex; justify-content: space-between; gap: 16px; padding: 6px 10px; font-size: 13px; color: var(--dim); }
  .menu .row b { color: var(--ink); font-weight: 500; font-family: var(--mono); font-size: 12px; }
  .menu hr { border: 0; border-top: 1px solid var(--faint); margin: 6px 0; width: 100%; }
  .menu > button { text-align: left; padding: 7px 10px; border-radius: 8px; font-size: 14px; }
  .menu > button:hover { background: var(--soft); }
  .mopt { display: flex; flex-direction: column; align-items: flex-start; gap: 1px; }
  .mopt span { font-size: 12px; color: var(--dim); }
  .mopt.sel b { color: var(--accent); }
  .mopt b { display: flex; align-items: center; gap: 6px; }
  .mopt em { font-style: normal; font-size: 10.5px; font-weight: 600; color: var(--dim); border: 1px solid var(--faint); border-radius: 99px; padding: 0 6px; }
  .mopt.more { font-size: 13px; color: var(--accent); font-weight: 600; }
  .mg { font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--dim); font-weight: 650; padding: 8px 12px 4px; white-space: nowrap; }
  .menu.up { min-width: 250px; }
  @media (max-width: 900px) { .menu.up.eff { left: auto; right: -60px; } }
  .mmenu { max-height: min(60vh, 460px); overflow: auto; }
  .mode { display: inline-flex; align-items: center; gap: 6px; white-space: nowrap; }
  .mode.open { background: var(--soft); color: var(--ink); }
  .bars { display: inline-flex; align-items: flex-end; gap: 2px; height: 11px; }
  .bars i { width: 3px; border-radius: 1px; background: var(--faint); }
  .bars i:nth-child(1) { height: 5px; } .bars i:nth-child(2) { height: 8px; } .bars i:nth-child(3) { height: 11px; }
  .bars i.on { background: var(--accent); }
  /* the / menu and the command pickers, over the message box */
  .menu.pop { left: 0; right: 0; bottom: calc(100% + 6px); max-height: 40vh; overflow: auto; }
  .pop .item { padding: 6px 10px; border-radius: 8px; white-space: pre; overflow: hidden; text-overflow: ellipsis; font-size: 13px; max-width: 100%; }
  .pop .item.cmd { white-space: normal; }
  .pop .item.on { background: var(--soft); }
  .pop .item b { font-family: var(--mono); font-weight: 550; font-size: 12.5px; }
  .pop .item em { border: 0; padding: 0; font-weight: 400; }

  .tag { display: inline-flex; align-items: center; gap: 5px; font-size: 11px; font-weight: 600; padding: 2px 9px 2px 7px; border-radius: 99px; white-space: nowrap; }
  .tag.work { color: var(--accent); background: color-mix(in srgb, var(--accent) 14%, transparent); }
  .tag.wait { color: var(--warn); background: color-mix(in srgb, var(--warn) 16%, transparent); }
  .tag.ro { color: var(--dim); background: var(--soft); }
  .dot { width: 6px; height: 6px; border-radius: 50%; flex: none; }
  .dot.work { background: var(--accent); animation: blink 1.2s ease-in-out infinite; }
  .dot.wait { background: var(--warn); }

  .atts { display: flex; flex-wrap: wrap; gap: 6px; padding: 0 0 8px; }
  .att { display: inline-flex; align-items: center; gap: 5px; font-size: 12.5px; padding: 3px 4px 3px 9px; border-radius: 99px; background: var(--soft); border: 1px solid var(--faint); max-width: 100%; }
  .att.bad { border-color: var(--err); color: var(--err); }
  .att.ctx { border-style: dashed; }
  .att .ico { width: 13px; height: 13px; }
  .att .atn { display: inline-flex; align-items: center; gap: 5px; width: auto; height: auto; border-radius: 0; color: inherit; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 220px; }
  .att .atn:hover { background: none; text-decoration: underline; }
  .att .ap { color: var(--dim); font-size: 11.5px; white-space: nowrap; }
  .att button:not(.atn) { width: 18px; height: 18px; border-radius: 50%; color: var(--dim); flex: none; }
  .att button:not(.atn):hover { background: var(--faint); color: var(--ink); }
  .cimg { width: 20px; height: 20px; object-fit: cover; border-radius: 4px; border: 1px solid var(--faint); }

  .note { font-size: 13px; color: var(--dim); white-space: pre-wrap; overflow-wrap: anywhere; }
  .note.err { color: var(--err); }
  .note.warn { color: var(--warn); }
  .note.accent { color: var(--accent); }
  .note.plain { color: var(--ink); }
  .note a { color: var(--accent); word-break: break-all; }
  .kline { font-size: 12.5px; }
  .mono { font-size: 12px; }
  .mono .mt { color: var(--dim); margin-bottom: 2px; font-family: var(--mono); }
  .mono pre { font-family: var(--mono); margin: 0; padding: 8px 10px; border-radius: 10px; background: var(--soft); white-space: pre-wrap; overflow-wrap: anywhere; }
  .help { font-size: 13px; display: flex; flex-direction: column; gap: 2px; }
  .hrow { display: grid; grid-template-columns: 10em 1fr; gap: 10px; }
  .hrow > :first-child { text-align: left; font-family: var(--mono); font-size: 12px; }
  .acc { color: var(--accent); }
  .earlier { align-self: center; color: var(--accent); font-size: 13px; }
  .jump { position: absolute; left: 50%; transform: translateX(-50%); bottom: calc(100% + 4px); z-index: 5; font-size: 12px; padding: 4px 12px; border-radius: 99px; background: var(--paper); border: 1px solid var(--faint); color: var(--accent); box-shadow: 0 2px 10px var(--shadow); }
  .conn { max-width: 44rem; margin: 0 auto 6px; font-size: 13px; color: var(--warn); }
  .conn button { text-decoration: underline dotted; }
  .pdraft { max-width: 44rem; margin: 0 auto 8px; border: 1px dashed var(--faint); border-radius: 14px; padding: 10px 12px; font-size: 13px; }
  .pdraft .pk { font-size: 11.5px; color: var(--dim); text-transform: uppercase; letter-spacing: 0.04em; }
  .pdraft pre { white-space: pre-wrap; overflow-wrap: anywhere; font: inherit; margin: 6px 0; max-height: 10rem; overflow: auto; }
  .pdraft .pb { display: flex; gap: 6px; }

  @media (max-width: 900px) {
    .back { font-size: 26px; margin-left: -10px; color: var(--ink); }
    .body { padding: 10px 14px; }
    .dock { padding: 6px 8px 8px; }
    .hrow { grid-template-columns: 7.5em 1fr; }
  }
</style>
