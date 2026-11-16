#!/usr/bin/env node
// Builds the marketplace's static page from marketplace/plugins.json (Node 22, no dependencies):
// marketplace/site/index.html (inline CSS, no script) and a copy of plugins.json beside it, so
// others can fetch the list. The .github/workflows/marketplace.yml workflow publishes the folder to
// GitHub Pages on every push to main that touches marketplace/. The entries are checked by
// scripts/marketplace-check.mjs before they get here; the page escapes everything regardless.
//
//   node scripts/marketplace-page.mjs [--out <dir>]     default marketplace/site
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const REPO = 'https://github.com/Zegoverno/zenbot';

const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const link = (url, text) => (/^https:\/\//.test(url) ? `<a href="${esc(url)}">${esc(text)}</a>` : esc(text));

/** A permission in plain words. */
export function permissionText(p) {
  if (p === 'files:read') return 'Read your files';
  if (p === 'files:write') return 'Read and change your files';
  if (p.startsWith('files:write:')) return `Read your files, and change those in the “${p.slice('files:write:'.length)}” folder`;
  if (p === 'network') return 'Use the internet';
  if (p === 'ai') return 'Ask zenbot’s AI model for answers';
  return p;
}

const SLOT_TEXT = {
  views: 'A tab on the board and in the side panel',
  openers: 'An “open with” choice for some files',
  games: 'A game in the Games tab',
  commands: 'Slash commands in chat, Matrix and the command line',
};

export function renderPage(doc) {
  const plugins = Array.isArray(doc.plugins) ? doc.plugins : [];
  const card = (p) => `
    <article aria-labelledby="p-${esc(p.name)}">
      <h2 id="p-${esc(p.name)}">${esc(p.title)} <span class="ver">${esc(p.version)}</span></h2>
      <p class="by">${esc(p.name)} · by ${esc(p.author)} · ${esc(p.license)}</p>
      <p>${esc(p.description)}</p>
      <dl>
        <dt>It can</dt>
        <dd>${(p.kernel ?? []).length
          ? `<ul>${p.kernel.map((k) => `<li>${esc(permissionText(k))}</li>`).join('')}</ul>`
          : 'Nothing beyond its own folder: no access to your files or the internet.'}</dd>
        <dt>It adds</dt>
        <dd><ul>${(p.contributes ?? []).map((s) => `<li>${esc(SLOT_TEXT[s] ?? s)}</li>`).join('')}</ul></dd>
        ${(p.categories ?? []).length ? `<dt>Categories</dt><dd>${p.categories.map(esc).join(', ')}</dd>` : ''}
      </dl>
      <p class="install">Install: <code>zen plugins add ${esc(p.name)}</code> or Settings → Marketplace in zenbot’s web app.</p>
      <p class="links">${link(p.repository, 'Source code')}${p.homepage ? ` · ${link(p.homepage, 'Homepage')}` : ''}</p>
    </article>`;

  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>zenbot plugin marketplace</title>
<meta name="description" content="Plugins for zenbot: apps that run in its web app, sandboxed, with the permissions they ask for shown before you install.">
<style>
:root { color-scheme: light dark; --bg: #fbfbfa; --fg: #1d1d1b; --muted: #55554f; --card: #ffffff; --line: #dcdcd6; --link: #0b5cad; --code: #efefea; }
@media (prefers-color-scheme: dark) { :root { --bg: #151514; --fg: #ecece8; --muted: #a8a8a0; --card: #1e1e1c; --line: #34342f; --link: #7db6f5; --code: #2a2a27; } }
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--fg); font: 16px/1.55 system-ui, -apple-system, "Segoe UI", sans-serif; }
header, main, footer { max-width: 46rem; margin: 0 auto; padding: 0 1.25rem; }
header { padding-top: 2.5rem; }
h1 { font-size: 1.75rem; margin: 0 0 .25rem; }
h2 { font-size: 1.2rem; margin: 0; }
.lead { color: var(--muted); margin: 0 0 1rem; }
a { color: var(--link); text-underline-offset: .15em; }
a:focus-visible { outline: 3px solid var(--link); outline-offset: 2px; border-radius: 2px; }
nav a { margin-right: 1rem; }
article, .empty { background: var(--card); border: 1px solid var(--line); border-radius: .6rem; padding: 1rem 1.25rem; margin: 1rem 0; }
.ver { font-size: .85rem; font-weight: 400; color: var(--muted); margin-left: .35rem; }
.by { color: var(--muted); font-size: .9rem; margin: .1rem 0 .6rem; }
dl { margin: .75rem 0; display: grid; grid-template-columns: max-content 1fr; gap: .25rem 1rem; }
dt { color: var(--muted); }
dd { margin: 0; }
ul { margin: 0; padding-left: 1.1rem; }
code { background: var(--code); border-radius: .3rem; padding: .1rem .35rem; font-size: .92em; }
.install, .links { margin: .5rem 0 0; font-size: .95rem; }
footer { color: var(--muted); font-size: .9rem; padding-top: 1rem; padding-bottom: 3rem; }
@media (max-width: 30rem) { dl { grid-template-columns: 1fr; } dd { margin-bottom: .4rem; } }
</style>
</head>
<body>
<header>
  <h1>zenbot plugin marketplace</h1>
  <p class="lead">Apps that run inside <a href="${REPO}">zenbot</a>. Each is sandboxed, and zenbot shows the permissions a plugin asks for before you install it.</p>
  <nav aria-label="Marketplace">
    <a href="${REPO}/blob/main/PLUGINS.md">Build a plugin (PLUGINS.md)</a>
    <a href="${REPO}/issues/new?template=list-a-plugin.yml">List a plugin</a>
    <a href="plugins.json">plugins.json</a>
  </nav>
</header>
<main>
  ${plugins.length
    ? plugins.map(card).join('\n')
    : `
  <section class="empty" aria-labelledby="none">
    <h2 id="none">No plugins listed yet</h2>
    <p>The first ones are on their way. If you built one, <a href="${REPO}/blob/main/PLUGINS.md">PLUGINS.md</a> says how to release it and get it listed.</p>
  </section>`}
</main>
<footer>
  <p>Every listing is reviewed by a maintainer at its release tag before it appears here. Fetch the list at <a href="plugins.json">plugins.json</a>.</p>
</footer>
</body>
</html>
`;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const root = join(dirname(fileURLToPath(import.meta.url)), '..');
  const oi = process.argv.indexOf('--out');
  const out = resolve(oi >= 0 && process.argv[oi + 1] ? process.argv[oi + 1] : join(root, 'marketplace/site'));
  const src = join(root, 'marketplace/plugins.json');
  const doc = JSON.parse(readFileSync(src, 'utf8'));
  mkdirSync(out, { recursive: true });
  writeFileSync(join(out, 'index.html'), renderPage(doc));
  copyFileSync(src, join(out, 'plugins.json'));
  console.log(`marketplace page: ${out}/index.html (${(doc.plugins ?? []).length} plugin(s)) and plugins.json`);
}
