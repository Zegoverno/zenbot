// The plugin host's test fixture (scripts/e2e.sh `plugins`): a dependency-free stdio MCP server.
// hello_greet is the plugin's tool and command; hello_peek and hello_dial report what the sandbox
// lets it reach (a file, a TCP port); hello_ask asks the host for a completion (sampling);
// hello_hidden is for the model only (_meta.ui.visibility), so a UI may not call it.
'use strict';
const fs = require('node:fs');
const net = require('node:net');
const path = require('node:path');
const readline = require('node:readline');

const UI = 'ui://hello/app';
const tools = [
  { name: 'hello_greet', description: 'Greet someone.', inputSchema: { type: 'object', properties: { name: { type: 'string' } }, required: ['name'] }, _meta: { ui: { resourceUri: UI } } },
  { name: 'hello_peek', description: 'Read a file, or write one with text.', inputSchema: { type: 'object', properties: { path: { type: 'string' }, text: { type: 'string' } }, required: ['path'] } },
  { name: 'hello_dial', description: 'Open a TCP connection.', inputSchema: { type: 'object', properties: { host: { type: 'string' }, port: { type: 'number' } }, required: ['host', 'port'] } },
  { name: 'hello_ask', description: 'Ask the host model.', inputSchema: { type: 'object', properties: { prompt: { type: 'string' }, maxTokens: { type: 'number' } }, required: ['prompt'] } },
  { name: 'hello_hidden', description: 'Only for the model.', inputSchema: { type: 'object' }, _meta: { ui: { visibility: ['model'] } } },
];

const send = (m) => process.stdout.write(JSON.stringify(m) + '\n');
const text = (t, isError = false) => ({ content: [{ type: 'text', text: t }], isError });
const waiting = new Map();
let nextId = 1;

function ask(method, params) {
  const id = `fixture-${nextId++}`;
  send({ jsonrpc: '2.0', id, method, params });
  return new Promise((resolve) => waiting.set(id, resolve));
}

function dial(host, port) {
  return new Promise((resolve) => {
    const s = net.connect({ host, port, timeout: 3000 });
    s.on('connect', () => { s.destroy(); resolve('connected'); });
    s.on('timeout', () => { s.destroy(); resolve('timeout'); });
    s.on('error', (e) => resolve(`error ${e.code}`));
  });
}

async function call(name, a) {
  switch (name) {
    case 'hello_greet': return text(`Hello, ${a.name}!`);
    case 'hello_peek':
      try {
        if (typeof a.text === 'string') { fs.writeFileSync(a.path, a.text); return text('written'); }
        return text(fs.readFileSync(a.path, 'utf8'));
      } catch (e) { return text(`error ${e.code}`, true); }
    case 'hello_dial': return text(await dial(a.host, a.port));
    case 'hello_ask': {
      const r = await ask('sampling/createMessage', { messages: [{ role: 'user', content: { type: 'text', text: a.prompt } }], maxTokens: a.maxTokens ?? 50 });
      return r.error ? text(`AI isn't available here: ${r.error.message}`, true) : text(r.result.content.text);
    }
    case 'hello_hidden': return text('hidden');
    default: return null;
  }
}

async function handle(m) {
  if (m.id !== undefined && !m.method) {
    const w = waiting.get(m.id);
    if (w) { waiting.delete(m.id); w(m); }
    return;
  }
  if (m.id === undefined) return;
  const reply = (result) => send({ jsonrpc: '2.0', id: m.id, result });
  const fail = (code, message) => send({ jsonrpc: '2.0', id: m.id, error: { code, message } });
  switch (m.method) {
    case 'initialize': {
      const env = { files: process.env.ZEN_PLUGIN_FILES ?? null, data: process.env.ZEN_PLUGIN_DATA ?? null, sampling: !!m.params?.capabilities?.sampling };
      fs.writeFileSync(path.join(process.env.ZEN_PLUGIN_DATA ?? '/tmp', 'started.json'), JSON.stringify(env));
      return reply({ protocolVersion: m.params?.protocolVersion ?? '2025-06-18', capabilities: { tools: {}, resources: {} }, serverInfo: { name: 'hello', version: '0.1.0' } });
    }
    case 'ping': return reply({});
    case 'tools/list': return reply({ tools });
    case 'tools/call': {
      const r = await call(m.params?.name, m.params?.arguments ?? {});
      return r ? reply(r) : fail(-32602, `unknown tool ${m.params?.name}`);
    }
    case 'resources/list': return reply({ resources: [{ uri: UI, name: 'Hello', mimeType: 'text/html;profile=mcp-app' }] });
    case 'resources/read':
      if (m.params?.uri !== UI) return fail(-32002, 'no such resource');
      return reply({ contents: [{ uri: UI, mimeType: 'text/html;profile=mcp-app', text: fs.readFileSync(path.join(__dirname, 'ui/app.html'), 'utf8'), _meta: { ui: { csp: { connectDomains: [] } } } }] });
    default: return fail(-32601, `unknown method ${m.method}`);
  }
}

readline.createInterface({ input: process.stdin }).on('line', (line) => {
  let m;
  try { m = JSON.parse(line); } catch { return; }
  handle(m).catch((e) => send({ jsonrpc: '2.0', id: m.id, error: { code: -32603, message: String(e) } }));
});
