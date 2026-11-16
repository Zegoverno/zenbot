// The plugin host's state on the page (ADR 0001, Host API): the installed plugins and the opener
// defaults from the kernel, what each slot offers (views, openers, games, commands), the chat
// context plugin UIs keep per session, and the side panel a plugin command fills.
//
// Plugins reach the owner's files through the kernel, under the vault (`ZEN_PLUGIN_FILES`), so a
// plugin sees vault-relative paths and openers apply only to vault files.

import { pluginApi, type OpenerDefaults, type Plugin, type PluginGame, type PluginOpener, type PluginView, type ToolResult } from './api.ts';
import { app } from './app.svelte.ts';

export const plugins = $state({
  list: [] as Plugin[],
  defaults: {} as OpenerDefaults,
  loaded: false,
  revision: 0,
  error: '',
});

export async function loadPlugins() {
  try {
    const r = await pluginApi.list();
    plugins.list = r.plugins ?? [];
    plugins.defaults = r.defaults ?? {};
    plugins.error = '';
    plugins.revision++;
  } catch (e) {
    plugins.error = (e as Error).message;
  } finally {
    plugins.loaded = true;
  }
}

const enabled = () => plugins.list.filter((p) => p.enabled);

/** A plugin view's tab id. */
export const viewId = (plugin: string, id: string) => `plugin:${plugin}/${id}`;
export function views(): (PluginView & { plugin: string; key: string })[] {
  return enabled().flatMap((p) => (p.contributes.views ?? []).map((v) => ({ ...v, plugin: p.name, key: viewId(p.name, v.id) })));
}
export function findView(key: string) {
  return views().find((v) => v.key === key) ?? null;
}

export type Game = PluginGame & { plugin: string; key: string };
export function games(): Game[] {
  return enabled().flatMap((p) => (p.contributes.games ?? []).map((g) => ({ ...g, plugin: p.name, key: `${p.name}/${g.id}` })));
}

// ---- openers ----

/** A glob on a file name (`*`, `?`), case-insensitive. */
export function glob(pattern: string, name: string): boolean {
  const re = pattern.replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '[^/]*').replace(/\?/g, '[^/]');
  return new RegExp(`^${re}$`, 'i').test(name);
}

export type Mode = 'read' | 'edit';
export type Opener = PluginOpener & { plugin: string; key: string; pattern: string };

/** Every enabled plugin opener for a file name (each with the first of its patterns it matches). */
export function openersFor(name: string): Opener[] {
  return enabled().flatMap((p) =>
    (p.contributes.openers ?? []).flatMap((o) => {
      const pattern = o.patterns.find((g) => glob(g, name));
      return pattern ? [{ ...o, plugin: p.name, key: `${p.name}/${o.id}`, pattern }] : [];
    }),
  );
}

/** The opener a file gets in a mode: the owner's default for one of its patterns, or the built-in. */
export function defaultOpener(name: string, mode: Mode): Opener | null {
  const all = openersFor(name).filter((o) => o.mode === mode);
  for (const o of all) {
    // Settings saves defaults per declared pattern, not just the first matching one.
    for (const pattern of o.patterns.filter((p) => glob(p, name))) {
      const chosen = plugins.defaults[`${pattern}:${mode}`];
      if (!chosen || chosen === 'builtin') continue;
      const hit = all.find((x) => x.key === chosen && x.patterns.includes(pattern));
      if (hit) return hit;
    }
  }
  return null;
}

/** Makes `value` (`<plugin>/<id>` or `builtin`) the default for a file's patterns in a mode. */
export async function setDefault(name: string, mode: Mode, value: string) {
  const keys = new Set(openersFor(name).filter((o) => o.mode === mode && (value === 'builtin' || o.key === value)).map((o) => `${o.pattern}:${mode}`));
  for (const key of keys) {
    await pluginApi.setDefault(key, value);
    plugins.defaults[key] = value;
  }
}

/** A pane's absolute path as the vault-relative path plugins see, or null outside the vault. */
export function inVault(path: string | null): string | null {
  if (!path || !app.vault || !path.startsWith(app.vault + '/')) return null;
  return path.slice(app.vault.length + 1);
}

// ---- permissions, in plain words ----

export function permissionWords(p: string): string {
  if (p === 'files:read') return 'Read your files (the vault)';
  if (p === 'files:write') return 'Read and change your files (the vault)';
  if (p.startsWith('files:write:')) return `Read your files, and change those in ${p.slice('files:write:'.length)}/`;
  if (p === 'network') return 'Reach the internet';
  if (p === 'ai') return "Ask zenbot's model for completions";
  return p;
}

// ---- what plugin UIs tell the chat (updateModelContext), kept per session ----

/** Session id ('new' for a chat not started) → plugin → the latest context, unsent. */
const contexts = new Map<string, Map<string, string>>();

export function keepContext(session: string | null, plugin: string, text: string) {
  if (!session) return;
  let m = contexts.get(session);
  if (!m) contexts.set(session, (m = new Map()));
  if (text) m.set(plugin, text);
  else m.delete(plugin);
}

/**
 * The context to send with a session's next prompt, as one delimited block, and forget it (it
 * goes once; a plugin that updates it again sends again). The kernel's prompt has no context
 * field yet, so the block travels at the head of the prompt text.
 */
export function takeContext(session: string): string {
  const m = contexts.get(session);
  if (!m?.size) return '';
  contexts.delete(session);
  const parts = [...m].map(([plugin, text]) => `<plugin name="${plugin}">\n${text}\n</plugin>`);
  return `<app_context>\n${parts.join('\n')}\n</app_context>\n\n`;
}

/** The text of a tool result or model-context update (its text blocks, else its structured content). */
export function resultText(r: { content?: { type: string; text?: string }[]; structuredContent?: unknown }): string {
  const text = (r.content ?? []).filter((c) => c.type === 'text' && c.text).map((c) => c.text).join('\n');
  return text || (r.structuredContent ? JSON.stringify(r.structuredContent) : '');
}

// ---- plugin slash commands ----

/**
 * A command's tool arguments from what follows it: the names in its `args` (`<path>`, `[n]`) take
 * a word each, the last the rest of the line; no names, no arguments.
 */
export function toolArgs(spec: string | undefined, arg: string): Record<string, unknown> {
  const names = [...(spec ?? '').matchAll(/[<[]([A-Za-z0-9_-]+)(?:\.\.\.)?[>\]]/g)].map((m) => m[1]);
  const words = arg.trim() ? arg.trim().split(/\s+/) : [];
  const out: Record<string, unknown> = {};
  names.forEach((n, i) => {
    const v = i === names.length - 1 ? words.slice(i).join(' ') : words[i];
    if (v) out[n] = v;
  });
  return out;
}

/** The UI a tool result names (`_meta.ui.resourceUri`, MCP Apps), if any. */
export function uiOf(r: ToolResult): string | null {
  const m = r._meta as { ui?: { resourceUri?: unknown }; 'ui/resourceUri'?: unknown } | undefined;
  const u = m?.ui?.resourceUri ?? m?.['ui/resourceUri'];
  return typeof u === 'string' && u.startsWith('ui://') ? u : null;
}

// ---- untrusted plugin drafts for the open chat ----

/** Queue only. This callback must never submit a prompt or execute a command. */
export const chat = { queue: null as ((plugin: string, text: string) => boolean) | null };

// ---- the side panel a plugin command fills ----

export const panel = $state({
  current: null as null | { plugin: string; ui: string; title: string; input: Record<string, unknown>; result: ToolResult; n: number },
});
let shown = 0;
export function showPanel(plugin: string, ui: string, title: string, input: Record<string, unknown>, result: ToolResult) {
  panel.current = { plugin, ui, title, input, result, n: ++shown };
  app.side.open = true;
  app.side.view = 'panel';
}
