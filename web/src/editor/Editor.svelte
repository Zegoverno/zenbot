<script lang="ts">
  // The writing page: one note, a bare page with a word count, in a pane of the zen app (the
  // board's, or the side panel's) in place of the reader. Saves by itself a moment after you stop
  // typing, and never over a newer copy on disk (the server checks the hash). Alt+W (or the word
  // count) goes back to reading, once saved; a link or Alt+O opens another note here.
  import { onMount, untrack } from 'svelte';
  import { EditorView, keymap, drawSelection, placeholder } from '@codemirror/view';
  import { Compartment, EditorState, Prec, Transaction } from '@codemirror/state';
  import { defaultKeymap, history, historyKeymap } from '@codemirror/commands';
  import { markdown } from './language.ts';
  import { syntaxHighlighting, HighlightStyle } from '@codemirror/language';
  import { tags } from '@lezer/highlight';
  import Side from './Side.svelte';
  import { changelogEntries, notesText, parseNote, withNotesText, serializeNote, titleOf, withoutGhosts, wordCount, type Group, type Note } from '../doc/note.ts';
  import { addGhost, deleteGhost, ghostHere, ghosts, ghostsField, keepOnly, reviveGhost, selectGhost, stepGhost, toggleGhost, type Refusal } from './ghosts.ts';
  import Tabs from './Tabs.svelte';
  import { wholeWords } from './words.ts';
  import { hoverFlip } from './hover.ts';
  import { preview, type Target } from './preview.ts';
  import Opener from './Opener.svelte';
  import { api, Conflict, type NoteProposal, type Version, type VersionState } from '../lib/api.ts';
  import {
    addGroup, addVariants, alternatives, dropLonely, ensureGroup, flip, flipTo, groupAt, groupById, groupsField, removeVariant, removeAll,
    resumeShowing, setGroup, showDefaults,
  } from './alternatives.ts';
  import Panel from './Panel.svelte';
  import {
    acceptAsAlternative, acceptFix, dropFix, familyOf, fixAt, fixesField, levelField, LEVELS, nextFix, placeFixes, selectFix, setFixes, setLevel, shownFixes, takeCut,
    type Family, type Fix,
  } from './fixes.ts';
  import CheckPanel from './CheckPanel.svelte';

  let { path, text, hash, keys = true, read, open }: {
    path: string; text: string; hash: string | null;
    /** Whether this pane has the keys (the board, or the side panel when it has them). */
    keys?: boolean;
    /** Back to the reader. */
    read: () => void;
    /** Writes another vault note here. */
    open: (path: string) => void;
  } = $props();

  let host: HTMLDivElement;
  let view: EditorView;
  let note: Note;
  // The page is made once per note: these start from the props and then live on their own.
  let base: string | null = untrack(() => hash);
  let words = $state(0);
  // The changelog, shown apart (the side panel's foot; a section of its own when reading).
  let changelog = $state('');
  // The notes section's words, edited in the side panel; shown as a section when reading.
  let notes = $state('');
  // The front matter, edited in the side panel's Properties.
  let frontmatter = $state('');
  // Two panels of tabs: the left (Alt+L) works on the text, the right (Alt+R) is about the note.
  // Alt+L/R opens its panel, then moves to the next tab, then closes it. The shared keys
  // (Alt+↑↓, Alt+Enter, Alt+Del, Alt+G) act on the tab of the panel opened last.
  const LEFT = ['alternatives', 'check', 'ghosts'] as const;
  const RIGHT = ['notes', 'versions', 'properties'] as const;
  type Tab = (typeof LEFT)[number] | (typeof RIGHT)[number];
  let left = $state<(typeof LEFT)[number] | null>(null);
  let right = $state<(typeof RIGHT)[number] | null>(null);
  let active = $state<'left' | 'right'>('left');
  const tab = $derived<Tab | null>(active === 'left' ? (left ?? right) : (right ?? left));
  // The versions tab: the closed version chosen (newest first), and a count that asks it to close.
  let vsel = $state(0);
  // Alt+O: the note finder.
  let opening = $state(false);
  let closeAsk = $state(0);
  // The ghosts tab's list.
  let ghostList = $state<Array<{ id: string; from: number; to: number; text: string; by?: string }>>([]);
  let ghostAt = $state<string | null>(null);
  // The note's versions; and the closed version being looked at, read-only, if any.
  let vs = $state<VersionState | null>(null);
  let viewing = $state<Version | null>(null);
  // The proofreader: its fixes on the page, and the one selected.
  // The AI's proposals (fixes, marks, cuts): which family it is reading for, if any.
  let busy = $state<Family | null>(null);
  let checkError = $state('');
  let fixList = $state<Array<Fix & { text: string }>>([]);
  // The check tab's pass (Alt+P): fixes or marks.
  let pass = $state<'check' | 'marks'>('check');
  // Shrink, in the ghosts tab: the level showing (0: none) and the words when it read (progress).
  let level = $state(0);
  let shrinkFrom = $state(0);
  let fixHere = $state<string | null>(null);
  // The editor with its tools; off only while an old version is open, read-only.
  let tools = $state(true);
  const editing = new Compartment();
  const editable = (on: boolean) => [EditorView.editable.of(on), EditorState.readOnly.of(!on)];
  let status = $state<'saved' | 'unsaved' | 'saving' | 'conflict' | 'error' | 'new'>(base === null ? 'new' : 'saved');
  let message = $state('');
  let panel = $state<{ group: Group; busy: boolean; error: string } | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;
  // The note as last read or written: a save that would write the same is skipped, so opening,
  // reading or flipping back never rewrites a file. One save runs at a time.
  let lastSaved = '';
  let inflight: Promise<void> | null = null;
  let again = false;
  let hint = $state('');
  let hintTimer: ReturnType<typeof setTimeout> | undefined;

  // The footer: the keys of the tab they act on.
  const KEYS: Record<Tab, Array<[string, string]>> = $derived({
    alternatives: [['↑↓', 'flip'], ['↵', 'versions'], ['Del', 'remove the one showing'], ['G', '🤖 versions']],
    check: pass === 'marks'
      ? [['↑↓', 'move'], ['↵', 'done'], ['G', 'mark the piece'], ['P', 'fixes']]
      : [['↑↓', 'move'], ['↵', 'accept'], ['Del', 'reject'], ['G', 'check the piece'], ['P', 'marks']],
    ghosts: [['↑↓', 'move'], ['↵', 'cut · bring back'], ['Del', 'keep · delete'], ['G', 'ghost the selection'], ['K', 'keep it, ghost the rest'],
      ['P', level ? (level === 50 ? 'shrink off' : `shrink ${LEVELS[LEVELS.indexOf(level) + 1]}%`) : '🤖 shrink 10%']],
    notes: [['↑↓', 'proposed notes'], ['↵', 'keep'], ['Del', 'dismiss'], ['G', 'find in wiki'], ['S', 'send the selection here']],
    versions: [['↑↓', 'choose'], ['↵', 'open'], ['G', 'close this version']],
    properties: [],
  });

  const msg = (e: unknown) => (e instanceof Error ? e.message : String(e));
  const fixById = (id: string) => view.state.field(fixesField).find((x) => x.id === id);

  const highlight = HighlightStyle.define([
    { tag: tags.heading1, class: 'zw-h1' },
    { tag: tags.heading2, class: 'zw-h2' },
    { tag: [tags.heading3, tags.heading4], class: 'zw-h3' },
    { tag: tags.processingInstruction, class: 'zw-mark' },
    { tag: tags.emphasis, fontStyle: 'italic' },
    { tag: tags.strong, fontWeight: '650' },
    { tag: tags.url, class: 'zw-mark' },
    { tag: tags.monospace, class: 'zw-code' },
    { tag: tags.quote, class: 'zw-quote' },
  ]);

  function setFrontmatter(fm: string) {
    frontmatter = fm;
    note = { ...note, frontmatter: fm };
    if (status !== 'conflict') status = 'unsaved';
    schedule();
  }


  function editNotes(text: string) {
    notes = text;
    note = { ...note, notes: withNotesText(note.notes ?? '', text) };
    if (status !== 'conflict') status = 'unsaved';
    schedule();
  }

  /** Alt+S: the selection leaves the page for the end of the notes. */
  function sendToNotes(v: EditorView): boolean {
    const { from, to, empty } = v.state.selection.main;
    if (empty) { flash('Select what to send to Notes.'); return true; }
    const words = v.state.sliceDoc(from, to).trim();
    v.dispatch({ changes: { from, to }, selection: { anchor: from }, userEvent: 'delete.cut' });
    if (words) editNotes(notes.trim() ? `${notes.replace(/\s+$/, '')}\n\n${words}` : words);
    flash('Sent to Notes.');
    return true;
  }

  // Notes > Find: zenbot's proposed notes, from the wiki, until kept or dismissed. In the notes
  // tab Alt+G finds and selects the first; Alt+Enter keeps, Alt+Del dismisses, Alt+↑/↓ move,
  // Esc (or typing) leaves them.
  let finding = $state(false);
  let findError = $state('');
  let proposals = $state<NoteProposal[]>([]);
  let picked = $state<number | null>(null);
  async function findNotes() {
    if (finding) return;
    finding = true;
    findError = '';
    picked = null;
    openRight('notes');
    try {
      const r = await api.findNotes(path, withoutGhosts(view.state.doc.toString(), view.state.field(ghostsField)));
      proposals = r.notes;
      picked = r.notes.length ? 0 : null;
      if (!r.notes.length) findError = 'Nothing in the wiki that helps this piece yet.';
    } catch (e) {
      findError = msg(e);
    } finally {
      finding = false;
      view?.focus();
    }
  }
  function keepProposal(p: NoteProposal) {
    const link = `[[${p.page}]]`;
    const line = `- ${p.text.includes(link) ? p.text : `${p.text} ${link}`}${p.quote ? ` “${p.quote}”` : ''}`;
    editNotes(notes.trim() ? `${notes.replace(/\s+$/, '')}\n${line}` : line);
    dropProposal(p);
  }
  /** Removes a proposal; the selection stays on the one that takes its place. */
  function dropProposal(p: NoteProposal) {
    const i = proposals.indexOf(p);
    proposals = proposals.filter((x) => x !== p);
    if (picked !== null) picked = proposals.length ? Math.min(i < 0 ? picked : i, proposals.length - 1) : null;
  }
  /** A proposal key's action on the selected proposal; false when none is selected. */
  function onPicked(act: (p: NoteProposal) => void): boolean {
    const p = picked === null ? undefined : proposals[picked];
    if (!p) return false;
    act(p);
    return true;
  }
  function movePicked(d: number): boolean {
    if (!proposals.length) return false;
    picked = picked === null ? 0 : (picked + d + proposals.length) % proposals.length;
    return true;
  }

  function currentText(): string {
    return serializeNote({ ...note, body: view.state.doc.toString(), groups: view.state.field(groupsField), ghosts: view.state.field(ghostsField) });
  }

  /** A new note is created only once it has something past its H1. */
  function worthSaving(): boolean {
    if (base !== null) return true;
    const body = view.state.doc.toString();
    return body.replace(/^\s*#[^\n]*\n?/, '').trim().length > 0;
  }

  /** The AI reads the piece for one family (fixes, marks or cuts); its proposals replace that
   *  family's others. Shrink reads once for every level and shows the first. */
  async function ask(family: Family) {
    if (busy || !tools) return;
    if (panel) closePanel();
    busy = family;
    openLeft(family === 'shrink' ? 'ghosts' : 'check');
    checkError = '';
    const others = () => view.state.field(fixesField).filter((f) => familyOf(f.kind) !== family);
    view.dispatch({ effects: setFixes.of(others()) });
    try {
      const doc = withoutGhosts(view.state.doc.toString(), view.state.field(ghostsField));
      const r = await (family === 'shrink' ? api.shrink(doc) : family === 'marks' ? api.marks(doc) : api.check(doc));
      const placed = placeFixes(view.state, r.fixes, words);
      if (family === 'shrink') (shrinkFrom = words, setShrink(level || 10));
      view.dispatch({ effects: setFixes.of([...others(), ...placed].sort((a, b) => a.from - b.from)) });
      if (placed.length) nextFix(view, family, true);
    } catch (e) {
      checkError = msg(e);
    }
    busy = null;
  }

  function setShrink(l: number) {
    level = l;
    view.dispatch({ effects: setLevel.of(l) });
  }
  /** Alt+P in the ghosts tab: the first press reads the piece; then 10, 20, 30, 50%, off. */
  function nextShrink(): boolean {
    if (!view.state.field(fixesField).some((f) => f.kind === 'cut')) return ask('shrink'), true;
    setShrink(LEVELS[LEVELS.indexOf(level) + 1] ?? 0);
    return true;
  }

  /** Runs an action on one proposal by id, then selects the next of its family. */
  function onFix(id: string, act: (f: Fix) => void) {
    const f = fixById(id);
    if (!f) return;
    act(f);
    nextFix(view, familyOf(f.kind));
  }
  const rejectFix = (f: Fix) => view.dispatch({ effects: dropFix.of(f.id) });
  const take = (f: Fix) => (f.kind === 'cut' ? refuse(takeCut(view, f)) : acceptFix(view, f));

  /** Alt+Enter / Alt+Del on the selected proposal; false when none is selected. */
  function fixKey(family: Family, act: (f: Fix) => void): boolean {
    const f = fixAt(view.state, family);
    if (!f) return false;
    onFix(f.id, act);
    return true;
  }

  /** Clears one family's proposals. */
  function clearCheck(family: Family) {
    view.dispatch({ effects: setFixes.of(view.state.field(fixesField).filter((f) => familyOf(f.kind) !== family)) });
    checkError = '';
    view.focus();
  }

  async function refreshVersions() {
    if (base === null) return;
    vs = await api.versions(path).catch(() => vs);
  }

  /** zenbot's changelog line for the open version, from what changed since the last (saves first). */
  async function suggestLog(): Promise<string> {
    await save();
    return status === 'saved' ? (await api.changelog(path)).log : '';
  }

  /** Closes the open version with its changelog line; an error to show, or null. */
  async function closeVersion(log: string): Promise<string | null> {
    await save();
    if (status !== 'saved' || base === null) return 'Save first: the note has unsaved changes.';
    try {
      const r = await api.close(path, base, log);
      adopt(r, true);
      vs = r;
      return null;
    } catch (e) {
      return e instanceof Conflict ? 'The note changed on disk: reload it first.' : msg(e);
    }
  }

  /** Opens a closed version, read-only. */
  async function viewVersion(n: number) {
    await save();
    if (status !== 'saved') return flash('Save first: the note has unsaved changes.');
    const r = await api.version(path, n).catch(() => null);
    const v = vs?.versions.find((x) => x.n === n);
    if (!r || !v) return flash(`Couldn't open v${n}.`);
    if (tools) setTools(false);
    viewing = v;
    load(r.text);
    view.scrollDOM.scrollTop = 0;
  }

  /** Back from an old version to the open one. */
  async function backToCurrent() {
    viewing = null;
    adopt(await api.read(path));
    setTools(true);
    await refreshVersions();
  }

  /** A new open version, starting from the one being looked at. */
  async function branchHere() {
    if (!viewing || base === null) return;
    try {
      const r = await api.branch(path, base, viewing.n);
      viewing = null;
      adopt(r);
      vs = r;
      setTools(true);
    } catch (e) {
      const m = msg(e);
      flash(m.startsWith('close') ? `Close the open version first (${m.slice(6)}), then branch.` : m);
    }
  }

  /** Runs a reload keeping the cursor and scroll where they were. */
  function keepPlace(f: () => void) {
    const at = view.state.selection.main.head;
    const top = view.scrollDOM.scrollTop;
    f();
    view.dispatch({ selection: { anchor: Math.min(at, view.state.doc.length) } });
    view.scrollDOM.scrollTop = top;
  }

  /** Picks up changes made elsewhere (zenbot, Obsidian, sync) while nothing is unsaved here. */
  async function poll() {
    if (viewing || panel || status !== 'saved' || base === null || document.visibilityState === 'hidden') return;
    const r = await api.read(path).catch(() => null);
    if (!r || r.hash === base || status !== 'saved' || viewing) return;
    adopt(r, true);
    await refreshVersions();
  }

  async function save(): Promise<void> {
    clearTimeout(timer);
    if (inflight) {
      again = true;
      return inflight;
    }
    inflight = write();
    try {
      await inflight;
    } finally {
      inflight = null;
    }
    if (again) {
      again = false;
      return save();
    }
  }

  async function write() {
    if (viewing || status === 'conflict' || !worthSaving()) return;
    const out = currentText();
    if (out === lastSaved && base !== null) {
      status = 'saved';
      return;
    }
    status = 'saving';
    try {
      base = (await api.write(path, out, base)).hash;
      lastSaved = out;
      status = currentText() === out ? 'saved' : 'unsaved';
      refreshVersions();
      if (status === 'unsaved') schedule();
    } catch (e) {
      if (e instanceof Conflict) {
        status = 'conflict';
        message = 'This note changed on disk (Obsidian or sync). Reload to get that version, or keep yours and overwrite.';
      } else {
        status = 'error';
        message = msg(e);
        schedule(5000);
      }
    }
  }

  function schedule(ms = 900) {
    clearTimeout(timer);
    timer = setTimeout(save, ms);
  }

  /** Takes the file as the server has it: the page shows it, and it counts as saved. */
  function adopt(r: { text: string; hash: string }, keep = false) {
    if (keep) keepPlace(() => load(r.text));
    else load(r.text);
    base = r.hash;
    status = 'saved';
    message = '';
  }

  async function reload() {
    adopt(await api.read(path));
  }

  async function overwrite() {
    const r = await api.read(path).catch(() => null);
    base = r ? r.hash : null;
    status = 'unsaved';
    message = '';
    await save();
  }

  function load(t: string) {
    note = parseNote(t);
    changelog = note.changelog ?? '';
    notes = notesText(note.notes ?? '');
    frontmatter = note.frontmatter;
    view.setState(makeState(note.body));
    if (note.groups.length) view.dispatch({ effects: note.groups.map((g) => addGroup.of(g)) });
    if (note.ghosts?.length) view.dispatch({ effects: note.ghosts.map((g) => addGhost.of(g)) });
    if (tools) resumeShowing(view);
    words = wordCount(withoutGhosts(note.body, note.ghosts));
    lastSaved = currentText();
    // Loading is not an edit: nothing to save.
    clearTimeout(timer);
    if (base !== null && status !== 'conflict') status = 'saved';
  }

  // --- the versions panel -------------------------------------------------------------------

  function refreshPanel() {
    if (!panel) return;
    const g = groupById(view.state, panel!.group.id);
    if (g) panel.group = g;
    else panel = null;
  }

  function openPanel(): boolean {
    const { from, to, empty } = view.state.selection.main;
    if (view.state.field(ghostsField).some((h) => (empty ? h.from < from && from < h.to : h.from < to && h.to > from))) {
      flash('That text is ghosted: bring it back first (Ghosts tab, Alt+Enter).');
      return true;
    }
    const g = ensureGroup(view);
    if (g === 'overlap') {
      flash('That touches more than one set of alternatives: put the cursor in one of them.');
      return true;
    }
    if (!g) {
      flash('Select a word, sentence or paragraph first.');
      return true;
    }
    const sel = view.state.selection.main;
    if (!sel.empty && (g.from > sel.from || g.to < sel.to || g.variants.length > 1 && (g.from !== sel.from || g.to !== sel.to))) {
      flash('That text already has alternatives: here they are. Alt+Del in the panel removes them all.');
    }
    openLeft('alternatives');
    panel = { group: g, busy: false, error: '' };
    return true;
  }

  /** Alt+Del in alternatives: the version showing goes; the next one shows. */
  function removeShowing(): boolean {
    const g = groupAt(view.state);
    if (!g || g.variants.length < 2) return false;
    removeVariant(view, g.id, g.active);
    dropLonely(view);
    return true;
  }

  /** Shows why a key did nothing; the key is handled either way. */
  function refuse(why: Refusal): boolean {
    if (why) flash(why);
    return true;
  }

  function flash(text: string) {
    clearTimeout(hintTimer);
    hint = text;
    hintTimer = setTimeout(() => (hint = ''), 2500);
  }

  function closePanel(def?: number) {
    if (panel && def !== undefined) {
      const g = groupById(view.state, panel!.group.id);
      if (g) flipTo(view, g, def);
    }
    panel = null;
    dropLonely(view);
    view.focus();
  }

  async function askAI(g: Group) {
    if (panel?.busy) return;
    panel = { group: g, busy: true, error: '' };
    const doc = view.state.doc;
    const line = doc.lineAt(g.from);
    try {
      const r = await api.suggest({
        title: titleOf(doc.toString()),
        paragraph: line.text,
        target: doc.sliceString(g.from, g.to),
        scope: g.scope,
        existing: g.variants.map((v) => v.text),
      });
      addVariants(view, g.id, r.alternatives.map((text) => ({ text, by: 'ai' as const, model: r.model })));
      if (panel) panel.busy = false;
      if (panel && !r.alternatives.length) panel.error = 'The AI had nothing new to offer.';
    } catch (e) {
      if (panel) panel = { ...panel, busy: false, error: msg(e) };
    }
    refreshPanel();
  }

  /** Leaves for another note (or a new one) once this one is safely saved. */
  /** A link clicked on the page: a web address opens in a new tab, a note here. */
  async function follow(t: Target) {
    if ('url' in t) return void window.open(t.url, '_blank', 'noopener');
    const want = t.note.replace(/\.md$/i, '').toLowerCase();
    const files = (await api.files().catch(() => ({ files: [] as string[] }))).files;
    const dir = path.includes('/') ? path.slice(0, path.lastIndexOf('/') + 1) : '';
    const name = (f: string) => f.replace(/\.md$/i, '').toLowerCase();
    // A path (from the vault, or from this note's folder), else a name, the nearest first.
    const found = files.find((f) => name(f) === want) ?? files.find((f) => name(f) === (dir + want).toLowerCase())
      ?? files.filter((f) => name(f).split('/').pop() === want.split('/').pop()).sort((a, b) => Number(b.startsWith(dir)) - Number(a.startsWith(dir)))[0];
    if (!found) return flash(`No note called “${t.note}” in the vault.`);
    if (found !== path) leaveTo(() => open(found));
  }

  /** Leaves this note (for reading, or another note) once it is safely saved. */
  async function leaveTo(go: () => void) {
    if (!viewing) await save();
    if (!viewing && status !== 'saved' && status !== 'new') {
      opening = false;
      return flash('This note is not saved yet: sort that out first.');
    }
    go();
  }
  const toRead = () => void leaveTo(read);

  function openLeft(t: (typeof LEFT)[number] | null) {
    if (panel && t !== 'alternatives') closePanel();
    left = t;
    active = t ? 'left' : 'right';
  }
  function openRight(t: (typeof RIGHT)[number] | null) {
    right = t;
    active = t ? 'right' : 'left';
  }
  /** The tab after this one; null (the panel closes) after the last. */
  const after = <T,>(tabs: readonly T[], t: T | null): T | null => tabs[t === null ? 0 : tabs.indexOf(t) + 1] ?? null;

  // What the shared keys do in each tab; false lets the key do what it normally does.
  type Verb = 'up' | 'down' | 'ok' | 'del' | 'go' | 'k' | 's' | 'p';
  const VERBS: Record<string, Verb> = {
    ArrowUp: 'up', ArrowDown: 'down', Enter: 'ok', NumpadEnter: 'ok', Delete: 'del', Backspace: 'del', KeyG: 'go', KeyK: 'k', KeyS: 's', KeyP: 'p',
  };
  const older = () => [...(vs?.versions ?? [])].reverse();
  const verbs: Record<Tab, Partial<Record<Verb, () => boolean>>> = {
    alternatives: {
      up: () => flip(view, -1), down: () => flip(view, 1), ok: openPanel, del: removeShowing,
      go: () => (openPanel(), panel && askAI(panel.group), true),
    },
    check: {
      up: () => nextFix(view, pass, false, true), down: () => nextFix(view, pass),
      ok: () => fixKey(pass, take), del: () => fixKey(pass, rejectFix),
      go: () => (ask(pass), true), p: () => ((pass = pass === 'check' ? 'marks' : 'check'), true),
    },
    // Shrink's cuts, while any show, come first: the arrows walk them, Enter cuts, Del keeps.
    ghosts: {
      up: () => nextFix(view, 'shrink', false, true) || stepGhost(view, -1), down: () => nextFix(view, 'shrink') || stepGhost(view, 1),
      ok: () => fixKey('shrink', take) || reviveGhost(view), del: () => fixKey('shrink', rejectFix) || deleteGhost(view),
      go: () => refuse(toggleGhost(view)), k: () => refuse(keepOnly(view)), p: nextShrink,
    },
    notes: {
      up: () => movePicked(-1), down: () => movePicked(1), ok: () => onPicked(keepProposal), del: () => onPicked(dropProposal),
      go: () => (findNotes(), true), s: () => sendToNotes(view),
    },
    versions: {
      up: () => (vsel = Math.max(0, vsel - 1), true), down: () => (vsel = Math.min(older().length - 1, vsel + 1), true),
      ok: () => (older()[vsel] ? (viewVersion(older()[vsel].n), true) : false),
      go: () => (vs?.open ? (closeAsk++, true) : refuse('Nothing to close: no changes since the last version.')),
    },
    properties: {},
  };

  /** Page-wide keys, wherever the focus is, seen before the editor's own. Matched on the
   *  physical key so Option+letter works on a Mac. */
  function pageKeys(e: KeyboardEvent) {
    if (!keys || !e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
    if (keyFor(e.code)) {
      e.preventDefault();
      e.stopPropagation();
    }
  }
  function keyFor(code: string): boolean {
    if (code === 'KeyO') return (opening = !opening), true;
    if (code === 'KeyW') return toRead(), true;
    if (!tools || viewing) return false;
    if (code === 'KeyL') return openLeft(after(LEFT, left)), true;
    if (code === 'KeyR') return openRight(after(RIGHT, right)), true;
    // The alternatives editor's own field keeps its keys (Alt+G, Alt+Del all).
    if (tab === 'alternatives' && document.activeElement?.closest('.alts')) return false;
    const verb = VERBS[code];
    return !!(tab && verb && verbs[tab][verb]?.());
  }

  /** Reading shows each group at its default; writing brings back the tools. */
  function setTools(on: boolean) {
    if (on && viewing) return void flash(`v${viewing.n} is closed: branch from it to edit, or go back.`);
    if (!on && panel) closePanel(panel.group.def ?? panel.group.active);
    if (on) resumeShowing(view);
    else showDefaults(view);
    tools = on;
    view.dispatch({ effects: editing.reconfigure(editable(on)) });
    if (on) view.focus();
  }

  // --- the editor ---------------------------------------------------------------------------

  function makeState(doc: string) {
    return EditorState.create({
      doc,
      extensions: [
        Prec.highest(keymap.of([
          { key: 'Escape', run: () => picked !== null && ((picked = null), true) },
          { key: 'Mod-s', run: () => (save(), true) },
        ])),
        history(),
        drawSelection(),
        wholeWords,
        keymap.of([...defaultKeymap, ...historyKeymap]),
        markdown(),
        syntaxHighlighting(highlight),
        EditorView.lineWrapping,
        EditorView.contentAttributes.of({ spellcheck: 'true', autocapitalize: 'sentences', 'aria-label': 'Note' }),
        placeholder('Write.'),
        alternatives(),
        ghosts(),
        levelField,
        fixesField,
        hoverFlip,
        preview(follow),
        editing.of(editable(tools)),
        EditorView.updateListener.of((u) => {
          if (u.docChanged && u.transactions.some((t) => t.isUserEvent('input'))) picked = null;
          if (u.docChanged || u.startState.field(ghostsField) !== u.state.field(ghostsField)) {
            words = wordCount(withoutGhosts(u.state.doc.toString(), u.state.field(ghostsField)));
          }
          const changedGroups = u.startState.field(groupsField) !== u.state.field(groupsField) ||
            u.startState.field(ghostsField) !== u.state.field(ghostsField);
          if (u.docChanged || changedGroups) {
            if (status !== 'conflict') status = base === null && !worthSaving() ? 'new' : 'unsaved';
            schedule();
          }
          refreshPanel();
          const fixesMoved = u.startState.field(fixesField) !== u.state.field(fixesField) || u.startState.field(levelField) !== u.state.field(levelField);
          if (fixesMoved || (u.docChanged && fixList.length)) {
            fixList = shownFixes(u.state).map((f) => ({ ...f, text: u.state.sliceDoc(f.from, f.to) }));
          }
          if (u.selectionSet || fixesMoved) fixHere = fixAt(u.state)?.id ?? null;
          const gs = u.state.field(ghostsField);
          if (u.startState.field(ghostsField) !== gs || (u.docChanged && ghostList.length)) {
            ghostList = [...gs].sort((a, b) => a.from - b.from).map((g) => ({ ...g, text: u.state.sliceDoc(g.from, g.to) }));
          }
          if (u.selectionSet || u.startState.field(ghostsField) !== gs) ghostAt = ghostHere(u.state)?.id ?? null;
        }),
      ],
    });
  }

  onMount(() => {
    view = new EditorView({ parent: host });
    // The end-to-end tests drive the page through this; it exists only in dev builds.
    if (import.meta.env.DEV) (window as unknown as { __zw: EditorView }).__zw = view;
    load(text);
    refreshVersions();
    const polling = setInterval(poll, 4000);
    // A fresh note starts writing under its title; a note opened to read, at its top.
    if (base === null) view.dispatch({ selection: { anchor: view.state.doc.length }, scrollIntoView: true });
    view.focus();
    const leave = (e: BeforeUnloadEvent) => {
      if (status === 'unsaved' || status === 'saving') {
        save();
        e.preventDefault();
      }
    };
    const hidden = () => document.visibilityState === 'hidden' && status === 'unsaved' && save();
    window.addEventListener('beforeunload', leave);
    document.addEventListener('visibilitychange', hidden);
    return () => {
      // Leaving the pane (another file, the reader, a closed panel) saves what is left.
      if (status === 'unsaved') save();
      window.removeEventListener('beforeunload', leave);
      document.removeEventListener('visibilitychange', hidden);
      clearInterval(polling);
      view.destroy();
    };
  });

  const statusText = $derived(
    { saved: 'Saved', unsaved: 'Unsaved', saving: 'Saving…', conflict: 'Changed on disk', error: 'Not saved', new: 'New' }[status],
  );
</script>

<svelte:window onkeydowncapture={pageKeys} />

<div class="page" class:bare={!tools} class:with-panel={tools && left} class:with-side={tools && right}>
  <header>
    <button class="home" onclick={toRead} title="Read (Alt+W)">Read</button>
    <button class="count" onclick={toRead} title="Read (Alt+W)">
      {words} {words === 1 ? 'word' : 'words'}
    </button>
    {#if viewing}
      <span class="ver">v{viewing.n} · closed {viewing.date}</span>
    {:else if vs && tools}
      <span class="ver" class:open={vs.open}>v{vs.open ? vs.current + 1 : vs.current}{vs.open ? ' · open' : ''}</span>
    {/if}
    {#if tools}
      <span class="file">{path}</span>
      <span class="status status-{status}">{statusText}</span>
    {/if}
  </header>

  {#if viewing}
    <div class="banner viewing">
      <span>v{viewing.n}, closed {viewing.date}: {viewing.log}. Read-only.</span>
      <button onclick={branchHere} disabled={vs?.open} title={vs?.open ? `Close v${vs.current + 1} first, then branch` : 'Start a new open version from this one'}>Branch from here</button>
      <button onclick={backToCurrent}>Back to the open version</button>
    </div>
  {/if}

  {#if message}
    <div class="banner" role="alert">
      <span>{message}</span>
      {#if status === 'conflict'}
        <button onclick={reload}>Reload</button>
        <button onclick={overwrite}>Keep mine</button>
      {/if}
    </div>
  {/if}

  {#if tools && left}
    <aside class="panel" aria-label="Tools">
      <Tabs tabs={LEFT} tab={left} pick={openLeft} keyHint="Alt+L: next tab" />
      {#if left === 'alternatives'}
        {#if panel}
          {#key panel.group.id}
          <Panel
            group={panel.group}
            busy={panel.busy}
            error={panel.error}
            mark={(d) => { const g = groupById(view.state, panel!.group.id); if (g && g.def !== d) view.dispatch({ effects: setGroup.of({ ...g, def: d }), annotations: Transaction.addToHistory.of(false) }); }}
            preview={(i) => { const g = groupAt(view.state, panel!.group.from); if (g) flipTo(view, g, i); }}
            add={(t) => addVariants(view, panel!.group.id, [{ text: t, by: 'owner' }])}
            remove={(i) => removeVariant(view, panel!.group.id, i)}
            removeAll={(d) => { removeAll(view, panel!.group.id, d); panel = null; view.focus(); }}
            ai={() => askAI(panel!.group)}
            close={closePanel}
          />
          {/key}
        {:else}
          <p class="dim">Select a word, sentence or paragraph: <kbd>Alt</kbd>+<kbd>↵</kbd> to write other ways to say it, <kbd>Alt</kbd>+<kbd>G</kbd> for 🤖 ones. In text with alternatives, <kbd>Alt</kbd>+<kbd>↑↓</kbd> flips them and <kbd>Alt</kbd>+<kbd>Del</kbd> removes the one showing.</p>
        {/if}
      {:else if left === 'check'}
        <CheckPanel family={pass} title={pass === 'marks' ? 'Marks' : 'Check'} fixes={fixList.filter((f) => familyOf(f.kind) === pass)} current={fixHere} busy={busy === pass} error={busy ? '' : checkError}
          go={(id) => { const f = fixById(id); if (f) selectFix(view, f); }}
          accept={(id) => onFix(id, take)}
          asAlternative={(id) => onFix(id, (f) => acceptAsAlternative(view, f))}
          reject={(id) => onFix(id, rejectFix)}
          again={() => ask(pass)} clear={() => clearCheck(pass)}>
          <div class="pass">{#each [['check', 'Fixes'], ['marks', 'Marks']] as const as [p, label]}<button class:on={p === pass} onclick={() => (pass = p)}>{label}</button>{/each}<span class="dim">Alt+P</span></div>
        </CheckPanel>
      {:else}
        {@const cuts = fixList.filter((f) => f.kind === 'cut')}
        <div class="pass shrink-goal">
          🤖 Shrink {#each LEVELS as l}<button class:on={l === level} onclick={() => (cuts.length || level ? setShrink(l) : ask('shrink'))}>{l}%</button>{/each}
          {#if shrinkFrom && shrinkFrom !== words}<span class="dim">{shrinkFrom} → {words} words, −{Math.round(((shrinkFrom - words) / shrinkFrom) * 100)}%</span>{/if}
        </div>
        {#if busy === 'shrink' || cuts.length}
          <CheckPanel family="shrink" title="Cuts" fixes={cuts} current={fixHere} busy={busy === 'shrink'} error=""
            go={(id) => { const f = fixById(id); if (f) selectFix(view, f); }}
            accept={(id) => onFix(id, take)} asAlternative={() => {}} reject={(id) => onFix(id, rejectFix)}
            again={() => ask('shrink')} clear={() => (clearCheck('shrink'), setShrink(0))}>
            {#if cuts.length > 1}<button class="all" onclick={() => { for (const c of [...cuts].reverse()) { const f = fixById(c.id); if (f) take(f); } }}>Cut all {cuts.length}</button>{/if}
          </CheckPanel>
        {:else if checkError && level}
          <p class="err">{checkError}</p>
        {/if}
        <div class="panel-head">Ghosts</div>
        {#if ghostList.length}
          <ol class="ghostlist">
            {#each ghostList as g (g.id)}
              <li class:sel={g.id === ghostAt} class:zb={g.by === 'zenbot'}><button onclick={() => selectGhost(view, g)}><span class="text">{g.text.trim()}</span>{#if g.by === 'zenbot'}<span class="who" title="zenbot's suggestion">zb</span>{/if}</button></li>
            {/each}
          </ol>
        {:else}
          <p class="dim">Nothing ghosted. Select text and <kbd>Alt</kbd>+<kbd>G</kbd> to fade it out without losing it, or <kbd>Alt</kbd>+<kbd>P</kbd> for 🤖 cuts.</p>
        {/if}
      {/if}
    </aside>
  {/if}

  <div class="sheet" bind:this={host}></div>


  {#if tools && right}
    <Side tab={right} tabs={RIGHT} pickTab={openRight} {vsel} ask={closeAsk} {changelog} {notes} edit={editNotes} suggest={suggestLog} find={{ busy: finding, error: findError, proposals, picked }} {findNotes} keep={keepProposal} drop={dropProposal} pick={(i) => { picked = i; view.focus(); }} {frontmatter} {setFrontmatter} {vs} {closeVersion} {viewVersion} />
  {/if}
  {#if !tools && notes.trim()}
    <section class="read-notes">
      <div class="panel-head">Notes</div>
      <div class="read-notes-text">{notes}</div>
    </section>
  {/if}
  {#if !tools && changelogEntries(changelog).length}
    <section class="read-log">
      <div class="panel-head">Changelog</div>
      <ul>
        {#each changelogEntries(changelog) as e}<li>{e}</li>{/each}
      </ul>
    </section>
  {/if}

  {#if opening}
    <Opener from={path} open={(p) => leaveTo(() => ((opening = false), open(p)))} create={(d) => leaveTo(() => (location.href = `/?new=${encodeURIComponent(d)}`))} close={() => { opening = false; view.focus(); }} />
  {/if}

  {#if tools || hint}
    <footer>
      {#if hint}<span class="flash">{hint}</span>{:else}
        {#each tab ? KEYS[tab] : [] as [k, what]}<span><kbd>Alt</kbd>+<kbd>{k}</kbd> {what}</span>{/each}
        <span class="global"><kbd>Alt</kbd>+<kbd>L</kbd> tools · <kbd>Alt</kbd>+<kbd>R</kbd> note · <kbd>Alt</kbd>+<kbd>W</kbd> read · <kbd>Alt</kbd>+<kbd>O</kbd> open</span>
      {/if}
    </footer>
  {/if}
</div>
