// Where the page is, as the redesign lays it out (web/mock/ChatMock2.svelte): no sidebar; Home and
// the views stand alone (one panel, `screen: 'home' | 'view'`); a chat opens full width
// (`screen: 'chat'`) and a tab picked while in a chat opens beside it (two panels, desktop only).
// On a phone, one thing at a time: a chat takes the whole screen, with a back arrow.
//
// The address follows: `/?s=<id>` a chat (`/?s=new` a fresh one), `&view=<tab>` the view beside it;
// `/?view=<tab>` a view on its own (`files&file=<path>` with that file open, `&edit=1` writing it,
// `settings&tab=marketplace`). The editor's old addresses still land: `?path=<note>` writing that
// vault note in Files, `?notes` Files at the vault. Back and forward work.

import { files } from './api.ts';

/** A tab key: a built-in view, Settings, the subagent panel, Preview, a plugin command's panel or a plugin view. */
export type View = 'home' | 'sessions' | 'files' | 'tasks' | 'routines' | 'agents' | 'games' | 'settings' | 'subagent' | 'panel' | 'preview' | `plugin:${string}`;
/** Kept for the plugin host's context: 'home' a view on its own, 'side' beside a chat. */
export type Pane = 'home' | 'side';
/** The built-in tabs, in order (Home first); plugin views come after them. */
export const VIEWS: View[] = ['sessions', 'files', 'tasks', 'routines', 'agents', 'games'];
const isView = (v: string | null): v is View =>
  !!v && (VIEWS.includes(v as View) || ['settings', 'preview', 'panel', 'subagent'].includes(v) || /^plugin:[a-z0-9-]{2,40}\/[A-Za-z0-9._-]+$/.test(v));

export type Reveal = { path: string; n: number; open?: boolean; edit?: boolean };

export const app = $state({
  screen: 'home' as 'home' | 'chat' | 'view',
  /** The view picked in the tab bar: the right panel beside a chat, or the page on its own. */
  view: null as View | null,
  /** Where a chat's back arrow (phone) goes. */
  back: 'home' as 'home' | 'view',
  /** The open chat's session; 'new' a chat whose session starts with its first prompt. */
  session: null as string | null,
  /** The agent picked at the top (the orb); a new chat starts with it. */
  agent: typeof localStorage === 'undefined' ? 'zenbot' : localStorage.getItem('zen.agent') || 'zenbot',
  /** A new chat's first words, put in its message box when it opens. */
  chatDraft: '',
  /** Open the file picker when the next chat opens (the ring's Attach). */
  attachOnOpen: false,
  /** Counts chats opened by the owner (openSession): the shell gives the chat the keyboard on each. */
  opened: 0,
  /** A phone-sized screen (≤900px): one panel at a time. */
  narrow: false,
  /** The subagent session shown in the right panel. */
  subagent: null as string | null,
  /** A file for Files to show (attachments, Settings' USER.md, plugin links). */
  reveal: { path: '', n: 0 } as Reveal,
  /** Where Files was opened from, for its "‹ Settings" button. */
  filesFrom: null as string | null,
  /** Settings' page. */
  settings: 'you' as string,
  /** A new note not yet on disk (vault path and its first text), for the editor to start from. */
  draft: null as { path: string; text: string } | null,
  home: '',
  vault: '',
  /** A one-line note at the bottom (cleared after a while), with an optional Undo. */
  notice: '',
  undo: null as null | (() => void),
  /** Sessions waiting on the owner, for the tab title. */
  waiting: 0,
  /** The open chat is working (the orb breathes). */
  working: false,
  /** Text for the open chat's message box (Files' Ask zenbot beside it); `n` counts the asks. */
  chatInsert: { text: '', n: 0 },
  /** The file open in Files (it goes with the next prompt when Files is beside the chat). */
  viewing: null as string | null,
});

let noticeTimer: ReturnType<typeof setTimeout> | undefined;
export function notice(text: string, ms = 6000, undo: (() => void) | null = null) {
  app.notice = text;
  app.undo = undo;
  clearTimeout(noticeTimer);
  if (text) noticeTimer = setTimeout(() => ((app.notice = ''), (app.undo = null)), ms);
}

/** Two panels: a chat with a view beside it (desktop only). */
export const split = () => app.screen === 'chat' && !app.narrow && app.view !== null;
/** Whether a tab is the one showing. */
export const tabOn = (k: string) => (k === 'home' ? app.screen === 'home' : app.screen !== 'home' && app.view === k);

function url(): string {
  const p = new URLSearchParams();
  if (app.screen === 'chat' && app.session) p.set('s', app.session);
  if (app.screen !== 'home' && app.view && app.view !== 'subagent' && app.view !== 'panel') p.set('view', app.view);
  if (app.screen === 'view' && app.view === 'settings' && app.settings === 'marketplace') p.set('tab', 'marketplace');
  const q = p.toString();
  return q ? `/?${q}` : '/';
}

function push(replace = false) {
  const u = url();
  if (u === location.pathname + location.search) return;
  if (replace) history.replaceState(null, '', u);
  else history.pushState(null, '', u);
}

/** Reads the address into the state (on load, back and forward). */
export function fromUrl() {
  const p = new URLSearchParams(location.search);
  const s = p.get('s');
  const v = p.get('view');
  app.session = s;
  app.view = isView(v) && v !== 'home' ? v : null;
  app.screen = s ? 'chat' : app.view ? 'view' : 'home';
  if (app.view === 'settings' && p.get('tab') === 'marketplace') app.settings = 'marketplace';
  const file = p.get('file');
  if (file && app.view === 'files') app.reveal = { path: file, n: app.reveal.n + 1, open: true, edit: p.has('edit') };
}

/** `?path=<note>` and `?notes`, the editor's old addresses, once the vault is known. */
function landing() {
  const p = new URLSearchParams(location.search);
  const note = p.get('path');
  if (note !== null && app.vault) {
    app.session = null;
    app.screen = 'view';
    app.view = 'files';
    app.reveal = { path: `${app.vault}/${note.replace(/^\/+/, '')}`, n: app.reveal.n + 1, open: true, edit: true };
    push(true);
  } else if (p.has('notes') && app.vault) {
    app.session = null;
    app.screen = 'view';
    app.view = 'files';
    app.reveal = { path: `${app.vault}/`, n: app.reveal.n + 1 };
    push(true);
  } else if (p.has('file')) push(true);
}

export async function start() {
  fromUrl();
  addEventListener('popstate', fromUrl);
  try {
    const h = await files.home();
    app.home = h.home;
    app.vault = h.vault;
    landing();
  } catch {}
}

/** A tab picked in the tab bar: Home alone; beside an open chat (desktop) it toggles the right panel. */
export function pickTab(k: string) {
  app.filesFrom = null;
  if (k === 'home') {
    app.screen = 'home';
    app.view = null;
  } else if (app.screen === 'chat' && !app.narrow) app.view = app.view === k ? null : (k as View);
  else {
    app.screen = 'view';
    app.view = k as View;
  }
  push();
}

/** The agent a new chat starts with (the orb at the top, Agents' "Switch agent"). */
export function setAgent(id: string) {
  app.agent = id;
  try { localStorage.setItem('zen.agent', id); } catch {}
}

/** Open a tab, never toggling it shut (menu and ring shortcuts). */
export function openTab(k: string) {
  if (!tabOn(k)) pickTab(k);
}

/** Open a chat ('new': a fresh one, as `agent`). */
export function openSession(id: string, agent?: string) {
  if (id === 'new' && agent) app.agent = agent;
  if (app.screen !== 'chat') app.back = app.screen === 'view' ? 'view' : 'home';
  app.session = id;
  app.screen = 'chat';
  app.opened++;
  if (app.back === 'home' || app.narrow) app.view = null;
  if (app.view === 'subagent') app.view = null;
  push();
}

/** The session's own id once its first prompt made it (the address changes, history doesn't grow). */
export function sessionStarted(id: string) {
  app.session = id;
  push(true);
}

/** A subagent's session in the right panel (phone: full screen); again on the open one keeps it. */
export function openSub(id: string) {
  app.subagent = id;
  if (app.narrow) {
    app.screen = 'view';
    app.view = 'subagent';
  } else app.view = 'subagent';
}

/** The chat's back arrow (phone). */
export function goBack() {
  app.screen = app.back;
  if (app.back === 'home') app.view = null;
  else if (!app.view || app.view === 'subagent') app.view = 'sessions';
  push();
}

/** Close the chat (×): a side view (Tasks, Files…) takes the page; otherwise (a subagent panel closes with its chat) Sessions. */
export function closeChat() {
  if (split() && app.view !== 'subagent') app.screen = 'view';
  else {
    app.screen = 'view';
    app.view = 'sessions';
  }
  push();
}

/** The kept views' API (plugin host, chat commands): `pane` no longer matters. */
export function setView(_pane: Pane, view: View) {
  openTab(view);
}

export function board(view?: View) {
  if (view) {
    app.screen = view === 'home' ? 'home' : 'view';
    app.view = view === 'home' ? null : view;
  } else {
    app.screen = 'home';
    app.view = null;
  }
  push();
}

/** Show a file in Files (beside the chat when one is open): its folder with it picked, `open` it, or `edit` it. */
export function showFile(path: string, how: { open?: boolean; edit?: boolean } = {}) {
  const from = tabOn('settings') ? 'settings' : null;
  if (!tabOn('files')) pickTab('files');
  app.filesFrom = from;
  app.reveal = { path, n: app.reveal.n + 1, ...how };
}

/** Open a file (plugin links, chat links); `write` opens it for writing. */
export function openFile(path: string, _pane?: Pane, write = false) {
  showFile(path, { open: true, edit: write });
}

/** Back to the file's folder. */
export function closeFile(_pane?: Pane) {
  const p = app.reveal.path;
  app.reveal = { path: p, n: app.reveal.n + 1 };
}

/** Re-roots Files at `dir`. */
export function openDir(dir: string) {
  showFile(dir.replace(/\/+$/, '') + '/');
}

/** Toggle a view beside the chat (`/files`, `/tasks`). */
export function toggleSide(view: View = 'files') {
  pickTab(view);
}

/** Settings (`tab`: its page). */
export function settings(tab = 'you') {
  app.settings = tab;
  openTab('settings');
}

/** A vault path (`wiki/Note.md`) for a full path, or null when it isn't a vault note. */
export function vaultNote(path: string | null): string | null {
  if (!path || !app.vault || !path.startsWith(app.vault + '/') || !/\.md$/i.test(path)) return null;
  return path.slice(app.vault.length + 1);
}

/** A path shown shortly: `~/…` under home. */
export const tilde = (p: string) => (app.home && (p === app.home || p.startsWith(app.home + '/')) ? '~' + p.slice(app.home.length) : p);

/** Whether a key event belongs to a text field (views leave those keys alone). */
export const typing = (e: KeyboardEvent) => textField(e.target as HTMLElement | null);

/** Inputs that take no typed text (a checkbox, a button, a slider): keys such as Space stay shortcuts there. */
const NON_TEXT = new Set(['checkbox', 'radio', 'button', 'submit', 'reset', 'range', 'color', 'file', 'image']);
/** Whether an element takes typed text: a text-like input, a textarea, a select or an editable area. */
export const textField = (t: EventTarget | null) =>
  t instanceof HTMLElement && (
    (t instanceof HTMLInputElement && !NON_TEXT.has(t.type)) || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable
  );
