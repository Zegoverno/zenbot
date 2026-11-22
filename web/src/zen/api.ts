// The page's side of zen: the kernel through the server's proxy (`/api/zen/…`, server/zen.ts; the
// plugin host's routes too), the home folder (`/api/fs/…`, server/home.ts) and the kernel commands
// (`/api/command`). Writes carry `X-Zen-Web: 1` (no cross-site writes); the kernel's token stays on
// the server.

async function call<T>(method: string, url: string, body?: unknown): Promise<T> {
  const res = await fetch(url, {
    method,
    headers: method === 'GET' ? {} : { 'content-type': 'application/json', 'x-zen-web': '1' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error((data as { error?: string }).error ?? `HTTP ${res.status}`);
  return data as T;
}

const enc = encodeURIComponent;

// ---- the kernel's shapes (crates/zend/src/api.rs, tasks.rs, jobs.rs) ----

export interface Session {
  id: string;
  title: string | null;
  model: string;
  effort: string | null;
  archived: boolean;
  /** null: the owner's; 'channel' (Matrix), 'job', 'subagent', 'verifier'. */
  kind: string | null;
  parent: string | null;
  /** Who the session is (agents.rs): 'zenbot' unless the owner picked another agent. */
  agent?: string;
  /** How it ended, the owner's call when archiving: 'done', 'partial', 'not-done' or null. */
  outcome?: Outcome | null;
  outcome_note?: string | null;
  /** The owner's 1–10 score given when archiving, or null. */
  rating?: number | null;
  /** The partial session this one continues. */
  continues?: string | null;
  created_at: string;
  updated_at: string;
  cost?: number;
}

export interface BoardSession extends Session {
  busy: boolean;
  waiting: boolean;
  /** A child's task (≤200 chars). */
  task?: string;
  /** How many children a top-level session has. */
  children?: number;
}

export interface Block {
  type: 'text' | 'toolCall' | 'thinking' | 'image' | string;
  text?: string;
  thinking?: string;
  id?: string;
  name?: string;
  arguments?: Record<string, unknown>;
}

export interface Message {
  role: 'user' | 'assistant' | 'toolResult';
  content: string | Block[];
  seq: number;
  timestamp?: number;
  /** A user message the kernel wrote (a job's prompt, a report), not the owner. */
  kernel?: boolean;
  toolCallId?: string;
  toolName?: string;
  isError?: boolean;
  model?: string;
  provider?: string;
  stopReason?: string;
  errorMessage?: string;
  usage?: { input?: number; output?: number; cacheRead?: number; cacheWrite?: number; totalTokens?: number };
}

export interface SessionDetail extends Session {
  messages: Message[];
  message_count: number;
  tokens: number;
  busy: boolean;
  /** The open suggested prompt (a partial session's handoff brief arrives before anyone watches). */
  suggestion?: { id: number; text: string } | null;
}

/** A subagent role (roles.rs); `tools` null means every tool the subagent gets anyway. */
export interface Role { name: string; description: string; model: string | null; effort: string | null; tools: string[] | null; skills: string[] }
/** An agent's roles file as the kernel read it: the roles, or `error` when the file is broken. */
export interface AgentRoles { default_model?: string | null; roles?: Role[]; error?: string }

/** An agent (agents.rs): what it may use, or `error` when its agent.toml is broken. */
export interface Agent {
  name: string;
  error?: string;
  description?: string;
  tools?: string[];
  all_tools?: boolean;
  mcp?: string[] | null;
  shared_skills?: boolean;
  model?: string | null;
  workspace?: string | null;
  context?: string[];
  roles?: AgentRoles;
}
/** An agent with its three files' text, its folder and how many sessions it has. */
export interface AgentDetail extends Agent { soul: string; identity: string; settings: string; dir: string; sessions: number }
export type AgentFiles = Partial<Pick<AgentDetail, 'soul' | 'identity' | 'settings'>>;

export interface Model { id: string; name?: string; engine?: string; efforts?: string[]; default_effort?: string | null; context?: number }
export interface Models { models: Model[]; authenticated: Record<string, boolean>; default: string }

export type Priority = 'urgent' | 'important' | 'medium' | 'not_urgent' | 'parking_lot' | null;
export interface Task {
  project: string;
  text: string;
  id: string | null;
  due: string | null;
  scheduled: string | null;
  priority: Priority;
  goals: string[];
  file: string;
  line: number;
  overdue: boolean;
  agent?: { session: string; title: string | null; state: 'running' | 'waiting' | 'idle' };
}
export interface Tasks { today: string; tz: string; count: number; projects: { project: string; tasks: Task[] }[]; problems: string[] }

/** Token count of one turn record (`end.turn`, `child_end.turn`). */
export interface TurnRecord { input_tokens?: number; output_tokens?: number; cache_read?: number; cache_write?: number; ms?: number; model?: string; effort?: string | null }

/** The session socket's events (crates/zen-proto/src/client.rs). A live `message` carries no `seq`
 * (stored ones do, and skip numbers): catch up by `message_count`, not by seq. */
export type ZenEvent =
  | { type: 'message'; message: Message }
  | { type: 'busy'; busy: boolean; turn_id?: string; harness?: string; model?: string; effort?: string | null }
  | { type: 'delta'; delta: string }
  | { type: 'thinking'; delta: string }
  | { type: 'tool_start'; call_id: string; name: string; args: Record<string, unknown> }
  | { type: 'tool_end'; call_id: string; is_error: boolean; ms: number }
  | { type: 'end'; error: string | null; cost?: number; turn?: TurnRecord; next?: boolean }
  | { type: 'child_end'; turn?: TurnRecord }
  | { type: 'idle' }
  | { type: 'title'; title: string }
  | { type: 'suggestion'; id: number; text: string }
  | { type: 'model'; model: string; effort: string | null }
  | { type: 'status'; text: string }
  | { type: 'questions'; questions: { question: string; options: string[] }[] }
  | { type: 'error'; error: string }
  | { type: 'resync'; skipped: number; busy: boolean };

export type Outcome = 'done' | 'partial' | 'not-done';

export interface Entry { name: string; path: string; dir: boolean; hidden: boolean; size: number; mtime: number }
export interface FileRead { path: string; mtime: number; size: number; text?: string; binary?: boolean; tooBig?: boolean; dir?: boolean; vault?: string | null }
/** A command of the shared table; a plugin's (`plugin`, `tool`) calls that plugin's tool. */
export interface Command {
  name: string;
  args: string;
  help: string;
  scope: 'kernel' | 'session' | 'screen' | 'plugin';
  /** A plugin's command: its plugin, its tool, and the tool's UI resource when it has one. */
  plugin?: string;
  tool?: string;
  description?: string;
  ui?: string;
  /** A plugin command's declared choices for its first argument: with no argument, the chat opens a picker. */
  choices?: CommandChoice[];
}

// ---- plugins (ADR 0001, Host API; the kernel's plugins.rs) ----

export interface PluginView { id: string; title: string; ui: string }
export interface PluginOpener { id: string; title: string; ui: string; patterns: string[]; mode: 'read' | 'edit' }
export interface PluginGame { id: string; title: string; ui: string; description?: string }
/** A declared value of a plugin command's first argument; `then` (an args hint) means it needs more input. */
export interface CommandChoice { value: string; description: string; then?: string }
export interface PluginCommand { name: string; tool: string; args?: string; description?: string; choices?: CommandChoice[] }
export interface Plugin {
  name: string;
  version: string;
  title: string;
  description: string;
  contributes: { views?: PluginView[]; openers?: PluginOpener[]; games?: PluginGame[]; commands?: PluginCommand[] };
  permissions: string[];
  enabled: boolean;
  /** The owner's AI switch (only matters with the `ai` permission): off refuses its completions. */
  ai_enabled?: boolean;
  /** Its answered completions (sampling) in the last 30 days. */
  ai_calls_30d?: number;
}
/** `"<pattern>:<mode>"` → `"<plugin>/<opener id>"` or `"builtin"`. */
export type OpenerDefaults = Record<string, string>;
export interface MarketEntry {
  name: string;
  title?: string;
  description?: string;
  author?: string;
  repository?: string;
  license?: string;
  version: string;
  kernel?: string[];
  contributes?: string[];
  categories?: string[];
  /** The installed version, or null. */
  installed: string | null;
}
export type Install = { needs_approval: true; permissions: string[] } | Plugin;
/** An MCP `CallToolResult`. */
export interface ToolResult { content?: { type: string; text?: string }[]; structuredContent?: Record<string, unknown>; isError?: boolean; _meta?: Record<string, unknown>; [k: string]: unknown }
export interface UiResource { uri: string; mimeType?: string; text: string; _meta?: { ui?: { csp?: Csp; permissions?: Record<string, object> } } }
export interface Csp { connectDomains?: string[]; resourceDomains?: string[]; frameDomains?: string[]; baseUriDomains?: string[] }

export const pluginApi = {
  list: () => call<{ plugins: Plugin[]; defaults: OpenerDefaults }>('GET', '/api/zen/plugins'),
  setDefault: (key: string, value: string) => call<unknown>('PUT', '/api/zen/plugins/defaults', { key, value }),
  setEnabled: (name: string, enabled: boolean) => call<unknown>('PATCH', `/api/zen/plugins/${enc(name)}`, { enabled }),
  setAi: (name: string, ai: boolean) => call<unknown>('PATCH', `/api/zen/plugins/${enc(name)}`, { ai }),
  remove: (name: string) => call<unknown>('DELETE', `/api/zen/plugins/${enc(name)}`),
  /** The entries; the kernel may answer with the list itself or `{ plugins }` / `{ entries }`. */
  marketplace: async () => {
    const r = await call<MarketEntry[] | { plugins?: MarketEntry[]; entries?: MarketEntry[] }>('GET', '/api/zen/plugins/marketplace');
    return Array.isArray(r) ? r : (r.entries ?? r.plugins ?? []);
  },
  install: (name: string, approve?: string[]) =>
    call<Install>('POST', '/api/zen/plugins/install', approve ? { name, approve: true, permissions: approve } : { name }),
  resource: (name: string, uri: string) => call<UiResource>('GET', `/api/zen/plugins/${enc(name)}/resource?uri=${enc(uri)}`),
  call: (name: string, tool: string, args: Record<string, unknown>, session?: string | null) =>
    call<ToolResult>('POST', `/api/zen/plugins/${enc(name)}/call`, { tool, arguments: args, ...(session ? { session } : {}) }),
};

export const zen = {
  board: (archived = false) => call<{ sessions: BoardSession[]; archived?: number }>('GET', `/api/zen/board${archived ? '?archived=true' : ''}`),
  sessions: (archived = false) => call<Session[]>('GET', `/api/zen/sessions?archived=${archived}`),
  session: (id: string, last = 40) => call<SessionDetail>('GET', `/api/zen/sessions/${enc(id)}?last=${last}`),
  create: (b: { title?: string; model?: string; effort?: string; agent?: string } = {}) => call<Session>('POST', '/api/zen/sessions', b),
  update: (id: string, b: { title?: string; model?: string; effort?: string; archived?: boolean; outcome?: Outcome; note?: string; rating?: number }) => call<Session & { continued_by?: string }>('PATCH', `/api/zen/sessions/${enc(id)}`, b),
  models: () => call<Models>('GET', '/api/zen/models'),
  agents: () => call<Agent[]>('GET', '/api/zen/agents'),
  agent: (name: string) => call<AgentDetail>('GET', `/api/zen/agents/${enc(name)}`),
  newAgent: (name: string, description: string) => call<Agent & { dir: string }>('POST', '/api/zen/agents', { name, description }),
  saveAgent: (name: string, change: AgentFiles) => call<AgentDetail>('PUT', `/api/zen/agents/${enc(name)}`, change),
  removeAgent: (name: string) => call<{ ok: boolean; moved_to: string }>('DELETE', `/api/zen/agents/${enc(name)}`),
  search: (query: string, scope = 'notes', limit = 10) => call<{ query: string; scope: string; results: SearchHit[] }>('POST', '/api/zen/search', { query, scope, limit }),
  tasks: () => call<Tasks>('GET', '/api/zen/tasks'),
  handTask: (id: string, rev: string) => call<{ task: Task; session: string; existing: boolean }>('POST', `/api/zen/tasks/${enc(id)}/agent`, {rev}),
  version: (refresh = false) => call<{ running: string; latest?: string; behind?: number; available?: boolean; commits?: string[]; error?: string }>('GET', `/api/zen/version${refresh ? '?refresh=true' : ''}`),
  upgrade: () => call<{ started_at: string }>('POST', '/api/zen/upgrade', {}),
  upgradeState: () => call<{ running: boolean; job?: { started_at: string; status: string; log: string }; last_result?: string }>('GET', '/api/zen/upgrade'),
  health: () => call<{ ok: boolean; commit?: string; version?: string; busy?: number }>('GET', '/api/zen/health'),
  mcpLogin: (server: string) => call<{ flow: string; authorize_url: string; auth_server?: string; redirect?: string }>('POST', '/api/zen/mcp/login', { server }),
  mcpCallback: (flow: string, callback: string) => call<{ server: string }>('POST', `/api/zen/mcp/login/${enc(flow)}`, { callback }),

  /** The session's live events; `onEvent` gets each, `onClose` once when it ends. */
  socket(id: string, onEvent: (e: ZenEvent) => void, onClose: () => void): { send: (m: object) => boolean; close: () => void } {
    const ws = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/api/zen/sessions/${enc(id)}/ws`);
    let closed = false;
    ws.onmessage = (m) => {
      try {
        onEvent(JSON.parse(m.data));
      } catch {}
    };
    ws.onclose = () => {
      if (!closed) onClose();
      closed = true;
    };
    const queue: string[] = [];
    ws.onopen = () => queue.splice(0).forEach((m) => ws.send(m));
    return {
      send(m) {
        if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(m));
        else if (ws.readyState === WebSocket.CONNECTING) queue.push(JSON.stringify(m));
        else return false;
        return true;
      },
      close() {
        closed = true;
        ws.close();
      },
    };
  },
};

/** A file or folder whose name matched (server/home.ts `find`); `rel` is its path in the searched folder. */
export interface Found { name: string; path: string; rel: string; dir: boolean }
/** A kernel search result (api.rs `search_once`); `path` is the note's file for wiki pages and notes. */
export interface SearchHit { kind: string; ref: string; title: string; snippet: string; at: string; exact: boolean; relevance: number | null; path: string | null }

export interface Routine { name: string; schedule: string; tz: string; enabled: boolean; next_run_at: string | null; last_status: string | null; last_error: string | null; failures: number }
export interface RoutineRun { id: number; job: string; status: string; started_at: string; error: string | null; session: string | null }
export const routines = {
  list: () => call<{ jobs: Routine[]; scheduler: boolean }>('GET', '/api/zen/jobs'),
  runs: () => call<RoutineRun[]>('GET', '/api/zen/jobs/runs'),
  run: (name: string) => call('POST', `/api/zen/jobs/${enc(name)}/run`, {}),
  enable: (name: string, enabled: boolean) => call('PATCH', `/api/zen/jobs/${enc(name)}`, { enabled }),
};

/** A dev server listening on this machine (server/servers.ts). */
export interface DevServer { port: number; name: string; command: string; cwd: string; unit: string | null; pid: number }
export const files = {
  home: () => call<{ home: string; vault: string }>('GET', '/api/fs/home'),
  /** A Preview pin's screenshot, taken on the server (server/shot.ts). */
  previewShot: (b: { url: string; selector: string; width: number; height: number }) => call<{ path: string; data: string }>('POST', '/api/preview/shot', b),
  /** The dev servers on this machine (server/servers.ts), named by app; stop or restart one by its port. */
  previewServers: () => call<{ servers: DevServer[] }>('GET', '/api/preview/servers'),
  previewStop: (port: number) => call<{ ok: boolean }>('POST', '/api/preview/servers/stop', { port }),
  previewRestart: (port: number) => call<{ server: DevServer }>('POST', '/api/preview/servers/restart', { port }),
  list: (path: string) => call<{ path: string; entries: Entry[] }>('GET', `/api/fs/list?path=${enc(path)}`),
  read: (path: string) => call<FileRead>('GET', `/api/fs/read?path=${enc(path)}`),
  stat: (path: string) => call<{ mtime: number; size: number }>('GET', `/api/fs/stat?path=${enc(path)}`),
  find: (root: string, q: string) => call<{ results: Found[]; truncated: boolean }>('GET', `/api/fs/find?root=${enc(root)}&q=${enc(q)}`),
  locate: (ref: string, from?: string) => call<{ found: { path: string; dir: boolean } | null }>('GET', `/api/fs/locate?ref=${enc(ref)}${from ? `&from=${enc(from)}` : ''}`),
  create: (dir: string, name: string) => call<{ path: string }>('POST', '/api/fs/create', { dir, name }),
  mkdir: (dir: string, name: string) => call<{ path: string }>('POST', '/api/fs/mkdir', { dir, name }),
  rename: (path: string, name: string) => call<{ path: string }>('POST', '/api/fs/rename', { path, name }),
};

export const commands = {
  /** The shared table; a plugin's command may carry `description` rather than `help`. */
  list: async () => {
    const r = await call<{ commands: Command[] }>('GET', '/api/commands');
    return { commands: r.commands.map((c) => ({ ...c, args: c.args ?? '', help: c.help ?? c.description ?? '' })) };
  },
  run: (name: string, args: string[] = []) => call<{ output: string; ok: boolean }>('POST', '/api/command', { name, args }),
};

/** "3m", "2h", "5d" since an ISO time. */
export function ago(iso: string, now = Date.now()): string {
  const s = Math.max(0, (now - Date.parse(iso)) / 1000);
  if (s < 60) return 'now';
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

/** The tool line the terminal shows: name, then its command, path, from → to, or its arguments; one line, ≤120 chars. */
export function toolSummary(name: string, args: Record<string, unknown> | undefined): string {
  const a = args ?? {};
  const detail =
    typeof a.command === 'string' ? a.command
    : typeof a.path === 'string' ? a.path
    : typeof a.from === 'string' ? `${a.from} → ${typeof a.to === 'string' ? a.to : ''}`
    : JSON.stringify(a);
  return `${name} ${(detail.split('\n')[0] ?? '').slice(0, 120)}`;
}

export const short = (id: string) => id.slice(0, 8);
/** A name the kernel takes for an agent: a lowercase letter, then lowercase letters, digits and hyphens; at most 32. */
export const AGENT_NAME = /^[a-z][a-z0-9-]{0,31}$/;
/** "@rex · " for another agent's session, nothing for zenbot's (the terminal's `agent_tag`). */
export const agentTag = (agent: string | null | undefined) => (agent && agent !== 'zenbot' ? `@${agent} · ` : '');
export const readOnly = (s: { kind: string | null }) => s.kind === 'subagent' || s.kind === 'verifier';

// ---- attachments (server/uploads.ts) ----

/** Send one file to `~/inbox/<day>/`; `progress` gets 0..1. Resolves to the saved path. */
export function upload(file: File, progress: (f: number) => void): { done: Promise<{ path: string; size: number }>; cancel: () => void } {
  const xhr = new XMLHttpRequest();
  const done = new Promise<{ path: string; size: number }>((ok, fail) => {
    xhr.open('POST', `/api/upload?name=${enc(file.name || 'pasted')}`);
    xhr.setRequestHeader('x-zen-web', '1');
    xhr.setRequestHeader('content-type', 'application/octet-stream');
    xhr.upload.onprogress = (e) => e.lengthComputable && progress(e.loaded / e.total);
    xhr.onload = () => {
      let data: { path?: string; size?: number; error?: string } = {};
      try {
        data = JSON.parse(xhr.responseText);
      } catch {}
      if (xhr.status === 200 && data.path) ok({ path: data.path, size: data.size ?? file.size });
      else fail(new Error(data.error ?? (xhr.status === 413 ? 'file too large' : `HTTP ${xhr.status}`)));
    };
    xhr.onerror = () => fail(new Error('network error'));
    xhr.onabort = () => fail(new Error('cancelled'));
    xhr.send(file);
  });
  return { done, cancel: () => xhr.abort() };
}
