<script lang="ts">
  // A session's chat, as the terminal's (crates/zen/src/tui: events.rs, transcript.rs, input.rs,
  // pickers.rs): the transcript (prompts, Markdown replies streamed as they come, tool runs folded
  // into one line), the composer with its / menu and pickers, the status line while zenbot works,
  // and the footer. 'new' is a chat whose session starts with its first prompt; that prompt is
  // handed to the chat re-created with the session's id (Shell keys Chat on the session).
  // Plugins: a plugin's slash command calls its tool and shows its UI in the side panel; a plugin
  // UI's `sendMessage` queues an owner-reviewed draft here (`chat.queue`), and what plugin UIs put in the model context goes
  // with the next prompt (plugins.svelte.ts).
  import { onMount, tick } from 'svelte';
  import { zen, files, commands, upload, toolSummary, short, readOnly, agentTag, AGENT_NAME, pluginApi, type Command, type Message, type Models, type SessionDetail, type ZenEvent } from './api.ts';
  import { chat, plugins, resultText, showPanel, takeContext, toolArgs, uiOf } from './plugins.svelte.ts';
  import { app, board, openSession, sessionStarted, openFile, openDir, toggleSide, settings, notice, typing, tilde } from './app.svelte.ts';
  import { render as mdRender } from './md.ts';
  import * as M from './chat/model.ts';
  import Work from './chat/Work.svelte';

  let { id, active, wide }: { id: string; active: boolean; wide: boolean } = $props();

  const SPINNER = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
  const PLACEHOLDER = 'Ask zenbot to do something…';
  const READ_ONLY = 'Read-only: zenbot drives this session';
  const PAGE = 40;

  // ---- the session ----
  // svelte-ignore state_referenced_locally
  const sid: string | null = id === 'new' ? null : id;
  let title = $state('');
  let model = $state('');
  let effort = $state<string | null>(null);
  let kind = $state<string | null>(null);
  /** Who the session is: a fresh chat starts as the agent it was opened with (the picker changes it). */
  // svelte-ignore state_referenced_locally
  let agent = $state(sid ? 'zenbot' : app.agent);
  /** The agents a fresh chat can start as (loaded only for a fresh chat). */
  let agents = $state<string[]>([]);
  let models = $state<Models | null>(null);
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

  // ---- the conversation ----
  let entries = $state<M.Entry[]>([]);
  let expandAll = $state(false);
  let stream = '';
  let streamShown = $state('');
  let streamTimer: ReturnType<typeof setTimeout> | null = null;
  let lastDraw = 0;
  /** Prompts sent here that the kernel hasn't echoed yet. */
  let pending: string[] = [];

  // ---- the turn ----
  let busy = $state(false);
  let status = $state('Working');
  let turnStarted = $state(Date.now());
  let toolSince = $state<number | null>(null);
  let turnTools = $state(0);
  let turnFiles = $state<string[]>([]);
  let turnTokens = 0;
  let turnModel = '';
  let turnEffort: string | null = null;
  let aborting = false;
  let sessionTokens = $state(0);
  let suggestion = $state<{ id: number; text: string; taken: boolean } | null>(null);
  let lastFile: string | null = null;
  let now = $state(Date.now());
  let spin = $state(0);

  // ---- the connection ----
  let conn: ReturnType<typeof zen.socket> | null = null;
  let gen = 0;
  let connected = $state(true);
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  let retries = 0;
  let alive = true;
  let upgrading = false;
  let mcpFlow: { server: string; flow: string } | null = null;

  // ---- the composer ----
  let input = $state('');
  let ta = $state<HTMLTextAreaElement>();
  let root = $state<HTMLElement>();
  let scroller = $state<HTMLElement>();
  let content = $state<HTMLElement>();
  let menuSel = $state(0);
  let creating = $state(false);
  type Pick = { title: string; items: { label: string; value: string }[]; sel: number; kind: 'session' | 'model' | 'effort' | 'decision' | 'agent' };
  let picker = $state<Pick | null>(null);
  let hist: string[] = [];
  let hi = $state(-1);
  let draft = '';
  let pinned = $state(true);
  let unseen = $state(0);

  // ---- attachments: sent to ~/inbox at once, named in the prompt on send ----
  type Attachment = { id: number; name: string; size: number; progress: number; path?: string; error?: string; cancel: () => void };
  let attachments = $state<Attachment[]>([]);
  let picker_ = $state<HTMLInputElement>();
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
  const info = $derived(models?.models.find((m) => m.id === model));
  const levels = $derived(info?.efforts ?? []);
  const shownEffort = $derived(levels.length ? (effort ?? info?.default_effort ?? null) : null);
  const earlier = $derived(Math.max(0, total - held));
  const placeholder = $derived(ro ? READ_ONLY : suggestion && !busy && !input ? suggestion.text : PLACEHOLDER);

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
      if (d.busy) beginTurn();
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
    turnTools = 0;
    turnFiles = [];
    toolSince = null;
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
        status = `Running ${M.clean(toolSummary(e.name, e.args))}`;
        toolSince = Date.now();
        turnTools++;
        const path = typeof e.args?.path === 'string' ? e.args.path : null;
        const b = base(e.name);
        if (path && (b === 'write' || b === 'edit') && !turnFiles.includes(path)) turnFiles = [...turnFiles, path];
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
    const secs = ((Date.now() - turnStarted) / 1000).toFixed(1);
    const name = turnModel || model.split('/').pop() || '';
    line(`${name}${turnEffort ? ` · ${turnEffort}` : ''} · ${M.fmtTokens(turnTokens)} tokens · ${secs}s`);
    sessionTokens += turnTokens;
    turnEffort = null;
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

  let pluginDrafts = $state<{ plugin: string; text: string }[]>([]);

  function sendPluginDraft(draft: { plugin: string; text: string }) {
    if (ro || creating || uploading || mcpFlow || ready.length) return;
    const ownDraft = input;
    input = `(from the ${draft.plugin} plugin) ${draft.text}`;
    void submit().finally(() => { if (!input) input = ownDraft; });
    pluginDrafts = pluginDrafts.filter((d) => d !== draft);
  }

  async function submit() {
    const raw = input.trim();
    if ((!raw && !ready.length) || creating) return;
    if (uploading) return notice('still sending the attachments; send when they show ✓');
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
    const viewing = app.side.open && app.side.view === 'files' ? app.file.side : null;
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
    if (plugged) return await pluginCommand(plugged, arg);
    const needSession = () => {
      if (sid) return false;
      notice('send a message first: this works on the session');
      return true;
    };
    try {
      switch (name) {
        case 'board':
        case 'exit':
          return board();
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
        case 'done': {
          if (needSession()) return;
          const [d, ...note] = arg.split(' ');
          if (!d) return void (picker = { title: 'How did it go?', kind: 'decision', sel: 0, items: M.DECISIONS.map(([v, h]) => ({ label: `${v.padEnd(8)} ${h}`, value: v })) });
          if (!M.DECISIONS.some(([v]) => v === d)) return notice('usage: /done accept|more|reshape|drop [note]');
          return await decide(d, note.join(' ').trim());
        }
        case 'rename':
          if (!arg) return notice('usage: /rename <title>');
          if (needSession()) return;
          await zen.update(sid!, { title: arg });
          title = arg;
          return notice('renamed');
        case 'archive':
          if (!sid) return notice('nothing to archive yet');
          await zen.update(sid, { archived: true });
          notice(`archived ${short(sid)}`);
          return openSession('new');
        case 'steer':
          return notice('usage: /steer <message>');
        case 'files':
          if (!arg) return toggleSide('files');
          return await locateAnd(arg, true, true);
        case 'open':
          if (arg) return await locateAnd(arg, true);
          if (lastFile) return await locateAnd(lastFile, true);
          return notice('no file yet: /open <path>');
        case 'close':
          if (!app.side.open) return notice('no side panel is open');
          app.side.open = false;
          app.focus = 'chat';
          return;
        case 'mouse':
          return notice('the mouse always works here');
        case 'tasks':
          return toggleSide('tasks');
        case 'restart':
          return location.reload();
        case 'help':
          return push({ k: 'help', rows: cmds.map((c) => [`/${c.name}`, `${c.help}${c.args ? `: /${c.name} ${c.args}` : ''}`]), keys: M.KEYS });
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

  /** A plugin's command: calls its tool; its UI opens in the side panel, or its text shows here. */
  async function pluginCommand(c: Command, arg: string) {
    const head = `/${c.name}${arg ? ' ' + arg : ''}`;
    push({ k: 'mono', text: '…', title: head });
    const e = entries[entries.length - 1] as Extract<M.Entry, { k: 'mono' }>;
    const args = toolArgs(c.args, arg);
    try {
      const r = await pluginApi.call(c.plugin!, c.tool!, args, sid);
      const ui = r.isError ? null : (c.ui ?? uiOf(r));
      const title = plugins.list.find((p) => p.name === c.plugin)?.title || c.plugin!;
      if (ui) {
        showPanel(c.plugin!, ui, title, args, r);
        e.text = `opened in the side panel (${title})`;
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
      let shown = 0;
      for (;;) {
        await sleep(1000);
        let st;
        try {
          st = await zen.upgradeState();
        } catch {
          break;
        }
        const log = (st.job?.log ?? '').split('\n').filter((l, i, a) => i < a.length - 1 || l);
        for (const l of log.slice(shown)) line(M.clean(l));
        shown = log.length;
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
    if (!sub || sub === 'list') return toggleSide('agents');
    if (sub === 'new' && name) {
      const a = await zen.newAgent(name.toLowerCase(), rest.join(' '));
      notice(`made @${a.name}: its files are in the Agents view`);
      return toggleSide('agents');
    }
    if (sub === 'rm' && name) {
      notice('remove an agent in the Agents view: it asks first');
      return toggleSide('agents');
    }
    notice('usage: /agents [new <name> [description] | rm <name>]');
  }

  // ---- pickers (pickers.rs) ----

  /** Who a fresh chat's session will be. */
  async function agentPicker() {
    if (sid) return notice(`this session is @${agent}; /new @<agent> starts one with another`);
    const list = (await zen.agents()).filter((a) => !a.error);
    picker = {
      title: 'Start this session as',
      kind: 'agent',
      sel: Math.max(0, list.findIndex((a) => a.name === agent)),
      items: list.map((a) => ({ label: `@${a.name}${a.description ? `  ${a.description}` : ''}${a.name === agent ? '  (current)' : ''}`, value: a.name })),
    };
  }

  async function sessionPicker() {
    const list = await zen.sessions();
    const items = list.map((s) => ({ label: `${s.updated_at.slice(5, 16).replace('T', ' ')}  ${M.clean(s.title || '(untitled)')}`, value: s.id }));
    if (!items.length) return notice('no other sessions');
    picker = { title: 'Resume a session', kind: 'session', sel: 0, items };
  }

  function modelPicker() {
    const ids = models?.models.map((m) => m.id) ?? [];
    if (!ids.length) return notice('no models yet');
    picker = { title: 'Choose a model', kind: 'model', sel: Math.max(0, ids.indexOf(model)), items: ids.map((m) => ({ label: `${m}${m === model ? '  (current)' : ''}`, value: m })) };
  }

  function effortPicker() {
    if (!levels.length) return notice(`${model} has no thinking levels to choose from`);
    const cur = (v: string | null) => (effort === v ? '  (current)' : '');
    const def = info?.default_effort ? ` (${info.default_effort})` : '';
    const items = [{ label: `default${def}${cur(null)}`, value: 'default' }, ...levels.map((l) => ({ label: `${l}${cur(l)}`, value: l }))];
    picker = { title: 'Choose a thinking level', kind: 'effort', sel: effort ? levels.indexOf(effort) + 1 : 0, items };
  }

  async function pick(i = picker?.sel ?? 0) {
    const p = picker;
    picker = null;
    const v = p?.items[i]?.value;
    if (!p || !v) return;
    try {
      if (p.kind === 'session') openSession(v);
      else if (p.kind === 'model') await setModel(v);
      else if (p.kind === 'effort') await setEffort(v);
      else if (p.kind === 'agent') agent = v;
      else await decide(v, '');
    } catch (e) {
      notice(String((e as Error).message ?? e));
    }
    ta?.focus();
  }

  async function setModel(value: string) {
    if (models && !models.models.some((m) => m.id === value)) {
      const m = models.models.filter((x) => x.id.endsWith('/' + value) || x.id.includes(value));
      if (m.length !== 1) return notice(`unknown model ${value}; /model lists them`);
      value = m[0].id;
    }
    const lv = models?.models.find((m) => m.id === value)?.efforts ?? [];
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
    await tick();
    notice(`model: ${model}${shownEffort ? ` · effort: ${shownEffort}${reset ? ' (default for this model)' : ''}` : ''}`);
  }

  async function setEffort(value: string) {
    if (value !== 'default' && levels.length && !levels.includes(value)) return notice(`${model} takes: default, ${levels.join(', ')}`);
    if (sid) {
      const s = await zen.update(sid, { effort: value });
      effort = s.effort;
    } else effort = value === 'default' ? null : value;
    await tick();
    notice(`effort: ${shownEffort ?? value}`);
  }

  async function decide(d: string, note: string) {
    if (!sid) return notice('nothing to judge yet; send a message first');
    await zen.decide(sid, d as 'accept', note || undefined);
    notice(`recorded: ${d}`);
  }

  // ---- links ----

  async function locateAnd(ref: string, loud: boolean, dirOnly = false) {
    try {
      const r = await files.locate(ref);
      if (!r.found) return loud && notice(`can't find ${ref}`);
      if (r.found.dir) openDir(r.found.path, 'side');
      else if (dirOnly) notice(`${ref} is a file`);
      else openFile(r.found.path, 'side');
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
    if (code && !code.closest('pre') && code.closest('.zc-md') && !getSelection()?.toString()) {
      const ref = (code.textContent ?? '').trim();
      if (ref && ref.length < 300 && !/\s/.test(ref)) locateAnd(ref, /[/~]/.test(ref));
    }
  }

  function markdown(node: HTMLElement, text: string) {
    const draw = (t: string) => node.replaceChildren(mdRender(t, { keepFront: true }).el);
    draw(text);
    return { update: draw };
  }

  function answer(qs: { question: string }[], qi: number, option: string) {
    if (qs.length <= 1) input = option;
    else {
      const rows = input.split('\n').filter((l) => l.trim());
      const tag = `${qi + 1}.`;
      const at = rows.findIndex((l) => l.startsWith(tag + ' '));
      const row = `${tag} ${option}`;
      if (at >= 0) rows[at] = row;
      else rows.push(row);
      input = rows.sort((x, y) => parseInt(x) - parseInt(y)).join('\n');
    }
    ta?.focus();
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
    ta?.setSelectionRange(v.length, v.length);
  }

  function key(e: KeyboardEvent) {
    const inTa = e.target === ta;
    // Keys in the composer are the chat's even when the side panel had them.
    if (!(active || inTa) || e.defaultPrevented || e.isComposing) return;
    if (!inTa && typing(e)) return;
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
      for (const x of entries) if (x.k === 'work') x.open = null;
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
      e.preventDefault();
      if (picker) picker = null;
      else if (ro) board();
      else if (busy) abort();
      else if (input.startsWith('/')) input = '';
      else if (!input.trim()) board();
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
    if (c.args.startsWith('<')) setInput(`/${c.name} `);
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

  /** A click or focus in the chat gives it the keys back from the side panel. */
  function takeKeys() {
    if (app.focus !== 'chat') app.focus = 'chat';
  }

  function onInput() {
    hi = -1;
    menuSel = 0;
  }

  // Grow with the text, up to half the pane.
  $effect(() => {
    void input;
    if (!ta) return;
    ta.style.height = 'auto';
    const max = Math.max(60, (root?.clientHeight ?? 600) / 2);
    ta.style.height = `${Math.min(ta.scrollHeight, max)}px`;
  });

  // The spinner and the clocks while a turn runs.
  $effect(() => {
    if (!busy) return;
    const t = setInterval(() => {
      now = Date.now();
      spin++;
    }, 100);
    return () => clearInterval(t);
  });

  // Keep the latest in view while it grows (streaming, math loading), unless scrolled up.
  $effect(() => {
    if (!content || !scroller) return;
    const ro = new ResizeObserver(() => {
      if (pinned && scroller) scroller.scrollTop = scroller.scrollHeight;
    });
    ro.observe(content);
    return () => ro.disconnect();
  });

  $effect(() => {
    if (active && loaded && matchMedia('(pointer: fine)').matches) ta?.focus();
  });

  onMount(() => {
    zen.models().then((m) => {
      models = m;
      if (!sid && !model) model = m.default;
    }).catch(() => {});

    if (sid) load();
    else {
      loaded = true;
      zen.agents().then((l) => (agents = l.filter((a) => !a.error).map((a) => a.name))).catch(() => {});
    }
    const online = () => !conn && sid && retry(0);
    addEventListener('online', online);
    // Untrusted iframe messages never enter submit() without an owner's Send click.
    const queue = (plugin: string, text: string) => {
      if (ro || pluginDrafts.length >= 20) return false;
      pluginDrafts = [...pluginDrafts, { plugin, text }];
      return true;
    };
    chat.queue = queue;
    return () => {
      if (chat.queue === queue) chat.queue = null;
      alive = false;
      gen++;
      conn?.close();
      clearTimeout(retryTimer);
      if (streamTimer) clearTimeout(streamTimer);
      removeEventListener('online', online);
    };
  });

  const secs = (from: number) => Math.max(0, Math.floor((now - from) / 1000));
  const shortModel = (m: string) => m.split('/').pop() ?? m;
</script>

<svelte:window onkeydown={key} />

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="zc" bind:this={root} onpointerdown={takeKeys} onfocusin={takeKeys}
  ondragover={(e) => { if (!ro && e.dataTransfer?.types.includes('Files')) { e.preventDefault(); dragging = true; } }}
  ondragleave={(e) => { if (!root?.contains(e.relatedTarget as Node)) dragging = false; }}
  ondrop={onDrop}>
  <!-- Clicks on links and paths in the messages (each has a keyboard way: /open). -->
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
  <div class="zc-scroll" bind:this={scroller} onscroll={onScroll} onclick={onClick} role="log" aria-live="polite">
    <div class="zc-body" bind:this={content}>
      {#if earlier > 0}
        <button class="zc-earlier" onclick={showEarlier}>show {Math.min(PAGE, earlier)} earlier · {earlier} not shown</button>
      {/if}
      <div class="zc-title">── {title || (sid ? short(sid) : agent !== 'zenbot' ? `new session with ${agent}` : 'new session')} ──</div>
      {#if !sid && agents.length > 1 && !entries.some((e) => e.k === 'user')}
        <div class="zc-as" role="radiogroup" aria-label="start as">
          <span class="zc-dim">start as</span>
          {#each agents as a}
            <button role="radio" aria-checked={a === agent} class:zc-on={a === agent} onclick={() => (agent = a)}>@{a}</button>
          {/each}
        </div>
      {/if}
      {#if failed}<div class="zc-line zc-err">couldn't load the session: {failed}</div>{/if}
      {#each entries as e (e.key)}
        {#if e.k === 'user'}
          {#if e.kernel}
            <div class="zc-kernel">zen › {e.text}</div>
          {:else}
            <div class="zc-user"><span class="zc-acc">›</span> <span>{e.text}</span></div>
          {/if}
        {:else if e.k === 'md'}
          <div class="zc-md" use:markdown={e.text}></div>
        {:else if e.k === 'work'}
          <Work steps={e.steps} open={e.open ?? expandAll} ontoggle={() => (e.open = !(e.open ?? expandAll))} />
        {:else if e.k === 'result'}
          <div class="zc-result" class:zc-err={e.result.error}>{#each e.result.lines.slice(0, 3) as l, i}<div><span class="zc-dim">{i ? '  ' : '└ '}</span>{l}</div>{/each}{#if e.result.lines.length > 3}<div class="zc-dim">  … {e.result.lines.length - 3} more lines</div>{/if}</div>
        {:else if e.k === 'line'}
          <div class="zc-line zc-{e.sty}">{e.text}</div>
        {:else if e.k === 'mono'}
          <div class="zc-mono">{#if e.title}<div class="zc-dim">{e.title}</div>{/if}<pre>{e.text}</pre></div>
        {:else if e.k === 'link'}
          <div class="zc-line"><a href={e.url} target="_blank" rel="noopener noreferrer">{e.text}</a></div>
        {:else if e.k === 'questions'}
          <div class="zc-qs">
            <div class="zc-dim">── Questions ──</div>
            {#each e.questions as q, qi}
              <div class="zc-q">{qi + 1}. {q.question}</div>
              {#each q.options as o, oi}
                <button class="zc-opt" onclick={() => answer(e.questions, qi, o)} title="put this answer in the composer">
                  {String.fromCharCode(97 + oi)}) {o}{#if oi === 0}<span class="zc-dim zc-rec">(recommended)</span>{/if}
                </button>
              {/each}
            {/each}
          </div>
        {:else if e.k === 'help'}
          <div class="zc-help">
            <b>Commands</b>
            {#each e.rows as [k, h]}<div class="zc-hrow"><button class="zc-acc" onclick={() => setInput(k + ' ')}>{k}</button><span>{h}</span></div>{/each}
            <b>Keys</b>
            {#each e.keys as [k, h]}<div class="zc-hrow"><span class="zc-acc">{k}</span><span>{h}</span></div>{/each}
          </div>
        {/if}
      {/each}
      {#if streamShown}<div class="zc-md" use:markdown={streamShown}></div>{/if}
    </div>
  </div>

  <div class="zc-dock">
    {#if !pinned && !picker && !(menu.length && hi < 0)}
      <button class="zc-jump" onclick={() => bottom()}>↓ {unseen ? `${unseen} more · jump` : 'jump to the latest'}</button>
    {/if}
    {#if picker}
      <div class="zc-pop" role="listbox" aria-label={picker.title}>
        <div class="zc-dim zc-pophead">{picker.title} <span>· ↑↓ enter · esc</span></div>
        {#each picker.items as it, i}
          <button class="zc-item" class:zc-on={i === picker.sel} role="option" aria-selected={i === picker.sel} onclick={() => pick(i)} onmouseenter={() => picker && (picker.sel = i)}>{it.label}</button>
        {/each}
      </div>
    {:else if menu.length && hi < 0}
      <div class="zc-pop" role="listbox" aria-label="commands">
        {#each menu as c, i}
          <button class="zc-item zc-cmd" class:zc-on={i === Math.min(menuSel, menu.length - 1)} onclick={() => runMenu(i)} onmouseenter={() => (menuSel = i)}>
            <span class="zc-acc">/{c.name}</span>{#if c.args}<span class="zc-dim"> {c.args}</span>{/if}<span class="zc-dim zc-help1">{c.help}</span>
          </button>
        {/each}
      </div>
    {/if}

    {#if !connected && sid}
      <div class="zc-status zc-warn">connection lost · reconnecting… <button class="zc-link" onclick={() => retry(0)}>retry now</button></div>
    {:else if busy}
      <div class="zc-status">
        <span class="zc-acc">{SPINNER[spin % SPINNER.length]}</span>
        <span class="zc-st">{status}</span>
        <span class="zc-dim zc-tail">{toolSince && secs(toolSince) > 0 ? ` (${secs(toolSince)}s)` : ''} · turn {secs(turnStarted)}s{turnTools ? ` · ${turnTools} tool${turnTools === 1 ? '' : 's'}` : ''}{turnFiles.length ? ` · ${turnFiles.length} file${turnFiles.length === 1 ? '' : 's'} edited` : ''} ·</span>
        {#if ro}<button class="zc-dim zc-link" onclick={() => board()}>esc: back to the board</button>{:else}<button class="zc-dim zc-link" onclick={abort}>esc to interrupt</button>{/if}
      </div>
    {/if}

    {#if attachments.length}
      <div class="zc-atts" aria-label="attachments">
        {#each attachments as a (a.id)}
          <span class="zc-att" class:zc-att-err={a.error} title={a.path ? tilde(a.path) : a.error ?? 'sending…'}>
            <span class="zc-att-name">{a.name}</span>
            <span class="zc-dim">{a.error ? a.error : a.path ? `${sizeText(a.size)} ✓` : `${Math.round(a.progress * 100)}%`}</span>
            <button class="zc-att-x" onclick={() => unattach(a.id)} aria-label={`remove ${a.name}`} title="remove">×</button>
          </span>
        {/each}
      </div>
    {/if}

    {#each pluginDrafts as draft}
      <section aria-label="plugin draft">
        <strong>DRAFT · plugin: {draft.plugin}</strong>
        <pre style="white-space: pre-wrap; overflow-wrap: anywhere">{draft.text}</pre>
        <button onclick={() => sendPluginDraft(draft)} disabled={ro || creating || uploading || !!mcpFlow || !!ready.length}>Send plugin draft</button>
        <button onclick={() => (pluginDrafts = pluginDrafts.filter((d) => d !== draft))}>Dismiss</button>
      </section>
    {/each}

    <div class="zc-compose" class:zc-ro={ro} class:zc-drag={dragging}>
      {#if !ro}
        <input bind:this={picker_} type="file" multiple hidden onchange={(e) => { attach((e.currentTarget as HTMLInputElement).files); (e.currentTarget as HTMLInputElement).value = ''; }} />
        <button class="zc-clip" onclick={() => picker_?.click()} title="attach files (or paste, or drop them here)" aria-label="attach files">📎</button>
      {/if}
      <textarea
        onpaste={onPaste}
        bind:this={ta}
        bind:value={input}
        oninput={onInput}
        rows="1"
        {placeholder}
        disabled={ro || creating}
        aria-label="message"
        spellcheck="true"
      ></textarea>
      {#if busy && !input.trim() && !ro}
        <button class="zc-send" onclick={abort} title="interrupt (esc)" aria-label="interrupt">■</button>
      {:else if !ro}
        <button class="zc-send" onclick={submit} disabled={(!input.trim() && !ready.length) || uploading} title="send (enter)" aria-label="send">↑</button>
      {/if}
    </div>
    {#if suggestion && !input && !busy && !ro}
      <button class="zc-hint" onclick={takeSuggestion}>tab: use suggestion</button>
    {/if}

    <div class="zc-foot">
      {#if !sid}<button class="zc-agent" onclick={agentPicker} title="choose who this session is (/new @agent)">@{agent}</button> ·
      {:else if agentTag(agent)}<span class="zc-agent">@{agent}</span> ·{/if}
      <button onclick={modelPicker} title="choose a model (/model)">{shortModel(model) || '…'}</button>
      {#if shownEffort}· <button onclick={effortPicker} title="choose a thinking level (/effort)">{shownEffort}</button>{/if}
      · <span class="zc-ttl">{title || 'new session'}{sid ? ` (${short(sid)})` : ''}</span>
      {#if ro}· read-only{/if}
      {#if sessionTokens}· {M.fmtTokens(sessionTokens)} tokens{/if}
      <span class="zc-grow"></span>
      <button onclick={() => push({ k: 'help', rows: cmds.map((c) => [`/${c.name}`, `${c.help}${c.args ? `: /${c.name} ${c.args}` : ''}`]), keys: M.KEYS })} title="/help">?</button>
      <button class="zc-dim" onclick={() => settings()}>Settings</button>
      {#if !wide}<button class="zc-panel" onclick={() => toggleSide()} title="side panel (ctrl+b)" aria-label="side panel">▤</button>{/if}
    </div>
  </div>
</div>

<style>
  .zc { flex: 1; min-height: 0; display: flex; flex-direction: column; position: relative; }
  .zc-scroll { flex: 1; min-height: 0; overflow-y: auto; overflow-x: hidden; overscroll-behavior: contain; }
  .zc-body { max-width: 860px; margin: 0 auto; padding: 14px 20px 18px; font-size: 15px; line-height: 1.55; }
  .zc-title { color: var(--dim); text-align: center; font-size: 13px; margin: 2px 0 14px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .zc-earlier { display: block; margin: 0 auto 8px; color: var(--accent); font-size: 13px; }
  .zc-user { white-space: pre-wrap; overflow-wrap: anywhere; font-weight: 560; margin: 14px 0 10px; }
  .zc-kernel { white-space: pre-wrap; overflow-wrap: anywhere; color: var(--dim); font-size: 13px; margin: 10px 0; }
  .zc-acc { color: var(--accent); }
  .zc-as { display: flex; flex-wrap: wrap; justify-content: center; align-items: baseline; gap: 6px; margin: -6px 0 14px; font-size: 13px; }
  .zc-as button { color: var(--dim); border: 1px solid var(--faint); border-radius: 6px; padding: 2px 8px; }
  .zc-as button:hover { color: var(--ink); }
  .zc-as button.zc-on { color: var(--accent); border-color: var(--accent); }
  .zc-agent { color: var(--accent); }
  .zc-dim { color: var(--dim); }
  .zc-line { font-size: 13px; margin: 2px 0 10px; white-space: pre-wrap; overflow-wrap: anywhere; }
  .zc-line.zc-dim { color: var(--dim); font-family: var(--mono); font-size: 12px; }
  .zc-warn { color: var(--warn); }
  .zc-err { color: var(--err); }
  .zc-accent { color: var(--accent); }
  .zc-line a { color: var(--accent); word-break: break-all; }
  .zc-mono { margin: 4px 0 12px; font-size: 12px; }
  .zc-mono pre { font-family: var(--mono); margin: 2px 0 0; padding: 8px 10px; border-radius: 6px; background: color-mix(in srgb, var(--faint) 30%, transparent); white-space: pre-wrap; overflow-wrap: anywhere; }
  .zc-qs { margin: 6px 0 12px; }
  .zc-q { margin-top: 4px; }
  .zc-opt { display: block; text-align: left; padding: 2px 8px 2px 18px; border-radius: 5px; }
  .zc-opt:hover { background: color-mix(in srgb, var(--accent) 10%, transparent); }
  .zc-rec { margin-left: 0.6em; }
  .zc-result { font-family: var(--mono); font-size: 12.5px; color: var(--dim); white-space: pre-wrap; overflow-wrap: anywhere; margin: 0 0 10px 14px; }
  .zc-result.zc-err { color: var(--err); }
  .zc-help { font-size: 13px; margin: 6px 0 14px; }
  .zc-hrow { display: grid; grid-template-columns: 10em 1fr; gap: 10px; }
  .zc-hrow > :first-child { text-align: left; font-family: var(--mono); font-size: 12px; }
  .zc-jump { position: absolute; left: 50%; transform: translateX(-50%); bottom: calc(100% + 8px); z-index: 5; font-size: 12px; padding: 4px 12px; border-radius: 14px; background: var(--paper); border: 1px solid var(--faint); color: var(--accent); box-shadow: 0 2px 10px rgb(0 0 0 / 0.08); }
  .zc-dock { flex: none; max-width: 860px; width: 100%; margin: 0 auto; padding: 0 20px 8px; box-sizing: border-box; position: relative; }
  .zc-pop { position: absolute; bottom: 100%; left: 20px; right: 20px; max-height: 40vh; overflow: auto; background: var(--paper); border: 1px solid var(--faint); border-radius: 8px; padding: 4px; box-shadow: 0 -4px 18px rgb(0 0 0 / 0.07); z-index: 6; font-size: 13px; }
  .zc-pophead { padding: 4px 8px; font-size: 12px; }
  .zc-item { display: flex; gap: 0; width: 100%; text-align: left; padding: 4px 8px; border-radius: 5px; white-space: pre; overflow: hidden; text-overflow: ellipsis; font-family: var(--mono); font-size: 12.5px; }
  .zc-item.zc-on { background: color-mix(in srgb, var(--accent) 12%, transparent); }
  .zc-cmd .zc-help1 { margin-left: auto; padding-left: 16px; font-family: var(--sans); overflow: hidden; text-overflow: ellipsis; }
  .zc-status { display: flex; gap: 6px; align-items: baseline; font-size: 13px; padding: 4px 2px; min-width: 0; }
  .zc-st { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; flex: 0 1 auto; }
  .zc-tail { white-space: nowrap; flex: none; }
  .zc-link { text-decoration: underline dotted; white-space: nowrap; }
  .zc-link:hover { color: var(--ink); }
  .zc-compose { display: flex; align-items: flex-end; gap: 8px; border: 1px solid var(--faint); border-radius: 10px; padding: 8px 8px 8px 12px; background: var(--paper); }
  .zc-compose:focus-within { border-color: color-mix(in srgb, var(--accent) 55%, var(--faint)); }
  .zc-compose textarea { flex: 1; resize: none; border: 0; outline: none; background: transparent; color: var(--ink); font: inherit; font-size: 15px; line-height: 1.45; padding: 0; min-height: 1.45em; overflow-y: auto; }
  .zc-compose textarea::placeholder { color: var(--dim); opacity: 0.8; }
  .zc-ro { opacity: 0.7; }
  .zc-drag { border-color: var(--accent); border-style: dashed; }
  .zc-clip { flex: none; width: 28px; height: 28px; border-radius: 50%; font-size: 15px; line-height: 28px; text-align: center; opacity: 0.7; }
  .zc-clip:hover { opacity: 1; background: color-mix(in srgb, var(--accent) 10%, transparent); }
  .zc-atts { display: flex; flex-wrap: wrap; gap: 6px; padding: 0 2px 6px; }
  .zc-att { display: inline-flex; align-items: center; gap: 6px; max-width: 100%; font-size: 12.5px; border: 1px solid var(--faint); border-radius: 14px; padding: 2px 4px 2px 10px; background: var(--paper); }
  .zc-att-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 220px; }
  .zc-att-err { border-color: var(--err); color: var(--err); }
  .zc-att-x { width: 20px; height: 20px; border-radius: 50%; line-height: 18px; text-align: center; }
  .zc-att-x:hover { background: color-mix(in srgb, var(--ink) 10%, transparent); }
  .zc-send { flex: none; width: 28px; height: 28px; border-radius: 50%; background: var(--accent); color: var(--paper); font-size: 14px; line-height: 28px; text-align: center; }
  .zc-send:disabled { background: var(--faint); cursor: default; }
  .zc-hint { font-size: 12px; color: var(--dim); padding: 2px 4px; }
  .zc-foot { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px; font-size: 12px; color: var(--dim); padding: 6px 2px 0; }
  .zc-foot button:hover { color: var(--ink); }
  .zc-ttl { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 50%; }
  .zc-grow { flex: 1; }
  .zc-panel { padding: 0 4px; }

  /* Markdown in the chat (md.ts builds div.md). */
  .zc-md { margin: 4px 0 12px; overflow-wrap: anywhere; }
  .zc-md :global(.md > :first-child) { margin-top: 0; }
  .zc-md :global(.md > :last-child) { margin-bottom: 0; }
  .zc-md :global(p), .zc-md :global(ul), .zc-md :global(ol), .zc-md :global(blockquote), .zc-md :global(table), .zc-md :global(pre) { margin: 0.5em 0; }
  .zc-md :global(ul), .zc-md :global(ol) { padding-left: 1.4em; }
  .zc-md :global(h1), .zc-md :global(h2), .zc-md :global(h3), .zc-md :global(h4) { font-size: 1.05em; margin: 0.9em 0 0.4em; font-weight: 650; }
  .zc-md :global(h1) { font-size: 1.2em; }
  .zc-md :global(code) { font-family: var(--mono); font-size: 0.88em; background: color-mix(in srgb, var(--faint) 40%, transparent); padding: 0.05em 0.3em; border-radius: 4px; cursor: pointer; }
  .zc-md :global(pre) { background: color-mix(in srgb, var(--faint) 30%, transparent); padding: 8px 10px; border-radius: 6px; overflow-x: auto; }
  .zc-md :global(pre code) { background: none; padding: 0; cursor: text; }
  .zc-md :global(table) { border-collapse: collapse; display: block; overflow-x: auto; font-size: 0.93em; }
  .zc-md :global(th), .zc-md :global(td) { border: 1px solid var(--faint); padding: 3px 9px; text-align: left; }
  .zc-md :global(blockquote) { border-left: 3px solid var(--faint); padding-left: 10px; color: var(--dim); }
  .zc-md :global(a) { color: var(--accent); text-decoration: none; cursor: pointer; }
  .zc-md :global(a:hover) { text-decoration: underline; }
  .zc-md :global(hr) { border: 0; border-top: 1px solid var(--faint); }
  .zc-md :global(img) { max-width: 100%; }
  .zc-md :global(.md-display) { overflow-x: auto; }

  @media (max-width: 600px) {
    .zc-body { padding: 10px 12px 14px; }
    .zc-dock { padding: 0 10px 6px; }
    .zc-pop { left: 10px; right: 10px; }
    .zc-hrow { grid-template-columns: 7.5em 1fr; }
    .zc-cmd .zc-help1 { display: none; }
  }
</style>
