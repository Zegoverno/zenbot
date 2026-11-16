// End-to-end: the real server (dev mode) on a scratch folder, a stand-in zen kernel for the AI,
// and headless Chromium typing like a person. `node tests/e2e.mjs [--shots]`.
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtempSync, readFileSync, writeFileSync, mkdirSync, existsSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
import { chromium } from 'playwright';

const ROOT = mkdtempSync(path.join(tmpdir(), 'zen-web-e2e-'));
mkdirSync(path.join(ROOT, 'wiki'));
const TOKEN = path.join(ROOT, '.token');
writeFileSync(TOKEN, 'test-token');
const PORT = Number(process.env.ZEN_E2E_PORT ?? 4410);
const KERNEL = Number(process.env.ZEN_E2E_KERNEL_PORT ?? 4411);
const shots = process.argv.includes('--shots');
const shotDir = path.resolve('tests/shots');
if (shots) mkdirSync(shotDir, { recursive: true });

// No installed plugins and no writer AI. Requests other than board discovery are inert.
const kernel = createServer((req, res) => {
  res.writeHead(200, { 'content-type': 'application/json' });
  const out = req.url.includes('plugins') ? { plugins: [], defaults: {} }
    : req.url.includes('board') ? { sessions: [], archived: 0 }
    : req.url.includes('agents') ? { agents: [] } : {};
  res.end(JSON.stringify(out));
}).listen(KERNEL, '127.0.0.1');

const server = spawn(process.execPath, ['server/main.ts', '--dev'], {
  env: { ...process.env, ZEN_WEB_HOME: ROOT, ZEN_WEB_ROOT: ROOT, ZEN_WEB_PORT: String(PORT), ZEN_WEB_AUTH: 'off', ZEN_URL: `http://127.0.0.1:${KERNEL}`, ZEN_TOKEN_FILE: TOKEN },
  stdio: ['ignore', 'pipe', 'inherit'],
});
await new Promise((ok, fail) => {
  const timer = setTimeout(() => fail(new Error('web server startup timed out')), 30000);
  server.once('error', fail);
  server.once('exit', (code) => { clearTimeout(timer); fail(new Error(`web server exited during startup (${code})`)); });
  server.stdout.on('data', (d) => { if (String(d).includes('zen web on')) { clearTimeout(timer); ok(); } });
});

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1200, height: 800 }, deviceScaleFactor: shots ? 3 : 1 });
page.setDefaultTimeout(10000);
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
const base = `http://127.0.0.1:${PORT}`;
const file = path.join(ROOT, 'wiki/Test idea.md');
const read = () => readFileSync(file, 'utf8');
const shot = async (name) => shots && page.screenshot({ path: path.join(shotDir, `${name}.png`) });
try {
  const request = (route, method = 'GET', body) => fetch(base + route, {
    method, headers: { 'content-type': 'application/json', 'x-zen-web': '1' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  // Legacy writer surfaces are gone, not merely hidden.
  for (const [method, route] of [
    ['GET', '/api/versions'], ['GET', '/api/version'], ['GET', '/api/reader'],
    ['PUT', '/api/reader'], ['POST', '/api/close'], ['POST', '/api/branch'],
    ['POST', '/api/changelog'], ['POST', '/api/notes/find'], ['POST', '/api/check'],
    ['POST', '/api/shrink'], ['POST', '/api/marks'], ['POST', '/api/suggest'],
    ['POST', '/api/zen/complete'],
  ]) assert.equal((await request(route, method, method === 'GET' ? undefined : {})).status, 404, route);

  writeFileSync(file, '# Test idea\n\n<script>window.pwned=1</script>\n\n[unsafe](javascript:alert(1))\n');
  await page.goto(`${base}/?view=files&file=${encodeURIComponent(file)}`);
  await page.getByRole('heading', { name: 'Test idea' }).waitFor();
  assert.equal(await page.evaluate(() => window.pwned), undefined);
  assert.equal(await page.locator('a[href^="javascript:"]').count(), 0);
  assert.equal(await page.getByRole('tab', { name: 'Reading', exact: true }).count(), 0);
  assert.equal(await page.locator('.cm-editor').count(), 0);
  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  const source = page.getByRole('textbox', { name: 'Markdown source' });
  await source.waitFor();
  const original = await source.inputValue();
  await source.fill(original + '\nPlain editing.\n');
  await source.press('Control+s');
  await page.getByRole('status').filter({ hasText: /^Saved$/ }).waitFor();
  assert.match(read(), /Plain editing/);
  const onDisk = read();
  await source.fill('Unsaved text');
  page.once('dialog', dialog => dialog.dismiss());
  await page.getByRole('button', { name: 'Read', exact: true }).click();
  assert.equal(await source.inputValue(), 'Unsaved text', 'declining discard keeps edits');
  await source.fill(onDisk);

  // A disk writer wins; conflicts keep local edits instead of silently rebasing them.
  writeFileSync(file, '# Disk wins\n');
  await source.fill('# Local edits kept\n');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await page.getByRole('alert').filter({ hasText: 'Changed on disk' }).waitFor();
  assert.equal(await source.inputValue(), '# Local edits kept\n');
  assert.equal(read(), '# Disk wins\n');
  await page.reload();
  await source.waitFor();
  assert.equal(await source.inputValue(), '# Disk wins\n');
  await page.getByRole('button', { name: 'Read', exact: true }).click();
  await page.getByRole('heading', { name: 'Disk wins' }).waitFor();

  // New Markdown remains possible without plugins, including explicit create-only hash.
  await page.goto(`${base}/?new=wiki`);
  await page.locator('#f').fill('New file');
  await page.locator('#f').press('Enter');
  await page.waitForFunction(() => document.querySelector('#f')?.value === 'New file');
  await page.locator('#f').fill('New title');
  await page.locator('#f').press('Enter');
  await source.waitFor();
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await page.getByRole('status').filter({ hasText: /^Saved$/ }).waitFor();
  assert.equal(readFileSync(path.join(ROOT, 'wiki/New file.md'), 'utf8'), '# New title\n\n');

  // API security and optimistic hashes, including concurrent writes.
  const route = '/api/file?path=' + encodeURIComponent('wiki/Test idea.md');
  const d = await (await request(route)).json();
  const writes = await Promise.all(['one', 'two'].map(text => request('/api/file', 'PUT', { path: 'wiki/Test idea.md', text, base: d.hash })));
  assert.deepEqual(writes.map(r => r.status).sort(), [200, 409]);
  assert.equal((await request('/api/file', 'PUT', { path: '../escape.md', text: '', base: null })).status, 400);
  assert.equal((await request('/api/file', 'PUT', { path: '.secret.md', text: '', base: null })).status, 400);
  assert.equal((await fetch(base + '/api/file', { method: 'PUT', body: '{}' })).status, 403);

  assert.equal(existsSync(path.join(ROOT, '.versions')), false, 'core never creates writer history');

  // Text/code must be text, never interpreted HTML or Markdown.
  for (const name of ['plain.txt', 'code.js']) {
    const p = path.join(ROOT, name);
    writeFileSync(p, '<img src=x onerror="window.pwned=1">\n# Not a heading');
    await page.goto(`${base}/?view=files&file=${encodeURIComponent(p)}`);
    await page.locator('.reader pre').waitFor();
    assert.match(await page.locator('.reader pre').innerText(), /<img/);
    assert.equal(await page.locator('.reader img').count(), 0);
    assert.equal(await page.getByRole('button', { name: 'Edit', exact: true }).count(), 0);
    assert.equal(await page.evaluate(() => window.pwned), undefined);
  }
  assert.deepEqual(errors, []);
  console.log('minimal Files e2e: all passed');
} finally {
  await browser.close();
  server.kill();
  kernel.close();
}
