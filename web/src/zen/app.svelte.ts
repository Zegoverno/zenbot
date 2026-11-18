// Where the page is, as the terminal app's: the board (home screen) with its Sessions · Files ·
// Tasks · Agents · Games views and the plugins' views after them, or a session's chat with the side
// panel holding the same views (and a plugin command's panel). Settings is a board page off the top
// bar. Each pane (the board's, the side panel's) has its own open file; the file viewer is not a
// tab, it opens in place of the Files tree, with the file's opener (the built-in reader and writer,
// or a plugin's).
//
// The address follows: `/?s=<id>` a session (`/?s=new` a fresh chat), `/?view=files&file=<path>` the
// board (`&edit=1`: writing it; `&with=<plugin>/<opener>` opened with that opener),
// `/?view=settings&tab=marketplace`. `?path=<note>` (a vault note, as the terminal app's `e` opens it)
// lands on the board writing that note; `?notes` on the board's Files at the vault. Back and
// forward work.

import { files } from './api.ts';

/** The built-in views, Games, Settings (the board's), Preview and a plugin command's panel (the side's) and plugin views. */
export type View = 'sessions' | 'files' | 'tasks' | 'agents' | 'games' | 'settings' | 'panel' | 'preview' | `plugin:${string}`;
export type Pane = 'home' | 'side';
/** The built-in tabs, in order; plugin views come after them. */
export const VIEWS: View[] = ['sessions', 'files', 'tasks', 'agents', 'games'];
const isView = (v: string | null): v is View => !!v && (VIEWS.includes(v as View) || v === 'settings' || /^plugin:[a-z0-9-]{2,40}\/[A-Za-z0-9._-]+$/.test(v));

export const app = $state({
  /** The open session; 'new' a chat whose session starts with its first prompt; null the board. */
  session: null as string | null,
  /** The agent a fresh chat ('new') starts its session as. */
  agent: 'zenbot',
  view: 'sessions' as View,
  side: { open: false, view: 'files' as View },
  /** Which pane has the keys in a session: the chat or the side panel. */
  focus: 'chat' as 'chat' | 'side',
  file: { home: null as string | null, side: null as string | null },
  /** Whether each pane's file (a vault note) is open for writing (the writer) rather than reading. */
  write: { home: false, side: false },
  /** The opener the owner picked for each pane's file ("Open with"): `<plugin>/<id>`, 'builtin', or null for the default. */
  opener: { home: null as string | null, side: null as string | null },
  /** Settings' page. */
  settings: 'plugins' as 'plugins' | 'marketplace',
  /** A new note not yet on disk (vault path and its first text), for the editor to start from. */
  draft: null as { path: string; text: string } | null,
  /** Each pane's Files tree root. */
  root: { home: '', side: '' },
  home: '',
  vault: '',
  /** A one-line note at the bottom (cleared after a while). */
  notice: '',
  /** Sessions waiting on the owner, for the tab title. */
  waiting: 0,
  /** The preview gateway's port (web/server/preview.ts), or null when it is off. */
  preview: null as number | null,
});

let noticeTimer: ReturnType<typeof setTimeout> | undefined;
export function notice(text: string, ms = 6000) {
  app.notice = text;
  clearTimeout(noticeTimer);
  if (text) noticeTimer = setTimeout(() => (app.notice = ''), ms);
}

function url(): string {
  const p = new URLSearchParams();
  if (app.session) p.set('s', app.session);
  else {
    if (app.view !== 'sessions') p.set('view', app.view);
    if (app.view === 'files' && app.file.home) p.set('file', app.file.home);
    if (app.view === 'files' && app.file.home && app.write.home) p.set('edit', '1');
    if (app.view === 'files' && app.file.home && app.opener.home) p.set('with', app.opener.home);
    if (app.view === 'settings' && app.settings !== 'plugins') p.set('tab', app.settings);
  }
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
  app.session = p.get('s');
  const v = p.get('view');
  app.view = isView(v) ? v : app.session ? app.view : 'sessions';
  if (!app.session) {
    app.file.home = p.get('file');
    app.write.home = p.has('edit');
    app.opener.home = p.get('with');
    app.settings = p.get('tab') === 'marketplace' ? 'marketplace' : 'plugins';
  }
  app.focus = 'chat';
}

/** `?path=<note>` and `?notes`, the editor's old addresses, once the vault is known. */
function landing() {
  const p = new URLSearchParams(location.search);
  const note = p.get('path');
  if (note !== null && app.vault) {
    app.session = null;
    app.view = 'files';
    app.file.home = `${app.vault}/${note.replace(/^\/+/, '')}`;
    app.write.home = true;
    push(true);
  } else if (p.has('notes') && app.vault) {
    app.session = null;
    app.view = 'files';
    app.root.home = app.vault;
    app.file.home = null;
    push(true);
  }
}

export async function start() {
  fromUrl();
  addEventListener('popstate', fromUrl);
  try {
    const h = await files.home();
    app.home = h.home;
    app.vault = h.vault;
    app.preview = h.preview ?? null;
    // Back from signing in to the preview gateway: the Preview tab opens again where it was.
    if (localStorage.getItem('zen.preview.reopen') && app.session) {
      app.side.open = true;
      app.side.view = 'preview';
    }
    localStorage.removeItem('zen.preview.reopen');
    // Files opens in the shared workspace (the vault, ~/.zenbot/global); `..` climbs to home.
    app.root.home ||= h.vault || h.home;
    app.root.side ||= h.vault || h.home;
    landing();
  } catch {}
}

/** Opens a session's chat (`'new'`: a fresh one, as `agent`, zenbot by default). */
export function openSession(id: string, agent = 'zenbot') {
  if (id === 'new') app.agent = agent;
  app.session = id;
  app.focus = 'chat';
  push();
}

/** The session's own id once its first prompt made it (the address changes, history doesn't grow). */
export function sessionStarted(id: string) {
  app.session = id;
  push(true);
}

export function board(view?: View) {
  app.session = null;
  if (view) app.view = view;
  push();
}

export function setView(pane: Pane, view: View) {
  if (pane === 'home') {
    app.view = view;
    push();
  } else {
    app.side.view = view;
    app.side.open = true;
  }
}

/** Opens a file in a pane's file viewer (the side panel opens on Files); `opener` as "Open with" picks. */
export function openFile(path: string, pane: Pane = app.session ? 'side' : 'home', write = false, opener: string | null = null) {
  app.file[pane] = path;
  app.write[pane] = write;
  app.opener[pane] = opener;
  if (pane === 'side') {
    app.side.open = true;
    app.side.view = 'files';
  } else {
    app.view = 'files';
    push();
  }
}

export function closeFile(pane: Pane) {
  app.file[pane] = null;
  app.write[pane] = false;
  app.opener[pane] = null;
  if (pane === 'home') push();
}

/** Re-roots a pane's Files tree at `dir`. */
export function openDir(dir: string, pane: Pane = app.session ? 'side' : 'home') {
  app.root[pane] = dir;
  app.file[pane] = null;
  app.write[pane] = false;
  app.opener[pane] = null;
  setView(pane, 'files');
}

export function toggleSide(view?: View) {
  if (view && (!app.side.open || app.side.view !== view)) {
    app.side.open = true;
    app.side.view = view;
  } else app.side.open = !app.side.open;
  if (!app.side.open) app.focus = 'chat';
}

/** Writes a vault note (`wiki/Note.md`) in a pane: the writer in place of the reader. */
export function editNote(vaultPath: string, pane: Pane = app.session ? 'side' : 'home') {
  openFile(`${app.vault}/${vaultPath}`, pane, true);
}

/** Settings on the board or, while chatting, in the side panel (`tab`: its page). */
export function settings(tab: 'plugins' | 'marketplace' = 'plugins') {
  app.settings = tab;
  if (app.session) {
    app.side.open = true;
    setView('side', 'settings');
  } else board('settings');
}

/** Back from writing to reading the same file. */
export function readNote(pane: Pane) {
  app.write[pane] = false;
  app.opener[pane] = null;
  if (pane === 'home') push();
}

/** A pane's file as a vault note path (`wiki/Note.md`), or null when it isn't a vault note. */
export function vaultNote(path: string | null): string | null {
  if (!path || !app.vault || !path.startsWith(app.vault + '/') || !/\.md$/i.test(path)) return null;
  return path.slice(app.vault.length + 1);
}

/** A path shown shortly: `~/…` under home. */
export const tilde = (p: string) => (app.home && (p === app.home || p.startsWith(app.home + '/')) ? '~' + p.slice(app.home.length) : p);

/** Whether a key event belongs to a text field (views leave those keys alone). */
export const typing = (e: KeyboardEvent) => {
  const t = e.target as HTMLElement | null;
  return !!t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable);
};
