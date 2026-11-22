// Keyboard-only control for the zen app: one registry of commands, one key handler, one focus model.
//
// Commands. Each view registers its own for its panel (`commands(kind, () => [...])`, cleaned up
// when the view is destroyed); the shell registers the global ones; a plugin UI's `zen/commands`
// are registered by its PluginFrame under the panel it sits in. A command is {id, title, icon, key?,
// ring?, run, enabled?}. The ring (Ctrl+.), the command list (Ctrl+K), the `?` overlay and the key
// handler all read this registry, so a command is declared once and shows up everywhere with its
// shortcut. Keys are in the key form of keys.ts (PLUGINS.md, "Keys and commands"); the owner's
// rebinding is kept in localStorage `zen.keys` (`{ <command id>: key | '' }`, '' = unbound).
//
// Focus. There is always one focused panel: `ui.panes` lists what is on screen (left, and right
// when split) and `ui.focus` picks one. Only the focused panel's commands (plus the global ones)
// are active. Clicking a panel, Ctrl+\ (switch) or opening a view moves the focus; Ctrl+Shift+\
// closes the side panel or reopens the last one.
//
// The handler. One capture-phase listener on the window (`install`) so it runs before anything in
// the page; a plugin UI's forwarded `zen/key` goes through the same `dispatch`, as if pressed in
// its pane. Mod chords always fire; every other key only when not typing (`typing` in
// app.svelte.ts). Enter is left to a focused button. While the ring is open it takes the keys
// (a letter or 1–6 runs an action, arrows and Enter too); `g` then a letter goes to a view; `?`
// lists the shortcuts. Lists are walked with a cursor (`data-cur`, `moveCursor`) rather than DOM
// focus, so Enter and Space stay commands.

import { untrack } from 'svelte';
import { app, split as isSplit, pickTab, openTab, notice, type View } from './app.svelte.ts';
import { chordOf, validKey, RESERVED, FIXED, type KeyLike } from './keys.ts';

export type PanelKind =
  | 'home' | 'sessions' | 'chat' | 'subagent' | 'files-tree' | 'files-reader' | 'files-writer' | 'tasks'
  | 'routines' | 'agents' | 'games' | 'preview' | 'settings' | 'plugin';
export type Scope = 'global' | PanelKind;

export type Cmd = {
  /** Stable: the rebinding is kept under it (a plugin's are `plugin:<name>/<id>`). */
  id: string;
  title: string;
  icon: string;
  /** The default shortcut, in the key form. */
  key?: string;
  /** More keys that do the same and can't be rebound (arrows beside j/k). */
  alt?: string[];
  /** Shown instead of a key (the go-to's `g f`); never matched. */
  hint?: string;
  /** A place in the ring (lower first); a command without one is only in the list and `?`. */
  ring?: number;
  /** Left out of the command list (j/k and friends); still in `?`. */
  quiet?: boolean;
  /** Its key is zenbot's own and can't be rebound. */
  reserved?: boolean;
  /** The plugin that declared it (runtime `zen/commands`). */
  plugin?: string;
  run: () => void;
  enabled?: () => boolean;
};
export type Registered = Cmd & { scope: Scope };

export const ui = $state({
  /** What is on screen: [left] or [left, right]. */
  panes: ['home'] as PanelKind[],
  focus: 0,
  ring: false,
  ringAt: 0,
  palette: false,
  help: false,
  /** `g` was pressed: the next key goes to a view. */
  goto: false,
  agentMenu: false,
  /** The side panel's view when it was last open (to reopen it). */
  side: null as View | null,
});

// ---- the registry ----

type Reg = { n: number; scope: Scope; list: Cmd[] };
// Plain storage plus a revision to depend on: what views register from their effects must not
// depend on, or be reordered by, what other effects wrote a moment before.
const regs = new Map<number, Reg>();
const rev = $state({ n: 0 });
let seq = 0;

/** Registers (or, with the same `n`, replaces) a panel's commands; returns its handle. */
export function register(scope: Scope, list: Cmd[], n = ++seq): number {
  regs.set(n, { n, scope, list });
  untrack(() => rev.n++);
  return n;
}
export function unregister(n: number) {
  if (regs.delete(n)) untrack(() => rev.n++);
}

/** Runs a registered command by id (a view's ring borrowing a global action); true if there was one. */
export function runCommand(id: string): boolean {
  const c = tagged().find((x) => x.id === id);
  if (!c || !usable(c)) return false;
  runSafe(c.run);
  return true;
}

/** For a view's script: its panel's commands, rebuilt when what the builder reads changes, dropped on destroy. */
export function commands(scope: Scope, build: () => Cmd[]) {
  const n = ++seq;
  $effect(() => {
    register(scope, build(), n);
    return () => unregister(n);
  });
}

const tagged = (): Registered[] => (void rev.n, [...regs.values()].flatMap((r) => r.list.map((c) => ({ ...c, scope: r.scope }))));
const usable = (c: Cmd) => !c.enabled || c.enabled();
export const all = (): Registered[] => tagged();
export const globals = (): Registered[] => tagged().filter((c) => c.scope === 'global');
export const forPane = (kind: PanelKind): Registered[] => tagged().filter((c) => c.scope === kind);

/** Which of its three panels Files shows (it says; the shell reads it to name the panel). */
export const filesPanel = $state({ kind: 'files-tree' as 'files-tree' | 'files-reader' | 'files-writer' });

/** The focused panel's kind. */
export const focused = (): PanelKind => ui.panes[Math.min(ui.focus, ui.panes.length - 1)] ?? 'home';
/** The commands that can run now: the focused panel's, then the global ones. */
export const active = (): Registered[] => [...forPane(focused()), ...globals()].filter(usable);

// ---- keys: defaults, the owner's overrides, clashes ----

const KEYS = 'zen.keys';
function loadOverrides(): Record<string, string> {
  try {
    const v = JSON.parse(localStorage.getItem(KEYS) ?? '{}');
    if (!v || typeof v !== 'object' || Array.isArray(v)) return {};
    return Object.fromEntries(Object.entries(v).filter(([, k]) => k === '' || validKey(k)).map(([id, k]) => [id, k as string]));
  } catch { return {}; }
}
const bound = $state<{ map: Record<string, string> }>({ map: loadOverrides() });
const keep = () => { try { localStorage.setItem(KEYS, JSON.stringify($state.snapshot(bound.map))); } catch {} };

const keysOf = (c: Cmd) => [c.key, ...(c.alt ?? [])].filter((k): k is string => !!k);
const sameTime = (a: Scope, b: Scope) => a === b || a === 'global' || b === 'global';

/** A plugin command's default key that a core key already holds (or an earlier plugin command holds). */
export function clashed(c: Registered): boolean {
  if (!c.plugin || !c.key || bound.map[c.id] !== undefined) return false;
  const key = c.key;
  if (FIXED.includes(key)) return true;
  return tagged().some((o) => {
    if (o.id === c.id || !sameTime(o.scope, c.scope)) return false;
    if (!o.plugin) return keyOf(o) === key || (o.alt ?? []).includes(key);
    return o.id < c.id && (bound.map[o.id] ?? o.key) === key;
  });
}

/** The key a command answers to now: the owner's choice, else its default (none while clashing). */
export function keyOf(c: Registered | Cmd): string | undefined {
  const o = bound.map[c.id];
  if (o !== undefined) return o || undefined;
  if (c.plugin && 'scope' in c && clashed(c as Registered)) return undefined;
  return c.key;
}

/** The command (active at the same time as `c`) already holding `chord`, if any. */
export function findClash(c: Registered, chord: string): Registered | null {
  for (const o of tagged()) {
    if (o.id === c.id || !sameTime(o.scope, c.scope)) continue;
    if (keyOf(o) === chord || (o.alt ?? []).includes(chord)) return o;
  }
  return null;
}
export const isReserved = (chord: string) => FIXED.includes(chord);

export function rebind(c: Cmd, chord: string, takeFrom?: Cmd) {
  if (takeFrom) bound.map[takeFrom.id] = '';
  bound.map[c.id] = chord;
  keep();
}
export function unbind(c: Cmd) {
  bound.map[c.id] = '';
  keep();
}
export function resetKey(c: Cmd) {
  delete bound.map[c.id];
  keep();
}
/** Whether the owner changed (or removed) a command's key. */
export const changed = (c: Cmd) => bound.map[c.id] !== undefined;

// ---- focus ----

/** The icon of a panel kind (the ring's button shows the focused one). */
export function kindIcon(kind: PanelKind): string {
  if (kind.startsWith('files')) return 'files';
  if (kind === 'subagent') return 'chat';
  return kind;
}
export const kindName = (kind: PanelKind) => (kind.startsWith('files') ? 'Files' : kind === 'plugin' ? 'Plugin' : kind[0].toUpperCase() + kind.slice(1));
const group = (k: PanelKind | undefined) => (k?.startsWith('files') ? 'files' : k);

/** What the shell shows: sets the panels, and moves the focus where it belongs (a new right panel takes it; none left, it's the left). */
export function setPanes(next: PanelKind[]) {
  const prev = ui.panes;
  const changedRight = next.length > 1 && group(next[1]) !== group(prev[1]);
  const same = prev.length === next.length && prev.every((k, i) => k === next[i]);
  if (same) return;
  ui.panes = next;
  if (next.length < 2) ui.focus = 0;
  else if (changedRight) ui.focus = 1;
  else if (ui.focus > 1) ui.focus = 0;
}
export function focusPane(i: number) {
  if (i >= 0 && i < ui.panes.length) ui.focus = i;
}
/** Ctrl+\: the other panel (when split). */
export function toggleFocus() {
  if (ui.panes.length > 1) ui.focus = ui.focus === 0 ? 1 : 0;
}
/** Ctrl+Shift+\: close the side panel, or bring back the last one. */
export function toggleSide() {
  if (isSplit() && app.view) {
    ui.side = app.view;
    pickTab(app.view);
  } else if (app.screen === 'chat' && !app.narrow && !app.view) pickTab(ui.side ?? 'sessions');
}

// ---- the go-to: `g` then a letter ----

export const GOTO: Record<string, [string, string]> = {
  h: ['home', 'Home'], s: ['sessions', 'Sessions'], f: ['files', 'Files'], t: ['tasks', 'Tasks'],
  r: ['routines', 'Routines'], a: ['agents', 'Agents'], m: ['games', 'Games'], p: ['preview', 'Preview'],
};
let gotoTimer: ReturnType<typeof setTimeout> | undefined;
function armGoto() {
  ui.goto = true;
  clearTimeout(gotoTimer);
  gotoTimer = setTimeout(() => (ui.goto = false), 1800);
}
function goTo(letter: string): boolean {
  const g = GOTO[letter];
  if (!g) return false;
  // Preview belongs to a chat; without one there is nothing to show.
  if (g[0] === 'preview' && !app.session) { notice('Preview needs an open chat'); return true; }
  if (g[0] === 'home') pickTab('home'); else openTab(g[0]);
  return true;
}

// ---- the ring ----

export type RingItem = { id: string; title: string; icon: string; key?: string; run: () => void };
export const RING_MAX = 6;

/** The ring's actions for the focused panel: its commands (plugin ones at most four), else the global ones; plus Focus ⟷ when split. */
export function ringItems(): RingItem[] {
  const own = forPane(focused()).filter((c) => c.ring !== undefined && usable(c));
  const byRing = (a: Registered, b: Registered) => (a.ring ?? 0) - (b.ring ?? 0);
  const plugged = own.filter((c) => c.plugin).sort(byRing).slice(0, 4);
  const core = own.filter((c) => !c.plugin).sort(byRing).slice(0, RING_MAX - plugged.length);
  let list = [...core, ...plugged];
  if (!list.length) list = globals().filter((c) => c.ring !== undefined && usable(c)).sort(byRing).slice(0, RING_MAX);
  const items: RingItem[] = list.map((c) => ({ id: c.id, title: c.title, icon: c.icon, key: keyOf(c), run: c.run }));
  if (ui.panes.length > 1) {
    const other = ui.panes[ui.focus === 0 ? 1 : 0];
    items.push({ id: 'focus', title: 'Focus ⟷', icon: kindIcon(other), run: toggleFocus });
  }
  return items;
}
export function toggleRing() {
  ui.palette = false;
  ui.help = false;
  ui.ring = !ui.ring;
  ui.ringAt = 0;
}
export function togglePalette() {
  ui.ring = false;
  ui.help = false;
  ui.palette = !ui.palette;
}

function runSafe(run: () => void) {
  try { run(); } catch (e) { notice(String((e as Error).message ?? e)); }
}
function runRing(i: number) {
  const it = ringItems()[i];
  if (!it) return;
  ui.ring = false;
  runSafe(it.run);
}
/** The ring is open: it takes every key that isn't a Mod chord. */
function ringKey(chord: string): boolean {
  const items = ringItems();
  if (chord === 'Escape') { ui.ring = false; return true; }
  if (/^[1-9]$/.test(chord)) { if (Number(chord) <= items.length) runRing(Number(chord) - 1); return true; }
  if (chord === 'ArrowDown' || chord === 'ArrowRight' || chord === 'Tab') ui.ringAt = (ui.ringAt + 1) % items.length;
  else if (chord === 'ArrowUp' || chord === 'ArrowLeft' || chord === 'Shift+Tab') ui.ringAt = (ui.ringAt - 1 + items.length) % items.length;
  else if (chord === 'Enter' || chord === 'Space') runRing(ui.ringAt);
  else {
    const i = items.findIndex((it) => it.key === chord);
    if (i >= 0) runRing(i);
  }
  return true;
}

// ---- the handler ----

export type KeyIn = KeyLike & { typing?: boolean; interactive?: boolean };

/** Runs what a key means in the focused panel; true when it did something (the caller swallows the key). */
export function dispatch(k: KeyIn): boolean {
  const chord = chordOf(k);
  if (!chord) return false;
  const mod = chord.startsWith('Mod+');
  if (chord === 'Mod+k') { togglePalette(); return true; }
  if (chord === 'Mod+.') { toggleRing(); return true; }
  if (chord === 'Mod+\\') { ui.ring = false; toggleFocus(); return true; }
  if (chord === 'Mod+Shift+\\') { ui.ring = false; toggleSide(); return true; }
  if (ui.ring) return mod ? false : ringKey(chord);
  if (ui.palette) return false;
  if (ui.help) {
    if (chord === 'Escape' || chord === '?') { ui.help = false; return true; }
    return false;
  }
  if (ui.agentMenu) {
    if (chord === 'Escape') { ui.agentMenu = false; return true; }
    return false;
  }
  if (k.typing && !mod) return false;
  if (ui.goto) {
    ui.goto = false;
    clearTimeout(gotoTimer);
    goTo(chord);
    return true;
  }
  if (chord === 'Enter' && k.interactive) return false;
  if (chord === '?') { ui.help = true; return true; }
  if (chord === 'g') { armGoto(); return true; }
  for (const c of active()) {
    if (keyOf(c) === chord || (c.alt ?? []).includes(chord)) {
      runSafe(c.run);
      return true;
    }
  }
  return false;
}

const interactive = (t: EventTarget | null) => t instanceof HTMLElement && !!t.closest('button, a[href], summary, [role=button], select');

/** The window's key handler: a capture-phase listener, so it runs first. Returns what removes it. */
export function install(): () => void {
  const on = (e: KeyboardEvent) => {
    if (e.defaultPrevented || e.isComposing) return;
    const t = e.target as HTMLElement | null;
    const typing = !!t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable);
    if (dispatch({ key: e.key, code: e.code, ctrlKey: e.ctrlKey, metaKey: e.metaKey, altKey: e.altKey, shiftKey: e.shiftKey, typing, interactive: interactive(t) })) {
      e.preventDefault();
      e.stopPropagation();
    }
  };
  window.addEventListener('keydown', on, true);
  return () => window.removeEventListener('keydown', on, true);
}

// ---- walking a list with the cursor ----

const paneEl = () => document.querySelector<HTMLElement>(`[data-pane="${Math.min(ui.focus, ui.panes.length - 1)}"]`);
const rowsOf = (sel: string) => [...(paneEl()?.querySelectorAll<HTMLElement>(sel) ?? [])].filter((r) => r.offsetParent !== null);

/** The row under the cursor (the one marked `data-cur`), if any. */
export const cursorRow = (sel: string): HTMLElement | null => rowsOf(sel).find((r) => r.hasAttribute('data-cur')) ?? null;

/** Moves the cursor over the rows matching `sel` in the focused panel: by `d` rows, or to the first or last. */
export function moveCursor(sel: string, d: number | 'first' | 'last'): HTMLElement | null {
  const rows = rowsOf(sel);
  if (!rows.length) return null;
  const at = rows.findIndex((r) => r.hasAttribute('data-cur'));
  const i = d === 'first' ? 0 : d === 'last' ? rows.length - 1 : at < 0 ? (d > 0 ? 0 : rows.length - 1) : Math.min(rows.length - 1, Math.max(0, at + d));
  rows.forEach((r) => r.removeAttribute('data-cur'));
  rows[i].setAttribute('data-cur', '');
  rows[i].scrollIntoView({ block: 'nearest' });
  // A button clicked earlier would take Enter for itself.
  if (document.activeElement instanceof HTMLElement && document.activeElement !== document.body && !document.activeElement.matches('input, textarea, select')) document.activeElement.blur();
  return rows[i];
}
/** Activates the cursor row: the row itself if it's a button, else its first button (or the one named). */
export function pressCursor(sel: string, inner = 'button'): boolean {
  const r = cursorRow(sel);
  const b = r && (r.matches('button') ? r : r.querySelector<HTMLElement>(inner));
  b?.click();
  return !!b;
}
/** Puts the cursor on the next row (after the current one, wrapping) that `test` accepts; false when none does. */
export function findCursor(sel: string, test: (r: HTMLElement) => boolean): boolean {
  const rows = rowsOf(sel);
  const at = rows.findIndex((r) => r.hasAttribute('data-cur'));
  for (let n = 1; n <= rows.length; n++) {
    const r = rows[(at + n + rows.length) % rows.length];
    if (!test(r)) continue;
    rows.forEach((x) => x.removeAttribute('data-cur'));
    r.setAttribute('data-cur', '');
    r.scrollIntoView({ block: 'nearest' });
    return true;
  }
  return false;
}
/** Drops the cursor marks of the focused panel (its list changed). */
export const clearCursor = () => paneEl()?.querySelectorAll('[data-cur]').forEach((r) => r.removeAttribute('data-cur'));

export { RESERVED };
