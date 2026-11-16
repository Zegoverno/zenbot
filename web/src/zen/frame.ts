// What a plugin UI runs under (ADR 0001, Host API): its HTML in an `<iframe sandbox="allow-scripts
// allow-forms">` (no `allow-same-origin`: an opaque origin, so no cookies, no storage of this site,
// no way to the page's API), with a Content-Security-Policy built from the resource's
// `_meta.ui.csp` (MCP Apps): no network unless it declares the domains, inline code only.

import type { Csp } from './api.ts';

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

/** A `zen://open?path=<vault-relative path>` link's path, or null (no `..`, no absolute paths). */
export function zenOpen(url: string): string | null {
  let u: URL;
  try {
    u = new URL(url);
  } catch {
    return null;
  }
  if (u.protocol !== 'zen:' || u.host !== 'open' || (u.pathname !== '' && u.pathname !== '/')) return null;
  const p = u.searchParams.get('path');
  if (!p || /[\\\x00-\x1f\x7f]/.test(p) || p.startsWith('/') || p.split('/').some((s) => s === '..' || s === '' || s === '.')) return null;
  return p;
}

/** Whether a link may open in a new tab: http(s) only. */
export const webLink = (url: string) => /^https?:\/\//i.test(url);
