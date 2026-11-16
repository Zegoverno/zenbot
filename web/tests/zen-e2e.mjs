// End-to-end for the zen app: the real server (dev mode) on a scratch home, a stand-in zen kernel
// (HTTP and a minimal WebSocket, checking the token on every request), and headless Chromium.
// The slash commands come from this checkout's `zen --commands` (built here, or ZEN_BIN).
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
const ZEN_BIN = process.env.ZEN_BIN ?? (() => {
  const root = path.resolve(import.meta.dirname, '../..');
  execFileSync('cargo', ['build', '-q', '-p', 'zen'], { cwd: root, stdio: 'inherit' });
  return path.join(root, 'target/debug/zen');
})();
const PORT = 4412;
const KERNEL = 4413;
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
const saves = [];

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
      { ...session('a1', 'Old chat', { archived: true }), busy: false, waiting: false },
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
  if (r === 'GET /api/tasks')
    return json(res, { today: '2026-10-30', tz: 'UTC', count: 1, problems: [], projects: [{ project: 'acme', tasks: [
      { project: 'acme', text: 'Price the offer', id: 't1', due: '2026-10-29', scheduled: null, priority: 'urgent', goals: [], file: 'x', line: 1, overdue: true },
    ] }] });
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
await new Promise((ok) => server.stdout.on('data', (d) => String(d).includes('zen web on') && ok()));

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
  assert.equal(await page.getByText('v1: first').isVisible().catch(() => false), false, 'the changelog is not in the body');
  // Next / previous file in the folder, without the tree (buttons and n / N).
  await page.locator('.reader button[title^="next file"]').click();
  await page.getByRole('heading', { name: 'Grace' }).waitFor();
  assert.ok(await page.locator('.reader button[title^="next file"]').isDisabled(), 'the last file has no next');
  // Alt+O opens the source named in the properties, in a new tab.
  const [tab] = await Promise.all([page.context().waitForEvent('page'), page.keyboard.press('Alt+o')]);
  await tab.waitForLoadState('commit').catch(() => {});
  assert.match(tab.url(), /example\.com\/grace/);
  await tab.close();
  await page.bringToFront();
  await page.keyboard.press('N');
  await page.getByRole('heading', { name: 'God' }).waitFor();

  // Reading aids: spacing, bionic, focus, width and play; the front matter unfolds.
  const reader = page.locator('.reader').first();
  await page.keyboard.press(':');
  await page.locator('.reader.spaced').waitFor();
  await page.keyboard.press(':');
  await page.locator('.reader.spaced').waitFor({ state: 'detached' });
  const isOn = (l) => l.evaluate((e) => e.classList.contains('on'));
  const bionic = reader.locator('button[title="bionic (b)"]');
  await page.keyboard.press('b');
  assert.ok(await isOn(bionic), 'b turns bionic on');
  await page.keyboard.press('b');
  assert.ok(!(await isOn(bionic)));
  const focusButton = reader.locator('button[title^="focus:"]');
  for (const want of [/para/, /line/, /off/]) {
    await page.keyboard.press('f');
    assert.match(await focusButton.innerText(), want);
  }
  const measure = () => reader.locator('.rpage').first().evaluate((e) => e.style.maxWidth);
  const measure0 = await measure();
  await page.keyboard.press('>');
  assert.notEqual(await measure(), measure0, '> widens the page');
  await page.keyboard.press('<');
  assert.equal(await measure(), measure0);
  const playButton = reader.locator('button[title^="play one word"]');
  await page.keyboard.press('p');
  assert.match(await playButton.innerText(), /stop/, 'p plays word by word');
  await page.keyboard.press('Escape');
  assert.match(await playButton.innerText(), /play/);
  await page.keyboard.press('i');
  await reader.locator('.fm.open', { hasText: 'idea' }).waitFor();
  await page.keyboard.press('i');
  await reader.locator('.fm.open').waitFor({ state: 'detached' });
  // Headings: ] and [ jump between them.
  await page.goto(`${base}/?view=files&file=${encodeURIComponent(path.join(VAULT, 'wiki/Atlas.md'))}`);
  await page.getByRole('heading', { name: 'Atlas' }).waitFor();
  const scrolled = () => page.locator('.reader .scroll').evaluate((e) => e.scrollTop);
  await page.keyboard.press(']'); // the title
  await page.keyboard.press(']'); // North
  await page.waitForFunction(() => document.querySelector('.reader .scroll').scrollTop > 100);
  const north = await scrolled();
  await page.keyboard.press(']');
  await page.waitForFunction((n) => document.querySelector('.reader .scroll').scrollTop > n + 100, north);
  await page.keyboard.press('[');
  await page.waitForFunction((n) => Math.abs(document.querySelector('.reader .scroll').scrollTop - n) < 4, north);
  await page.goto(`${base}/?view=files&file=${encodeURIComponent(path.join(VAULT, 'wiki/God.md'))}`);
  await page.getByRole('heading', { name: 'God' }).waitFor();

  // The same pane writes it (e), in the writer, and Alt+W reads it again.
  await page.keyboard.press('e');
  await page.locator('.page .cm-content').waitFor();
  assert.match(page.url(), /edit=1/);
  assert.equal(await page.locator('.board .page').count(), 1, 'the editor sits in the board');
  await page.keyboard.press('Alt+w');
  await page.locator('.reader').waitFor();
  assert.equal(await page.locator('.page').count(), 0);
  // The editor's old address lands on the board, writing the note.
  await page.goto(`${base}/?path=${encodeURIComponent('wiki/God.md')}`);
  await page.locator('.board .page .cm-content').waitFor();
  assert.match(page.url(), /view=files&file=.*God\.md&edit=1/);

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
  await page.locator('.zside .reader button[title^="write"]').click();
  await page.locator('.zside .page .cm-content').waitFor();
  if (shots) {
    await page.screenshot({ path: 'tests/shots/zen-side-editor.png' });
    await page.keyboard.press('Alt+l');
    await page.keyboard.press('Alt+r');
    await page.screenshot({ path: 'tests/shots/zen-side-panels.png' });
  }
  await page.locator('.zside .page header .home').click();
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

  assert.deepEqual(errors, []);
  console.log('zen e2e: all passed');
} finally {
  await browser.close();
  server.kill();
  kernel.close();
}
