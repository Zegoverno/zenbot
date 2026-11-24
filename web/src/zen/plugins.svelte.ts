// The plugin host's state on the page (ADR 0001, Host API): the installed plugins and the opener
// defaults from the kernel, what each slot offers (views, openers, games, widgets, commands), the chat
// context plugin UIs keep per session, and the side panel a plugin command fills.
//
// Plugins reach the owner's files through the kernel, under the vault (`ZEN_PLUGIN_FILES`), so a
// plugin sees vault-relative paths and openers apply only to vault files.

import { pluginApi, upload, type OpenerDefaults, type Plugin, type PluginGame, type PluginWidget, type PluginOpener, type PluginView, type ToolResult } from './api.ts';
import { app, openTab, tilde } from './app.svelte.ts';

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

/** Home cards from plugins; `key` is the Home catalog entry (`plugin:<name>/<id>`). */
export type Widget = PluginWidget & { plugin: string; key: string };
export function widgets(): Widget[] {
  return enabled().flatMap((p) => (p.contributes.widgets ?? []).map((w) => ({ ...w, plugin: p.name, key: viewId(p.name, w.id) })));
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
//
// Each plugin's latest context shows as a chip above the chat box, so the owner sees what will go
// with the next prompt and can remove it. Images in an update (a screenshot of what the owner
// pointed at) are saved to ~/inbox as they arrive, as chat attachments are, and the block names
// them by path: zenbot reads them with its own tools. Plugin content is untrusted: it travels only
// inside the delimited block, and only when the owner sends.

export type ContextImage = { name: string; thumb: string; path?: string; error?: string };
export type PluginContext = { id: number; text: string; images: ContextImage[] };

const IMAGE_TYPES: Record<string, string> = { 'image/png': 'png', 'image/jpeg': 'jpg', 'image/webp': 'webp', 'image/gif': 'gif' };
export const MAX_CONTEXT_IMAGES = 8;
export const MAX_CONTEXT_IMAGE_BYTES = 5 * 1024 * 1024;
export const MAX_CONTEXT_TEXT = 20_000;

/** Session id ('new' for a chat not started) → plugin → its latest context, unsent. */
export const contexts = $state<Record<string, Record<string, PluginContext>>>({});
let contextIds = 0;

/** The images of an update: base64 PNG, JPEG, WebP or GIF, each under the size cap, at most 8. */
export function contextImages(content: unknown): { mimeType: string; data: string; bytes: Uint8Array }[] {
  if (!Array.isArray(content)) return [];
  const out: { mimeType: string; data: string; bytes: Uint8Array }[] = [];
  for (const c of content as { type?: unknown; mimeType?: unknown; data?: unknown }[]) {
    if (out.length >= MAX_CONTEXT_IMAGES) break;
    if (c?.type !== 'image' || typeof c.data !== 'string' || typeof c.mimeType !== 'string' || !IMAGE_TYPES[c.mimeType]) continue;
    if ((c.data.length * 3) / 4 > MAX_CONTEXT_IMAGE_BYTES) continue;
    try {
      out.push({ mimeType: c.mimeType, data: c.data, bytes: Uint8Array.from(atob(c.data), (ch) => ch.charCodeAt(0)) });
    } catch {
      // Not base64: skip it rather than send a broken file.
    }
  }
  return out;
}

/**
 * Keep a plugin's latest context for a session, replacing the one before (empty clears it).
 * Returns the kept context's id, or null when nothing was kept.
 */
export function keepContext(session: string | null, plugin: string, text: string, images: { mimeType: string; data: string; path?: string; error?: string }[] = []): number | null {
  if (!session) return null;
  text = text.slice(0, MAX_CONTEXT_TEXT);
  const m = (contexts[session] ??= {});
  if (!text && !images.length) {
    delete m[plugin];
    return null;
  }
  const id = ++contextIds;
  const stem = plugin.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'plugin';
  m[plugin] = {
    id,
    text,
    images: images.map((im, k) => ({ name: `${stem}-${id}-${k + 1}.${IMAGE_TYPES[im.mimeType]}`, thumb: `data:${im.mimeType};base64,${im.data}`, ...(im.path ? { path: im.path } : {}), ...(im.error ? { error: im.error } : {}) })),
  };
  return id;
}

/**
 * keepContext, then save its images to ~/inbox (those not saved before: an image that carries its
 * `path` is kept as it is). The chip shows each as saving until it is.
 */
export function keepContextSaving(session: string | null, plugin: string, text: string, images: { mimeType: string; data: string; path?: string }[] = []): number | null {
  const id = keepContext(session, plugin, text, images);
  if (!session || id === null) return id;
  const kept = contexts[session]?.[plugin]?.images ?? [];
  images.forEach((im, k) => {
    if (im.path || !kept[k]) return;
    const bytes = Uint8Array.from(atob(im.data), (ch) => ch.charCodeAt(0));
    upload(new File([bytes as BlobPart], kept[k].name, { type: im.mimeType }), () => {}).done.then(
      (r) => settleImage(session, plugin, id, k, { path: r.path }),
      (e) => settleImage(session, plugin, id, k, { error: (e as Error).message }),
    );
  });
  return id;
}

/** Record where an image was saved (or why it wasn't), if its context is still the current one. */
export function settleImage(session: string, plugin: string, id: number, k: number, patch: { path?: string; error?: string }) {
  const c = contexts[session]?.[plugin];
  if (c?.id === id && c.images[k]) Object.assign(c.images[k], patch);
}

export function dropContext(session: string, plugin: string) {
  if (contexts[session]) delete contexts[session][plugin];
}

/** Whether a session's context still has images being saved. */
export function contextSaving(session: string): boolean {
  return Object.values(contexts[session] ?? {}).some((c) => c.images.some((im) => !im.path && !im.error));
}

/** Untrusted text can't close or open the block it travels in: its tags lose their `<`. */
export const blockText = (t: string) => t.replace(/<(\/?\s*(?:app_context|plugin)\b)/gi, '‹$1');
const attrText = (t: string) => t.replace(/[<>"&]/g, '_');

/**
 * The context to send with a session's next prompt, as one delimited block, and forget it (it
 * goes once; a plugin that updates it again sends again). The kernel's prompt has no context
 * field yet, so the block travels at the head of the prompt text.
 */
export function takeContext(session: string): string {
  const m = contexts[session];
  if (!m || !Object.keys(m).length) return '';
  delete contexts[session];
  const parts = Object.entries(m).map(([plugin, c]) => {
    const images = c.images.map((im) => (im.path ? `(attached ${tilde(im.path)})` : `(image not saved: ${im.error ?? 'unknown error'})`));
    return `<plugin name="${attrText(plugin)}">\n${[blockText(c.text), ...images].filter(Boolean).join('\n')}\n</plugin>`;
  });
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
export const chat = {
  queue: null as ((plugin: string, text: string) => boolean) | null,
  /** Puts the cursor in the open chat's box (after the owner pointed at something to comment on). */
  focus: null as (() => void) | null,
  /** Runs (or, with `submit` false, only types) a slash command in the open chat: the command list's way to a plugin command. */
  run: null as ((text: string, submit: boolean) => void) | null,
};

// ---- the side panel a plugin command fills ----

export const panel = $state({
  current: null as null | { plugin: string; ui: string; title: string; input: Record<string, unknown>; result: ToolResult; n: number },
});
let shown = 0;
export function showPanel(plugin: string, ui: string, title: string, input: Record<string, unknown>, result: ToolResult) {
  panel.current = { plugin, ui, title, input, result, n: ++shown };
  openTab('panel');
}
