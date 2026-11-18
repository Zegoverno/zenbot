// The Preview tab's rules (views/Preview.svelte): which addresses it may show, and what a pin from
// the app inside may say.
//
// It shows a dev server on this page's own host, on a port the proxy forwards (3000–9999), e.g.
// `https://zenbot.exe.xyz:5173/` beside `https://zenbot.exe.xyz/`: the same login, nothing served
// through zenbot. The frame is drawn here, not in a plugin's sandbox, because a frame inside an
// opaque-origin sandbox inherits it and a dev server's own scripts then fail CORS (and the proxy's
// login cookie isn't sent). Another port is another origin, so the app can't reach this page; its
// requests to this server's API carry no `X-Zen-Web` header and can't write (main.ts).
//
// The app reports what the owner points at by `postMessage` (public/zen-preview.js). Its messages are
// untrusted: only the framed window at the shown origin is heard, every field is checked and cut to
// size, and a pin reaches the agent only inside the delimited context block, when the owner sends.

export const MIN_PORT = 3000;
export const MAX_PORT = 9999;

/**
 * The address to show for what the owner typed (`5173`, `:5173/path`, or a full URL), or null when
 * it isn't on this page's host and protocol, on a port 3000–9999 other than the page's own.
 */
export function previewTarget(input: string, page: { protocol: string; hostname: string; port: string }): URL | null {
  const raw = input.trim();
  if (!raw) return null;
  let u: URL;
  try {
    u = /^:?\d{2,5}(\/|$)/.test(raw) ? new URL(`${page.protocol}//${page.hostname}:${raw.replace(/^:/, '')}`) : new URL(raw);
  } catch {
    return null;
  }
  const port = Number(u.port);
  if (u.protocol !== page.protocol || u.hostname !== page.hostname || u.username || u.password) return null;
  if (!u.port || port < MIN_PORT || port > MAX_PORT || u.port === page.port) return null;
  return u;
}

export type Source = { file: string; line?: number; column?: number };
export type Pin = {
  tag: string;
  selector: string;
  text: string;
  url: string;
  rect: { x: number; y: number; w: number; h: number };
  viewport: { w: number; h: number };
  source: Source | null;
  styles: Record<string, string>;
  errors: string[];
  image: string | null;
  /** Where its screenshot was saved, or why it wasn't (set by the Preview tab). */
  saved?: { path?: string; error?: string };
};

const str = (v: unknown, max: number): string => (typeof v === 'string' ? v.replace(/[\u0000-\u0008\u000b-\u001f\u007f]/g, '').slice(0, max) : '');
const num = (v: unknown): number => (typeof v === 'number' && Number.isFinite(v) ? Math.round(v) : 0);
/** A PNG data URL: its bytes must start with PNG's signature (base64 `iVBORw0KGgo`). */
const PNG = /^data:image\/png;base64,(iVBORw0KGgo[A-Za-z0-9+/]*={0,2})$/;
export const MAX_PIN_IMAGE = 4 * 1024 * 1024;

/** A pin from the app, checked and cut to size, or null when the message isn't one. */
export function readPin(data: unknown): Pin | null {
  const d = data as Record<string, unknown> | null;
  if (!d || typeof d !== 'object' || d.zen !== 'preview' || d.type !== 'pin') return null;
  const p = d.pin as Record<string, unknown> | null;
  if (!p || typeof p !== 'object') return null;
  const tag = str(p.tag, 40).toLowerCase();
  if (!/^[a-z][a-z0-9-]*$/.test(tag)) return null;
  const rect = (p.rect ?? {}) as Record<string, unknown>;
  const vp = (p.viewport ?? {}) as Record<string, unknown>;
  const s = p.source as Record<string, unknown> | null;
  const file = s && typeof s === 'object' ? str(s.file, 300) : '';
  const styles: Record<string, string> = {};
  if (p.styles && typeof p.styles === 'object') {
    for (const [k, v] of Object.entries(p.styles as Record<string, unknown>).slice(0, 20)) {
      if (/^[a-z-]{1,40}$/.test(k)) styles[k] = str(v, 120);
    }
  }
  const image = typeof p.image === 'string' && p.image.length <= (MAX_PIN_IMAGE * 4) / 3 + 32 && PNG.test(p.image) ? p.image : null;
  return {
    tag,
    selector: str(p.selector, 400),
    text: str(p.text, 200).replace(/\s+/g, ' ').trim(),
    url: str(p.url, 400),
    rect: { x: num(rect.x), y: num(rect.y), w: num(rect.w), h: num(rect.h) },
    viewport: { w: num(vp.w), h: num(vp.h) },
    source: file ? { file, line: num(s!.line) || undefined, column: num(s!.column) || undefined } : null,
    styles,
    errors: Array.isArray(p.errors) ? p.errors.slice(-5).map((e) => str(e, 300)).filter(Boolean) : [],
    image,
  };
}

/** Where a pin lives in the source, as `file:line:column`. */
export const where = (s: Source | null) => (s ? [s.file, s.line, s.column].filter((x) => x !== undefined).join(':') : '');

/** The context block's text for the pins, numbered as the owner refers to them (#1, #2…). */
export function pinsText(origin: string, pins: Pin[]): string {
  const lines = [`Preview of ${origin}: the owner pointed at ${pins.length === 1 ? 'one element' : `${pins.length} elements`}.`];
  let shot = 0;
  pins.forEach((p, i) => {
    const name = `${p.tag}${p.text ? ` "${p.text.slice(0, 60)}"` : ''}`;
    lines.push(`#${i + 1} ${name}${p.source ? ` at ${where(p.source)}` : ' (source unknown)'}`);
    lines.push(`  page ${p.url || '/'}, viewport ${p.viewport.w}×${p.viewport.h}, box ${p.rect.w}×${p.rect.h} at ${p.rect.x},${p.rect.y}`);
    if (p.selector) lines.push(`  selector ${p.selector}`);
    const st = Object.entries(p.styles).map(([k, v]) => `${k}: ${v}`).join('; ');
    if (st) lines.push(`  styles ${st}`);
    if (p.errors.length) lines.push(`  console errors: ${p.errors.join(' | ')}`);
    lines.push(p.image ? `  screenshot: attached image ${++shot} below` : '  no screenshot');
  });
  return lines.join('\n');
}
