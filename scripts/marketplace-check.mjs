#!/usr/bin/env node
// Checks marketplace/plugins.json (Node 22, no dependencies). Run by CI on every pull request that
// touches marketplace/ (.github/workflows/marketplace.yml) and by scripts/check.sh (--self-test).
//
//   node scripts/marketplace-check.mjs                  the list's rules, and every archive
//   node scripts/marketplace-check.mjs --base origin/main
//                                                       archives only for entries new or changed
//                                                       against that git ref (the list is always
//                                                       checked whole)
//   node scripts/marketplace-check.mjs --self-test      offline: good and bad archives, built here
//
// The rules are PLUGINS.md's "Releasing" and "Listing in the marketplace", and the install rules of
// ADR 0001 (crates/zend/src/plugins.rs enforces the same ones on the owner's machine). The checks
// are written by hand so the script stays one file; marketplace/schema.json is the same entry
// shape for editors and other tools.
//
// An archive is untrusted: it is only ever read in memory here, never unpacked to disk or run.
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { join, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { gzipSync, gunzipSync } from 'node:zlib';

export const MAX_BYTES = 50 * 1024 * 1024; // the download, and the unpacked total
const TAR_SLACK = 4 * 1024 * 1024; // headers and padding on top of MAX_BYTES of content
export const SLOTS = ['views', 'openers', 'games', 'commands'];
const KERNEL_RE = /^(files:read|files:write|files:write:[^\s]+|network|ai)$/;
const SEMVER_RE = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$/;
const REPO_RE = /^https:\/\/github\.com\/([A-Za-z0-9_.-]+)\/([A-Za-z0-9_.-]+)$/;
const ENTRY_KEYS = [
  'name', 'title', 'description', 'author', 'repository', 'license', 'version', 'archive', 'sha256',
  'kernel', 'contributes', 'categories', 'homepage', 'icon',
];
const REQUIRED = ENTRY_KEYS.filter((k) => k !== 'homepage' && k !== 'icon');

const isStr = (v) => typeof v === 'string';
const isObj = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);

// ---- the list ---------------------------------------------------------------------------------

/** Errors in one entry's own fields (no network). */
export function validateEntry(e) {
  const errs = [];
  if (!isObj(e)) return ['not an object'];
  for (const k of Object.keys(e)) if (!ENTRY_KEYS.includes(k)) errs.push(`unknown field "${k}"`);
  for (const k of REQUIRED) if (!(k in e)) errs.push(`missing "${k}"`);
  const str = (k, max) => {
    if (!(k in e)) return;
    if (!isStr(e[k]) || e[k].trim() === '') errs.push(`"${k}" must be a non-empty string`);
    else if (e[k].length > max) errs.push(`"${k}" is ${e[k].length} characters; at most ${max}`);
  };
  if ('name' in e && !(isStr(e.name) && /^[a-z0-9-]{2,40}$/.test(e.name))) errs.push('"name" must match [a-z0-9-]{2,40}');
  str('title', 80);
  str('description', 200);
  str('author', 80);
  str('license', 60);
  const repo = isStr(e.repository) ? REPO_RE.exec(e.repository) : null;
  if ('repository' in e && !repo) errs.push('"repository" must look like https://github.com/<owner>/<repo>');
  if ('version' in e && !(isStr(e.version) && SEMVER_RE.test(e.version))) errs.push('"version" must be semver (1.2.3)');
  if ('archive' in e) {
    const m = isStr(e.archive) ? /^https:\/\/github\.com\/([^/]+)\/([^/]+)\/releases\/download\/([^/]+)\/([^/]+)$/.exec(e.archive) : null;
    if (!m) errs.push('"archive" must be a https://github.com/<owner>/<repo>/releases/download/v<version>/<name>-<version>.tgz URL');
    else {
      if (repo && `${m[1]}/${m[2]}`.toLowerCase() !== `${repo[1]}/${repo[2]}`.toLowerCase()) {
        errs.push('"archive" must be a release asset of "repository"');
      }
      if (isStr(e.version) && m[3] !== `v${e.version}`) errs.push(`"archive" tag must be v${e.version}`);
      if (isStr(e.name) && isStr(e.version) && m[4] !== `${e.name}-${e.version}.tgz`) {
        errs.push(`"archive" file must be ${e.name}-${e.version}.tgz`);
      }
    }
  }
  if ('sha256' in e && !(isStr(e.sha256) && /^[0-9a-f]{64}$/.test(e.sha256))) errs.push('"sha256" must be 64 lowercase hex characters');
  const list = (k, ok, what, min = 0) => {
    if (!(k in e)) return;
    if (!Array.isArray(e[k])) return errs.push(`"${k}" must be an array`);
    for (const v of e[k]) if (!ok(v)) errs.push(`"${k}": "${v}" is not ${what}`);
    if (new Set(e[k]).size !== e[k].length) errs.push(`"${k}" has duplicates`);
    if (e[k].length < min) errs.push(`"${k}" needs at least ${min} item`);
  };
  list('kernel', (v) => isStr(v) && KERNEL_RE.test(v) && !/(^|[/:])\.\.([/:]|$)|^files:write:\//.test(v),
    'a permission (files:read, files:write, files:write:<subfolder>, network, ai)');
  list('contributes', (v) => SLOTS.includes(v), `a slot (${SLOTS.join(', ')})`);
  list('categories', (v) => isStr(v) && /^[a-z0-9-]{2,30}$/.test(v), 'a category ([a-z0-9-]{2,30})', 1);
  for (const k of ['homepage', 'icon']) {
    if (k in e && !(isStr(e[k]) && /^https:\/\/\S+$/.test(e[k]))) errs.push(`"${k}" must be a https URL`);
  }
  return errs;
}

/** Errors for the whole file: shape, each entry, unique names, sorted by name. */
export function validateList(doc) {
  const errs = []; // {name|null, msg}
  if (!isObj(doc)) return [{ name: null, msg: 'plugins.json must be an object' }];
  for (const k of Object.keys(doc)) if (k !== 'version' && k !== 'plugins') errs.push({ name: null, msg: `unknown field "${k}"` });
  if (doc.version !== 1) errs.push({ name: null, msg: '"version" must be 1' });
  if (!Array.isArray(doc.plugins)) return [...errs, { name: null, msg: '"plugins" must be an array' }];
  const seen = new Set();
  doc.plugins.forEach((e, i) => {
    const name = isObj(e) && isStr(e.name) ? e.name : `#${i}`;
    for (const msg of validateEntry(e)) errs.push({ name, msg });
    if (isObj(e) && isStr(e.name)) {
      if (seen.has(e.name)) errs.push({ name, msg: 'duplicate name' });
      seen.add(e.name);
      const prev = doc.plugins[i - 1];
      if (i > 0 && isObj(prev) && isStr(prev.name) && prev.name >= e.name && prev.name !== e.name) {
        errs.push({ name, msg: `not sorted by name (after "${prev.name}")` });
      }
    }
  });
  return errs;
}

// ---- the archive ------------------------------------------------------------------------------

function octal(buf) {
  if (buf[0] & 0x80) { // base-256, for sizes over 8 GiB; far above our cap, so just read it
    let n = 0;
    for (let i = 1; i < buf.length; i++) n = n * 256 + buf[i];
    return n;
  }
  const s = buf.toString('latin1').replace(/\0.*$/s, '').trim();
  return s === '' ? 0 : parseInt(s, 8);
}
const cstr = (buf) => buf.toString('utf8').replace(/\0.*$/s, '');

function paxRecords(data) {
  const out = {};
  let i = 0;
  while (i < data.length) {
    const sp = data.indexOf(0x20, i);
    if (sp < 0) break;
    const len = parseInt(data.toString('latin1', i, sp), 10);
    if (!(len > 0) || i + len > data.length) break;
    const rec = data.toString('utf8', sp + 1, i + len - 1); // key=value
    const eq = rec.indexOf('=');
    if (eq > 0) out[rec.slice(0, eq)] = rec.slice(eq + 1);
    i += len;
  }
  return out;
}

/**
 * Reads a tar (already gunzipped) into [{ name, type, size, data }]; type is 'file' or 'dir'.
 * Throws on anything malformed or of another kind (links, devices, fifos).
 */
export function readTar(tar) {
  const entries = [];
  let off = 0;
  let longName = null;
  let pax = {};
  while (off + 512 <= tar.length) {
    const h = tar.subarray(off, off + 512);
    if (h.every((b) => b === 0)) break;
    let sum = 0;
    for (let i = 0; i < 512; i++) sum += i >= 148 && i < 156 ? 0x20 : h[i];
    if (sum !== octal(h.subarray(148, 156))) throw new Error('corrupt tar header (bad checksum)');
    const size = octal(h.subarray(124, 136));
    const type = String.fromCharCode(h[156] || 0x30);
    let name = cstr(h.subarray(0, 100));
    if (h.toString('latin1', 257, 262) === 'ustar') {
      const prefix = cstr(h.subarray(345, 500));
      if (prefix) name = `${prefix}/${name}`;
    }
    const dataStart = off + 512;
    if (dataStart + size > tar.length) throw new Error('truncated tar');
    const data = tar.subarray(dataStart, dataStart + size);
    off = dataStart + Math.ceil(size / 512) * 512;
    if (type === 'x') { pax = paxRecords(data); continue; }
    if (type === 'g') continue;
    if (type === 'L') { longName = cstr(data); continue; }
    if (pax.path) name = pax.path;
    if (longName) name = longName;
    pax = {};
    longName = null;
    if (type === '0' || type === '7') entries.push({ name, type: 'file', size, data });
    else if (type === '5') entries.push({ name, type: 'dir', size: 0, data });
    else throw new Error(`"${name}": entry of type '${type}' (${{ 1: 'hard link', 2: 'symlink', 3: 'character device', 4: 'block device', 6: 'fifo' }[type] ?? 'unsupported'}); only files and folders are allowed`);
  }
  return entries;
}

/** The safe, normalised path of a tar entry, or throws why not. */
function cleanPath(raw) {
  if (raw.includes('\\')) throw new Error(`"${raw}": backslash in path`);
  if (raw.startsWith('/')) throw new Error(`"${raw}": absolute path`);
  const parts = raw.split('/').filter((p) => p !== '' && p !== '.');
  for (const p of parts) {
    if (p === '..') throw new Error(`"${raw}": contains ..`);
    if (p.startsWith('.')) throw new Error(`"${raw}": dot file or folder`);
    if (p === 'node_modules') throw new Error(`"${raw}": node_modules (bundle your dependencies)`);
    if (/[\0-\x1f]/.test(p)) throw new Error(`"${raw}": control character in path`);
  }
  return parts.join('/');
}

const sorted = (a) => [...new Set(a)].sort();
const same = (a, b) => JSON.stringify(sorted(a)) === JSON.stringify(sorted(b));

/**
 * Checks archive bytes against its entry. Returns { errors, files } where files is the list of
 * paths. Doesn't touch the network.
 */
export function inspectArchive(bytes, entry) {
  const errors = [];
  const sha = createHash('sha256').update(bytes).digest('hex');
  if (sha !== entry.sha256) errors.push(`sha256 is ${sha}, the entry says ${entry.sha256}`);

  let tar;
  try {
    tar = gunzipSync(bytes, { maxOutputLength: MAX_BYTES + TAR_SLACK });
  } catch (e) {
    errors.push(/larger than|maxOutputLength|buffer/i.test(String(e.message)) ? `unpacks to more than ${MAX_BYTES >> 20} MB` : `not a gzip file: ${e.message}`);
    return { errors, files: [] };
  }
  let entries;
  try { entries = readTar(tar); } catch (e) { return { errors: [...errors, e.message], files: [] }; }

  const files = new Map();
  let total = 0;
  for (const en of entries) {
    let path;
    try { path = cleanPath(en.name); } catch (e) { errors.push(e.message); continue; }
    if (path === '') continue;
    if (en.type === 'file') {
      if (files.has(path)) errors.push(`"${path}": appears twice`);
      files.set(path, en.data);
      total += en.size;
    }
  }
  if (total > MAX_BYTES) errors.push(`unpacks to ${(total / 1048576).toFixed(1)} MB; at most ${MAX_BYTES >> 20} MB`);
  if (errors.some((m) => !m.startsWith('sha256'))) return { errors, files: [...files.keys()] }; // unsafe: read nothing from it

  const pj = files.get('plugin.json');
  if (!pj) return { errors: [...errors, 'plugin.json is not at the archive root'], files: [...files.keys()] };
  let m;
  try { m = JSON.parse(pj.toString('utf8')); } catch (e) { return { errors: [...errors, `plugin.json is not JSON: ${e.message}`], files: [...files.keys()] }; }
  if (!isObj(m)) return { errors: [...errors, 'plugin.json must be an object'], files: [...files.keys()] };

  if (m.name !== entry.name) errors.push(`plugin.json name is ${JSON.stringify(m.name)}, the entry says "${entry.name}"`);
  if (m.version !== entry.version) errors.push(`plugin.json version is ${JSON.stringify(m.version)}, the entry says "${entry.version}"`);

  const kernel = m.kernel ?? [];
  if (!Array.isArray(kernel) || !kernel.every((k) => isStr(k) && KERNEL_RE.test(k))) errors.push('plugin.json "kernel" has an unknown permission');
  else if (!same(kernel, entry.kernel)) errors.push(`plugin.json permissions [${sorted(kernel)}] differ from the entry's [${sorted(entry.kernel)}]`);

  const contributes = m.contributes ?? {};
  if (!isObj(contributes)) errors.push('plugin.json "contributes" must be an object');
  else {
    for (const k of Object.keys(contributes)) if (!SLOTS.includes(k)) errors.push(`plugin.json contributes unknown slot "${k}"`);
    const used = SLOTS.filter((s) => Array.isArray(contributes[s]) && contributes[s].length > 0);
    if (!same(used, entry.contributes)) errors.push(`plugin.json slots [${used}] differ from the entry's [${sorted(entry.contributes)}]`);
    const prefix = `ui://${entry.name}/`;
    for (const s of SLOTS) {
      for (const item of Array.isArray(contributes[s]) ? contributes[s] : []) {
        if (isObj(item) && 'ui' in item && !(isStr(item.ui) && item.ui.startsWith(prefix) && item.ui.length > prefix.length && !item.ui.includes('..'))) {
          errors.push(`${s}: ui ${JSON.stringify(item.ui)} must start with ${prefix}`);
        }
      }
    }
  }

  const server = m.server;
  if (!isObj(server)) errors.push('plugin.json has no "server"');
  else {
    if (server.command !== 'node') errors.push(`server.command must be "node", not ${JSON.stringify(server.command)}`);
    const file = Array.isArray(server.args) ? server.args[0] : undefined;
    if (!isStr(file)) errors.push('server.args[0] (the server file) is missing');
    else {
      let p = null;
      try { p = cleanPath(file); } catch { /* reported below */ }
      if (!p || !files.has(p)) errors.push(`server file "${file}" is not in the archive`);
    }
  }
  return { errors, files: [...files.keys()] };
}

// ---- download ---------------------------------------------------------------------------------

/** Downloads a URL into memory, refusing more than `cap` bytes. */
export async function download(url, cap = MAX_BYTES) {
  const res = await fetch(url, { redirect: 'follow', signal: AbortSignal.timeout(120_000) });
  if (!res.ok) throw new Error(`download failed: HTTP ${res.status}`);
  const len = Number(res.headers.get('content-length'));
  if (len > cap) throw new Error(`download is ${len} bytes; at most ${cap}`);
  const chunks = [];
  let n = 0;
  for await (const c of res.body) {
    n += c.length;
    if (n > cap) throw new Error(`download is over ${cap >> 20} MB`);
    chunks.push(c);
  }
  return Buffer.concat(chunks);
}

// ---- main -------------------------------------------------------------------------------------

function loadBase(ref, file) {
  try {
    return JSON.parse(execFileSync('git', ['show', `${ref}:${file}`], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }));
  } catch (e) {
    const msg = String(e.stderr || e.message);
    if (/exists on disk, but not in|does not exist in/i.test(msg)) return null; // no list in base yet: everything is new
    throw new Error(`cannot read ${file} at ${ref}: ${msg.trim()}`);
  }
}

const canon = (o) => JSON.stringify(o, (_, v) => (isObj(v) ? Object.fromEntries(Object.entries(v).sort(([a], [b]) => (a < b ? -1 : 1))) : v));

async function main(argv) {
  const root = join(dirname(fileURLToPath(import.meta.url)), '..');
  const file = join(root, 'marketplace/plugins.json');
  const bi = argv.indexOf('--base');
  const base = bi >= 0 ? argv[bi + 1] : null;
  if (bi >= 0 && !base) { console.error('--base needs a git ref'); return 2; }

  let doc;
  try { doc = JSON.parse(readFileSync(file, 'utf8')); } catch (e) { console.error(`marketplace/plugins.json: ${e.message}`); return 1; }
  let failed = 0;
  const listErrs = validateList(doc);
  for (const { name, msg } of listErrs) { console.error(`FAIL ${name ?? 'list'}: ${msg}`); failed++; }
  console.log(`list: ${Array.isArray(doc.plugins) ? doc.plugins.length : 0} plugin(s), ${listErrs.length ? `${listErrs.length} problem(s)` : 'ok'}`);

  let baseDoc = null;
  if (base) {
    try { baseDoc = loadBase(base, 'marketplace/plugins.json'); } catch (e) { console.error(`FAIL ${e.message}`); return 1; }
  }
  const before = new Map((baseDoc?.plugins ?? []).map((p) => [p.name, canon(p)]));
  const bad = new Set(listErrs.map((e) => e.name));
  const todo = (Array.isArray(doc.plugins) ? doc.plugins : []).filter((e) => isObj(e) && isStr(e.name) && !bad.has(e.name) && (!base || before.get(e.name) !== canon(e)));
  if (base) console.log(`base ${base}: ${todo.length} new or changed entr${todo.length === 1 ? 'y' : 'ies'} to download`);

  for (const e of todo) {
    process.stdout.write(`${e.name} ${e.version}: downloading ${e.archive}\n`);
    try {
      const bytes = await download(e.archive);
      const { errors, files } = inspectArchive(bytes, e);
      if (errors.length) { for (const m of errors) console.error(`FAIL ${e.name}: ${m}`); failed += errors.length; }
      else console.log(`ok   ${e.name} ${e.version}: ${bytes.length} bytes, ${files.length} files, sha256 and plugin.json match`);
    } catch (err) {
      console.error(`FAIL ${e.name}: ${err.message}`);
      failed++;
    }
  }
  console.log(failed ? `\n${failed} problem(s)` : '\nmarketplace ok');
  return failed ? 1 : 0;
}

// ---- self-test --------------------------------------------------------------------------------

function tarEntry(name, data = '', opts = {}) {
  const body = Buffer.isBuffer(data) ? data : Buffer.from(data);
  const h = Buffer.alloc(512);
  const put = (s, o, l) => h.write(s, o, l, 'utf8');
  const type = opts.type ?? (opts.dir ? '5' : '0');
  put(name.slice(0, 100), 0, 100);
  put('0000644\0', 100, 8); put('0000000\0', 108, 8); put('0000000\0', 116, 8);
  put(`${(type === '0' ? body.length : 0).toString(8).padStart(11, '0')}\0`, 124, 12);
  put('00000000000\0', 136, 12);
  put('        ', 148, 8);
  put(type, 156, 1);
  if (opts.linkname) put(opts.linkname, 157, 100);
  put('ustar\0', 257, 6); put('00', 263, 2);
  if (opts.prefix) put(opts.prefix, 345, 155);
  let sum = 0;
  for (const b of h) sum += b;
  put(`${sum.toString(8).padStart(6, '0')}\0 `, 148, 8);
  const pad = Buffer.alloc((512 - (type === '0' ? body.length : 0) % 512) % 512);
  return Buffer.concat([h, type === '0' ? body : Buffer.alloc(0), pad]);
}
const makeTgz = (parts) => gzipSync(Buffer.concat([...parts, Buffer.alloc(1024)]));
const sha = (b) => createHash('sha256').update(b).digest('hex');

async function selfTest() {
  let passed = 0, failed = 0;
  const ok = (cond, what) => { if (cond) passed++; else { failed++; console.error(`SELF-TEST FAIL: ${what}`); } };

  const entry = (extra = {}, bytes = null) => ({
    name: 'demo', title: 'Demo', description: 'A demo plugin.', author: 'A. Author',
    repository: 'https://github.com/someone/demo', license: 'MIT', version: '1.2.3',
    archive: 'https://github.com/someone/demo/releases/download/v1.2.3/demo-1.2.3.tgz',
    sha256: bytes ? sha(bytes) : '0'.repeat(64), kernel: ['files:read'], contributes: ['views', 'commands'],
    categories: ['reading'], ...extra,
  });
  const manifest = (o = {}) => JSON.stringify({
    name: 'demo', version: '1.2.3', server: { command: 'node', args: ['mcp/dist/server.js'] },
    contributes: { views: [{ id: 'a', title: 'A', ui: 'ui://demo/app' }], commands: [{ name: 'x', tool: 'demo_x' }] },
    kernel: ['files:read'], ...o,
  });
  const goodParts = (m = manifest(), extra = []) => [
    tarEntry('./', '', { dir: true }), tarEntry('./plugin.json', m), tarEntry('./mcp/', '', { dir: true }),
    tarEntry('./mcp/dist/server.js', 'console.log(1)'), tarEntry('./README.md', '# demo'), ...extra,
  ];
  const run = (parts, e = {}) => { const b = makeTgz(parts); return inspectArchive(b, entry(e, b)); };
  const expectOk = (what, r) => ok(r.errors.length === 0, `${what}: expected ok, got ${r.errors}`);
  const expectBad = (what, r, re) => ok(r.errors.some((m) => re.test(m)), `${what}: expected /${re}/, got ${JSON.stringify(r.errors)}`);

  // archives
  expectOk('good archive', run(goodParts()));
  expectOk('no leading ./', run([tarEntry('plugin.json', manifest()), tarEntry('mcp/dist/server.js', 'x')]));
  { // a long name through ustar prefix resolves to prefix/name
    const t = readTar(Buffer.concat([tarEntry('server.js', 'x', { prefix: 'mcp/dist' }), Buffer.alloc(1024)]));
    ok(t[0]?.name === 'mcp/dist/server.js', 'ustar prefix joins');
  }
  { // a pax path header names the next entry
    const rec = ' path=mcp/dist/server.js\n';
    let len = rec.length + 1;
    while (`${len}${rec}`.length !== len) len = `${len}${rec}`.length;
    const t = readTar(Buffer.concat([paxHeader(`${len}${rec}`), tarEntry('short', 'x'), Buffer.alloc(1024)]));
    ok(t[0]?.name === 'mcp/dist/server.js', 'pax path is used');
  }
  expectBad('symlink', run(goodParts(manifest(), [tarEntry('link', '', { type: '2', linkname: '/etc/passwd' })])), /symlink/);
  expectBad('hardlink', run(goodParts(manifest(), [tarEntry('hl', '', { type: '1', linkname: 'plugin.json' })])), /hard link/);
  expectBad('device', run(goodParts(manifest(), [tarEntry('dev', '', { type: '3' })])), /device/);
  expectBad('absolute path', run(goodParts(manifest(), [tarEntry('/etc/evil', 'x')])), /absolute/);
  expectBad('dotdot', run(goodParts(manifest(), [tarEntry('../evil', 'x')])), /\.\./);
  expectBad('nested dotdot', run(goodParts(manifest(), [tarEntry('a/../../evil', 'x')])), /\.\./);
  expectBad('dotfile', run(goodParts(manifest(), [tarEntry('.env', 'x')])), /dot file/);
  expectBad('dot folder', run(goodParts(manifest(), [tarEntry('.git/config', 'x')])), /dot file/);
  expectBad('node_modules', run(goodParts(manifest(), [tarEntry('mcp/node_modules/x/index.js', 'x')])), /node_modules/);
  expectBad('duplicate path', run(goodParts(manifest(), [tarEntry('README.md', 'again')])), /twice/);
  expectBad('unpacked too big', run(goodParts(manifest(), [tarEntry('big.bin', Buffer.alloc(MAX_BYTES + 1024))])), /more than|at most/);
  expectBad('sha mismatch', inspectArchive(makeTgz(goodParts()), entry()), /sha256/);
  expectBad('not gzip', inspectArchive(Buffer.from('hello'), entry({ sha256: sha(Buffer.from('hello')) })), /gzip/);
  expectBad('no plugin.json', run([tarEntry('README.md', 'x')]), /plugin\.json is not at the archive root/);
  expectBad('plugin.json in a folder', run([tarEntry('demo/plugin.json', manifest()), tarEntry('demo/mcp/dist/server.js', 'x')]), /archive root/);
  expectBad('plugin.json not JSON', run(goodParts('{nope')), /not JSON/);
  expectBad('wrong name', run(goodParts(manifest({ name: 'other' }))), /name is "other"/);
  expectBad('wrong version', run(goodParts(manifest({ version: '9.9.9' }))), /version is "9.9.9"/);
  expectBad('more permissions than listed', run(goodParts(manifest({ kernel: ['files:read', 'network'] }))), /permissions/);
  expectBad('fewer permissions than listed', run(goodParts(manifest({ kernel: [] }))), /permissions/);
  expectBad('unknown permission', run(goodParts(manifest({ kernel: ['root'] }))), /unknown permission/);
  expectBad('extra slot', run(goodParts(manifest({ contributes: { views: [{ id: 'a', ui: 'ui://demo/a' }], commands: [{ name: 'x' }], games: [{ id: 'g', ui: 'ui://demo/g' }] } }))), /slots/);
  expectBad('missing slot', run(goodParts(manifest({ contributes: { views: [{ id: 'a', ui: 'ui://demo/a' }] } }))), /slots/);
  expectBad('unknown slot', run(goodParts(manifest({ contributes: { widgets: [{ id: 'a' }] } }))), /unknown slot/);
  expectBad('ui of another plugin', run(goodParts(manifest({ contributes: { views: [{ id: 'a', ui: 'ui://other/app' }], commands: [{ name: 'x' }] } }))), /ui:\/\/demo\//);
  expectBad('ui not ui://', run(goodParts(manifest({ contributes: { views: [{ id: 'a', ui: 'https://evil.example/app' }], commands: [{ name: 'x' }] } }))), /ui:\/\/demo\//);
  expectBad('server not node', run(goodParts(manifest({ server: { command: 'bash', args: ['mcp/dist/server.js'] } }))), /"node"/);
  expectBad('server file missing', run(goodParts(manifest({ server: { command: 'node', args: ['mcp/dist/nope.js'] } }))), /not in the archive/);
  expectBad('server file escapes', run(goodParts(manifest({ server: { command: 'node', args: ['../x.js'] } }))), /not in the archive/);
  expectBad('no server', run(goodParts(manifest({ server: undefined }))), /no "server"/);
  expectBad('truncated tar', { errors: (() => { try { readTar(Buffer.concat([tarEntry('a', 'x'.repeat(2000)).subarray(0, 800)])); return []; } catch (e) { return [e.message]; } })() }, /truncated/);

  // entries and the list
  const good = entry();
  ok(validateEntry(good).length === 0, `a good entry: ${validateEntry(good)}`);
  ok(validateEntry({ ...good, homepage: 'https://example.com', icon: 'https://example.com/i.png' }).length === 0, 'homepage and icon');
  const bad = (what, over, re) => ok(validateEntry({ ...good, ...over }).some((m) => re.test(m)), `entry ${what}`);
  bad('name uppercase', { name: 'Demo' }, /name/);
  bad('name short', { name: 'd' }, /name/);
  bad('description too long', { description: 'x'.repeat(201) }, /description/);
  bad('empty author', { author: ' ' }, /author/);
  bad('repository not github', { repository: 'https://gitlab.com/a/b' }, /repository/);
  bad('repository http', { repository: 'http://github.com/a/b' }, /repository/);
  bad('version not semver', { version: '1.2' }, /version/);
  bad('archive http', { archive: 'http://github.com/someone/demo/releases/download/v1.2.3/demo-1.2.3.tgz' }, /archive/);
  bad('archive other host', { archive: 'https://example.com/demo-1.2.3.tgz' }, /archive/);
  bad('archive other repo', { archive: 'https://github.com/evil/demo/releases/download/v1.2.3/demo-1.2.3.tgz' }, /release asset/);
  bad('archive wrong tag', { archive: 'https://github.com/someone/demo/releases/download/v1.2.4/demo-1.2.3.tgz' }, /tag/);
  bad('archive wrong file', { archive: 'https://github.com/someone/demo/releases/download/v1.2.3/other-1.2.3.tgz' }, /file must be/);
  bad('archive not tgz', { archive: 'https://github.com/someone/demo/releases/download/v1.2.3/demo-1.2.3.zip' }, /file must be/);
  bad('sha uppercase', { sha256: 'A'.repeat(64) }, /sha256/);
  bad('sha short', { sha256: 'abc' }, /sha256/);
  bad('permission unknown', { kernel: ['sudo'] }, /permission/);
  bad('write subfolder absolute', { kernel: ['files:write:/etc'] }, /permission/);
  bad('write subfolder dotdot', { kernel: ['files:write:a/../b'] }, /permission/);
  bad('slot unknown', { contributes: ['widgets'] }, /slot/);
  bad('no categories', { categories: [] }, /at least 1/);
  bad('duplicate permission', { kernel: ['network', 'network'] }, /duplicates/);
  bad('homepage http', { homepage: 'http://x.example' }, /homepage/);
  bad('unknown field', { star: 1 }, /unknown field/);
  const { title: _t, ...noTitle } = good;
  ok(validateEntry(noTitle).some((m) => /missing "title"/.test(m)), 'missing field');
  ok(validateEntry({ ...good, kernel: ['files:write:notes/inbox', 'files:write', 'ai', 'network'] }).length === 0, 'all permission forms');

  const list = (plugins, extra = {}) => validateList({ version: 1, plugins, ...extra });
  ok(list([]).length === 0, 'empty list');
  const named = (name) => entry({ name, archive: `https://github.com/someone/demo/releases/download/v1.2.3/${name}-1.2.3.tgz` });
  ok(list([named('aa'), named('bb')]).length === 0, 'sorted list');
  ok(list([named('bb'), named('aa')]).some((e) => /not sorted/.test(e.msg)), 'unsorted');
  ok(list([named('aa'), named('aa')]).some((e) => /duplicate/.test(e.msg)), 'duplicates');
  ok(list([], { version: 2 }).some((e) => /version/.test(e.msg)), 'list version');
  ok(validateList({ version: 1 }).some((e) => /array/.test(e.msg)), 'plugins missing');
  ok(validateList([]).length > 0, 'not an object');
  ok(list([], { extra: 1 }).length > 0, 'unknown top-level field');

  // the committed list is valid, and the schema file parses
  const here = dirname(fileURLToPath(import.meta.url));
  ok(validateList(JSON.parse(readFileSync(join(here, '../marketplace/plugins.json'), 'utf8'))).length === 0, 'committed plugins.json is valid');
  ok(JSON.parse(readFileSync(join(here, '../marketplace/schema.json'), 'utf8')).$defs.entry.required.every((k) => ENTRY_KEYS.includes(k)), 'schema keys known');

  // download cap, over a local server
  const body = Buffer.alloc(3 * 1024 * 1024, 7);
  const srv = createServer((req, res) => {
    if (req.url === '/chunked') { res.write(body); res.end(); } // no content-length
    else if (req.url === '/missing') { res.statusCode = 404; res.end(); }
    else { res.setHeader('content-length', body.length); res.end(body); }
  });
  await new Promise((r) => srv.listen(0, '127.0.0.1', r));
  const base = `http://127.0.0.1:${srv.address().port}`;
  const rejects = async (url, cap, re, what) => { try { await download(url, cap); ok(false, `${what}: should reject`); } catch (e) { ok(re.test(e.message), `${what}: ${e.message}`); } };
  await rejects(`${base}/sized`, 1 << 20, /bytes; at most/, 'cap by content-length');
  await rejects(`${base}/chunked`, 1 << 20, /over 1 MB/, 'cap while streaming');
  await rejects(`${base}/missing`, 1 << 20, /HTTP 404/, 'http error');
  ok((await download(`${base}/sized`, 4 << 20)).length === body.length, 'download under the cap');
  srv.closeAllConnections?.();
  srv.close();

  console.log(`self-test: ${passed} passed, ${failed} failed`);
  return failed ? 1 : 0;
}

function paxHeader(content) { // a pax 'x' entry holding `content`
  const body = Buffer.from(content);
  const h = tarEntry('PaxHeader', body, {});
  h[156] = 'x'.charCodeAt(0);
  // recompute the checksum with the type changed
  let sum = 0;
  for (let i = 0; i < 512; i++) sum += i >= 148 && i < 156 ? 0x20 : h[i];
  h.write(`${sum.toString(8).padStart(6, '0')}\0 `, 148, 8, 'utf8');
  return h;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const args = process.argv.slice(2);
  const code = args.includes('--self-test') ? await selfTest() : await main(args);
  process.exit(code);
}
