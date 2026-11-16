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

// The stand-in kernel: answers /api/complete with two alternatives, checks the token.
const asked = [];
const kernel = createServer((req, res) => {
  let body = '';
  req.on('data', (c) => (body += c));
  req.on('end', () => {
    assert.equal(req.headers.authorization, 'Bearer test-token');
    // The zen app around the editor asks the kernel for its board too: nothing there.
    if (!req.url.startsWith('/api/complete')) {
      res.writeHead(200, { 'content-type': 'application/json' });
      return res.end(JSON.stringify(req.url.startsWith('/api/board') ? { sessions: [], archived: 0 } : {}));
    }
    const b = JSON.parse(body);
    asked.push(b);
    res.writeHead(200, { 'content-type': 'application/json' });
    const text = b.purpose === 'writer shrink'
      ? '{"fixes": [{"find": " different", "replace": "", "why": "not needed"}, {"find": "Test idea", "replace": "Idea", "why": "shorter"}]}'
      : b.purpose === 'writer marks' ? '{"marks": [{"find": "paragraph", "kind": "filler", "why": "test"}]}'
      : '{"alternatives": ["Next sentence.", "Another line."]}';
    res.end(JSON.stringify({ text, model: 'claude/test' }));
  });
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
/** Selects the n-th occurrence of `text` in the page, like a drag with the mouse. */
async function select(text, n = 0) {
  await page.evaluate(([text, n]) => {
    const v = window.__zw;
    const doc = v.state.doc.toString();
    let at = -1;
    for (let i = 0; i <= n; i++) at = doc.indexOf(text, at + 1);
    if (at < 0) throw new Error(`no ${text}`);
    v.dispatch({ selection: { anchor: at, head: at + text.length } });
    v.focus();
  }, [text, n]);
}
/** Opens a tab of the left panel (Alt+L moves through them), keeping the focus where it is. */
async function tab(name) {
  for (let i = 0; i < 4 && !(await page.locator('.tabs button.on', { hasText: name }).count()); i++) await page.keyboard.press('Alt+l');
}

async function waitSaved() {
  await page.waitForFunction(() => document.querySelector('.status')?.textContent === 'Saved', null, { timeout: 5000 });
}
let step = '';
try {
  step = 'new note: file name, then H1';
  await page.goto(`${base}/?new=wiki`);
  await page.keyboard.type('Test idea');
  await page.keyboard.press('Enter');
  assert.equal(await page.inputValue('#f'), 'Test idea');
  await page.keyboard.press('Enter');
  await page.waitForSelector('.cm-content');
  await page.waitForTimeout(1500);
  assert.equal(existsSync(file), false, 'a note with only its H1 is not created');

  step = 'writing saves by itself';
  await page.keyboard.type('Hello world. Second sentence.');
  await waitSaved();
  assert.equal(read(), '# Test idea\n\nHello world. Second sentence.');
  assert.equal(await page.textContent('.count'), '6 words');

  const showing = (t) => page.waitForFunction((t) => document.querySelector('.cm-content')?.textContent?.includes(t), t);

  step = 'Alt+A with nothing selected only hints';
  await page.evaluate(() => { const v = window.__zw; v.dispatch({ selection: { anchor: 2 } }); v.focus(); });
  await tab('alternatives'); await page.keyboard.press('Alt+Enter');
  await page.waitForSelector('.flash');
  assert.equal(await page.locator('.alts').count(), 0);

  step = 'my own alternative: added, shown, but the default stays';
  await select('world');
  await tab('alternatives'); await page.keyboard.press('Alt+Enter');
  await page.waitForSelector('.alts');
  await page.keyboard.type('earth');
  await page.keyboard.press('Enter');
  await showing('Hello earth.');
  assert.equal(await page.locator('.zw-dot').count(), 2);
  assert.equal(await page.locator('.panel li.sel .text').textContent(), 'earth');
  assert.equal(await page.locator('.panel li.def .text').textContent(), 'world');
  assert.equal(await page.locator('.panel .badge').count(), 1);
  assert.equal(await page.locator('.panel li.def .badge').textContent(), 'D');
  assert.equal(await page.locator('.panel li.def .who').textContent(), '✍️', 'the default still says who wrote it');
  assert.equal(await page.locator('.cm-content .zw-dot-def:not(.zw-dot-on)').count(), 1, 'the default is blue but small while another shows');
  assert.equal(await page.locator('.cm-content .zw-dot-on:not(.zw-dot-def)').count(), 1, 'the version showing is big');
  await shot('1-panel');
  if (shots) await page.locator('.cm-line').nth(2).screenshot({ path: path.join(shotDir, 'dots-preview.png') });
  await page.keyboard.press('Escape');
  await showing('Hello world.');
  assert.equal(await page.locator('.cm-content .zw-dot-on.zw-dot-def').count(), 1, 'closed: the default shows');
  if (shots) await page.locator('.cm-line').nth(2).screenshot({ path: path.join(shotDir, 'dots-closed.png') });

  step = 'a new note starts in the editor';
  assert.equal(await page.locator('.page.bare').count(), 0);

  step = 'Alt+arrows with the panel closed show another version; the default stays blue';
  await tab('alternatives'); await page.keyboard.press('Alt+ArrowDown');
  await showing('Hello earth.');
  assert.equal(await page.locator('.cm-content .zw-dot-def:not(.zw-dot-on)').count(), 1, 'blue stays on the default');
  assert.equal(await page.locator('.cm-content .zw-dot-on:not(.zw-dot-def)').count(), 1, 'the big dot moved');
  await waitSaved();
  assert.match(read(), /^# Test idea\n\nHello world\. /, 'the file keeps the default while another version shows');
  await tab('alternatives'); await page.keyboard.press('Alt+ArrowUp');
  await showing('Hello world.');
  await waitSaved();
  let t = read();
  assert.match(t, /^# Test idea\n\nHello world\. Second sentence\.\n\n<!-- zen:alternatives\n/);
  assert.match(t, /"text":"earth","by":"owner"/);

  step = 'flipping is not undone by Ctrl+Z, writing is';
  await tab('alternatives'); await page.keyboard.press('Alt+ArrowDown');
  await showing('Hello earth.');
  await page.keyboard.press('End');
  await page.keyboard.type(' More');
  await page.keyboard.press('Control+z');
  await page.waitForFunction(() => !document.querySelector('.cm-content')?.textContent?.includes('More'));
  assert.ok((await page.textContent('.cm-content')).includes('Hello earth.'), 'undo kept the flip');

  step = 'the arrows flip the alternatives under the pointer, the caret stays';
  const caret = await page.evaluate(() => window.__zw.state.selection.main.head);
  await page.hover('.cm-content .zw-alt');
  await page.waitForSelector('.zw-hover');
  await page.keyboard.press('ArrowLeft');
  await showing('Hello world.');
  assert.equal(await page.evaluate(() => window.__zw.state.selection.main.head), caret);
  await page.keyboard.press('ArrowRight');
  await showing('Hello earth.');
  await page.mouse.move(1, 1);
  await page.waitForFunction(() => !document.querySelector('.zw-hover'));

  step = 'Enter in the panel makes the version showing the default';
  await select('earth');
  await tab('alternatives'); await page.keyboard.press('Alt+Enter');
  await page.waitForSelector('.alts');
  await page.keyboard.press('Enter');
  await page.keyboard.press('Escape');
  assert.equal(await page.locator('.cm-content .zw-dot-on.zw-dot-def').count(), 1);
  await waitSaved();
  assert.doesNotMatch(read(), /"default"/);

  step = 'AI alternatives from the panel, Enter makes one the default';
  await select('Second sentence.');
  await tab('alternatives'); await page.keyboard.press('Alt+Enter');
  await page.waitForSelector('.alts');
  await page.keyboard.press('Alt+g');
  await page.waitForSelector('.panel li:nth-child(3)');
  assert.equal(await page.locator('.panel .who', { hasText: '🤖' }).count(), 2);
  assert.equal(asked.length, 1);
  assert.match(asked[0].prompt, /Second sentence\./);
  assert.match(asked[0].prompt, /Give 3 alternatives/);
  await showing('Next sentence.');
  assert.equal(await page.locator('.cm-content .zw-robot').count(), 1, '🤖 shows in the page when an AI version shows');
  await page.keyboard.press('Enter');
  assert.equal(await page.locator('.panel li.def .text').textContent(), 'Next sentence.');
  assert.equal(await page.locator('.panel li.def .who').textContent(), '🤖', 'an AI default stays marked as the AI\'s');
  await shot('2-ai');

  step = 'Del deletes the selected version';
  await page.keyboard.press('ArrowDown');
  await showing('Another line.');
  await page.keyboard.press('Delete');
  assert.equal(await page.locator('.panel li').count(), 2);
  await page.keyboard.press('Escape');
  await showing('Next sentence.');
  await waitSaved();
  t = read();
  assert.match(t, /"text":"Next sentence\.","by":"ai","model":"claude\/test"/);
  assert.doesNotMatch(t, /Another line/);

  step = 'editing an AI version makes it mine';
  await page.keyboard.press('End');
  await page.keyboard.press('ArrowLeft');
  await page.keyboard.type(' too');
  await waitSaved();
  t = read();
  assert.match(t, /Next sentence too\./);
  assert.match(t, /"text":"Next sentence too\.","by":"owner"/);
  assert.equal(await page.locator('.cm-content .zw-robot').count(), 0);

  const reading = (t) => page.waitForFunction((t) => document.querySelector('.reader')?.textContent?.includes(t), t);

  step = 'reopening writes again (the address keeps edit=1) and places the groups again';
  await page.reload();
  await page.waitForSelector('.zw-dots:visible');
  assert.equal(await page.locator('.zw-dots').count(), 2);
  assert.equal(await page.getAttribute('.cm-content', 'contenteditable'), 'true');
  await shot('3-reopened');

  step = 'Alt+W reads the note in the reader, at its defaults';
  await select('earth');
  await tab('alternatives'); await page.keyboard.press('Alt+ArrowDown');
  await showing('Hello world.');
  await page.keyboard.press('Alt+w');
  await reading('Hello earth.');
  assert.equal(await page.locator('.page').count(), 0, 'the editor is gone');
  assert.doesNotMatch(page.url(), /edit=1/);
  await shot('3-read');

  step = 'e writes again, with the version left showing';
  await page.keyboard.press('e');
  await page.waitForSelector('.zw-dots:visible');
  await showing('Hello world.');

  step = 'the file keeps the default and remembers the version left showing';
  await waitSaved();
  t = read();
  assert.match(t, /^# Test idea\n\nHello earth\. /);
  assert.match(t, /"active":1,"showing":0,/);
  await page.reload();
  await page.waitForSelector('.zw-dots:visible');
  await showing('Hello world.');
  assert.equal(await page.locator('.cm-content .zw-dot-on:not(.zw-dot-def)').count(), 1, 'big grey on the one showing, blue on the default');
  await select('world');
  await tab('alternatives'); await page.keyboard.press('Alt+ArrowUp');
  await showing('Hello earth.');
  await waitSaved();
  assert.doesNotMatch(read(), /"showing"/);

  step = 'a selection over alternatives never replaces them';
  await select('Hello earth. Next sentence too.');
  await tab('alternatives'); await page.keyboard.press('Alt+Enter');
  await page.waitForSelector('.flash');
  assert.equal(await page.locator('.alts').count(), 0);
  assert.equal(await page.locator('.zw-dots').count(), 2, 'both groups are still there');
  await select('Next');
  await tab('alternatives'); await page.keyboard.press('Alt+Enter');
  await page.waitForSelector('.alts');
  assert.match(await page.textContent('.panel-head'), /Sentence · 2 versions/, 'a word inside a sentence opens the sentence');

  step = 'Alt+Delete removes all alternatives, keeping the default';
  await page.keyboard.press('Alt+Delete');
  await page.waitForFunction(() => !document.querySelector('.alts'));
  assert.equal(await page.locator('.zw-dots').count(), 1);
  await showing('Hello earth. Next sentence too.');
  await page.keyboard.press('Control+z');
  await page.waitForFunction(() => document.querySelectorAll('.zw-dots').length === 2, null, { timeout: 3000 });
  await page.keyboard.press('Control+y');
  await page.waitForFunction(() => document.querySelectorAll('.zw-dots').length === 1, null, { timeout: 3000 });
  await select('earth');
  await tab('alternatives'); await page.keyboard.press('Alt+Enter');
  await page.waitForSelector('.alts');
  await page.keyboard.press('Alt+Delete');
  await page.waitForFunction(() => !document.querySelector('.zw-dots'));
  await waitSaved();
  assert.equal(read(), '# Test idea\n\nHello earth. Next sentence too.\n');

  step = 'paragraph group: margin line and dots down it';
  await select('Hello earth. Next sentence too.');
  await tab('alternatives'); await page.keyboard.press('Alt+Enter');
  await page.keyboard.type('A different paragraph. It has two sentences.');
  await page.keyboard.press('Enter');
  await page.keyboard.press('Enter');
  await page.keyboard.press('Escape');
  await page.waitForSelector('.cm-line.zw-alt-para .zw-dots-para');
  await showing('A different paragraph.');
  await shot('4-paragraph');
  await waitSaved();
  assert.match(read(), /"scope":"paragraph"/);

  step = 'deleting down to one version drops the alternatives';
  await tab('alternatives'); await page.keyboard.press('Alt+Enter');
  await page.waitForSelector('.alts');
  await page.keyboard.press('ArrowUp');
  await page.keyboard.press('Delete');
  await page.keyboard.press('Escape');
  await waitSaved();
  assert.equal(read(), '# Test idea\n\nA different paragraph. It has two sentences.\n');

  const cursorIn = (text) => page.evaluate((text) => {
    const v = window.__zw;
    const at = v.state.doc.toString().indexOf(text) + 2;
    v.dispatch({ selection: { anchor: at } });
    v.focus();
  }, text);
  const ghosted = () => page.evaluate(() => [...document.querySelectorAll('.cm-content .zw-ghost')].map((e) => e.textContent).join('|'));

  step = 'Alt+G in the ghosts tab ghosts the selection: faded here, gone from the file';
  await select('It has two sentences.');
  await tab('ghosts'); await page.keyboard.press('Alt+g');
  await page.waitForSelector('.zw-ghost');
  assert.equal(await page.locator('.ghostlist li').count(), 1, 'the ghosts tab lists it');
  assert.equal(await ghosted(), ' It has two sentences.');
  assert.equal(await page.textContent('.count'), '5 words');
  await waitSaved();
  t = read();
  assert.ok(t.startsWith('# Test idea\n\nA different paragraph.\n\n<!-- zen:alternatives'), t);
  assert.match(t, /"ghosts":\[\{"id":"[^"]+","text":" It has two sentences\.","at":/);
  await shot('5-ghost');

  step = 'Alt+Enter in a ghost brings it back';
  await cursorIn('two sentences');
  await page.keyboard.press('Alt+Enter');
  await page.waitForFunction(() => !document.querySelector('.zw-ghost'));
  await waitSaved();
  assert.equal(read(), '# Test idea\n\nA different paragraph. It has two sentences.\n');

  step = 'Alt+K keeps the selection and ghosts the rest of the paragraph';
  await select('A different paragraph.');
  await page.keyboard.press('Alt+k');
  await page.waitForSelector('.zw-ghost');
  assert.equal(await ghosted(), ' It has two sentences.');
  await waitSaved();
  assert.ok(read().startsWith('# Test idea\n\nA different paragraph.\n'));

  step = 'reading leaves ghosts out';
  await page.click('.count');
  await reading('A different paragraph.');
  assert.equal((await page.textContent('.reader')).includes('two sentences'), false);
  await page.click('.reader button[title^="write"]');
  await page.waitForSelector('.zw-ghost:visible');

  step = 'no alternatives over ghosted text';
  await cursorIn('two sentences');
  await tab('alternatives'); await page.keyboard.press('Alt+Enter');
  await page.waitForSelector('.flash');
  assert.match(await page.textContent('.flash'), /ghosted/);
  assert.equal(await page.locator('.alts').count(), 0);

  step = 'Alt+Delete deletes a ghost for good; undo brings it back';
  await cursorIn('two sentences');
  await tab('ghosts'); await page.keyboard.press('Alt+Delete');
  await page.waitForFunction(() => !document.querySelector('.cm-content')?.textContent?.includes('two sentences'));
  await waitSaved();
  assert.equal(read(), '# Test idea\n\nA different paragraph.\n');
  await page.keyboard.press('Control+z');
  await page.waitForSelector('.zw-ghost');
  assert.equal(await ghosted(), ' It has two sentences.');

  step = 'ghosts come back on reopening';
  await waitSaved();
  const written = statSync(file).mtimeMs;
  await page.reload();
  await page.waitForSelector('.zw-ghost:visible');
  assert.equal(await ghosted(), ' It has two sentences.');
  step = 'opening and turning on the editor never rewrite the file';
  await page.waitForTimeout(1500);
  assert.equal(statSync(file).mtimeMs, written);
  await page.evaluate(() => { const v = window.__zw; v.dispatch({ selection: { anchor: v.state.doc.length } }); v.focus(); });

  step = 'Shrink (Alt+P in ghosts) reads once and shows cuts by level; a cut taken is ghosted';
  await tab('ghosts');
  await page.keyboard.press('Alt+p');
  await page.waitForSelector('.zw-fix-cut');
  assert.match(asked.at(-1).prompt, /least needed first/);
  assert.equal(await page.textContent('.shrink-goal button.on'), '10%');
  assert.equal(await page.locator('.cm-content .zw-fix-cut').count(), 1, 'only the 10% cut shows');
  await page.keyboard.press('Alt+p');
  await page.keyboard.press('Alt+p');
  assert.equal(await page.textContent('.shrink-goal button.on'), '30%');
  assert.equal(await page.textContent('.zw-fix-new'), 'Idea', 'a reworded cut shows its shorter words in place');
  assert.match(await page.textContent('.checkpanel .counts'), /2 cuts, −2 words/);
  const kernelCalls = asked.length;
  await page.keyboard.press('Alt+Enter');
  await page.waitForFunction(() => document.querySelectorAll('.cm-content .zw-fix-cut').length === 1);
  assert.equal(await ghosted(), 'different | It has two sentences.', 'the cut is ghosted, not deleted');
  await page.keyboard.press('Alt+Delete');
  await page.waitForFunction(() => !document.querySelector('.zw-fix-cut'));
  assert.equal(asked.length, kernelCalls, 'changing level asks nothing more');
  await waitSaved();
  assert.ok(read().startsWith('# Test idea\n\nA paragraph.'), read());
  assert.match(await page.textContent('.shrink-goal'), /5 → 4 words, −20%/);
  await page.keyboard.press('Control+z');
  await page.waitForFunction(() => document.querySelectorAll('.cm-content .zw-ghost').length === 1);
  await waitSaved();

  step = 'marks (Alt+P in check) point and change nothing';
  const unmarked = read();
  await tab('check');
  await page.keyboard.press('Alt+p');
  await page.keyboard.press('Alt+g');
  await page.waitForSelector('.zw-fix-filler');
  assert.equal(asked.at(-1).purpose, 'writer marks');
  await page.keyboard.press('Alt+Enter');
  await page.waitForFunction(() => !document.querySelector('.zw-fix-filler'));
  await page.waitForTimeout(1200);
  assert.equal(read(), unmarked);

  step = 'Markdown reads as it renders, its marks back on the caret line; links open';
  writeFileSync(path.join(ROOT, 'wiki/Linked.md'), '# Linked\n\nHi.\n');
  await page.evaluate(() => { const v = window.__zw; v.dispatch({ changes: { from: v.state.doc.length, insert: '\n\n## Links\n\n- See [[Linked]] and **[site](https://example.com)**.\n- [ ] task\n\nSo $SR = \\mu/\\sigma$, not $5 and $10.\n\n$$NW=A-L,\\qquad\nR=\\frac{a}{b}.$$ After.' } }); v.dispatch({ selection: { anchor: 0 } }); v.focus(); });
  await page.waitForSelector('.zw-wiki');
  const line = () => page.evaluate(() => [...document.querySelectorAll('.cm-line')].find((l) => l.textContent.includes('See'))?.innerText);
  assert.equal(await line(), '•See Linked and site.', 'marks hidden away from the caret');
  assert.equal(await page.locator('.cm-content input.zw-task').count(), 1, 'a task shows its box');
  await page.waitForSelector('.zw-math .katex');
  assert.equal(await page.locator('.zw-math').count(), 2, 'math drawn, over lines too, money left alone');
  await page.evaluate(() => { const v = window.__zw; v.dispatch({ selection: { anchor: v.state.doc.toString().indexOf('Linked]]') } }); });
  assert.equal(await line(), '- See [[Linked]] and site.', 'the caret line shows its list mark, inline marks only where the caret is');
  await page.evaluate(() => { const v = window.__zw; v.dispatch({ selection: { anchor: v.state.doc.toString().indexOf('site]') } }); });
  assert.equal(await line(), '- See Linked and **[site](https://example.com)**.', 'the touched link shows its source');
  await page.evaluate(() => { const v = window.__zw; v.dispatch({ selection: { anchor: 0 } }); });
  await waitSaved();
  await page.click('.zw-wiki');
  await page.waitForURL(/Linked\.md/);
  await page.waitForFunction(() => window.__zw?.state.doc.toString().startsWith('# Linked'));
  assert.match(page.url(), /edit=1/, 'a link opens the note for writing, in the same pane');
  await page.goto(`${base}/?path=${encodeURIComponent('wiki/Test idea.md')}`);
  await page.waitForFunction(() => window.__zw?.state.doc.toString().startsWith('# Test idea'));
  await page.evaluate(() => { const v = window.__zw; v.dispatch({ selection: { anchor: v.state.doc.length } }); v.focus(); });

  step = 'a change on disk is never overwritten';
  writeFileSync(file, '# Test idea\n\nChanged in Obsidian.\n');
  await page.keyboard.type(' x');
  await page.waitForSelector('.banner');
  assert.equal(read(), '# Test idea\n\nChanged in Obsidian.\n');

  assert.deepEqual(errors, []);
  console.log('e2e: all passed');
} catch (e) {
  console.error(`e2e FAILED at: ${step}\n`, e);
  await page.screenshot({ path: path.join(tmpdir(), 'zen-web-fail.png') });
  console.error('screenshot:', path.join(tmpdir(), 'zen-web-fail.png'), 'errors:', errors);
  process.exitCode = 1;
} finally {
  await browser.close();
  server.kill();
  kernel.close();
}
