// End-to-end for the zen app: the real server (dev mode) on a scratch home, a stand-in zen kernel
// (HTTP and a minimal WebSocket, checking the token on every request), and headless Chromium.
// The slash commands come from this checkout's `zen --commands` (built here, or ZEN_BIN), plus a
// stand-in plugin's command. The stand-in kernel also hosts plugins (ADR 0001, Host API): a list,
// a marketplace, installs, and a fixture UI built on the real MCP Apps `App`.
// `node tests/zen-e2e.mjs [--shots]`.
import { execFileSync, spawn } from 'node:child_process';
import { createServer } from 'node:http';
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
mkdirSync(path.join(VAULT, 'games'));
writeFileSync(path.join(VAULT, 'games/board.fx'), 'four in a row\n');
const REAL_ZEN = process.env.ZEN_BIN ?? (() => {
  const root = path.resolve(import.meta.dirname, '../..');
  execFileSync('cargo', ['build', '-q', '-p', 'zen'], { cwd: root, stdio: 'inherit' });
  return path.join(root, 'target/debug/zen');
})();
// `zen --commands` as the kernel lists it with a plugin enabled: the shared table plus its command.
const table = JSON.parse(execFileSync(REAL_ZEN, ['--commands'], { encoding: 'utf8' }));
table.commands.push({ name: 'roll', args: '<sides>', description: 'Roll a die.', scope: 'kernel', plugin: 'zenfx', tool: 'zenfx_roll' });
writeFileSync(path.join(HOME, 'commands.json'), JSON.stringify(table));
const ZEN_BIN = path.join(HOME, 'zen');
writeFileSync(ZEN_BIN, `#!/bin/sh\nif [ "$1" = --commands ]; then cat '${path.join(HOME, 'commands.json')}'; else exec '${REAL_ZEN}' "$@"; fi\n`, { mode: 0o755 });
const PORT = Number(process.env.ZEN_E2E_PORT ?? 4412);
const KERNEL = Number(process.env.ZEN_E2E_KERNEL_PORT ?? 4413);
const shots = process.argv.includes('--shots');
const now = new Date().toISOString();
const session = (id, title, extra = {}) => ({ id, title, model: 'faux/smoke', effort: null, archived: false, kind: null, parent: null, created_at: now, updated_at: now, ...extra });
const prompts = [];
/** What else the page sent on the socket: steers, aborts; and tasks handed to an agent. */
const steers = [];
const aborts = [];
const handed = [];
/** Waits until the stand-in kernel has seen what the page sent. */
const until = async (ok, what) => {
  for (let i = 0; i < 100 && !ok(); i++) await new Promise((r) => setTimeout(r, 50));
  assert.ok(ok(), what);
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
await app.connect();
show();
$('roll').onclick = async () => { const r = await app.callServerTool({ name: 'zenfx_roll', arguments: { sides: 20 } }); $('out').textContent = r.content[0].text; };
$('open').onclick = () => app.openLink({ url: 'zen://open?path=wiki%2FGrace.md' });
$('edit').onclick = () => app.openLink({ url: 'zen://open?path=wiki%2FGrace.md&mode=edit' });
$('close').onclick = () => app.openLink({ url: 'zen://close' });
$('say').onclick = async () => { await app.updateModelContext({ content: [{ type: 'text', text: 'Board shows 4' }] }); $('out').textContent = 'told'; };
$('pin').onclick = async () => { await app.updateModelContext({ content: [{ type: 'text', text: '#1 button.hero' }, { type: 'image', mimeType: 'image/png', data: 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==' }, { type: 'image', mimeType: 'text/html', data: 'PGI+' }] }); $('out').textContent = 'pinned'; };
</script></body></html>`;
let maliciousInit = false;
const fxPlugin = {
  name: 'zenfx', version: '0.1.0', title: 'Fixture', description: 'A test plugin.', permissions: ['files:read'], enabled: true,
  contributes: {
    views: [{ id: 'board', title: 'Fx board', ui: 'ui://zenfx/app' }],
    openers: [{ id: 'read', title: 'Fx read', ui: 'ui://zenfx/app', patterns: ['*.fx'], mode: 'read' }],
    games: [{ id: 'dice', title: 'Dice', description: 'Roll a die.', ui: 'ui://zenfx/app' }],
    commands: [{ name: 'roll', tool: 'zenfx_roll', args: '<sides>', description: 'Roll a die.' }],
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
  { name: 'zenfx', title: 'Fixture', description: 'A test plugin.', version: '0.1.0', kernel: ['files:read'], contributes: ['views', 'openers', 'games', 'commands'] },
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
const agentSummary = (name) => {
  const a = agents.get(name);
  return a.error ? { name, error: a.error } : { name, ...a.summary };
};
const agentDetail = (name) => ({ ...agentSummary(name), soul: agents.get(name).soul, identity: agents.get(name).identity, settings: agents.get(name).settings, dir: `${HOME}/.zenbot/agents/${name}`, sessions: name === 'rex' ? 1 : 0 });

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
      { ...session('k1', 'verifier', { kind: 'verifier', parent: 's1' }), task: 'Check the plan', busy: true, waiting: false },
      ...old,
    ], archived: 2 });
  }
  if (r === 'POST /api/sessions') {
    created.push(body);
    return json(res, session('s2', null, { agent: body.agent ?? 'zenbot' }));
  }
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
      { role: 'assistant', content: [{ type: 'text', text: 'Standup at **07:00**, see [[God]].' }], seq: 2 },
    ] });
  if (r === 'GET /api/models') return json(res, { models: [{ id: 'faux/smoke', efforts: [] }], authenticated: {}, default: 'faux/smoke' });
  if (r === 'POST /api/search') {
    searches.push(body);
    const hit = (kind, ref, title, snippet, file) => ({ kind, ref, title, snippet, at: now, exact: false, relevance: null, path: path.join(VAULT, file) });
    return json(res, { query: body.query, scope: body.scope, reranked: false, results: body.query === 'gift' ? [hit('wiki', 'Grace', 'Grace', 'A gift, given freely.', 'wiki/Grace.md'), hit('note', 'Atlas.md', 'Atlas', 'all names matched too', 'wiki/Atlas.md')] : [] });
  }
  if (r === 'GET /api/tasks')
    return json(res, { today: '2026-10-30', tz: 'UTC', count: 1, problems: [], projects: [{ project: 'acme', tasks: [
      { project: 'acme', text: 'Price the offer', id: 't1', due: '2026-10-29', scheduled: null, priority: 'urgent', goals: [], file: 'x', line: 1, overdue: true },
    ] }] });
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
      return json(res, { content: [{ type: 'text', text: `rolled ${body.arguments?.sides ?? 6}: 4` }], _meta: { ui: { resourceUri: 'ui://zenfx/app' } } });
    }
  }
  if (r === 'POST /api/tasks/t1/agent') {
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
    if (m.type === 'abort') {
      aborts.push(1);
      if (long) {
        clearTimeout(long);
        long = null;
        send({ type: 'end', error: null, turn: { input_tokens: 1, output_tokens: 1 }, next: false });
        send({ type: 'idle' });
      }
      return;
    }
    if (m.type !== 'prompt') return;
    if (m.mode === 'steer') return void steers.push(m.text);
    prompts.push(m.text);
    if (long) return; // queued: the kernel runs it after this turn
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
  env: { ...process.env, ZEN_WEB_HOME: HOME, ZEN_WEB_ROOT: VAULT, ZEN_WEB_PORT: String(PORT), ZEN_WEB_AUTH: 'off', ZEN_URL: `http://127.0.0.1:${KERNEL}`, ZEN_TOKEN_FILE: path.join(HOME, '.zenbot/token'), ZEN_BIN },
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
const base = `http://127.0.0.1:${PORT}`;
try {
  // The board: the session, waiting on the owner.
  await page.goto(`${base}/`);
  await page.getByText('Plan the week').first().waitFor();
  assert.ok(await page.getByText('waiting on you').first().isVisible());
  // Channels apart; waiting first; a subagent under its session.
  await page.locator('.head', { hasText: 'CHANNELS' }).waitFor();
  const boardRows = (await page.locator('.row').allInnerTexts()).map((t) => t.replace(/\s+/g, ' '));
  const at = (re) => boardRows.findIndex((t) => re.test(t));
  assert.ok(at(/zen room/) >= 0 && at(/zen room/) < at(/Plan the week/), `channels come first: ${boardRows}`);
  assert.ok(at(/Plan the week/) < at(/Pitch practice/), 'the session waiting on the owner comes first');
  assert.equal(at(/verifier: Check the plan/), at(/Plan the week/) + 1, 'the subagent sits under its session');
  if (shots) await page.screenshot({ path: 'tests/shots/zen-board.png' });

  // Sessions: a `+ new session` button does what `n` does.
  await page.getByRole('button', { name: '+ new session', exact: true }).click();
  await page.waitForURL(/\?s=new/);
  await page.locator('.zc-foot', { hasText: '@zenbot' }).waitFor();
  await page.goto(`${base}/`);
  await page.getByText('Plan the week').first().waitFor();
  // Archived jobs stay out of Archived (and its count) until `J` or the jobs chip.
  assert.equal(await page.getByRole('button', { name: 'jobs', exact: true }).count(), 0, 'the jobs chip shows with archived');
  await page.keyboard.press('J');
  assert.equal(await page.evaluate(() => localStorage.getItem('zen.sessions.archivedJobs')), null, 'J does nothing while archived is hidden');
  await page.keyboard.press('a');
  await page.getByText('Old chat').waitFor();
  assert.equal(await page.getByText('vault-sync').count(), 0, 'archived jobs are hidden by default');
  const archivedHead = page.locator('.head', { hasText: 'ARCHIVED' });
  assert.match((await archivedHead.innerText()).replace(/\s+/g, ' '), /ARCHIVED 1 · a hides · J shows jobs \(1\)/);
  const badge = page.locator('.outcome.partial');
  assert.equal(await badge.innerText(), 'partial', 'an archived session shows how it ended');
  assert.equal(await badge.getAttribute('title'), 'tests red', 'the badge carries the note');
  await page.keyboard.press('J');
  await page.getByText('vault-sync').waitFor();
  assert.match((await archivedHead.innerText()).replace(/\s+/g, ' '), /ARCHIVED 2 · a hides · J hides jobs/);
  assert.equal(await page.evaluate(() => localStorage.getItem('zen.sessions.archivedJobs')), '1');
  await page.reload();
  await page.keyboard.press('a');
  await page.getByText('vault-sync').waitFor(); // kept across reloads
  await page.getByRole('button', { name: 'jobs', exact: true }).click(); // the chip hides them again
  await page.getByText('vault-sync').waitFor({ state: 'detached' });
  assert.equal(await page.evaluate(() => localStorage.getItem('zen.sessions.archivedJobs')), '0');
  await page.keyboard.press('a');
  await page.getByText('Old chat').waitFor({ state: 'detached' });

  // Tasks: the urgent task, overdue.
  await page.keyboard.press('3');
  await page.getByText('Price the offer').waitFor();
  assert.ok(await page.getByText(/overdue/).first().isVisible());
  // Handing a task to an agent: `a` arms, `a` again hands it over and opens its session.
  await page.keyboard.press('a');
  await page.getByText(/a again: hand/).waitFor();
  await page.keyboard.press('a');
  await page.waitForURL(/\?s=s1/);
  assert.deepEqual(handed, ['t1']);
  await page.goto(`${base}/`);
  await page.getByText('Plan the week').first().waitFor();

  // Files: opens in the shared workspace (the vault); `..` climbs to the home folder, without the
  // token; a note opens in the reader, changelog apart.
  await page.keyboard.press('2');
  await page.getByText('wiki/', { exact: true }).waitFor();
  assert.equal(await page.getByText('plain.txt').count(), 0, 'Files starts in the vault, not home');
  // Search: names at once (client side over the vault, through the server's find), content from the
  // kernel after a pause or Enter, names first; a result opens in the reader; Esc clears.
  const search = page.getByLabel('search files');
  const results = page.locator('.result');
  await page.keyboard.press('/');
  assert.equal(await search.evaluate((i) => i === document.activeElement), true, '/ focuses the search');
  await search.fill('at');
  await results.filter({ hasText: 'Atlas.md' }).waitFor();
  assert.equal(await page.getByRole('tree', { name: 'files' }).count(), 0, 'results replace the tree');
  assert.equal(await results.filter({ hasText: 'Grace' }).count(), 0, 'only names that match');
  await new Promise((r) => setTimeout(r, 900));
  assert.deepEqual(searches, [], 'under 3 characters only names are searched');
  await search.fill('atl');
  await until(() => searches.length === 1, 'a pause asks the kernel');
  assert.deepEqual(searches[0], { query: 'atl', scope: 'notes', limit: 10 });
  await search.fill('gift');
  const grace = results.filter({ hasText: 'Grace' });
  await grace.waitFor();
  assert.match(await grace.innerText(), /A gift, given freely\./);
  assert.match(await grace.innerText(), /wiki\/Grace\.md/);
  assert.equal(await results.count(), 2);
  assert.match(await results.first().innerText(), /Grace/, 'content results keep the kernel order');
  await grace.click();
  await page.locator('.reader').waitFor();
  await page.getByText('A gift.').first().waitFor();
  await page.keyboard.press('Escape'); // closes the file; the results are still there
  await grace.waitFor();
  await search.fill('god'); // names: ArrowDown, Enter open the first
  await results.filter({ hasText: 'God.md' }).waitFor();
  await search.press('ArrowDown');
  await search.press('Enter');
  await page.locator('.reader').waitFor();
  await page.keyboard.press('Escape');
  await search.press('Escape'); // clears the search
  await page.getByText('wiki/', { exact: true }).waitFor();
  assert.equal(await search.inputValue(), '');
  await page.getByRole('button', { name: 'home', exact: true }).click();
  await page.getByText('plain.txt').waitFor();
  await page.getByRole('button', { name: 'vault', exact: true }).click();
  await page.getByText('wiki/', { exact: true }).waitFor();
  await page.locator('.crumbs button').first().click(); // ~ : back to the home folder
  await page.getByText('plain.txt').waitFor();
  await page.getByText('.zenbot', { exact: true }).waitFor({ state: 'detached' }).catch(() => {}); // dotfiles hidden
  // New file, folder and rename, asked inline; the server writes under the scratch home.
  await page.keyboard.press('a'); // nothing selected: the root
  await page.getByLabel('name', { exact: true }).fill('..');
  await page.keyboard.press('Enter');
  await page.getByText(/couldn't create \.\.: bad name/).waitFor();
  assert.equal(await page.getByLabel('name', { exact: true }).count(), 1, 'a refused name keeps the input');
  await page.getByLabel('name', { exact: true }).fill('.env');
  await page.keyboard.press('Enter');
  await page.getByText(/couldn't create \.env: not allowed here/).waitFor();
  assert.equal(existsSync(path.join(HOME, '.env')), false);
  await page.getByLabel('name', { exact: true }).fill('plain.txt');
  await page.keyboard.press('Enter');
  await page.getByText(/couldn't create plain\.txt: already exists/).waitFor();
  assert.equal(readFileSync(path.join(HOME, 'plain.txt'), 'utf8'), 'hello file', 'nothing is overwritten');
  await page.keyboard.press('Escape');
  assert.equal(await page.getByLabel('name', { exact: true }).count(), 0, 'Esc cancels');
  await page.getByRole('button', { name: '+ folder', exact: true }).click();
  await page.getByLabel('name', { exact: true }).fill('proj');
  await page.keyboard.press('Enter');
  await page.locator('.row[data-sel="1"]', { hasText: 'proj/' }).waitFor();
  assert.ok(statSync(path.join(HOME, 'proj')).isDirectory());
  await page.keyboard.press('a'); // a folder is selected: inside it, and open
  await page.getByLabel('name', { exact: true }).fill('sub/');
  await page.keyboard.press('Enter');
  await page.locator('.row[data-sel="1"]', { hasText: 'sub/' }).waitFor();
  assert.ok(statSync(path.join(HOME, 'proj/sub')).isDirectory());
  await page.keyboard.press('Shift+A');
  await page.getByLabel('name', { exact: true }).fill('docs');
  await page.keyboard.press('Enter'); // inside sub, the selected folder
  await page.locator('.row[data-sel="1"]', { hasText: 'docs/' }).waitFor();
  assert.ok(statSync(path.join(HOME, 'proj/sub/docs')).isDirectory());
  await page.locator('.row', { hasText: 'plain.txt' }).click(); // opens it; closing it keeps it selected
  await page.locator('.reader').waitFor();
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: '+ file', exact: true }).waitFor();
  await page.keyboard.press('a'); // a file selected: beside it, in the root
  await page.getByLabel('name', { exact: true }).fill('note.txt');
  await page.keyboard.press('Enter');
  await page.locator('.reader').waitFor(); // the new file opens in the reader
  assert.equal(readFileSync(path.join(HOME, 'note.txt'), 'utf8'), '');
  await page.keyboard.press('Escape');
  await page.locator('.row[data-sel="1"]', { hasText: 'note.txt' }).waitFor();
  await page.keyboard.press('Shift+R');
  const rename = page.getByLabel('new name');
  assert.equal(await rename.inputValue(), 'note.txt');
  assert.deepEqual(await rename.evaluate((i) => [i.selectionStart, i.selectionEnd]), [0, 4], 'the stem is selected');
  await rename.fill('renamed.txt');
  await page.keyboard.press('Enter');
  await page.locator('.row[data-sel="1"]', { hasText: 'renamed.txt' }).waitFor();
  assert.equal(existsSync(path.join(HOME, 'note.txt')), false);
  assert.equal(existsSync(path.join(HOME, 'renamed.txt')), true);
  await page.getByRole('button', { name: 'rename', exact: true }).click(); // the button too
  await page.getByLabel('new name').fill('plain.txt');
  await page.keyboard.press('Enter');
  await page.getByText(/couldn't rename plain\.txt: already exists/).waitFor();
  await page.keyboard.press('Escape');
  rmSync(path.join(HOME, 'renamed.txt'));
  await page.goto(`${base}/?view=files&file=${encodeURIComponent(path.join(VAULT, 'wiki/God.md'))}`);
  await page.getByRole('heading', { name: 'God' }).waitFor();
  // Minimal core fallback, with no rich reader/editor tools.
  await page.getByText('v1: first', { exact: false }).waitFor();
  assert.equal(await page.getByRole('tab', { name: 'Reading', exact: true }).count(), 0);
  assert.equal(await page.locator('.cm-editor').count(), 0);
  await page.keyboard.press('e');
  const source = page.getByRole('textbox', { name: 'Markdown source' });
  await source.waitFor();
  assert.match(page.url(), /edit=1/);
  const original = await source.inputValue();
  await source.fill(original + '\nSaved in Files.\n');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await page.getByRole('status').filter({ hasText: /^Saved$/ }).waitFor();
  assert.match(readFileSync(path.join(VAULT, 'wiki/God.md'), 'utf8'), /Saved in Files/);
  await page.keyboard.press('Alt+w');
  await page.getByRole('heading', { name: 'God' }).waitFor();
  await page.getByText('Saved in Files.').waitFor();
  await page.goto(`${base}/?path=${encodeURIComponent('wiki/God.md')}`);
  await page.getByRole('textbox', { name: 'Markdown source' }).waitFor();
  assert.match(page.url(), /view=files&file=.*God\.md&edit=1/);

  // Plugins. Settings (off the top bar) lists the installed ones, with permissions in plain words;
  // disabling one takes its view tab away, enabling brings it back.
  await page.goto(`${base}/`);
  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  await page.waitForURL(/view=settings/);
  const fxRow = page.locator('section[aria-label="Fixture"]');
  await fxRow.getByText('0.1.0').waitFor();
  await fxRow.getByText('Read your files (the vault)').waitFor();
  const fxTab = page.getByRole('tab', { name: 'Fx board', exact: true });
  await fxTab.waitFor();
  await page.getByLabel('enable Fixture').uncheck();
  await fxTab.waitFor({ state: 'detached' });
  assert.deepEqual(toggles.at(-1), { name: 'zenfx', enabled: false });
  await page.getByLabel('enable Fixture').check();
  await fxTab.waitFor();
  assert.deepEqual(toggles.at(-1), { name: 'zenfx', enabled: true });
  // The marketplace: an installed entry says so; installing asks the kernel what it needs, shows
  // it, and installs only once approved.
  await page.getByRole('tab', { name: 'Marketplace', exact: true }).click();
  await page.waitForURL(/tab=marketplace/);
  await page.locator('section[aria-label="Fixture"]', { hasText: 'Installed' }).waitFor();
  await page.locator('section[aria-label="Cards"]').getByRole('button', { name: 'Install', exact: true }).click();
  const ask = page.getByRole('dialog', { name: 'Install Cards' });
  await ask.getByText('Reach the internet').waitFor();
  await ask.getByText("Ask zenbot's model for completions").waitFor();
  assert.deepEqual(installs, [{ name: 'zencards' }], 'the first ask installs nothing');
  assert.equal(installedPlugins.length, 1);
  if (shots) await page.screenshot({ path: 'tests/shots/zen-install.png' });
  await ask.getByRole('button', { name: 'Install', exact: true }).click();
  // A changed release asks again; the host must not claim it installed the old approval.
  await ask.getByText('Read your files (the vault)').waitFor();
  assert.equal(installedPlugins.length, 1);
  assert.deepEqual(installs.at(-1), { name: 'zencards', approve: true, permissions: ['network', 'ai'] });
  await ask.getByRole('button', { name: 'Install', exact: true }).click();
  await page.locator('section[aria-label="Cards"]', { hasText: 'Installed' }).waitFor();
  assert.deepEqual(installs.at(-1), { name: 'zencards', approve: true, permissions: ['network', 'ai', 'files:read'] });
  await page.getByRole('tab', { name: 'Plugins', exact: true }).click();
  await page.locator('section[aria-label="Cards"]').getByText('Reach the internet').waitFor();
  // A plugin granted `ai` shows its AI switch and last 30 days' AI calls; one without has neither.
  await page.locator('section[aria-label="Cards"]').getByText('3 AI calls in the last 30 days').waitFor();
  assert.equal(await page.getByLabel('AI for Fixture').count(), 0);
  await page.getByLabel('AI for Cards').uncheck();
  await until(() => toggles.at(-1)?.ai === false, 'AI switched off');
  assert.deepEqual(toggles.at(-1), { name: 'zencards', ai: false });
  assert.equal(await page.getByLabel('AI for Cards').isChecked(), false);
  await page.getByLabel('AI for Cards').check();
  await until(() => toggles.at(-1)?.ai === true, 'AI switched on');
  if (shots) await page.screenshot({ path: 'tests/shots/zen-settings.png' });

  // A plugin's view is a tab after the built-ins; its UI runs sandboxed, with no same origin.
  await fxTab.click();
  const frame = page.locator('iframe[data-plugin="zenfx"]');
  await frame.waitFor();
  assert.equal(await frame.getAttribute('sandbox'), 'allow-scripts allow-forms');
  const fx = page.frameLocator('iframe[data-plugin="zenfx"]');
  await fx.locator('#ctx', { hasText: 'slot=view pane=board path=-' }).waitFor();
  assert.equal(await fx.locator('body').evaluate(() => { try { return String(window.parent.document); } catch { return 'blocked'; } }), 'blocked', "the UI can't reach the page");
  assert.equal(await fx.locator('body').evaluate(() => window.earlyBlocked), true, 'CSP blocks network even before the resource head');
  // Games: every enabled plugin's games; one opens sandboxed, its tool call reaches the kernel.
  await page.getByRole('tab', { name: 'Games', exact: true }).click();
  await page.getByRole('button', { name: /Solitaire/ }).waitFor();
  await page.getByRole('button', { name: /Dice.*Roll a die\./ }).click();
  await fx.locator('#ctx', { hasText: 'slot=game pane=board' }).waitFor();
  assert.doesNotMatch(await frame.getAttribute('sandbox'), /allow-same-origin/);
  await fx.getByRole('button', { name: 'Roll' }).click();
  await fx.locator('#out', { hasText: 'rolled 20: 4' }).waitFor();
  assert.deepEqual(calls.at(-1), { plugin: 'zenfx', tool: 'zenfx_roll', arguments: { sides: 20 } });
  if (shots) await page.screenshot({ path: 'tests/shots/zen-game.png' });
  // The last game played opens with the tab.
  await page.reload();
  await fx.locator('#ctx', { hasText: 'slot=game' }).waitFor();
  // A zen://open link opens the file in the same pane, through its opener (the built-in reader).
  await fx.getByRole('button', { name: 'Open Grace' }).click();
  await page.getByRole('heading', { name: 'Grace' }).waitFor();
  assert.match(page.url(), /view=files&file=.*Grace\.md/);
  // Open with: a plugin's opener for the file, made the default, opens it from then on.
  await page.goto(`${base}/?view=files&file=${encodeURIComponent(path.join(VAULT, 'games/board.fx'))}`);
  await page.getByText('four in a row').waitFor();
  await page.getByRole('button', { name: 'Open with…' }).click();
  const withMenu = page.getByRole('menu', { name: 'open with' });
  await withMenu.locator('.item', { hasText: 'Reader (built-in)' }).getByText('default').waitFor();
  await withMenu.locator('.item', { hasText: 'Fx read · Fixture' }).getByRole('button', { name: 'make default' }).click();
  await fx.locator('#ctx', { hasText: 'slot=opener pane=board path=games/board.fx' }).waitFor();
  assert.deepEqual(defaultsSet, [{ key: '*.fx:read', value: 'zenfx/read' }]);
  await page.reload();
  await fx.locator('#ctx', { hasText: 'slot=opener' }).waitFor();
  // Host toolbar remains outside the iframe, and mode changes drop an explicit opener.
  const actions = page.getByRole('toolbar', { name: 'file actions' });
  await page.getByRole('button', { name: 'Open with…' }).click();
  await withMenu.getByRole('menuitem', { name: 'Reader (built-in)' }).click();
  assert.equal(await frame.count(), 0);
  await actions.getByRole('button', { name: 'Read', exact: true }).click();
  await fx.locator('#ctx', { hasText: 'slot=opener' }).waitFor();
  // Plugin edit links honor mode; close returns the core Files tree, not its library.
  await fx.getByRole('button', { name: 'Edit Grace' }).click();
  await page.getByRole('textbox', { name: 'Markdown source' }).waitFor();
  await actions.getByRole('button', { name: 'Back', exact: true }).click();
  await page.getByRole('tree', { name: 'files' }).waitFor();
  await page.getByRole('treeitem', { name: /wiki\// }).click();
  await page.getByRole('treeitem', { name: /Grace.md/ }).click();
  await actions.getByRole('button', { name: 'Back', exact: true }).click();
  await page.getByRole('button', { name: 'Edit selected file' }).click();
  await page.getByRole('textbox', { name: 'Markdown source' }).waitFor();
  await page.goto(`${base}/?view=files&file=${encodeURIComponent(path.join(VAULT, 'games/board.fx'))}`);
  await fx.locator('#ctx', { hasText: 'slot=opener' }).waitFor();
  await fx.getByRole('button', { name: 'Close file' }).click();
  await page.getByRole('tree', { name: 'files' }).waitFor();
  await page.goto(`${base}/?view=files&file=${encodeURIComponent(path.join(VAULT, 'games/board.fx'))}`);
  // A default selected in Settings for a second matching pattern must also apply.
  fxPlugin.contributes.openers[0].patterns.push('board.*');
  delete openerDefaults['*.fx:read'];
  openerDefaults['board.*:read'] = 'zenfx/read';
  await page.reload();
  await fx.locator('#ctx', { hasText: 'slot=opener' }).waitFor();
  // ...and the built-in reader is one pick away.
  await page.getByRole('button', { name: 'Open with…' }).click();
  await withMenu.getByRole('menuitem', { name: 'Reader (built-in)' }).click();
  await page.getByText('four in a row').waitFor();
  assert.equal(await frame.count(), 0);

  // Agents: listed with what each may use, a broken one says why; another agent's session says
  // whose on the board.
  await page.goto(`${base}/`);
  await page.getByText('Pitch practice').waitFor();
  assert.match(await page.locator('.row', { hasText: 'Pitch practice' }).innerText(), /@rex · smoke/);
  assert.doesNotMatch(await page.locator('.row', { hasText: 'Plan the week' }).innerText(), /@zenbot/);
  await page.keyboard.press('4');
  await page.getByText('@rex').waitFor();
  assert.match(page.url(), /view=agents/);
  const rowOf = (name) => page.locator('[aria-label="agents"] .row', { hasText: `@${name}` });
  assert.match(await rowOf('zenbot').innerText(), /all tools/);
  assert.match(await rowOf('rex').innerText(), /read, web_search · no MCP · claude\/opus/);
  assert.match(await rowOf('broken').innerText(), /⚠ agent `broken`: agent.toml: expected a value/);
  if (shots) await page.screenshot({ path: 'tests/shots/zen-agents.png' });

  // Create one: a bad name is refused before the kernel; a good one opens its files.
  await page.getByRole('button', { name: 'New agent' }).click();
  await page.getByLabel('agent name').fill('Bad Name');
  await page.getByRole('button', { name: 'Create' }).click();
  await page.getByText(/a name is a lowercase letter/).waitFor();
  await page.getByLabel('agent name').fill('rex');
  await page.getByRole('button', { name: 'Create' }).click();
  await page.getByText('agent `rex` already exists').waitFor();
  await page.getByLabel('agent name').fill('scout');
  await page.getByLabel('agent description').fill('finds sources');
  await page.getByLabel('agent description').press('Enter');
  await page.getByText('@scout', { exact: true }).waitFor();
  assert.equal(await page.getByLabel('SOUL.md').inputValue(), '# scout\n');

  // Edit and save: only the changed file goes; a refused agent.toml shows why, nothing kept.
  const save = page.locator('.view .act.primary');
  assert.equal(await save.isDisabled(), true, 'nothing to save yet');
  await page.getByLabel('IDENTITY.md').fill('# How scout works\n');
  await save.click();
  await page.getByRole('button', { name: 'Saved' }).waitFor();
  assert.deepEqual(saves.at(-1), { name: 'scout', body: { identity: '# How scout works\n' } });
  await page.getByLabel('agent.toml').fill('tools = ["nope"]\n');
  await page.keyboard.press('Control+s');
  await page.getByText('Not saved: agent.toml: unknown tool(s) nope; tools are: read, bash').waitFor();
  assert.deepEqual(saves.at(-1), { name: 'scout', body: { settings: 'tools = ["nope"]\n' } });
  assert.equal(await save.isDisabled(), false, 'the refused change stays to fix');
  if (shots) await page.screenshot({ path: 'tests/shots/zen-agent-editor.png' });

  // Delete asks first; zenbot can't be deleted.
  await page.getByRole('button', { name: 'Delete', exact: true }).click();
  await page.getByText(/Delete @scout\? Its folder moves/).waitFor();
  assert.ok(agents.has('scout'), 'nothing removed before the confirm');
  await page.getByRole('button', { name: 'Yes, delete @scout' }).click();
  await page.getByText('@rex').waitFor();
  assert.equal(agents.has('scout'), false);
  assert.equal(await rowOf('scout').count(), 0);
  await rowOf('zenbot').getByRole('button', { name: 'edit' }).click(); // the first, chosen again
  await page.getByLabel('SOUL.md').waitFor();
  assert.equal(await page.getByRole('button', { name: 'Delete', exact: true }).isDisabled(), true, 'zenbot stays');
  await page.getByRole('button', { name: '← agents' }).click();

  // A new session with an agent: from its editor, the session is started as it.
  await rowOf('rex').click();
  await rowOf('rex').getByRole('button', { name: 'edit' }).click();
  await page.getByRole('button', { name: 'New session with this agent' }).click();
  await page.getByText('new session with rex').waitFor();
  await page.locator('.zc-foot', { hasText: /@rex · smoke/ }).waitFor();
  assert.match((await page.locator('.zc-foot').textContent()).replace(/\s+/g, ' '), /@rex · smoke/);
  // The picker in a fresh chat changes who it starts as.
  await page.getByRole('radio', { name: '@zenbot' }).click();
  await page.getByRole('radio', { name: '@rex' }).click();
  const fresh = page.locator('textarea').first();
  await fresh.fill('Grill my idea');
  await fresh.press('Enter');
  await page.waitForURL(/\?s=s2/);
  assert.deepEqual(created.at(-1), { agent: 'rex' });
  await page.locator('.zc-foot', { hasText: '@rex' }).waitFor();
  // A plain new session is zenbot's, with no agent sent.
  await page.goto(`${base}/?s=new`);
  await page.locator('.zc-foot', { hasText: '@zenbot' }).waitFor();
  await page.locator('textarea').first().fill('Hi');
  await page.locator('textarea').first().press('Enter');
  await page.waitForURL(/\?s=s2/);
  assert.deepEqual(created.at(-1), {});
  for (let i = 0; i < 100 && prompts.length < 2; i++) await new Promise((ok) => setTimeout(ok, 50));
  assert.deepEqual(prompts.splice(0), ['Grill my idea', 'Hi'], 'each first prompt reaches its new session');

  // A session: history, then a prompt streams an answer through the relayed socket.
  await page.goto(`${base}/?s=s1`);
  await page.getByText('What is on today?').waitFor();
  const box = page.locator('textarea').first();
  await box.fill('Book the gym');
  await box.press('Enter');
  await page.getByText('Streaming answer, done.').waitFor();
  assert.deepEqual(prompts, ['Book the gym']);
  assert.equal(await page.getByText('Book the gym').count(), 1, 'the prompt shows once');

  // Attachments: a file goes to ~/inbox/<day>/ at once; the prompt names its path.
  await page.locator('input[type=file]').setInputFiles({ name: '../clip.txt', mimeType: 'text/plain', buffer: Buffer.from('frame one') });
  await page.locator('.zc-att', { hasText: '✓' }).waitFor();
  await box.fill('Study this');
  await box.press('Enter');
  const day = new Date().toISOString().slice(0, 10);
  for (let i = 0; i < 100 && prompts.length < 2; i++) await new Promise((ok) => setTimeout(ok, 50));
  assert.equal(prompts.at(-1), `Study this\n\n(attached ~/inbox/${day}/clip.txt)`);
  assert.equal(readFileSync(path.join(HOME, 'inbox', day, 'clip.txt'), 'utf8'), 'frame one');
  assert.equal(await page.locator('.zc-att').count(), 0, 'sent attachments leave the composer');
  const noHeader = await page.evaluate(() => fetch('/api/upload?name=x.txt', { method: 'POST', body: 'x' }).then((r) => r.status));
  assert.equal(noHeader, 403, 'an upload without X-Zen-Web is refused');

  // A wiki link in the chat opens the note in the side panel.
  await page.locator('a[data-wiki="God"]').first().click();
  await page.getByRole('heading', { name: 'God' }).waitFor();
  // ...where it is written too, beside the chat.
  await page.locator('.zside').getByRole('toolbar', { name: 'file actions' }).getByRole('button', { name: 'Edit', exact: true }).click();
  await page.locator('.zside textarea').waitFor();
  if (shots) {
    await page.screenshot({ path: 'tests/shots/zen-side-editor.png' });
    await page.keyboard.press('Alt+l');
    await page.keyboard.press('Alt+r');
    await page.screenshot({ path: 'tests/shots/zen-side-panels.png' });
  }
  await page.locator('.zside').getByRole('button', { name: 'Read', exact: true }).click();
  await page.locator('.zside .reader').waitFor();

  // The separator resizes the side panel, and the width is kept for the next visit.
  const width = () => page.locator('.zside').evaluate((e) => e.getBoundingClientRect().width);
  const before = await width();
  const g = await page.locator('.gutter').boundingBox();
  await page.mouse.move(g.x + g.width / 2, g.y + 200);
  await page.mouse.down();
  await page.mouse.move(g.x - 200, g.y + 200, { steps: 5 });
  await page.mouse.up();
  const after = await width();
  assert.ok(after > before + 150, `the side panel widens (${before} → ${after})`);
  await page.reload();
  await page.locator('a[data-wiki="God"]').first().click();
  await page.locator('.zside').waitFor();
  assert.ok(Math.abs((await width()) - after) < 4, 'the width is kept');
  if (shots) await page.screenshot({ path: 'tests/shots/zen-session.png' });


  // A turn keeps streaming while the side panel changes views, and the composer keeps its keys.
  await page.goto(`${base}/?s=s1`);
  await page.getByText('What is on today?').waitFor();
  await page.keyboard.press('Control+b');
  await page.locator('.zside').waitFor();
  const sidePane = page.locator('.zside');
  // The side panel's Sessions leave out the open session but show its own subagents working.
  await sidePane.getByRole('tab', { name: 'Sessions', exact: true }).click();
  await sidePane.getByText('THIS SESSION').waitFor();
  assert.match(await sidePane.locator('.row', { hasText: 'Check the plan' }).innerText(), /verifier:/);
  assert.equal(await sidePane.locator('.row', { hasText: 'Plan the week' }).count(), 0, 'the open session itself stays out');
  await box.fill('Keep streaming while I read');
  await box.press('Enter');
  await page.getByText('Streaming', { exact: true }).waitFor();
  await sidePane.getByRole('tab', { name: 'Tasks', exact: true }).click();
  await sidePane.getByText('Price the offer').waitFor();
  await page.getByText('Streaming answer, done.').waitFor();
  assert.equal(prompts.at(-1), 'Keep streaming while I read');
  await box.click();
  await box.press('7');
  assert.equal(await box.inputValue(), '7', 'chat typing is untouched');
  await box.fill('');

  // Tool runs fold into one line; a click or ctrl+o shows the steps.
  const run = page.locator('.work').last();
  assert.match(await run.innerText(), /echo hi/);
  assert.ok(!(await run.evaluate((e) => e.classList.contains('wk-open'))), 'a run starts folded');
  await run.locator('button.head').click();
  await page.locator('.work.wk-open').last().waitFor();
  await box.click();
  await page.keyboard.press('Control+o');
  await page.keyboard.press('Control+o');
  assert.equal(await page.locator('.work.wk-open').count(), 0, 'ctrl+o folds every run again');

  // Questions with options, and the next-prompt suggestion (Tab takes it).
  await box.fill('Ask me');
  await box.press('Enter');
  await page.getByText('── Questions ──').waitFor();
  await page.getByRole('button', { name: 'tab: use suggestion' }).waitFor();
  assert.equal(await box.getAttribute('placeholder'), 'Thursday, just me');
  await box.press('Tab');
  assert.equal(await box.inputValue(), 'Thursday, just me');
  await box.fill('');
  await page.locator('.zc-opt', { hasText: 'Friday' }).click();
  await page.locator('.zc-opt', { hasText: 'The team' }).click();
  assert.equal(await box.inputValue(), '1. Friday\n2. The team', 'options fill the answer, one row per question');
  await box.fill('');

  // The slash commands are the shared table's, from `zen --commands`.
  await box.fill('/ste');
  await page.locator('.zc-pop', { hasText: 'change direction at the next tool boundary' }).waitFor();
  await box.fill('');

  // During a turn: Enter queues, /steer steers, Esc interrupts.
  await box.fill('Take your time');
  await box.press('Enter');
  await page.getByText('Thinking it over').waitFor();
  await box.fill('And then this');
  await box.press('Enter');
  await page.getByText(/queued: zenbot reads it when this turn ends/).waitFor();
  await until(() => prompts.at(-1) === 'And then this', 'the queued prompt reaches the kernel');
  await box.fill('/steer Shorter, please');
  await box.press('Enter');
  await page.getByText(/steering: zenbot reads it at the next tool boundary/).waitFor();
  await until(() => steers.length === 1, 'the steer reaches the kernel');
  assert.deepEqual(steers, ['Shorter, please']);
  await box.press('Escape');
  await page.getByText('interrupted', { exact: true }).waitFor();
  await until(() => aborts.length >= 1, 'Esc sends an abort');
  assert.equal(aborts.length, 1, 'Esc sends one abort');

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
  await page.locator('.zc-pop', { hasText: 'Roll a die.' }).waitFor();
  await box.fill('/roll 12');
  await box.press('Enter');
  await until(() => calls.at(-1)?.arguments?.sides === '12', 'the command reaches the plugin');
  assert.deepEqual(calls.at(-1), { plugin: 'zenfx', tool: 'zenfx_roll', arguments: { sides: '12' }, session: 's1' });
  const sideFx = page.frameLocator('.zside iframe[data-plugin="zenfx"]');
  await sideFx.locator('#out', { hasText: 'result: rolled 12: 4' }).waitFor();
  await sideFx.locator('#ctx', { hasText: 'slot=view pane=side' }).waitFor();
  await page.locator('.zside').getByRole('tab', { name: 'Fixture', exact: true }).waitFor();
  const pluginDraft = page.getByRole('region', { name: 'plugin draft' });
  await pluginDraft.getByText('/danger malicious init', { exact: true }).waitFor();
  assert.match(await pluginDraft.innerText(), /DRAFT · plugin: zenfx/);
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

  await page.locator('.chat-pane').getByRole('button', { name: 'Settings', exact: true }).click();
  await page.locator('.zside').getByRole('tab', { name: 'Plugins', exact: true }).waitFor();
  assert.match(page.url(), /s=s1/);
  await page.goto(`${base}/`);
  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  const cardsRow = page.locator('section[aria-label="Cards"]');
  await cardsRow.getByRole('button', { name: 'remove', exact: true }).click();
  assert.equal(installedPlugins.length, 2, 'remove asks first');
  await cardsRow.getByRole('button', { name: 'Yes, remove Cards', exact: true }).click();
  await cardsRow.waitFor({ state: 'detached' });
  assert.equal(installedPlugins.length, 1);

  const fixtureRow = page.locator('section[aria-label="Fixture"]');
  await fixtureRow.getByRole('button', { name: 'remove', exact: true }).click();
  await fixtureRow.getByRole('button', { name: 'Yes, remove Fixture', exact: true }).click();
  await fixtureRow.waitFor({ state: 'detached' });
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
