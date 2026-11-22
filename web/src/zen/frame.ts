// What a plugin UI runs under (ADR 0001, Host API): its HTML in an `<iframe sandbox="allow-scripts
// allow-forms">` (no `allow-same-origin`: an opaque origin, so no cookies, no storage of this site,
// no way to the page's API), with a Content-Security-Policy built from the resource's
// `_meta.ui.csp` (MCP Apps): no network unless it declares the domains, inline code only.

import type { Csp } from './api.ts';
import { hasIcon } from './icons.ts';
import { validKey } from './keys.ts';

/** The iframe's sandbox: scripts and forms, never same-origin, popups, or top navigation. */
export const SANDBOX = 'allow-scripts allow-forms';

/** A source a CSP may name: an http(s)/ws(s) origin, `*.` subdomains allowed; nothing else passes. */
const SOURCE = /^(https?|wss?):\/\/(\*\.)?[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*(:\d{1,5})?$/;
const sources = (list: unknown): string[] => (Array.isArray(list) ? list.filter((s): s is string => typeof s === 'string' && SOURCE.test(s)) : []);
const or = (list: string[], fallback = "'none'") => (list.length ? list.join(' ') : fallback);

/** The policy for a UI resource's declared domains (MCP Apps' defaults: no network, no frames). */
export function cspFor(csp: Csp | undefined): string {
  const res = sources(csp?.resourceDomains);
  const r = res.length ? ` ${res.join(' ')}` : '';
  return [
    "default-src 'none'",
    `script-src 'unsafe-inline'${r}`,
    `style-src 'unsafe-inline'${r}`,
    `img-src data: blob:${r}`,
    `font-src data:${r}`,
    `media-src data: blob:${r}`,
    `connect-src ${or(sources(csp?.connectDomains))}`,
    `frame-src ${or(sources(csp?.frameDomains))}`,
    `base-uri ${or(sources(csp?.baseUriDomains))}`,
    "form-action 'none'",
  ].join('; ');
}

const attr = (s: string) => s.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;');

/** Start the document with the host policy, before *any* untrusted bytes. Looking for a head
 * with a regex is unsafe: a plugin can put scripts before it, or a fake head in a comment. The
 * parser creates the head for this leading meta; later html/head tags cannot undo the policy. */
export function withCsp(html: string, csp: Csp | undefined): string {
  return `<!doctype html><meta http-equiv="Content-Security-Policy" content="${attr(cspFor(csp))}">` + html;
}

/** Strict pane navigation: no credentials, fragments, duplicate or unknown parameters. */
export function zenNavigation(url: string): { action: 'close' } | { action: 'open'; path: string; mode: 'read' | 'edit' } | { action: 'task'; id: string; done: boolean } | null {
  let u: URL;
  try { u = new URL(url); } catch { return null; }
  if (u.protocol !== 'zen:' || u.username || u.password || u.port || u.hash || (u.pathname !== '' && u.pathname !== '/')) return null;
  if (u.host === 'close') return u.search ? null : { action: 'close' };
  if (u.host === 'task') {
    if ([...u.searchParams.keys()].some((k) => !['id', 'done'].includes(k)) ||
        u.searchParams.getAll('id').length !== 1 || u.searchParams.getAll('done').length !== 1) return null;
    const id = u.searchParams.get('id');
    const done = u.searchParams.get('done');
    if (!id || id.length > 1024 || /[\x00-\x1f\x7f]/.test(id) || (done !== '0' && done !== '1')) return null;
    return { action: 'task', id, done: done === '1' };
  }
  if (u.host !== 'open' || [...u.searchParams.keys()].some((k) => !['path', 'mode'].includes(k)) ||
      u.searchParams.getAll('path').length !== 1 || u.searchParams.getAll('mode').length > 1) return null;
  const p = u.searchParams.get('path');
  const mode = u.searchParams.get('mode') ?? 'read';
  if (mode !== 'read' && mode !== 'edit') return null;
  if (!p || /[\\\x00-\x1f\x7f]/.test(p) || p.startsWith('/') || p.split('/').some((s) => s === '..' || s === '' || s === '.')) return null;
  return { action: 'open', path: p, mode };
}

/** Compatibility helper for callers needing only the validated path. */
export function zenOpen(url: string): string | null {
  const nav = zenNavigation(url);
  return nav?.action === 'open' ? nav.path : null;
}

/** Whether a link may open in a new tab: http(s) only. */
export const webLink = (url: string) => /^https?:\/\//i.test(url);

// ---- keys and commands (PLUGINS.md, "Keys and commands") ----
// What a plugin UI sends over `zen/commands` and `zen/key` is untrusted: checked strictly here, and
// the page only ever renders it as text. A command that fails any check is dropped.

export type PluginCommand = { id: string; title: string; key?: string; ring: boolean; icon?: string };
export const MAX_COMMANDS = 30;
const COMMAND_ID = /^[a-z0-9-]{1,32}$/;
const plain = (v: unknown): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v);
const printable = (s: string) => !/[\x00-\x1f\x7f-\x9f\u2028\u2029]/.test(s);
/** Bidi controls (they reorder what's drawn around them): dropped from plugin titles. */
const BIDI = /[\u200e\u200f\u202a-\u202e\u2066-\u2069]/g;

/** The valid commands of a `zen/commands` message, in order, at most 30; an unknown icon falls back to the plugin's. */
export function validCommands(params: unknown): PluginCommand[] {
  const list = plain(params) && Array.isArray(params.commands) ? params.commands.slice(0, MAX_COMMANDS) : [];
  const seen = new Set<string>();
  const out: PluginCommand[] = [];
  for (const c of list) {
    if (!plain(c) || typeof c.id !== 'string' || !COMMAND_ID.test(c.id) || seen.has(c.id)) continue;
    const title = typeof c.title === 'string' ? c.title.replace(BIDI, '').trim() : '';
    if (!title || !printable(title) || [...title].length > 60) continue;
    if (c.key !== undefined && !validKey(c.key)) continue;
    if (c.ring !== undefined && typeof c.ring !== 'boolean') continue;
    if (c.icon !== undefined && typeof c.icon !== 'string') continue;
    seen.add(c.id);
    out.push({ id: c.id, title, ...(c.key !== undefined ? { key: c.key } : {}), ring: c.ring === true, ...(hasIcon(c.icon) ? { icon: c.icon } : {}) });
  }
  return out;
}

export type ForwardedKey = { key: string; code: string; ctrlKey: boolean; metaKey: boolean; altKey: boolean; shiftKey: boolean; typing: boolean };

/** A `zen/key` message's fields, or null when any is missing or the wrong type. */
export function validKeyEvent(params: unknown): ForwardedKey | null {
  if (!plain(params)) return null;
  const { key, code, ctrl, meta, alt, shift, typing } = params;
  if (typeof key !== 'string' || !key || key.length > 20 || !printable(key)) return null;
  if (code !== undefined && (typeof code !== 'string' || code.length > 24 || !printable(code))) return null;
  for (const f of [ctrl, meta, alt, shift, typing]) if (f !== undefined && typeof f !== 'boolean') return null;
  return { key, code: (code as string | undefined) ?? '', ctrlKey: ctrl === true, metaKey: meta === true, altKey: alt === true, shiftKey: shift === true, typing: typing === true };
}
