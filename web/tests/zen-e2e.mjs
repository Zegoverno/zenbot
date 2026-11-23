// End-to-end for the zen app: the real server (dev mode) on a scratch home, a stand-in zen kernel
// (HTTP and a minimal WebSocket, checking the token on every request), and headless Chromium.
// The slash commands come from this checkout's `zen --commands` (built here, or ZEN_BIN), plus a
// stand-in plugin's command. The stand-in kernel also hosts plugins (ADR 0001, Host API): a list,
// a marketplace, installs, and a fixture UI built on the real MCP Apps `App`.
// `node tests/zen-e2e.mjs [--shots]`.
import { execFileSync, spawn } from 'node:child_process';
import { createServer } from 'node:http';
import net from 'node:net';
import { createHash } from 'node:crypto';
import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
import { chromium } from 'playwright';

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
const HOME = mkdtempSync(path.join(tmpdir(), 'zen-e2e-'));
const VAULT = path.join(HOME, '.zenbot/global');
mkdirSync(path.join(VAULT, 'wiki'), { recursive: true });
writeFileSync(path.join(VAULT, 'wiki/God.md'), '---\ntype: idea\n---\n# God\n\nThree persons.\n\n## Changelog\n\n- v1: first\n');
writeFileSync(path.join(VAULT, 'wiki/Grace.md'), '---\ntype: source\nlink: https://example.com/grace\n---\n# Grace\n\nA gift.\n');
const para = (n) => Array.from({ length: n }, (_, i) => `Paragraph ${i + 1} of plain words to read through slowly.`).join('\n\n');
writeFileSync(path.join(VAULT, 'wiki/Atlas.md'), `# Atlas\n\n${para(40)}\n\n## North\n\n${para(40)}\n\n## South\n\n${para(40)}\n`);
writeFileSync(path.join(HOME, '.zenbot/token'), 'test-token');
writeFileSync(path.join(HOME, 'plain.txt'), 'hello file');
writeFileSync(path.join(HOME, 'home-only.txt'), 'home');
writeFileSync(path.join(VAULT, 'wiki/plain.txt'), 'hello file');
writeFileSync(path.join(VAULT, '.hidden-test'), 'hidden');
mkdirSync(path.join(VAULT, 'games'));
writeFileSync(path.join(VAULT, 'games/board.fx'), 'four in a row\n');
writeFileSync(path.join(VAULT, 'games/fxnote-a.md'), '# A note the fixture plugin reads\n');
const REAL_ZEN = process.env.ZEN_BIN ?? (() => {
  const root = path.resolve(import.meta.dirname, '../..');
  execFileSync('cargo', ['build', '-q', '-p', 'zen'], { cwd: root, stdio: 'inherit' });
  return path.join(root, 'target/debug/zen');
})();
// `zen --commands` as the kernel lists it with a plugin enabled: the shared table plus its command.
const table = JSON.parse(execFileSync(REAL_ZEN, ['--commands'], { encoding: 'utf8' }));
table.commands.push({ name: 'roll', args: '<sides>', description: 'Roll a die.', scope: 'kernel', plugin: 'zenfx', tool: 'zenfx_roll' });
table.commands.push({ name: 'desk', args: '<action> [rest]', description: 'Work on the desk.', scope: 'plugin', plugin: 'zenfx', tool: 'zenfx_desk',
  choices: [{ value: 'lint', description: 'Check the desk.' }, { value: 'find', description: 'Search the desk.', then: '<words>' }] });
writeFileSync(path.join(HOME, 'commands.json'), JSON.stringify(table));
const ZEN_BIN = path.join(HOME, 'zen');
writeFileSync(ZEN_BIN, `#!/bin/sh\nif [ "$1" = --commands ]; then cat '${path.join(HOME, 'commands.json')}'; else exec '${REAL_ZEN}' "$@"; fi\n`, { mode: 0o755 });
const PORT = Number(process.env.ZEN_E2E_PORT ?? 4412);
const KERNEL = Number(process.env.ZEN_E2E_KERNEL_PORT ?? 4413);
const shots = process.argv.includes('--shots');
/** Screenshots of the slash menu and the rating step (with --shots). */
const shotDir = process.env.ZEN_SHOT_DIR ?? '/tmp/zen-shots';
if (shots) mkdirSync(shotDir, { recursive: true });
const now = new Date().toISOString();
const session = (id, title, extra = {}) => ({ id, title, model: 'faux/smoke', effort: null, archived: false, kind: null, parent: null, created_at: now, updated_at: now, ...extra });
const prompts = [];
/** What else the page sent on the socket: steers, aborts; and tasks handed to an agent. */
const steers = [];
/** The queue the stand-in kernel keeps while a turn runs (the kernel's `queue` events), and the steer commands it got. */
let queue = [];
let queueId = 0;
const promoted = [];
/** What the page changed on sessions (title, archived). */
const patched = [];
const aborts = [];
const handed = [];
/** Waits until the stand-in kernel has seen what the page sent. */
const until = async (ok, what) => {
  for (let i = 0; i < 100 && !ok(); i++) await new Promise((r) => setTimeout(r, 50));
  assert.ok(ok(), what);
};
/** The same for a check that is asynchronous (reads the page). */
const untilAsync = async (ok, what) => {
  for (let i = 0; i < 100; i++) {
    if (await ok().catch(() => false)) return;
    await new Promise((r) => setTimeout(r, 50));
  }
  assert.fail(what);
};
/** What the page asked the stand-in kernel to do: new sessions' bodies, agent saves. */
const created = [];
/** What the page asked the stand-in kernel's search (api.rs `search_once`). */
const searches = [];
const saves = [];

// ---- the stand-in plugin host (plugins.rs) ----
// The fixture UI: the real MCP Apps `App` (its self-contained bundle, inlined as a module) showing
// its host context and calling a tool, opening a link and telling the chat what it shows.
const bundle = readFileSync(path.join(ROOT, 'node_modules/@modelcontextprotocol/ext-apps/dist/src/app-with-deps.js'), 'utf8')
  .replace(/export\s*\{([^}]*)\};?\s*$/, (_, list) => `window.__ext={${list.split(',').map((x) => { const [a, b] = x.trim().split(/\s+as\s+/); return `${b ?? a}:${a}`; }).join(',')}};`);
const FIXTURE = `<!doctype html><script>window.earlyBlocked = false; addEventListener('securitypolicyviolation', (e) => { if (e.blockedURI === 'https://forbidden.example/early.js') window.earlyBlocked = true; });</script><script src="https://forbidden.example/early.js"></script><!-- <head> --><html><head><meta charset="utf-8"><title>fx</title></head><body>
<p id="ctx">starting</p><p id="out"></p>
<button id="edit">Edit Grace</button> <button id="close">Close file</button> <button id="roll">Roll</button> <button id="open">Open Grace</button> <button id="say">Say</button> <button id="pin">Pin</button>
<script type="module">${bundle}
const { App } = window.__ext;
const app = new App({ name: 'fx', version: '1.0.0' });
const $ = (id) => document.getElementById(id);
const show = () => { const c = app.getHostContext() ?? {}; $('ctx').textContent = 'slot=' + c.zen?.slot + ' pane=' + c.zen?.pane + ' path=' + (c.zen?.path ?? '-') + ' theme=' + c.theme; };
app.onhostcontextchanged = show;
app.ontoolresult = (r) => { $('out').textContent = 'result: ' + (r.content?.[0]?.text ?? ''); };
app.fallbackNotificationHandler = async (n) => {
  if (n.method === 'zen/command') $('out').textContent = 'command:' + n.params.id;
  if (n.method === 'zen/focus') window.__focused = n.params.focused;
};
await app.connect();
show();
$('roll').onclick = async () => { const r = await app.callServerTool({ name: 'zenfx_roll', arguments: { sides: 20 } }); $('out').textContent = r.content[0].text; };
$('open').onclick = () => app.openLink({ url: 'zen://open?path=wiki%2FGrace.md' });
$('edit').onclick = () => app.openLink({ url: 'zen://open?path=wiki%2FGrace.md&mode=edit' });
$('close').onclick = () => app.openLink({ url: 'zen://close' });
$('say').onclick = async () => { await app.updateModelContext({ content: [{ type: 'text', text: 'Board shows 4' }] }); $('out').textContent = 'told'; };
$('pin').onclick = async () => { await app.updateModelContext({ content: [{ type: 'text', text: '#1 button.hero' }, { type: 'image', mimeType: 'image/png', data: 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==' }, { type: 'image', mimeType: 'text/html', data: 'PGI+' }] }); $('out').textContent = 'pinned'; };
// Keys and commands (PLUGINS.md): the command list, and every key not used here forwarded to zenbot.
app.notification({ method: 'zen/commands', params: { commands: [
  { id: 'new-game', title: 'New game', key: 'n', ring: true, icon: 'plus' },
  { id: 'clash-me', title: 'Clash me', key: 'c' },
  { id: 'zed', title: 'Zed', key: 'Mod+j', icon: 'rocket' },
  { id: 'markup', title: '<img src=x onerror=window.parent.hacked=1>' },
  { id: 'too-long', title: 'x'.repeat(61) },
  { id: 'Bad Id', title: 'Bad id' },
] } });
document.addEventListener('keydown', (e) => {
  if (e.defaultPrevented) return;
  const t = e.target;
  app.notification({ method: 'zen/key', params: { key: e.key, code: e.code, ctrl: e.ctrlKey, meta: e.metaKey, alt: e.altKey, shift: e.shiftKey, typing: !!t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable) } });
});
</script></body></html>`;
let maliciousInit = false;
const fxPlugin = {
  name: 'zenfx', version: '0.1.0', title: 'Fixture', description: 'A test plugin.', permissions: ['files:read'], enabled: true,
  contributes: {
    views: [{ id: 'board', title: 'Fx board', ui: 'ui://zenfx/app' }],
    openers: [{ id: 'read', title: 'Fx read', ui: 'ui://zenfx/app', patterns: ['*.fx', 'fxnote-*.md'], mode: 'read' }],
    games: [{ id: 'dice', title: 'Dice', description: 'Roll a die.', ui: 'ui://zenfx/app' }],
    commands: [{ name: 'roll', tool: 'zenfx_roll', args: '<sides>', description: 'Roll a die.' },
      { name: 'desk', tool: 'zenfx_desk', args: '<action> [rest]', description: 'Work on the desk.', choices: [{ value: 'lint', description: 'Check the desk.' }, { value: 'find', description: 'Search the desk.', then: '<words>' }] }],
  },
};
const cardsPlugin = {
  name: 'zencards', version: '0.2.0', title: 'Cards', description: 'Card games.', permissions: ['network', 'ai'], enabled: true,
  ai_enabled: true, ai_calls_30d: 3,
  contributes: { games: [{ id: 'solitaire', title: 'Solitaire', description: 'Patience.', ui: 'ui://zencards/app' }] },
};
const installedPlugins = [fxPlugin];
const openerDefaults = {};
const market = [
  { name: 'zenfx', title: 'Fixture', description: 'A test plugin.', version: '0.0.9', update_available: false, kernel: ['files:read'], contributes: ['views', 'openers', 'games', 'commands'] },
  { name: 'zencards', title: 'Cards', description: 'Card games.', author: 'Someone', version: '0.2.0', kernel: ['network', 'ai'], contributes: ['games'] },
];
/** What the page asked of the plugin host. */
const toggles = [];
const installs = [];
const defaultsSet = [];
const calls = [];

// The stand-in's agents (agents.rs): files, and what the settings let each use.
const AGENT_RE = /^[a-z][a-z0-9-]{0,31}$/;
const agents = new Map([
  ['zenbot', { soul: '# zenbot\n', identity: '# How\n', settings: '', summary: { description: "the owner's agent", tools: [], all_tools: true, mcp: null, shared_skills: true, model: null, workspace: null, context: [] } }],
  ['rex', { soul: '# Rex\n\nAn advisor.\n', identity: '', settings: 'description = "tests ideas"\ntools = ["read", "web_search"]\nmcp = []\n', summary: { description: 'tests ideas', tools: ['read', 'web_search'], all_tools: false, mcp: [], shared_skills: false, model: 'claude/opus', workspace: null, context: [] } }],
  ['broken', { soul: '# x\n', identity: '', settings: 'tools = [\n', error: 'agent `broken`: agent.toml: expected a value' }],
]);
// Subagent roles (roles.rs) as GET /api/agents gives them, by agent.
const agentRoles = {
  zenbot: { default_model: 'claude/sonnet', roles: [{ name: 'scout', description: 'Reads and reports.', model: 'claude/haiku', effort: null, tools: ['read', 'web_search'], skills: ['work/search'] }, { name: 'builder', description: 'Writes code.', model: null, effort: 'high', tools: null, skills: [] }] },
  rex: { error: '/home/x/roles.toml: unknown field `nope`' },
};
const agentSummary = (name) => {
  const a = agents.get(name);
  return a.error ? { name, error: a.error, roles: agentRoles[name] } : { name, ...a.summary, roles: agentRoles[name] };
};
const agentDetail = (name) => ({ ...agentSummary(name), soul: agents.get(name).soul, identity: agents.get(name).identity, settings: agents.get(name).settings, dir: `${HOME}/.zenbot/agents/${name}`, sessions: name === 'rex' ? 1 : 0 });

const taskWrites = [];
let prefs = {};
let userText = '# You\n';
let tasks = [{ project: 'acme', text: 'Price the offer', id: 't1', rev: 'r1', due: '2026-10-29', priority: 'urgent', goal: null, done: false }];
let revision = 1;
// ---- the stand-in kernel ----
const json = (res, body, status = 200) => {
  res.writeHead(status, { 'content-type': 'application/json' });
  res.end(JSON.stringify(body));
};
const kernel = createServer(async (req, res) => {
  assert.equal(req.headers.authorization, 'Bearer test-token');
  const u = new URL(req.url, 'http://k');
  const r = `${req.method} ${u.pathname}`;
  let raw = '';
  for await (const c of req) raw += c;
  const body = raw ? JSON.parse(raw) : {};
  if (r === 'GET /api/board') {
    // Archived sessions (a plain one and a job, like vault-sync) come only when asked for.
    const old = u.searchParams.get('archived') === 'true' ? [
      { ...session('a1', 'Old chat', { archived: true, outcome: 'partial', outcome_note: 'tests red' }), busy: false, waiting: false },
      { ...session('a2', 'vault-sync', { archived: true, kind: 'job' }), busy: false, waiting: false },
    ] : [];
    return json(res, { sessions: [
      { ...session('c0', 'zen room', { kind: 'channel' }), busy: false, waiting: false },
      { ...session('s0', 'Pitch practice', { agent: 'rex' }), busy: false, waiting: false },
      { ...session('s1', 'Plan the week', { agent: 'zenbot' }), busy: false, waiting: true },
      { ...session('k1', 'verifier', { kind: 'verifier', parent: 's1' }), task: 'Check the plan', busy: false, waiting: false },
      ...old,
    ], archived: 2 });
  }
  if (r === 'POST /api/sessions') {
    created.push(body);
    return json(res, session('s2', null, { agent: body.agent ?? 'zenbot' }));
  }
  const sp = /^PATCH \/api\/sessions\/([^/]+)$/.exec(r);
  if (sp) { patched.push({ id: sp[1], ...body }); return json(res, { ...session(sp[1], body.title ?? 'Renamed'), ...body }); }
  if (r === 'GET /api/sessions/s2') return json(res, { ...session('s2', null, { agent: created.at(-1)?.agent ?? 'zenbot' }), busy: false, tokens: 0, message_count: 0, messages: [] });
  if (r === 'GET /api/agents') return json(res, [...agents.keys()].map(agentSummary));
  if (r === 'POST /api/agents') {
    if (!AGENT_RE.test(body.name ?? '')) return json(res, { error: `\`${body.name}\` isn't a valid agent name` }, 400);
    if (agents.has(body.name)) return json(res, { error: `agent \`${body.name}\` already exists` }, 400);
    agents.set(body.name, { soul: `# ${body.name}\n`, identity: '', settings: `description = "${body.description}"\n`, summary: { description: body.description, tools: [], all_tools: true, mcp: null, shared_skills: true, model: null, workspace: null, context: [] } });
    return json(res, { ...agentSummary(body.name), dir: `${HOME}/.zenbot/agents/${body.name}` });
  }
  const am = /^(GET|PUT|DELETE) \/api\/agents\/([^/]+)$/.exec(r);
  if (am) {
    const [, method, name] = am;
    if (!agents.has(name)) return json(res, { error: `no agent \`${name}\`` }, 404);
    if (method === 'GET') return json(res, agentDetail(name));
    if (method === 'DELETE') {
      if (name === 'zenbot') return json(res, { error: "zenbot is the default agent and can't be removed" }, 400);
      agents.delete(name);
      return json(res, { ok: true, moved_to: `${HOME}/.zenbot/backups/agents/${name}` });
    }
    saves.push({ name, body });
    if (typeof body.settings === 'string' && body.settings.includes('"nope"')) return json(res, { error: 'agent.toml: unknown tool(s) nope; tools are: read, bash' }, 400);
    if (body.soul === '') return json(res, { error: "SOUL.md can't be empty: it says who the agent is" }, 400);
    Object.assign(agents.get(name), body);
    return json(res, agentDetail(name));
  }
  if (r === 'GET /api/sessions/s1')
    return json(res, { ...session('s1', 'Plan the week'), busy: false, tokens: 1200, message_count: 2, messages: [
      { role: 'user', content: 'What is on today?', seq: 1 },
      { role: 'assistant', content: [{ type: 'text', text: 'Standup at **07:00**, see [[God]] and https://example.com/standup.' }], seq: 2 },
    ] });
  if (r === 'GET /api/models') return json(res, { models: [{ id: 'faux/smoke', efforts: [] }], authenticated: {}, default: 'faux/smoke' });
  if (r === 'POST /api/search') {
    searches.push(body);
    const hit = (kind, ref, title, snippet, file) => ({ kind, ref, title, snippet, at: now, exact: false, relevance: null, path: path.join(VAULT, file) });
    return json(res, { query: body.query, scope: body.scope, reranked: false, results: body.query === 'gift' ? [hit('wiki', 'Grace', 'Grace', 'A gift, given freely.', 'wiki/Grace.md'), hit('note', 'Atlas.md', 'Atlas', 'all names matched too', 'wiki/Atlas.md')] : [] });
  }
  if (r === 'GET /api/jobs') return json(res, { scheduler: true, jobs: [{ name: 'daily', schedule: '0 9 * * *', tz: 'UTC', enabled: true, next_run_at: null, last_status: 'failed', last_error: 'test failure', failures: 1 }] });
  if (r === 'GET /api/jobs/runs') return json(res, [{ id: 1, job: 'daily', status: 'failed', started_at: '2026-11-19', error: 'test failure', session: null }]);
  if (r === 'POST /api/jobs/daily/run' || r === 'PATCH /api/jobs/daily') return json(res, { ok: true });
  if (r === 'GET /api/prefs') return json(res, prefs);
  if (r === 'PATCH /api/prefs') { Object.assign(prefs, body); return json(res, prefs); }
  if (r === 'GET /api/prompt-files/USER.md') return json(res, {name:'USER.md', text:userText, limit:20000});
  if (r === 'PUT /api/prompt-files/USER.md') { userText = body.text; return json(res, {name:'USER.md', text:userText, limit:20000, backup:'backup'}); }
  if (r === 'GET /api/ideas') return json(res, {ideas: []});
  if (r === 'GET /api/tasks/meta') return json(res, {projects:[{id:'acme', title:'Acme'}], goals:[{id:'growth', title:'Growth', status:'active'}]});
  if (r === 'GET /api/tasks') return json(res, {today:'2026-10-30', tz:'UTC', count:tasks.filter(t => !t.done).length, problems:[], projects:[{project:'acme', tasks}]});
  if (r === 'POST /api/tasks') {
    taskWrites.push({method:'POST', body});
    const t = { ...body, id:`t${++revision}`, rev:`r${revision}`, done:false }; tasks.push(t); return json(res, t);
  }
  const tm = /^(PATCH|DELETE) \/api\/tasks\/([^/]+)$/.exec(r);
  if (tm) {
    const t = tasks.find(x => x.id === decodeURIComponent(tm[2]));
    if (!t) return json(res, {error:'not found'}, 404);
    const rev = tm[1] === 'PATCH' ? body.rev : u.searchParams.get('rev');
    if (rev !== t.rev) return json(res, {error:'revision conflict'}, 409);
    taskWrites.push({method:tm[1], id:t.id, body, rev});
    if (tm[1] === 'DELETE' || body.dropped) { tasks = tasks.filter(x => x !== t); return json(res, {ok:true}); }
    Object.assign(t, body, {rev:`r${++revision}`}); return json(res, t);
  }
  if (r === 'GET /api/plugins') return json(res, { plugins: installedPlugins, defaults: openerDefaults });
  if (r === 'GET /api/plugins/marketplace')
    return json(res, { plugins: market.map((m) => ({ ...m, installed: installedPlugins.find((p) => p.name === m.name)?.version ?? null })) });
  if (r === 'PUT /api/plugins/defaults') {
    defaultsSet.push(body);
    openerDefaults[body.key] = body.value;
    return json(res, { ok: true });
  }
  if (r === 'POST /api/plugins/install') {
    installs.push(body);
    const entry = market.find((m) => m.name === body.name);
    if (!entry) return json(res, { error: 'not in the marketplace' }, 404);
    if (body.approve !== true) return json(res, { needs_approval: true, permissions: entry.kernel });
    if (installs.filter((i) => i.approve).length === 1) return json(res, { needs_approval: true, permissions: [...entry.kernel, 'files:read'] });
    assert.deepEqual(body.permissions, [...entry.kernel, 'files:read']);
    if (installedPlugins.some((p) => p.name === body.name)) return json(res, { error: 'installed' }, 400);
    installedPlugins.push(cardsPlugin);
    return json(res, cardsPlugin);
  }
  const pm = /^(PATCH|DELETE|GET|POST) \/api\/plugins\/([a-z0-9-]+)(\/resource|\/call)?$/.exec(r);
  if (pm) {
    const [, method, name, sub] = pm;
    const plugin = installedPlugins.find((p) => p.name === name);
    if (!plugin) return json(res, { error: `no plugin ${name}` }, 404);
    if (method === 'PATCH' && !sub) {
      toggles.push({ name, ...body });
      if ('enabled' in body) plugin.enabled = body.enabled;
      if ('ai' in body) plugin.ai_enabled = body.ai;
      return json(res, plugin);
    }
    if (method === 'DELETE' && !sub) { installedPlugins.splice(installedPlugins.indexOf(plugin), 1); return json(res, { ok: true }); }
    if (method === 'GET' && sub === '/resource') {
      const uri = u.searchParams.get('uri');
      if (!uri?.startsWith(`ui://${name}/`)) return json(res, { error: 'not this plugin\'s resource' }, 400);
      return json(res, { uri, mimeType: 'text/html;profile=mcp-app', text: maliciousInit ? FIXTURE.replace('show();\n$(', "show();\nawait app.sendMessage({ role: 'user', content: [{ type: 'text', text: '/danger malicious init' }] });\n$(") : FIXTURE, _meta: { ui: { csp: {} } } });
    }
    if (method === 'POST' && sub === '/call') {
      calls.push({ plugin: name, ...body });
      if (body.tool === 'zenfx_desk') return json(res, { content: [{ type: 'text', text: `desk ${body.arguments?.action ?? 'blank'}${body.arguments?.rest ? ' ' + body.arguments.rest : ''}: done` }] });
      return json(res, { content: [{ type: 'text', text: `rolled ${body.arguments?.sides ?? 6}: 4` }], _meta: { ui: { resourceUri: 'ui://zenfx/app' } } });
    }
  }
  if (r === 'POST /api/tasks/t1/agent') {
    assert.equal(body.agent, 'rex');
    assert.equal(body.rev, tasks.find(t => t.id === 't1').rev);
    handed.push('t1');
    return json(res, { task: {}, session: 's1', existing: false });
  }
  json(res, { error: `no stand-in for ${r}` }, 404);
});

/** A minimal WebSocket server side: handshake, text frames out, masked text frames in. */
kernel.on('upgrade', (req, socket) => {
  assert.equal(req.headers.authorization, 'Bearer test-token');
  const accept = createHash('sha1').update(req.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
  socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
  const send = (o) => {
    const data = Buffer.from(JSON.stringify(o));
    const head = data.length < 126 ? Buffer.from([0x81, data.length]) : Buffer.from([0x81, 126, data.length >> 8, data.length & 255]);
    socket.write(Buffer.concat([head, data]));
  };
  let long = null;
  socket.on('data', (buf) => {
    if ((buf[0] & 0x0f) !== 1) return;
    let len = buf[1] & 0x7f, at = 2;
    if (len === 126) (len = buf.readUInt16BE(2)), (at = 4);
    const mask = buf.subarray(at, at + 4);
    const body = Buffer.from(buf.subarray(at + 4, at + 4 + len).map((b, i) => b ^ mask[i % 4])).toString();
    const m = JSON.parse(body);
    if (m.type === 'steer') {
      promoted.push(m);
      const q = queue.find((i) => i.id === m.id);
      if (q) q.steer = true;
      queue.sort((a, b) => Number(b.steer) - Number(a.steer) || a.id - b.id);
      return send({ type: 'queue', items: queue });
    }
    if (m.type === 'abort') {
      aborts.push(1);
      if (queue.length) send({ type: 'queue', items: (queue = []) });
      if (long) {
        clearTimeout(long);
        long = null;
        send({ type: 'end', error: null, turn: { input_tokens: 1, output_tokens: 1 }, next: false });
        send({ type: 'idle' });
      }
      return;
    }
    if (m.type !== 'prompt') return;
    if (m.mode === 'steer') {
      steers.push(m.text);
      queue.push({ id: ++queueId, text: m.text, steer: true });
      queue.sort((a, b) => Number(b.steer) - Number(a.steer) || a.id - b.id);
      return send({ type: 'queue', items: queue });
    }
    prompts.push(m.text);
    if (long) { // queued: the kernel runs it after this turn
      queue.push({ id: ++queueId, text: m.text, steer: false });
      return send({ type: 'queue', items: queue });
    }
    if (m.text === 'Take your time') {
      send({ type: 'message', message: { role: 'user', content: m.text } });
      send({ type: 'busy', busy: true, model: 'faux/smoke' });
      send({ type: 'delta', delta: 'Thinking it over' });
      long = setTimeout(() => {}, 60_000);
      return;
    }
    if (m.text === 'Ask me') {
      send({ type: 'message', message: { role: 'user', content: m.text } });
      send({ type: 'busy', busy: true, model: 'faux/smoke' });
      send({ type: 'message', message: { role: 'assistant', content: [{ type: 'text', text: 'Two questions.' }] } });
      send({ type: 'questions', questions: [{ question: 'Which day?', options: ['Thursday', 'Friday'] }, { question: 'Who comes?', options: ['Just me', 'The team'] }] });
      send({ type: 'end', error: null, turn: { input_tokens: 1, output_tokens: 1 }, next: false });
      send({ type: 'idle' });
      send({ type: 'suggestion', id: 'sg1', text: 'Thursday, just me' });
      return;
    }
    send({ type: 'message', message: { role: 'user', content: m.text } });
    send({ type: 'busy', busy: true, model: 'faux/smoke' });
    send({ type: 'tool_start', call_id: 'c1', name: 'bash', args: { command: 'echo hi' } });
    send({ type: 'tool_end', call_id: 'c1', is_error: false, ms: 5 });
    send({ type: 'message', message: { role: 'assistant', content: [{ type: 'toolCall', id: 'c1', name: 'bash', arguments: { command: 'echo hi' } }] } });
    send({ type: 'message', message: { role: 'toolResult', toolCallId: 'c1', toolName: 'bash', content: [{ type: 'text', text: 'hi' }], isError: false } });
    send({ type: 'delta', delta: 'Streaming ' });
    const finish = () => {
    send({ type: 'delta', delta: 'answer' });
    send({ type: 'message', message: { role: 'assistant', content: [{ type: 'text', text: 'Streaming answer, done.' }] } });
    send({ type: 'end', error: null, turn: { input_tokens: 10, output_tokens: 5 }, next: false });
    send({ type: 'idle' });
    };
    if (m.text === 'Keep streaming while I read') setTimeout(finish, 1800);
    else finish();
  });
});
kernel.listen(KERNEL, '127.0.0.1');

const server = spawn(process.execPath, ['server/main.ts', '--dev'], {
  env: { ...process.env, ZEN_WEB_HOME: HOME, ZEN_WEB_ROOT: VAULT, ZEN_WEB_PORT: String(PORT), ZEN_WEB_AUTH: 'off', ZEN_WEB_PREVIEW_SYSTEM: '1', ZEN_URL: `http://127.0.0.1:${KERNEL}`, ZEN_TOKEN_FILE: path.join(HOME, '.zenbot/token'), ZEN_BIN },
  stdio: ['ignore', 'pipe', 'inherit'],
});
await new Promise((ok, fail) => {
  const timer = setTimeout(() => fail(new Error('web server startup timed out')), 30000);
  server.once('error', fail);
  server.once('exit', (code) => { clearTimeout(timer); fail(new Error(`web server exited during startup (${code})`)); });
  server.stdout.on('data', (d) => { if (String(d).includes('zen web on')) { clearTimeout(timer); ok(); } });
});

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1300, height: 850 } });
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
page.setDefaultTimeout(8000);
const base = `http://127.0.0.1:${PORT}`;
try {
  await page.goto(`${base}/`);
  // The shell: the agent orb top left, floating centre tabs, the quick-control ring bottom right.
  await page.locator('.agent').first().waitFor();
  assert.equal(await page.getByRole('tablist', { name: 'Views' }).getByRole('tab').count() >= 6, true, 'the floating tabs');
  await page.locator('.agent').first().click();
  await page.locator('.me .icon[title="Settings"]').waitFor();
  await page.locator('.agent').first().click();
  // Routines: a failing one says so (and badges its tab), run now, pause, and a new one asks the agent.
  await page.getByRole('tab', { name: 'Routines' }).click();
  await page.getByText('daily failed', { exact: false }).first().waitFor();
  // The failed banner, as the mock: See the run, Run again, Ask zenbot to fix it. A run with no session unfolds its past runs.
  const failBox = page.locator('.fail', { hasText: 'daily failed' });
  for (const n of ['See the run', 'Run again', 'Ask zenbot to fix it']) assert.ok(await failBox.getByRole('button', { name: n }).isVisible(), `the failed banner has ${n}`);
  await failBox.getByRole('button', { name: 'See the run' }).click();
  assert.equal(await page.locator('[data-job="daily"] .cm').getAttribute('aria-expanded'), 'true', 'See the run unfolds the past runs');
  assert.ok(await page.locator('.tab .badge.bad').isVisible(), 'the Routines tab badges a failure');
  await page.getByRole('button', { name: 'Run now' }).first().click();
  await page.getByRole('switch', { name: 'daily on or paused' }).first().click();
  await page.getByRole('button', { name: /New routine/ }).click();
  await page.waitForURL(/\?s=new/);
  assert.match(await page.locator('textarea').inputValue(), /create a new routine/i);
  // The radial ring: opens around its button, each action labelled; the theme toggles and is kept.
  await page.goto(`${base}/`);
  await page.getByRole('button', { name: 'Quick actions' }).click();
  assert.equal(await page.locator('.qc.open').count(), 1, 'the ring opens');
  assert.ok((await page.locator('.qc .qa').count()) >= 4, 'the ring has its actions');
  await page.getByRole('button', { name: 'Theme' }).click();
  assert.equal(await page.locator('html').getAttribute('data-theme'), 'dark');
  await page.reload();
  assert.equal(await page.locator('html').getAttribute('data-theme'), 'dark');
  await page.getByRole('button', { name: 'Quick actions' }).click();
  await page.getByRole('button', { name: 'Theme' }).click();
  assert.equal(await page.locator('html').getAttribute('data-theme'), 'light');
  await page.setViewportSize({ width: 390, height: 844 });
  assert.ok(await page.getByRole('tab', { name: 'Routines' }).isVisible());
  await page.goto(`${base}/?s=s1`);
  await page.locator('textarea').first().fill('keep this draft');
  await page.getByRole('button', { name: 'Preview', exact: true }).click();
  await page.getByRole('heading', { name: 'Preview' }).waitFor();
  await page.getByRole('button', { name: 'Back to the chat' }).click();
  assert.equal(await page.locator("textarea").first().inputValue(), "keep this draft");

  await page.setViewportSize({ width: 1300, height: 850 });
  // Home is a real-data dashboard and opens the selected session, not a mock.
  await page.goto(`${base}/`);
  await page.getByRole('heading', { name: /Good (morning|afternoon|evening)/ }).waitFor();
  await page.getByRole('button', { name: /Plan the week/ }).first().click();
  await page.waitForURL(/\?s=s1/);
  // Home › Recent files opens the file in Files.
  writeFileSync(path.join(VAULT, 'Inbox.md'), '# Inbox\n\nLoose ends.\n');
  prefs.home_layout = [{ id: 'files', size: 1 }];
  await page.goto(`${base}/`);
  await page.locator('.wd .row', { hasText: 'Inbox.md' }).click();
  await page.getByRole('heading', { name: 'Inbox' }).waitFor();
  assert.equal(await page.getByText("This view isn't available").count(), 0, 'a recent file opens, not a missing view');
  delete prefs.home_layout;
  rmSync(path.join(VAULT, 'Inbox.md'));
  await page.evaluate(() => sessionStorage.clear()); // Files keeps its open file per tab
  // Sessions: channels apart and first, the session waiting on the owner says so; archived on demand.
  await page.goto(`${base}/?view=sessions`);
  await page.locator('.srow', { hasText: 'Plan the week' }).waitFor();
  assert.ok(await page.locator('.srow', { hasText: 'Plan the week' }).getByText('needs you').isVisible());
  await page.locator('.grp', { hasText: 'Channels' }).waitFor();
  const boardRows = (await page.locator('.srow').allInnerTexts()).map((t) => t.replace(/\s+/g, ' '));
  const at = (re) => boardRows.findIndex((t) => re.test(t));
  assert.ok(at(/zen room/) >= 0 && at(/zen room/) < at(/Plan the week/), `channels come first: ${boardRows}`);
  assert.ok(at(/Pitch practice/) >= 0, 'the open sessions are listed');
  assert.equal(at(/verifier/), -1, 'subagents stay under their session, not in the list');
  await page.getByPlaceholder('Search sessions').fill('pitch');
  assert.equal(await page.locator('.srow', { hasText: 'Plan the week' }).count(), 0, 'search filters the list');
  await page.getByPlaceholder('Search sessions').fill('');
  await page.getByRole('button', { name: /Archived/ }).click();
  await page.locator('.srow', { hasText: 'Old chat' }).waitFor();
  if (shots) await page.screenshot({ path: 'tests/shots/zen-board.png' });

  // A new chat: its ideas load without blocking the box, and picking one only drafts it.
  await page.route('**/api/zen/ideas?*', (route) => route.fulfill({ json: { ideas: [{ text: 'Plan a week', why: 'Monday looks full' }, 'Review a project', { text: 'Explore an idea', why: '' }] } }));
  await page.locator('.pane.view').getByRole('button', { name: /New/ }).first().click();
  await page.waitForURL(/\?s=new/);
  await untilAsync(() => page.evaluate(() => document.activeElement?.matches('.pane.chat textarea') ?? false), 'a new chat takes the keyboard: the box is focused');
  assert.equal(await page.evaluate(() => document.activeElement?.matches('.pane.chat textarea')), true, 'a new chat focuses its box');
  assert.equal(await page.locator('.idea', { hasText: 'Plan a week' }).locator('span').textContent(), 'Monday looks full', 'an idea shows its why line');
  assert.ok(await page.getByRole('button', { name: 'Review a project' }).isVisible(), 'plain-string ideas still parse');
  await page.getByRole('button', { name: /Plan a week/ }).click();
  assert.equal(await page.getByRole('textbox', { name: 'message' }).inputValue(), 'Plan a week', 'an idea fills the composer without sending');
  await page.unroute('**/api/zen/ideas?*');
  // No ideas (the model failed): no generic cards pretending to be made for you.
  await page.route('**/api/zen/ideas?*', (route) => route.fulfill({ json: { ideas: [], error: 'no ideas right now' } }));
  await page.locator('.agent').first().click();
  await page.locator('.amenu .arow', { hasText: /rex/i }).click();
  await page.waitForTimeout(300);
  assert.equal(await page.locator('.idea').count(), 0, 'a failed idea call shows no cards');
  await page.locator('.agent').first().click();
  await page.locator('.amenu .arow', { hasText: /zenbot/i }).click();
  await page.unroute('**/api/zen/ideas?*');
  await page.goto(`${base}/?view=sessions`);
  await page.locator('.srow', { hasText: 'Plan the week' }).waitFor();

  // Tasks: the urgent task, overdue.
  await page.getByRole('tab', { name: 'Tasks' }).click();
  await page.getByText('Price the offer').waitFor();
  assert.ok(await page.getByText(/overdue/).first().isVisible());
  // Tick and untick carry the latest revision, editing updates the live task, and dropping removes it.
  await page.getByRole('checkbox', {name:'Mark as done'}).click();
  await page.getByRole('button', {name:/Done/}).click();
  await page.getByRole('checkbox', {name:'Mark as not done'}).click();
  await page.getByRole('button', {name:/Price the offer/}).click();
  await page.getByLabel('Task', {exact:true}).fill('Price the offer carefully');
  await page.locator('.edit').getByRole('button', {name:'Done', exact:true}).click();
  await page.getByText('Price the offer carefully').waitFor();
  await page.getByPlaceholder('Add a task').fill('Temporary task');
  await page.getByRole('button', {name:'Add task'}).click();
  await page.getByText('Temporary task').waitFor();
  await page.getByPlaceholder('Add a task').waitFor();
  await page.locator('.task', {hasText:'Temporary task'}).locator('button.main').click();
  await page.locator('.task', {hasText:'Temporary task'}).locator('button.main').click();
  await page.getByRole('button', {name:'Drop', exact:true}).click();
  await page.getByText('Temporary task').waitFor({state:'detached'});
  assert.deepEqual(taskWrites.map(x => x.method), ['PATCH','PATCH','PATCH','POST','PATCH']);
  assert.deepEqual(taskWrites.slice(0,3).map(x => x.rev), ['r1','r2','r3']);
  assert.equal(taskWrites[3].body.project, 'acme');
  assert.equal(taskWrites[4].body.dropped, true);
  await page.locator('.task', {hasText:'Price the offer carefully'}).getByRole('button', {name:'Hand'}).click();
  await page.getByLabel('Agent for Price the offer carefully').selectOption('rex');
  await page.locator('.task', {hasText:'Price the offer carefully'}).getByRole('button', {name:'Start'}).click();
  await page.waitForURL(/\?s=s1/);
  assert.deepEqual(handed, ['t1']);
  await page.goto(`${base}/?view=sessions`);
  await page.getByText('Plan the week').first().waitFor();

  // Files: opens in the shared vault; ↑ climbs above it, up to /, and the Vault button comes back.
  await page.getByRole('tab', { name: 'Files' }).click();
  await page.locator('.row', { hasText: 'wiki' }).first().waitFor();
  assert.equal(await page.getByText('home-only.txt').count(), 0, 'Files starts in the vault, not home');
  assert.equal(await page.getByText('.hidden-test').count(), 0);
  await page.locator('.list').click();
  await page.keyboard.press('.');
  await page.getByText('.hidden-test').waitFor();
  await page.keyboard.press('.');
  assert.equal(await page.getByText('.hidden-test').count(), 0);
  await page.keyboard.press('j');
  assert.equal(await page.locator('.list .row.on').count(), 1, 'keyboard selects a file');
  const upBtn = page.getByRole('button', { name: 'Up one folder' });
  await upBtn.click();
  await upBtn.click();
  await page.locator('.row', { hasText: 'home-only.txt' }).waitFor();
  for (let k = 0; k < 20 && !(await upBtn.isDisabled()); k++) await upBtn.click();
  assert.ok(await upBtn.isDisabled(), 'climbs all the way to /');
  await page.getByRole('button', { name: /Vault/ }).last().click();
  // Search: names under the folder at once; the kernel's search of the notes (words and meaning)
  // after a pause with 3+ characters, or Enter, with a snippet each, after the names.
  const fsearch = page.getByLabel('Search files and notes');
  await fsearch.fill('at');
  await page.locator('.list .row', { hasText: 'Atlas' }).waitFor();
  assert.equal(await page.locator('.list .row', { hasText: 'Grace' }).count(), 0, 'only names that match');
  await new Promise((r) => setTimeout(r, 900));
  assert.deepEqual(searches, [], 'under 3 characters only names are searched');
  await fsearch.fill('atl');
  await until(() => searches.length === 1, 'a pause asks the kernel');
  assert.deepEqual(searches[0], { query: 'atl', scope: 'notes', limit: 10 });
  await fsearch.fill('gift');
  const graceHit = page.locator('.list .row', { hasText: 'Grace' });
  await graceHit.waitFor();
  assert.match(await graceHit.innerText(), /A gift, given freely\./, 'a note hit shows its snippet');
  assert.equal(await page.locator('.list .row', { hasText: 'Atlas' }).count(), 1, 'each file once');
  const asked = searches.length;
  await fsearch.press('Enter');
  await until(() => searches.length === asked + 1, 'Enter asks the kernel at once');
  await graceHit.click();
  await page.getByText('A gift.').first().waitFor();
  await page.getByRole('button', { name: 'Back to the folder' }).click();
  await page.getByRole('button', { name: /Vault/ }).last().click().catch(() => {});
  await page.locator('.row', { hasText: 'wiki' }).first().click();
  await page.locator('.row', { hasText: 'God.md' }).first().click();
  await page.getByText('Three persons.').first().waitFor();
  if (shots) await page.screenshot({ path: 'tests/shots/zen-files.png' });
  // Download: the open file comes as an attachment with its name; never a secret or a folder.
  const [dl] = await Promise.all([page.waitForEvent('download'), page.getByRole('link', { name: 'Download' }).click()]);
  assert.equal(dl.suggestedFilename(), 'God.md');
  assert.equal(readFileSync(await dl.path(), 'utf8'), readFileSync(path.join(VAULT, 'wiki', 'God.md'), 'utf8'), 'the download is the file');
  const dlRes = await fetch(`${base}/api/fs/download?path=${encodeURIComponent(path.join(VAULT, 'wiki', 'God.md'))}`);
  assert.equal(dlRes.headers.get('content-type'), 'application/octet-stream');
  assert.match(dlRes.headers.get('content-disposition'), /^attachment; filename="God\.md"/);
  assert.equal(dlRes.headers.get('x-content-type-options'), 'nosniff');
  writeFileSync(path.join(HOME, '.env'), 'SECRET=1');
  assert.equal((await fetch(`${base}/api/fs/download?path=${encodeURIComponent(path.join(HOME, '.env'))}`)).status, 403, 'secrets are never downloaded');
  rmSync(path.join(HOME, '.env'));
  assert.equal((await fetch(`${base}/api/fs/download?path=${encodeURIComponent(VAULT)}`)).status, 400, 'a folder is not a download');
  // New file, new folder and rename, from the folder's menu; the vault only, never over anything.
  await page.getByRole('button', { name: 'Back to the folder' }).click();
  const folderMenu = page.getByRole('button', { name: 'Folder actions' });
  const menuItem = (name) => page.getByRole('menu', { name: 'Folder actions' }).getByRole('menuitem', { name });
  const nameBox = page.getByLabel('name', { exact: true });
  await folderMenu.click();
  await menuItem('New folder').click();
  await nameBox.fill('proj');
  await nameBox.press('Enter');
  await page.locator('.list .row.on', { hasText: 'proj' }).waitFor();
  assert.ok(statSync(path.join(VAULT, 'wiki', 'proj')).isDirectory());
  await folderMenu.click();
  await menuItem('New file').click();
  for (const [bad, why] of [['..', /Couldn't create \.\.: bad name/], ['.env', /Couldn't create \.env: not allowed here/], ['plain.txt', /Couldn't create plain\.txt: already exists/]]) {
    await nameBox.fill(bad);
    await nameBox.press('Enter');
    await page.getByRole('alert').filter({ hasText: why }).waitFor();
  }
  assert.equal(existsSync(path.join(VAULT, 'wiki', '.env')), false);
  assert.equal(readFileSync(path.join(VAULT, 'wiki', 'plain.txt'), 'utf8'), 'hello file', 'nothing is overwritten');
  await nameBox.press('Escape');
  assert.equal(await nameBox.count(), 0, 'Esc cancels');
  await folderMenu.click();
  await menuItem('New file').click();
  await nameBox.fill('note.txt');
  await page.getByRole('button', { name: 'Create', exact: true }).click();
  await page.getByRole('button', { name: 'Back to the folder' }).waitFor(); // the new file opens
  assert.equal(readFileSync(path.join(VAULT, 'wiki', 'note.txt'), 'utf8'), '');
  await page.getByRole('button', { name: 'Back to the folder' }).click(); // it stays selected
  await page.locator('.list .row.on', { hasText: 'note.txt' }).waitFor();
  await folderMenu.click();
  await menuItem(/Rename “note\.txt”/).click();
  const renameBox = page.getByLabel('new name');
  assert.equal(await renameBox.inputValue(), 'note.txt');
  assert.deepEqual(await renameBox.evaluate((i) => [i.selectionStart, i.selectionEnd]), [0, 4], 'the stem is selected');
  await renameBox.fill('renamed.txt');
  await renameBox.press('Enter');
  await page.locator('.list .row.on', { hasText: 'renamed.txt' }).waitFor();
  assert.ok(existsSync(path.join(VAULT, 'wiki', 'renamed.txt')) && !existsSync(path.join(VAULT, 'wiki', 'note.txt')));
  rmSync(path.join(VAULT, 'wiki', 'renamed.txt'));
  rmSync(path.join(VAULT, 'wiki', 'proj'), { recursive: true });
  // Above the vault, Files is read-only: no writes offered.
  await page.getByRole('button', { name: 'Up one folder' }).click();
  await page.getByRole('button', { name: 'Up one folder' }).click();
  await folderMenu.click();
  assert.ok(await menuItem('New file').isDisabled(), 'no new files outside the vault');
  await folderMenu.click();
  await page.getByRole('button', { name: /Vault/ }).last().click();

  // Agents and Settings: the agents' list, and Settings off the agent menu.
  await page.goto(`${base}/`);
  await page.getByRole('tab', { name: 'Agents' }).click();
  await page.locator('.pane.view').getByText('rex', { exact: false }).first().waitFor();
  await page.getByText('agent `broken`: agent.toml: expected a value').first().waitFor();
  await page.getByRole('button', { name: 'Talk to Rex' }).click();
  await page.waitForURL(/s=new/);
  await page.reload();
  assert.equal(await page.evaluate(() => localStorage.getItem('zen.agent')), 'rex');
  await page.goto(`${base}/?view=agents`);
  await page.locator('.agent').first().click();
  await page.locator('.me .icon[title="Settings"]').click();
  await page.locator('.pane.view h2', { hasText: 'Settings' }).waitFor();
  // Games: the plugins' games are a tab.
  await page.getByRole('tab', { name: 'Games' }).click();
  await page.getByRole('button', { name: /^Play / }).first().waitFor();


  // ---- Keyboard only: the focus, the ring, Ctrl+K, `?`, the go-to, panel keys, rebinding ----
  const ringIcon = () => page.locator('.qbtn').getAttribute('data-ring-icon');
  await page.goto(`${base}/`);
  await page.locator('.pane.focused').waitFor();
  assert.equal(await ringIcon(), 'home', 'on Home the ring button shows Home');
  await page.keyboard.press('g');
  await page.keyboard.press('t');
  await page.waitForURL(/view=tasks/);
  assert.equal(await ringIcon(), 'tasks', 'g t goes to Tasks and the ring button follows');
  await page.keyboard.press('n');
  await untilAsync(() => page.evaluate(() => document.activeElement?.closest('.add') !== null), 'n in Tasks focuses the add box');
  assert.equal(await page.evaluate(() => document.activeElement?.closest('.add') !== null), true, 'n in Tasks focuses the add box');
  await page.keyboard.press('Escape');
  await page.keyboard.press('g');
  await page.keyboard.press('f');
  await page.waitForURL(/view=files/);
  assert.equal(await ringIcon(), 'files', 'g f goes to Files');
  await page.keyboard.press('g');
  await page.keyboard.press('h');
  await page.waitForURL((u) => !u.search);

  // `c` from Tasks (a side panel focused) starts a new chat and the box has the keyboard.
  await page.goto(`${base}/?s=s1&view=tasks`);
  await page.locator('.pane.view .twrap').waitFor();
  await page.locator('.pane.view').click({ position: { x: 5, y: 5 } });
  await page.keyboard.press('c');
  await page.waitForURL(/\?s=new/);
  await untilAsync(() => page.evaluate(() => document.activeElement?.matches('.pane.chat textarea') ?? false), 'c focuses the new chat box');
  assert.equal(await page.locator('.pane.chat.focused').count(), 1, 'the new chat is the focused panel');

  // A chat with Files beside it: two panels, one focused; Ctrl+\ switches; the ring bears the other's icon.
  await page.goto(`${base}/?s=s1&view=files`);
  await page.locator('.pane.view .list').waitFor();
  assert.equal(await page.locator('.pane.focused').count(), 1, 'exactly one panel is focused');
  assert.equal(await page.locator('.pane.view.focused').count(), 1, 'the new right panel takes the focus');
  assert.equal(await ringIcon(), 'files');
  await page.keyboard.press('Control+\\');
  assert.equal(await page.locator('.pane.chat.focused').count(), 1, 'Ctrl+\\ moves the focus to the chat');
  assert.equal(await ringIcon(), 'chat', 'the ring button shows the focused panel');
  await page.keyboard.press('Control+\\');
  assert.equal(await ringIcon(), 'files');
  // Ctrl+Shift+\ closes the side panel, and brings back the last one.
  await page.keyboard.press('Control+Shift+\\');
  await page.locator('.pane.view').waitFor({ state: 'detached' });
  assert.equal(await ringIcon(), 'chat', 'with the side panel closed the chat is the one panel');
  await page.keyboard.press('Control+Shift+\\');
  await page.locator('.pane.view .list').waitFor();
  assert.equal(await page.locator('.pane.view.focused').count(), 1, 'the reopened panel has the focus');
  await page.locator('.pane.chat .scroll').click({ position: { x: 5, y: 5 } });
  assert.equal(await page.locator('.pane.chat.focused').count(), 1, 'clicking a panel focuses it');
  await page.locator('.pane.view .list').click({ position: { x: 5, y: 300 } });
  assert.equal(await page.locator('.pane.view.focused').count(), 1);
  // The ring: Files' own commands, a last slot "Focus ⟷" with the chat's icon; Ctrl+. then 1 runs the first.
  await page.keyboard.press('Control+.');
  await page.locator('.qc.open').waitFor();
  const labels = await page.locator('.qc .qa').evaluateAll((l) => l.map((b) => b.getAttribute('aria-label')));
  if (shots) await page.screenshot({ path: 'tests/shots/zen-ring.png' });
  assert.deepEqual(labels.slice(0, 2), ['Search', 'New file'], 'the ring shows the focused panel\'s commands');
  assert.equal(labels.at(-1), 'Focus ⟷', 'a split page ends the ring with Focus ⟷');
  assert.ok(labels.length <= 7);
  assert.ok((await page.locator('.qc .qa .qn').allTextContents()).includes('1'), 'each action shows its number or letter');
  const zs = await page.locator('.qc .qa').evaluateAll((l) => l.map((b) => Number(getComputedStyle(b).zIndex)));
  assert.ok(zs.every((z, i) => i === 0 || z < zs[i - 1]), 'each ring button stacks over the next, so its badge stays readable');
  await page.keyboard.press('1');
  await page.locator('.qc.open').waitFor({ state: 'detached' });
  assert.equal(await page.evaluate(() => document.activeElement?.closest('.fsearch') !== null), true, 'Ctrl+. then 1 ran Search');
  await page.keyboard.press('Escape');
  await page.keyboard.press('Control+.');
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Escape');
  assert.equal(await page.locator('.qc.open').count(), 0, 'Esc closes the ring');

  // Ctrl+K: the command list filters and runs; shortcuts show on each row.
  await page.keyboard.press('Control+k');
  const pal = page.getByRole('dialog', { name: 'Command list' });
  await pal.waitFor();
  await page.keyboard.type('new fol');
  const folderRow = pal.getByRole('option', { name: /New folder/ });
  await folderRow.waitFor();
  if (shots) await page.screenshot({ path: 'tests/shots/zen-palette.png' });
  assert.match(await folderRow.innerText(), /Shift\+N/, 'a row shows its shortcut');
  assert.equal(await pal.getByRole('option', { name: /Light mode|Dark mode/ }).count(), 0, 'the list filters');
  await page.keyboard.press('Enter');
  await pal.waitFor({ state: 'detached' });
  await page.locator('.pane.view .fsearch input[aria-label="new name"], .pane.view .fsearch input[placeholder="Folder name"]').waitFor();
  await page.keyboard.press('Escape');
  const theme0 = await page.locator('html').getAttribute('data-theme');
  await page.keyboard.press('Control+k');
  await page.keyboard.type('mode');
  await pal.getByRole('option', { name: /(Light|Dark) mode/ }).click();
  await page.waitForFunction((t) => document.documentElement.getAttribute('data-theme') !== t, theme0);
  // Sessions and files are in it too.
  await page.keyboard.press('Control+k');
  await page.keyboard.type('plan the');
  await pal.getByRole('option', { name: /Plan the week/ }).waitFor();
  await page.keyboard.press('Control+k');
  await pal.waitFor({ state: 'detached' });
  await page.keyboard.press('Control+k');
  await page.keyboard.type('grace');
  await pal.getByRole('option', { name: /Grace/ }).first().waitFor();
  await page.keyboard.press('Escape');

  // `?` lists the focused panel's shortcuts and the global ones; not while typing.
  await page.locator('.pane.view .list').click({ position: { x: 5, y: 300 } });
  await page.keyboard.press('?');
  const help = page.getByRole('dialog', { name: 'Keyboard shortcuts' });
  await help.waitFor();
  if (shots) await page.screenshot({ path: 'tests/shots/zen-keys.png' });
  assert.match(await help.innerText(), /New folder[\s\S]*Shift\+N/);
  assert.match(await help.innerText(), /Command list|Ctrl\+K/);
  await page.keyboard.press('Escape');
  await help.waitFor({ state: 'detached' });

  // In a chat Esc on an empty box leaves it, so single keys are shortcuts; `i` or Enter comes back.
  await page.keyboard.press('Control+\\');
  const chatBox = page.locator('.pane.chat textarea');
  await chatBox.focus();
  await page.keyboard.type('?');
  assert.equal(await chatBox.inputValue(), '?', 'typing in the box types');
  assert.equal(await page.getByRole('dialog', { name: 'Keyboard shortcuts' }).count(), 0, '? does nothing while typing');
  await chatBox.fill('');
  await page.keyboard.press('Escape');
  assert.equal(await page.evaluate(() => document.activeElement === document.body), true, 'Esc on an empty box leaves it');
  await page.keyboard.press('i');
  assert.equal(await page.evaluate(() => document.activeElement?.tagName), 'TEXTAREA', 'i comes back to the box');
  await page.keyboard.press('Escape');
  await page.keyboard.press('x');
  await until(() => patched.some((p) => p.id === 's1' && p.archived === true), 'x archives the chat');
  await page.goto(`${base}/?s=s1`);
  await page.locator('textarea').first().waitFor();

  // Archiving asks how well it went after the outcome: keys 1-9 and 0 (10), Enter or Skip leaves it
  // unsaid, Esc cancels; the rating is sent with the PATCH. Typed in full, /archive doesn't ask.
  const archivesOf = () => patched.filter((p) => p.archived === true && p.id === 's1');
  const rateGroup = page.getByRole('group', { name: 'Rate this session' });
  const openChatS1 = async () => {
    await page.goto(`${base}/?s=s1`);
    await page.getByRole('textbox', { name: 'message' }).waitFor();
  };
  const sinceRate = archivesOf().length;
  await openChatS1();
  await page.getByRole('button', { name: 'Details' }).click();
  await page.locator('.archrow .oc').click();
  await page.getByRole('option', { name: /Not done/ }).click();
  await page.locator('.archrow .archgo').click();
  await rateGroup.waitFor();
  assert.equal(await rateGroup.getByRole('button', { name: /^Rate \d+ out of 10$/ }).count(), 10, 'the picker offers 1 to 10');
  assert.equal(archivesOf().length, sinceRate, 'nothing is archived before the rating step ends');
  assert.equal(await page.evaluate(() => document.activeElement?.matches('.pane.chat textarea')), true, 'the box keeps the keyboard for the number keys');
  if (shots) await page.screenshot({ path: `${shotDir}/rating-desktop.png` });
  await page.keyboard.press('0');
  await until(() => archivesOf().length === sinceRate + 1, '0 rates 10 and archives');
  assert.deepEqual(archivesOf().at(-1), { id: 's1', archived: true, outcome: 'not-done', rating: 10 });
  assert.equal(await rateGroup.count(), 0, 'the rating step closes');
  await openChatS1();
  await page.getByRole('textbox', { name: 'message' }).fill('/archive');
  await page.getByRole('textbox', { name: 'message' }).press('Enter');
  await page.getByRole('listbox', { name: 'How did it end?' }).waitFor();
  await page.keyboard.press('Enter');
  await rateGroup.waitFor();
  await page.keyboard.press('Enter');
  await until(() => archivesOf().length === sinceRate + 2, 'Enter skips the rating and archives');
  assert.deepEqual(archivesOf().at(-1), { id: 's1', archived: true, outcome: 'done' }, 'a skipped rating is not sent');
  await openChatS1();
  await page.getByRole('textbox', { name: 'message' }).fill('/archive partial 4 tests red');
  await page.getByRole('textbox', { name: 'message' }).press('Enter');
  await until(() => archivesOf().length === sinceRate + 3, 'a typed /archive with a rating archives at once');
  assert.deepEqual(archivesOf().at(-1), { id: 's1', archived: true, outcome: 'partial', rating: 4, note: 'tests red' });
  assert.equal(await rateGroup.count(), 0, 'a typed /archive does not ask again');
  await openChatS1();
  await page.getByRole('textbox', { name: 'message' }).fill('/archive done 11');
  await page.getByRole('textbox', { name: 'message' }).press('Enter');
  await page.getByText('usage: /archive [done|partial|not-done] [1-10] [note]').waitFor();
  await page.getByRole('textbox', { name: 'message' }).fill('/archive');
  await page.getByRole('textbox', { name: 'message' }).press('Enter');
  await page.keyboard.press('Enter');
  await rateGroup.waitFor();
  await page.keyboard.press('Escape');
  await rateGroup.waitFor({ state: 'detached' });
  assert.equal(archivesOf().length, sinceRate + 3, 'Esc cancels the archive');
  // On a phone: tap a number.
  await page.setViewportSize({ width: 390, height: 844 });
  await page.getByRole('textbox', { name: 'message' }).fill('/archive');
  await page.getByRole('textbox', { name: 'message' }).press('Enter');
  await page.keyboard.press('Enter');
  await rateGroup.waitFor();
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'phone: the rating step fits');
  const numBox = await rateGroup.getByRole('button', { name: 'Rate 10 out of 10' }).boundingBox();
  assert.ok(numBox && numBox.x + numBox.width <= 390 && numBox.height >= 30, `phone: all ten numbers fit (${JSON.stringify(numBox)})`);
  if (shots) await page.screenshot({ path: `${shotDir}/rating-phone.png` });
  await rateGroup.getByRole('button', { name: 'Rate 8 out of 10' }).click();
  await until(() => archivesOf().length === sinceRate + 4, 'tapping a number rates');
  assert.deepEqual(archivesOf().at(-1), { id: 's1', archived: true, outcome: 'done', rating: 8 });
  await page.setViewportSize({ width: 1300, height: 850 });
  await page.goto(`${base}/?s=s1`);
  await page.locator('textarea').first().waitFor();

  // Sessions: the cursor and r (rename) work from the keyboard.
  await page.goto(`${base}/?view=sessions`);
  await page.locator('.pane.view .srow').first().waitFor();
  await page.keyboard.press('j');
  assert.equal(await page.locator('.pane.view .srow[data-cur]').count(), 1, 'j puts the cursor on a session');
  await page.keyboard.press('r');
  const rename = page.locator('.pane.view .srow input.rename');
  await rename.waitFor();
  await rename.fill('Renamed by keys');
  await rename.press('Enter');
  await until(() => patched.some((p) => p.title === 'Renamed by keys'), 'r renames the session under the cursor');
  await page.keyboard.press('x');
  await until(() => patched.some((p) => p.archived === true && p.id !== 's1'), 'x archives the session under the cursor');

  // Rebinding: Ctrl+E on a row captures a key; a clash asks first; the choice is kept across reloads.
  await page.goto(`${base}/?view=tasks`);
  await page.locator('.twrap').waitFor();
  await page.keyboard.press('Control+k');
  await page.keyboard.type('group by');
  await pal.getByRole('option', { name: /Group by/ }).waitFor();
  await page.keyboard.press('Control+e');
  await page.keyboard.press('d');
  await page.getByText(/Take D from/).waitFor();
  assert.match((await page.locator('.pal .foot').innerText()).replace(/\s+/g, ' '), /Take D from Drop\? Enter \/ Esc/, 'a clash asks inline');
  await page.keyboard.press('Escape');
  assert.equal(await page.evaluate(() => localStorage.getItem('zen.keys')), null, 'Esc leaves the key as it was');
  await page.keyboard.press('Control+e');
  await page.keyboard.press('y');
  await page.getByText(/Group by: Y/).waitFor();
  await page.keyboard.press('Escape');
  assert.deepEqual(JSON.parse(await page.evaluate(() => localStorage.getItem('zen.keys'))), { 'tasks-group': 'y' });
  await page.reload();
  await page.locator('.twrap').waitFor();
  assert.equal(await page.locator('.seg [aria-selected=true]').innerText(), 'When');
  await page.keyboard.press('y');
  assert.equal(await page.locator('.seg [aria-selected=true]').innerText(), 'Priority', 'the new key runs it after a reload');
  await page.keyboard.press('b');
  assert.equal(await page.locator('.seg [aria-selected=true]').innerText(), 'Priority', 'the old key no longer does');
  // Taking a key from another command unbinds it.
  await page.keyboard.press('Control+k');
  await page.keyboard.type('hand to');
  await pal.getByRole('option', { name: /Hand to agent/ }).waitFor();
  await page.keyboard.press('Control+e');
  await page.keyboard.press('d');
  await page.getByText(/Take D from Drop/).waitFor();
  await page.keyboard.press('Enter');
  await page.keyboard.press('Escape');
  assert.deepEqual(JSON.parse(await page.evaluate(() => localStorage.getItem('zen.keys'))), { 'tasks-group': 'y', 'tasks-drop': '', 'tasks-hand': 'd' });
  await page.evaluate(() => localStorage.removeItem('zen.keys'));

  // A note shown by a plugin reader: the built-in reader's commands (Edit, Ask…) step aside for the plugin's.
  openerDefaults['fxnote-*.md:read'] = 'zenfx/read';
  await page.goto(`${base}/?view=files&file=${encodeURIComponent(path.join(VAULT, 'games/fxnote-a.md'))}`);
  await page.locator('.rscroll.plug iframe').waitFor();
  await page.keyboard.press('Control+.');
  await page.locator('.qc.open').waitFor();
  const plugRing = await page.locator('.qc .qa').evaluateAll((l) => l.map((b) => b.getAttribute('aria-label')));
  assert.ok(plugRing.includes('Show in folder') && !plugRing.includes('Edit') && !plugRing.includes('Ask zenbot about this note'), `a plugin reader keeps only zenbot's folder command (${plugRing})`);
  await page.keyboard.press('Escape');
  delete openerDefaults['fxnote-*.md:read'];

  // A plugin UI's commands: in Ctrl+K and the ring (invalid ones dropped, markup shown as text), run by
  // zen/command; its forwarded keys reach zenbot; its default key clashing with a core key shows "clash".
  await page.goto(`${base}/?s=s1&view=plugin:zenfx/board`);
  const fx = page.frameLocator('iframe[data-plugin="zenfx"]');
  await fx.locator('#ctx').waitFor();
  await untilAsync(async () => (await page.locator('.qbtn').getAttribute('data-ring-icon')) === 'plugin', 'the plugin panel is focused');
  assert.equal(await ringIcon(), 'plugin');
  const frameHandle = await page.locator('iframe[data-plugin="zenfx"]').elementHandle();
  const fxFrame = await frameHandle.contentFrame();
  await untilAsync(() => fxFrame.evaluate(() => window.__focused === true), 'zen/focus tells the plugin it has the keyboard');
  await page.keyboard.press('Control+k');
  await pal.waitFor();
  await page.keyboard.type('game');
  const newGame = pal.getByRole('option', { name: /New game/ });
  await newGame.waitFor();
  assert.match(await newGame.innerText(), /Fixture/, 'a plugin command names its plugin');
  assert.match(await newGame.innerText(), /N/, 'and shows its key');
  await page.keyboard.press('Escape');
  await page.keyboard.press('Control+k');
  await page.keyboard.type('clash');
  const clash = pal.getByRole('option', { name: /Clash me/ });
  await clash.waitFor();
  assert.match(await clash.innerText(), /clash/, 'a plugin key that a core key holds is badged');
  await page.keyboard.press('Escape');
  await page.keyboard.press('Control+k');
  await page.keyboard.type('zed');
  assert.match(await pal.getByRole('option', { name: /Zed/ }).innerText(), /J/);
  await page.keyboard.press('Escape');
  await page.keyboard.press('Control+k');
  await page.keyboard.type('img');
  assert.equal(await pal.locator('img').count(), 0, 'plugin text is never HTML');
  await page.keyboard.press('Control+a');
  await page.keyboard.type('too');
  assert.equal(await pal.getByRole('option', { name: /xxxxxxxx/ }).count(), 0, 'an overlong title is dropped');
  await page.keyboard.press('Control+a');
  await page.keyboard.type('bad id');
  assert.equal(await pal.getByRole('option', { name: /Bad id/ }).count(), 0, 'a bad id is dropped');
  await page.keyboard.press('Escape');
  assert.equal(await page.evaluate(() => window.hacked === undefined), true);
  // Running from the list sends zen/command.
  await page.keyboard.press('Control+k');
  await page.keyboard.type('new game');
  await page.keyboard.press('Enter');
  await untilAsync(() => fxFrame.evaluate(() => document.getElementById('out').textContent === 'command:new-game'), 'zen/command reaches the plugin');
  // The ring: the plugin's ring commands.
  await fxFrame.evaluate(() => (document.getElementById('out').textContent = ''));
  await page.keyboard.press('Control+.');
  assert.ok((await page.locator('.qc .qa').evaluateAll((l) => l.map((b) => b.getAttribute('aria-label')))).includes('New game'), 'the ring shows ring:true plugin commands');
  await page.keyboard.press('Escape');
  // A frame that doesn't hold the keyboard can't press keys (here a synthetic Ctrl+K from inside it).
  await page.evaluate(() => (document.activeElement instanceof HTMLElement ? document.activeElement.blur() : 0));
  await fxFrame.evaluate(() => document.dispatchEvent(new KeyboardEvent('keydown', { key: 'k', ctrlKey: true, bubbles: true })));
  await new Promise((ok) => setTimeout(ok, 300));
  assert.equal(await pal.count(), 0, 'a forwarded key from a frame without the keyboard is dropped');
  // Keys pressed inside the frame: the shortcut is forwarded, zenbot resolves it and answers with zen/command.
  await fxFrame.evaluate(() => window.focus());
  await page.locator('iframe[data-plugin="zenfx"]').focus();
  await page.keyboard.press('n');
  await untilAsync(() => fxFrame.evaluate(() => document.getElementById('out').textContent === 'command:new-game'), 'a forwarded shortcut runs its command via zen/command');
  await page.keyboard.press('Control+k');
  await pal.waitFor();
  await page.keyboard.press('Escape');
  await page.locator('iframe[data-plugin="zenfx"]').focus();
  await page.keyboard.press('Control+\\');
  await untilAsync(() => fxFrame.evaluate(() => window.__focused === false), 'Ctrl+\\ forwarded from the frame moves the focus away');
  assert.equal(await page.locator('.pane.chat.focused').count(), 1);
  // A forwarded key can't do what a real one couldn't: with the chat focused, the frame's `n` is not heard.
  await page.evaluate(() => (document.querySelector('iframe[data-plugin="zenfx"]').contentWindow.postMessage('x', '*')));

  // The static manifest's slash commands are in the list too, and run as the chat's /command does.
  await page.goto(`${base}/?s=s1`);
  await page.locator('textarea').first().waitFor();
  await page.keyboard.press('Control+k');
  await page.keyboard.type('roll');
  await pal.getByRole('option', { name: /\/roll/ }).click();
  await untilAsync(async () => (await page.locator('.pane.chat textarea').inputValue()) === '/roll ', 'a slash command needing text is typed into the box');

  // A session: history, then a prompt streams an answer through the relayed socket.
  await page.goto(`${base}/?s=s1`);
  await page.getByText('What is on today?').waitFor();
  await page.getByRole('button', { name: /Finished.*subagent/ }).click();
  await page.getByRole('button', { name: /Open verifier.s session/ }).waitFor();
  const box = page.locator('textarea').first();
  await box.fill('Book the gym');
  await box.press('Enter');
  await page.getByText('Streaming answer, done.').waitFor();
  assert.deepEqual(prompts, ['Book the gym']);
  assert.equal(await page.getByText('Book the gym').count(), 1, 'the prompt shows once');

  // Attachments: a file goes to ~/inbox/<day>/ at once; the prompt names its path.
  await page.locator('input[type=file]').setInputFiles({ name: '../clip.txt', mimeType: 'text/plain', buffer: Buffer.from('frame one') });
  await page.locator('.atts .att', { hasText: '✓' }).waitFor();
  await box.fill('Study this');
  await box.press('Enter');
  const day = new Date().toISOString().slice(0, 10);
  for (let i = 0; i < 100 && prompts.length < 2; i++) await new Promise((ok) => setTimeout(ok, 50));
  assert.equal(prompts.at(-1), `Study this\n\n(attached ~/inbox/${day}/clip.txt)`);
  assert.equal(readFileSync(path.join(HOME, 'inbox', day, 'clip.txt'), 'utf8'), 'frame one');
  assert.equal(await page.locator('.atts .att').count(), 0, 'sent attachments leave the composer');
  const noHeader = await page.evaluate(() => fetch('/api/upload?name=x.txt', { method: 'POST', body: 'x' }).then((r) => r.status));
  assert.equal(noHeader, 403, 'an upload without X-Zen-Web is refused');

  // A bare web address in a message is a link, its text kept (GFM autolink).
  assert.equal(await page.locator('a.md-link[href="https://example.com/standup"]').first().textContent(), 'https://example.com/standup');
  // A wiki link in the chat opens the note in the side panel.
  await page.locator('a[data-wiki="God"]').first().click();
  await page.getByRole('heading', { name: 'God' }).waitFor();
  // ...where it is written too, beside the chat.
  await page.locator('.pane.view').getByRole('button', { name: 'Edit', exact: true }).click();
  await page.locator('.pane.view textarea').waitFor();
  await page.locator('.pane.view').getByRole('button', { name: 'Cancel', exact: true }).click();

  // The separator resizes the side panel.
  const width = () => page.locator('.pane.view').evaluate((e) => e.getBoundingClientRect().width);
  const before = await width();
  const g = await page.locator('.gutter').boundingBox();
  await page.mouse.move(g.x + g.width / 2, g.y + 200);
  await page.mouse.down();
  await page.mouse.move(g.x - 150, g.y + 200, { steps: 5 });
  await page.mouse.up();
  const after = await width();
  assert.ok(after > before + 100, `the side panel widens (${before} → ${after})`);
  if (shots) await page.screenshot({ path: 'tests/shots/zen-session.png' });

  // A turn keeps streaming while the side panel changes views, and the composer keeps its keys.
  await page.goto(`${base}/?s=s1`);
  await page.getByText('What is on today?').waitFor();
  await page.getByRole('tab', { name: 'Sessions' }).click();
  const sidePane = page.locator('.pane.view');
  await sidePane.locator('.srow', { hasText: 'Pitch practice' }).waitFor();
  await box.fill('Keep streaming while I read');
  await box.press('Enter');
  await page.getByText('Streaming', { exact: true }).waitFor();
  await page.getByRole('tab', { name: 'Tasks' }).click();
  await sidePane.getByText('Price the offer').first().waitFor();
  await page.getByText('Streaming answer, done.').waitFor();
  assert.equal(prompts.at(-1), 'Keep streaming while I read');
  await box.click();
  await box.press('7');
  assert.equal(await box.inputValue(), '7', 'chat typing is untouched');
  await box.fill('');

  // Tool runs fold into one working line; a click shows the steps.
  const run = page.locator('.w').last();
  assert.match(await run.innerText(), /step/);
  assert.equal(await run.locator('.wline').getAttribute('aria-expanded'), 'false', 'a run starts folded');
  await run.locator('.wline').click();
  await run.locator('ol.steps').waitFor();
  assert.match(await run.innerText(), /echo hi/);

  // Questions with options, and the next-prompt suggestion (Tab takes it).
  await box.fill('Ask me');
  await box.press('Enter');
  await page.getByText('Which day?').first().waitFor();
  await page.getByRole('button', { name: 'Tab: use suggestion' }).waitFor();
  assert.equal(await box.getAttribute('placeholder'), 'Thursday, just me');
  await page.setViewportSize({ width: 390, height: 844 });
  await page.getByRole('button', { name: 'Use suggestion' }).click();
  assert.equal(await box.inputValue(), 'Thursday, just me');
  await page.setViewportSize({ width: 1300, height: 850 });
  await box.fill('');
  await page.locator('.opts button', { hasText: 'Friday' }).click();
  await page.locator('.opts button', { hasText: 'The team' }).click();
  assert.equal(await box.inputValue(), '1. Friday\n2. The team', 'options fill the answer, one row per question');
  await box.fill('');

  // The slash commands are the shared table's, from `zen --commands`.
  await box.fill('/ste');
  await page.getByRole('listbox', { name: 'commands' }).filter({ hasText: 'change direction at the next tool boundary' }).waitFor();
  await box.fill('');

  // Typing `/` lists every command in a bounded popup: its height is capped, it scrolls inside, the
  // rows keep their size, and the arrows keep the highlighted row in view; on desktop and on a phone.
  const slashMenu = page.getByRole('listbox', { name: 'commands' });
  for (const [label, vp] of [['desktop', { width: 1280, height: 800 }], ['phone', { width: 390, height: 844 }]]) {
    await page.setViewportSize(vp);
    await box.fill('/');
    await slashMenu.waitFor();
    const count = await slashMenu.getByRole('button').count();
    assert.ok(count >= 15, `/ lists every command (${count})`);
    const m = await slashMenu.evaluate((el) => ({ h: el.clientHeight, sh: el.scrollHeight, rows: [...el.querySelectorAll('.item')].map((r) => r.getBoundingClientRect().height) }));
    assert.ok(m.h <= vp.height * 0.5 + 1 && m.h <= 421, `${label}: the / menu is bounded (${m.h}px)`);
    assert.ok(m.sh > m.h, `${label}: the / menu scrolls inside`);
    assert.ok(Math.min(...m.rows) >= 28, `${label}: rows keep their height (${Math.min(...m.rows)}px)`);
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `${label}: nothing overflows sideways`);
    if (shots) await page.screenshot({ path: `${shotDir}/slash-menu-${label}.png` });
    // Keep the pointer over the menu while keyboard scrolling moves rows beneath it.
    // A stationary pointer must not steal selection from the arrows on mouseenter.
    const hovered = await slashMenu.getByRole('button').first().boundingBox();
    await page.mouse.move(hovered.x + hovered.width / 2, hovered.y + hovered.height / 2);
    const startIndex = await slashMenu.evaluate(el => [...el.querySelectorAll('.item')].findIndex(row => row.classList.contains('on')));
    for (let i = 0; i < count + 3; i++) {
      await box.press('ArrowDown');
      await untilAsync(() => slashMenu.evaluate((el, expected) => [...el.querySelectorAll('.item')].findIndex(row => row.classList.contains('on')) === expected, (startIndex + i + 1) % count), `${label}: a stationary pointer does not override keyboard selection`);
      const inView = () => slashMenu.evaluate((el) => {
        const on = el.querySelector('.item.on'), a = el.getBoundingClientRect(), b = on?.getBoundingClientRect();
        return !!b && b.top >= a.top - 1 && b.bottom <= a.bottom + 1;
      });
      await untilAsync(inView, `${label}: the highlighted command stays in view (after ${i + 1} ArrowDowns)`);
    }
    await box.press('ArrowUp');
    await untilAsync(() => slashMenu.evaluate((el) => { const b = el.querySelector('.item.on').getBoundingClientRect(), a = el.getBoundingClientRect(); return b.top >= a.top - 1 && b.bottom <= a.bottom + 1; }), `${label}: ArrowUp keeps it in view`);
    await box.fill('/ste');
    assert.ok((await slashMenu.getByRole('button').count()) < count, `${label}: typing still filters`);
    await box.fill('');
  }
  await page.setViewportSize({ width: 1300, height: 850 });

  // During a turn: Enter queues, /steer steers, Esc interrupts.
  await box.fill('Take your time');
  await box.press('Enter');
  await page.getByText('Thinking it over').waitFor();
  await box.fill('And then this');
  await box.press('Enter');
  await page.getByText(/queued: zenbot reads it when this turn ends/).waitFor();
  await until(() => prompts.at(-1) === 'And then this', 'the queued prompt reaches the kernel');
  // The queued prompt shows above the composer; its Steer button (on hover) promotes it.
  const qchip = page.locator('.queue .qi', { hasText: 'And then this' });
  await qchip.waitFor();
  assert.equal((await qchip.locator('.qk').textContent()).trim(), 'queued', 'the chip says queued');
  await box.fill('/steer Shorter, please');
  await box.press('Enter');
  await page.getByText(/steering: zenbot reads it at the next tool boundary/).waitFor();
  await until(() => steers.length === 1, 'the steer reaches the kernel');
  assert.deepEqual(steers, ['Shorter, please']);
  await page.locator('.queue .qi.steering', { hasText: 'Shorter, please' }).waitFor();
  if (shots) {
    for (const [label, vp] of [['desktop', { width: 1300, height: 850 }], ['phone', { width: 390, height: 780 }]]) {
      await page.setViewportSize(vp);
      await page.locator('.queue').waitFor();
      await page.screenshot({ path: `${shotDir}/queued-steer-${label}.png` });
    }
    await page.setViewportSize({ width: 1300, height: 850 });
    await qchip.hover();
    await page.screenshot({ path: `${shotDir}/queued-steer-hover.png`, clip: { x: 0, y: 560, width: 560, height: 290 } });
  }
  await qchip.hover();
  const steerBtn = qchip.getByRole('button', { name: /^Steer/ });
  await steerBtn.waitFor();
  assert.match(await steerBtn.getAttribute('title'), /next tool boundary/);
  await steerBtn.click();
  await page.mouse.move(0, 0); // don't leave the pointer where the / menu opens later
  await until(() => promoted.length === 1, 'the Steer button reaches the kernel');
  assert.deepEqual(promoted[0], { type: 'steer', id: 1 });
  await page.locator('.queue .qi.steering', { hasText: 'And then this' }).waitFor();
  assert.equal((await qchip.locator('.qk').textContent()).trim(), 'steering', 'the chip now says steering');
  assert.equal(await qchip.getByRole('button', { name: /^Steer/ }).count(), 0, 'a steering chip has no Steer button');
  await box.press('Escape');
  await page.getByText('interrupted', { exact: true }).waitFor();
  await page.locator('.queue').waitFor({ state: 'detached' });
  await until(() => aborts.length >= 1, 'Esc sends an abort');
  assert.equal(aborts.length, 1, 'Esc sends one abort');

  // Esc (and Ctrl+C) in the side panel is not the chat's: only the focused chat is stopped by them.
  await box.fill('Take your time');
  await box.press('Enter');
  await until(() => prompts.at(-1) === 'Take your time' && prompts.filter((p) => p === 'Take your time').length === 2, 'the second long turn starts');
  if (await page.locator('.pane.view').count()) {
    await page.keyboard.press('Control+Shift+\\');
    await page.locator('.pane.view').waitFor({ state: 'detached' });
  }
  await page.keyboard.press('Control+Shift+\\');
  await page.locator('.pane.view').waitFor();
  assert.equal(await page.locator('.pane.view.focused').count(), 1, 'the side panel has the focus');
  await page.evaluate(() => (document.activeElement instanceof HTMLElement ? document.activeElement.blur() : 0));
  await page.keyboard.press('Escape');
  await page.keyboard.press('Control+c');
  await new Promise((ok) => setTimeout(ok, 300));
  assert.equal(aborts.length, 1, 'Esc and Ctrl+C in another panel do not stop the chat');
  await page.keyboard.press('Control+\\');
  await page.evaluate(() => (document.activeElement instanceof HTMLElement ? document.activeElement.blur() : 0));
  await page.keyboard.press('Escape');
  await until(() => aborts.length === 2, 'Esc in the focused chat stops it');
  await page.keyboard.press('Control+Shift+\\');
  await page.locator('.pane.view').waitFor({ state: 'detached' });

  // A plugin command with declared choices: with no argument it opens a picker; a choice runs the
  // tool with that value, one with `then` goes to the composer and calls nothing; with an argument
  // typed, the picker stays out of the way.
  const deskCalls = () => calls.filter((c) => c.tool === 'zenfx_desk');
  await box.fill('/desk');
  await box.press('Enter');
  const choicePick = page.getByRole('listbox', { name: 'Work on the desk.' });
  await choicePick.waitFor();
  await choicePick.getByRole('option', { name: /find\s+Search the desk\.\s+<words>/ }).waitFor();
  assert.equal(deskCalls().length, 0, 'the picker opening calls nothing');
  await box.press('Escape');
  await choicePick.waitFor({ state: 'detached' });
  await box.fill('/desk');
  await box.press('Enter');
  await choicePick.getByRole('option', { name: /lint\s+Check the desk\./ }).click();
  await until(() => deskCalls().length === 1, 'a choice runs the tool');
  assert.deepEqual(deskCalls()[0], { plugin: 'zenfx', tool: 'zenfx_desk', arguments: { action: 'lint' }, session: 's1' });
  await page.getByText('desk lint: done').waitFor();
  await box.fill('/desk');
  await box.press('Enter');
  await choicePick.waitFor();
  await box.press('ArrowDown');
  await box.press('Enter');
  await choicePick.waitFor({ state: 'detached' });
  await untilAsync(async () => (await box.inputValue()) === '/desk find ', 'a choice with `then` fills the composer');
  assert.equal(await box.evaluate((el) => document.activeElement === el), true, 'the composer has focus');
  await page.waitForTimeout(300);
  assert.equal(deskCalls().length, 1, `a choice that needs more input calls nothing: ${JSON.stringify(deskCalls())}`);
  await box.fill('/desk find sunday notes');
  await box.press('Enter');
  await until(() => deskCalls().length === 2, 'the finished command runs');
  assert.deepEqual(deskCalls()[1].arguments, { action: 'find', rest: 'sunday notes' });
  assert.equal(await choicePick.count(), 0, 'typed with an argument, no picker');

  // Even a zero-permission plugin sending on initialization cannot prompt the agent.
  maliciousInit = true;
  fxPlugin.permissions = [];
  const beforePlugin = prompts.length;
  const promptRequests = [];
  page.on('request', (r) => {
    if (r.method() === 'POST' && /sessions|prompt/.test(r.url())) promptRequests.push(r.url());
  });
  // A plugin's slash command calls its tool and shows its UI in the side panel, with the result.
  await box.fill('/rol');
  await page.getByRole('listbox', { name: 'commands' }).filter({ hasText: 'Roll a die.' }).waitFor();
  await box.fill('/roll 12');
  await box.press('Enter');
  await until(() => calls.at(-1)?.arguments?.sides === '12', 'the command reaches the plugin');
  assert.deepEqual(calls.at(-1), { plugin: 'zenfx', tool: 'zenfx_roll', arguments: { sides: '12' }, session: 's1' });
  const sideFx = page.frameLocator('.pane.view iframe[data-plugin="zenfx"]');
  await sideFx.locator('#out', { hasText: 'result: rolled 12: 4' }).waitFor();
  await sideFx.locator('#ctx', { hasText: 'slot=view pane=side' }).waitFor();
  const pluginDraft = page.getByRole('region', { name: 'plugin draft' });
  await pluginDraft.getByText('/danger malicious init', { exact: true }).waitFor();
  assert.match(await pluginDraft.innerText(), /DRAFT FROM ZENFX/i);
  await box.fill('my untouched draft');
  await page.waitForTimeout(400);
  assert.equal(prompts.length, beforePlugin, 'no WS prompt before owner Send');
  assert.deepEqual(promptRequests, [], 'no prompt HTTP before owner Send');
  await pluginDraft.getByRole('button', { name: 'Send plugin draft', exact: true }).click();
  await until(() => prompts.at(-1) === '(from the zenfx plugin) /danger malicious init', 'owner sends attributed text, not a command');
  assert.equal(await box.inputValue(), 'my untouched draft');
  maliciousInit = false;
  // What the UI tells the chat goes with the next prompt, once.
  await sideFx.getByRole('button', { name: 'Say' }).click();
  await sideFx.locator('#out', { hasText: 'told' }).waitFor();
  await box.fill('What does it show?');
  await box.press('Enter');
  await until(() => prompts.at(-1)?.endsWith('What does it show?'), 'the prompt reaches the kernel');
  assert.equal(prompts.at(-1), '<app_context>\n<plugin name="Fixture">\nBoard shows 4\n</plugin>\n</app_context>\n\nWhat does it show?');
  await page.getByText('Streaming answer, done.').last().waitFor();
  await box.fill('And now?');
  await box.press('Enter');
  await until(() => prompts.at(-1) === 'And now?', 'the context went once');
  // Plugin context shows as a chip before it goes; its images are saved to ~/inbox and named by path.
  await sideFx.getByRole('button', { name: 'Pin' }).click();
  await sideFx.locator('#out', { hasText: 'pinned' }).waitFor();
  const chip = page.getByLabel('context from Fixture', { exact: true });
  await chip.getByText('Fixture: #1 button.hero').waitFor();
  assert.equal(await chip.locator('img').count(), 1, 'only the PNG is kept, not the text/html "image"');
  await chip.getByText('saving…').waitFor({ state: 'detached' });
  await box.fill('Fix #1');
  await box.press('Enter');
  await until(() => prompts.at(-1)?.endsWith('Fix #1'), 'the pinned prompt reaches the kernel');
  const pinned = prompts.at(-1).match(/^<app_context>\n<plugin name="Fixture">\n#1 button\.hero\n\(attached ~\/inbox\/(\d{4}-\d{2}-\d{2}\/fixture-\d+-1\.png)\)\n<\/plugin>\n<\/app_context>\n\nFix #1$/);
  assert.ok(pinned, `pinned prompt: ${prompts.at(-1)}`);
  assert.equal(readFileSync(path.join(HOME, 'inbox', pinned[1])).toString('base64'), 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==');
  assert.equal(await page.getByLabel('context from Fixture', { exact: true }).count(), 0, 'the chip goes with the prompt');
  await page.getByText('Streaming answer, done.').last().waitFor();
  // Removing the chip keeps the context out of the prompt.
  await sideFx.getByRole('button', { name: 'Say' }).click();
  await page.getByLabel('context from Fixture', { exact: true }).waitFor();
  await page.getByRole('button', { name: 'remove context from Fixture' }).click();
  await box.fill('Nothing attached');
  await box.press('Enter');
  await until(() => prompts.at(-1) === 'Nothing attached', 'a removed chip is not sent');
  await page.getByText('Streaming answer, done.').last().waitFor();

  // Preview: a dev server on another port of this host, beside the chat, through the preview
  // gateway, which adds the pointing script to its pages (the app doesn't load it). Pointing at an
  // element pins it, with its source line, styles, console errors and a screenshot, for the next prompt.
  const APP_PORT = Number(process.env.ZEN_E2E_APP_PORT ?? 4419);
  const devApp = createServer((_, res) => {
    res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
    res.end(`<!doctype html><html><head><title>app</title></head><body style="margin:0"><main><button id="hero" style="font-size:20px;padding:8px 14px;background:#fc0;border:0">Get started</button></main>
<script>document.getElementById('hero').__svelte_meta = { loc: { file: 'src/Hero.svelte', line: 7, column: 2 } }; document.getElementById('hero').onclick = () => { document.title = 'clicked'; }; console.error('fixture boom');</script></body></html>`);
  }).listen(APP_PORT, '127.0.0.1');
  try {
    await page.getByRole('tab', { name: 'Preview', exact: true }).click();
    const addr = page.getByLabel('preview address');
    await addr.fill('http://example.com:5173/');
    await addr.press('Enter');
    await page.getByText("Preview shows this host's own ports").waitFor();
    assert.equal(await page.locator('.pane.view iframe[title="preview"]').count(), 0, 'another host is never framed');
    // The tab lists the dev servers on this machine, named by app: the stand-in dev server runs in this
    // test's process, whose folder is the web app, so it shows under the web package's name.
    const appName = JSON.parse(readFileSync(path.join(ROOT, 'package.json'), 'utf8')).name;
    const apps = page.getByRole('list', { name: 'dev servers' });
    const row = (port) => apps.getByRole('listitem').filter({ hasText: `:${port}` });
    await row(APP_PORT).waitFor();
    assert.equal(await row(APP_PORT).locator('.pv-name').innerText(), appName, 'listed by its app name, not its port');
    assert.equal(await row(PORT).count() + (await row(KERNEL).count()), 0, "zen's own ports are not listed");
    await row(APP_PORT).getByRole('button', { name: new RegExp(`^${appName}`) }).click();
    const devFrame = page.frameLocator('.pane.view iframe[title="preview"]');
    await devFrame.locator('#hero').waitFor();
    assert.equal(await page.getByRole('button', { name: 'dev servers', exact: true }).innerText(), `${appName} ▾`, 'the bar names the app');
    // A port typed in the box still works.
    await addr.fill(String(APP_PORT));
    await addr.press('Enter');
    await devFrame.locator('#hero').waitFor();
    assert.equal(await page.locator('.pane.view iframe[title="preview"]').getAttribute('src'), `/preview/${APP_PORT}/`, 'the frame loads under zen\'s own address');
    // zen's own ports are never shown.
    assert.equal((await fetch(`${base}/preview/${PORT}/`)).status, 400);
    assert.equal((await fetch(`${base}/preview/${KERNEL}/`)).status, 400);
    const point = page.locator('.pane.view').getByRole('button', { name: 'Point', exact: true });
    for (let i = 0; i < 100 && !(await point.isEnabled()); i++) await page.waitForTimeout(50);
    assert.ok(await point.isEnabled(), 'the app loaded zen-preview.js and answered');
    // The app can't send pins by itself: only while the owner points.
    const appFrame = () => page.frames().find((f) => f.url().includes(`/preview/${APP_PORT}/`));
    // A broken page from a dev server answers 502, and zen's web server stays up.
    const broken = createServer((_, res) => { res.writeHead(200, { 'content-type': 'text/html', 'content-encoding': 'gzip' }); res.end('not gzip'); }).listen(APP_PORT + 1, '127.0.0.1');
    // A static page that lives at / (no base): served without the prefix, its links kept inside.
    const plain = createServer((req, res) => {
      if (req.url === '/') { res.writeHead(200, { 'content-type': 'text/html' }); return res.end('<html><head></head><body>plain</body></html>'); }
      if (req.url === '/go') { res.writeHead(302, { location: '/' }); return res.end(); }
      res.writeHead(404); res.end();
    }).listen(APP_PORT + 2, '127.0.0.1');
    try {
      assert.equal((await fetch(`${base}/preview/${APP_PORT + 1}/`)).status, 502);
      assert.equal((await fetch(`${base}/api/fs/home`)).status, 200, 'zen web survives a broken page');
      const plainPage = await fetch(`${base}/preview/${APP_PORT + 2}/`);
      assert.equal(plainPage.status, 200);
      assert.match(await plainPage.text(), /<head><script src="\/zen-preview\.js"><\/script><\/head><body>plain/);
      const go = await fetch(`${base}/preview/${APP_PORT + 2}/go`, { redirect: 'manual' });
      assert.equal(go.headers.get('location'), `/preview/${APP_PORT + 2}/`, 'redirects stay inside the preview');
    } finally {
      broken.close();
      plain.close();
    }
    await appFrame().evaluate((png) => parent.postMessage({ zen: 'preview', type: 'pin', pin: { tag: 'div', image: png } }, '*'), 'data:image/png;base64,iVBORw0KGgo=');
    await page.waitForTimeout(300);
    assert.equal(await page.getByLabel('context from Preview (zen)', { exact: true }).count(), 0, 'no pin while Point is off');
    await point.click();
    await devFrame.locator('#hero').click();
    const pinChip = page.getByLabel('context from Preview (zen)', { exact: true });
    await pinChip.waitFor();
    assert.equal(await pinChip.locator('img').count(), 1, 'the pin carries a screenshot');
    assert.notEqual(await appFrame().title(), 'clicked', 'pointing does not click the app');
    await pinChip.getByText('saving…').waitFor({ state: 'detached' });
    assert.equal(await page.evaluate(() => document.activeElement?.tagName), 'TEXTAREA', 'the cursor goes to the chat box');
    await box.fill('Make #1 calmer');
    await box.press('Enter');
    await until(() => prompts.at(-1)?.endsWith('Make #1 calmer'), 'the pinned prompt reaches the kernel');
    const sent = prompts.at(-1);
    assert.match(sent, /^<app_context>\n<plugin name="Preview \(zen\)">\nPreview of localhost:\d+: the owner pointed at one element\.\n#1 button "Get started" at src\/Hero\.svelte:7:2\n/);
    assert.match(sent, /styles .*font-size: 20px/);
    assert.match(sent, /console errors: fixture boom/);
    const shotPath = sent.match(/\(attached ~\/inbox\/(\d{4}-\d{2}-\d{2}\/preview-\d+-[a-z0-9]+\.png)\)/);
    assert.ok(shotPath, `screenshot path in: ${sent}`);
    const png = readFileSync(path.join(HOME, 'inbox', shotPath[1]));
    assert.equal(png.subarray(1, 4).toString(), 'PNG');
    // Taken on the server by headless Chromium: the button alone, at twice its size.
    const [w, h] = [png.readUInt32BE(16), png.readUInt32BE(20)];
    assert.ok(w > 150 && w < 600 && h > 50 && h < 200, `button-sized screenshot, got ${w}×${h}`);
    await page.getByText('Streaming answer, done.').last().waitFor();
    // Pointing off, the app works as usual.
    await point.click();
    await devFrame.locator('#hero').click();
    for (let i = 0; i < 40 && (await appFrame().title()) !== 'clicked'; i++) await page.waitForTimeout(50);
    assert.equal(await appFrame().title(), 'clicked', 'with Point off, clicks reach the app');
    // Stop: a throwaway dev server started here appears in the list by its folder, and ✕ (after a
    // confirm) frees its port. A port that isn't listed can't be touched.
    const throwaway = path.join(mkdtempSync(path.join(tmpdir(), 'zen-e2e-throw-')), 'throwaway-app');
    mkdirSync(throwaway);
    const TPORT = APP_PORT + 3;
    const child = spawn(process.execPath, ['-e', `require('http').createServer((q, r) => r.end('x')).listen(${TPORT}, '127.0.0.1')`], { cwd: throwaway, stdio: 'ignore' });
    try {
      const listening = (port) => new Promise((ok) => { const c = net.connect(port, '127.0.0.1', () => (c.destroy(), ok(true))); c.on('error', () => ok(false)); });
      for (let i = 0; i < 100 && !(await listening(TPORT)); i++) await page.waitForTimeout(50);
      await page.getByRole('button', { name: 'dev servers', exact: true }).click();
      await page.getByRole('button', { name: 'refresh dev servers' }).click();
      await row(TPORT).waitFor();
      assert.equal(await row(TPORT).locator('.pv-name').innerText(), 'throwaway-app');
      page.once('dialog', (d) => d.accept());
      await page.getByRole('button', { name: 'stop throwaway-app' }).click();
      await row(TPORT).waitFor({ state: 'detached' });
      assert.equal(await listening(TPORT), false, 'stop frees the port');
      assert.equal((await fetch(`${base}/api/preview/servers/stop`, { method: 'POST', headers: { 'x-zen-web': '1', 'content-type': 'application/json' }, body: JSON.stringify({ port: TPORT }) })).status, 404, 'a port nobody listens on is not stoppable');
      assert.equal((await fetch(`${base}/api/preview/servers/stop`, { method: 'POST', headers: { 'x-zen-web': '1', 'content-type': 'application/json' }, body: JSON.stringify({ port: PORT }) })).status, 400, "zen's own port is not stoppable");
      assert.equal((await fetch(`${base}/api/preview/servers/stop`, { method: 'POST', body: JSON.stringify({ port: TPORT }) })).status, 403, 'writes need X-Zen-Web');
    } finally {
      child.kill('SIGKILL');
      rmSync(path.dirname(throwaway), { recursive: true, force: true });
    }
    await page.getByRole('tab', { name: 'Files', exact: true }).click();
  } finally {
    devApp.close();
  }

  // Settings › You edits USER.md through the kernel, in place; MEMORY.md is shown as read-only.
  await page.goto(`${base}/?view=settings`);
  await page.locator('.card', { hasText: 'USER.md' }).getByRole('button', { name: 'Edit', exact: true }).click();
  const userBox = page.getByRole('textbox', { name: 'USER.md' });
  assert.equal(await userBox.inputValue(), '# You\n', 'the editor loads USER.md from the kernel');
  await userBox.fill('# You\n\nLikes calm.\n');
  await page.locator('.uedit').getByRole('button', { name: 'Save', exact: true }).click();
  await userBox.waitFor({ state: 'detached' });
  assert.equal(userText, '# You\n\nLikes calm.\n', 'Save PUTs USER.md to the kernel');
  assert.ok(page.url().includes('view=settings'), 'editing USER.md stays in Settings');
  await page.getByRole('navigation', { name: 'Settings sections' }).getByRole('button', { name: 'Memory', exact: true }).click();
  await page.getByRole('button', { name: /View MEMORY\.md read-only/ }).waitFor();

  // Settings › Plugins: remove asks first, then the plugin goes.
  await page.goto(`${base}/?view=sessions`);
  await page.locator('.agent').first().click();
  await page.locator('.me .icon[title="Settings"]').click();
  await page.getByRole('navigation', { name: 'Settings sections' }).getByRole('button', { name: 'Plugins', exact: true }).click();
  await page.getByText(/Everything in the marketplace is installed and up to date|Each plugin says what it needs before anything is installed/).waitFor();
  assert.equal(await page.getByRole('button', { name: 'Update to 0.0.9', exact: true }).count(), 0, 'a stale catalog never offers a downgrade');
  const removeAll = installedPlugins.length;
  for (const p of [...installedPlugins]) {
    const t = p.title || p.name;
    const row = page.locator('.li[aria-label="' + t + '"]');
    await row.getByRole('button', { name: 'Remove', exact: true }).first().click();
    assert.equal(installedPlugins.length, removeAll, 'remove asks first');
    await row.locator('.confirm').getByRole('button', { name: 'Remove', exact: true }).click();
    await row.getByRole('button', { name: 'Remove', exact: true }).waitFor({ state: 'detached' }); // back in the marketplace
  }
  assert.equal(installedPlugins.length, 0);
  await page.goto(`${base}/?view=files&file=${encodeURIComponent(path.join(VAULT, 'wiki/God.md'))}`);
  await page.getByRole('heading', { name: 'God' }).waitFor();
  assert.equal(await page.getByRole('tab', { name: 'Reading', exact: true }).count(), 0);
  assert.equal(await page.locator('iframe').count(), 0);
  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  await page.getByRole('textbox', { name: 'Markdown source' }).waitFor();

  assert.deepEqual(errors, []);
  console.log('zen e2e: all passed');
} finally {
  await browser.close();
  server.kill();
  kernel.close();
}
