import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { browserOwned, chordOf, fuzzy, keyLabel, validKey, RESERVED } from './keys.ts';
import { clashed, dispatch, findClash, focused, heldAsAlt, install, keyOf, rebind, register as put, resetKey, ringItems, setPanes, ui, unbind, unregister, type Cmd, type Scope } from './keys.svelte.ts';

const ev = (key: string, o: Partial<{ code: string; ctrlKey: boolean; metaKey: boolean; altKey: boolean; shiftKey: boolean }> = {}) => ({ key, ...o });

describe('the key form', () => {
  it('writes letters in lower case with Shift and Mod in order', () => {
    expect(chordOf(ev('n'))).toBe('n');
    expect(chordOf(ev('N', { shiftKey: true }))).toBe('Shift+n');
    expect(chordOf(ev('Enter', { ctrlKey: true }))).toBe('Mod+Enter');
    expect(chordOf(ev('Enter', { metaKey: true }))).toBe('Mod+Enter');
    expect(chordOf(ev('ArrowUp', { altKey: true }))).toBe('Alt+ArrowUp');
    expect(chordOf(ev('s', { ctrlKey: true, altKey: true, shiftKey: true }))).toBe('Mod+Alt+Shift+s');
    expect(chordOf(ev(' '))).toBe('Space');
  });
  it('writes a symbol as it is, Shift only beside Mod or Alt', () => {
    expect(chordOf(ev('?', { shiftKey: true }))).toBe('?');
    expect(chordOf(ev('@', { shiftKey: true }))).toBe('@');
    expect(chordOf(ev('.', { ctrlKey: true }))).toBe('Mod+.');
    expect(chordOf(ev('\\', { ctrlKey: true }))).toBe('Mod+\\');
    expect(chordOf(ev('|', { ctrlKey: true, shiftKey: true, code: 'Backslash' }))).toBe('Mod+Shift+\\');
  });
  it('reads Alt+letter from the physical key (a Mac types other letters)', () => {
    expect(chordOf(ev('å', { altKey: true, code: 'KeyA' }))).toBe('Alt+a');
  });
  it('reads the ABNT2 keys beside the Shifts as their symbols', () => {
    expect(chordOf(ev('|', { ctrlKey: true, shiftKey: true, code: 'IntlBackslash' }))).toBe('Mod+Shift+\\');
    expect(chordOf(ev('?', { ctrlKey: true, shiftKey: true, code: 'IntlRo' }))).toBe('Mod+Shift+/');
  });
  it('knows the chords the browser keeps', () => {
    for (const k of ['Mod+1', 'Mod+9', 'Mod+t', 'Mod+w', 'Mod+n', 'Mod+l', 'Mod+f', 'Mod+p', 'Mod+r', 'Mod+Tab', 'Mod+Shift+t', 'Mod+Shift+n', 'Mod+Shift+w']) expect(browserOwned(k), k).toBe(true);
    for (const k of ['Mod+s', 'Mod+Shift+s', 'n', '1', 'Mod+Enter', 'Alt+1']) expect(browserOwned(k), k).toBe(false);
  });
  it('ignores a modifier on its own', () => {
    for (const k of ['Control', 'Shift', 'Alt', 'Meta', 'Dead']) expect(chordOf(ev(k))).toBeNull();
  });
  it('accepts exactly the written form', () => {
    for (const k of ['n', 'Shift+n', 'Mod+Enter', 'Alt+ArrowUp', 'Mod+Shift+\\', '?', '.', 'Space', 'Mod+.', 'F5', '1']) expect(validKey(k), k).toBe(true);
    for (const k of ['N', 'Ctrl+n', 'Shift+Mod+n', 'Shift+?', 'Mod+', '', 'nn', 'Enter+Mod', 'mod+k', 'Mod+Shift+N', 'é', 'x'.repeat(30), 5, null, {}]) expect(validKey(k), String(k)).toBe(false);
  });
  it('knows every key it keeps for itself', () => {
    for (const k of RESERVED) expect(validKey(k), k).toBe(true);
    expect(RESERVED).toEqual(['Mod+k', 'Mod+.', 'Mod+\\', 'Mod+Shift+\\', '?', 'Escape', 'g']);
  });
  it('labels a key as the owner reads it', () => {
    expect(keyLabel('Mod+Shift+n')).toMatch(/^(Ctrl|⌘)\+Shift\+N$/);
    expect(keyLabel('ArrowLeft')).toBe('←');
    expect(keyLabel('n')).toBe('N');
    expect(keyLabel(undefined)).toBe('');
  });
});

describe('fuzzy search', () => {
  it('needs the letters in order, and likes word starts and runs', () => {
    expect(fuzzy('xyz', 'New chat')).toBeNull();
    expect(fuzzy('nc', 'New chat')).not.toBeNull();
    expect(fuzzy('chat', 'New chat')!).toBeGreaterThan(fuzzy('chat', 'Check the old attachments')!);
    expect(fuzzy('', 'anything')).toBe(0);
  });
});

/** What a test registered, dropped after it. */
const handles: number[] = [];
const register = (scope: Scope, list: Cmd[], _n?: number) => { const n = put(scope, list); handles.push(n); return n; };
const cmd = (id: string, o: Partial<Cmd> = {}): Cmd => ({ id, title: id, icon: 'plus', run: () => {}, ...o });

describe('the registry and the key handler', () => {
  beforeEach(() => {
    ui.ring = ui.palette = ui.help = ui.goto = ui.agentMenu = false;
    setPanes(['chat', 'tasks']);
    ui.focus = 1;
  });

  afterEach(() => { handles.splice(0).forEach(unregister); });

  it('runs the focused panel\'s command, never another panel\'s', () => {
    const tasks = vi.fn(), chat = vi.fn();
    register('tasks', [cmd('t-n', { key: 'n', run: tasks })]);
    register('chat', [cmd('c-n', { key: 'n', run: chat })]);
    expect(focused()).toBe('tasks');
    expect(dispatch(ev('n'))).toBe(true);
    expect([tasks.mock.calls.length, chat.mock.calls.length]).toEqual([1, 0]);
    ui.focus = 0;
    dispatch(ev('n'));
    expect([tasks.mock.calls.length, chat.mock.calls.length]).toEqual([1, 1]);
  });

  it('fires single keys only when not typing, and Mod chords always', () => {
    const plain = vi.fn(), chord = vi.fn();
    register('tasks', [cmd('p', { key: 'e', run: plain }), cmd('m', { key: 'Mod+s', run: chord })], 901);
    expect(dispatch({ key: 'e', typing: true })).toBe(false);
    expect(dispatch({ key: 's', ctrlKey: true, typing: true })).toBe(true);
    expect(dispatch({ key: 'e' })).toBe(true);
    expect([plain.mock.calls.length, chord.mock.calls.length]).toEqual([1, 1]);
  });

  it('leaves Enter to a focused button', () => {
    const open = vi.fn();
    register('tasks', [cmd('o', { key: 'Enter', run: open })], 902);
    expect(dispatch({ key: 'Enter', interactive: true })).toBe(false);
    expect(dispatch({ key: 'Enter' })).toBe(true);
    expect(open).toHaveBeenCalledTimes(1);
  });

  it('leaves Space to a focused button or checkbox', () => {
    const run = vi.fn();
    register('tasks', [cmd('sp', { key: 'Space', run })], 909);
    expect(dispatch({ key: ' ', interactive: true })).toBe(false);
    expect(dispatch({ key: ' ' })).toBe(true);
    expect(run).toHaveBeenCalledTimes(1);
  });

  it('skips a disabled command and takes the next holder of the key', () => {
    const second = vi.fn();
    register('tasks', [cmd('x1', { key: 'x', enabled: () => false }), cmd('x2', { key: 'x', run: second })], 903);
    dispatch(ev('x'));
    expect(second).toHaveBeenCalledTimes(1);
  });

  it('opens the ring, runs an action by number or letter, closes on Esc', () => {
    const run = vi.fn();
    register('tasks', [cmd('r1', { key: 'n', ring: 0, run }), cmd('r2', { key: 'z', ring: 1, run: () => {} })], 904);
    dispatch({ key: '.', ctrlKey: true });
    expect(ui.ring).toBe(true);
    expect(ringItems().map((i) => i.id)).toEqual(['r1', 'r2', 'focus']);
    expect(dispatch(ev('Escape'))).toBe(true);
    expect(ui.ring).toBe(false);
    dispatch({ key: '.', ctrlKey: true });
    dispatch(ev('1'));
    expect(run).toHaveBeenCalledTimes(1);
    expect(ui.ring).toBe(false);
    dispatch({ key: '.', ctrlKey: true });
    dispatch(ev('n'));
    expect(run).toHaveBeenCalledTimes(2);
    dispatch({ key: '.', ctrlKey: true });
    dispatch(ev('ArrowDown'));
    expect(ui.ringAt).toBe(1);
    dispatch({ key: '.', ctrlKey: true });
  });

  it('gives the ring the other panel\'s icon as its last slot, only when split', () => {
    register('tasks', [cmd('r1', { ring: 0 })], 905);
    expect(ringItems().at(-1)).toMatchObject({ id: 'focus', icon: 'chat' });
    setPanes(['tasks']);
    expect(ringItems().map((i) => i.id)).not.toContain('focus');
  });

  it('switches focus with Ctrl+\\ and goes to a view with g then a letter', () => {
    dispatch({ key: '\\', ctrlKey: true });
    expect(ui.focus).toBe(0);
    dispatch({ key: '\\', ctrlKey: true });
    expect(ui.focus).toBe(1);
    expect(dispatch(ev('g'))).toBe(true);
    expect(ui.goto).toBe(true);
    expect(dispatch(ev('z'))).toBe(true);
    expect(ui.goto).toBe(false);
  });

  it('lets a chord through during the go-to window instead of swallowing it', () => {
    const run = vi.fn();
    register('tasks', [cmd('fnd', { key: 'Mod+s', run }), cmd('alt1', { key: 'Alt+x', run })], 910);
    dispatch(ev('g'));
    expect(ui.goto).toBe(true);
    expect(dispatch({ key: 's', ctrlKey: true })).toBe(true);
    expect(ui.goto).toBe(false);
    expect(run).toHaveBeenCalledTimes(1);
    dispatch(ev('g'));
    // Not ours at all: the key isn't consumed (the browser keeps Ctrl+L), and the go-to is over.
    expect(dispatch({ key: 'l', ctrlKey: true })).toBe(false);
    expect(ui.goto).toBe(false);
    dispatch(ev('g'));
    expect(dispatch({ key: '1', altKey: true, code: 'Digit1' })).toBe(false);
    expect(ui.goto).toBe(false);
  });

  it('forgets the go-to when the handler is removed', () => {
    vi.stubGlobal('window', { addEventListener: () => {}, removeEventListener: () => {} });
    const off = install();
    dispatch(ev('g'));
    expect(ui.goto).toBe(true);
    off();
    vi.unstubAllGlobals();
    expect(ui.goto).toBe(false);
  });

  it('does not go round a ring with nothing in it', () => {
    ui.panes = ['chat'];
    ui.focus = 0;
    ui.ringAt = 0;
    dispatch({ key: '.', ctrlKey: true });
    expect(ui.ring).toBe(true);
    expect(dispatch(ev('ArrowDown'))).toBe(true);
    expect(dispatch(ev('Tab'))).toBe(true);
    expect(ui.ringAt).toBe(0);
    dispatch(ev('Escape'));
    expect(ui.ring).toBe(false);
  });

  it('opens the shortcuts list on ? and the command list on Ctrl+K, not while typing', () => {
    expect(dispatch({ key: '?', typing: true })).toBe(false);
    expect(dispatch({ key: '?' })).toBe(true);
    expect(ui.help).toBe(true);
    expect(dispatch(ev('Escape'))).toBe(true);
    expect(ui.help).toBe(false);
    expect(dispatch({ key: 'k', ctrlKey: true, typing: true })).toBe(true);
    expect(ui.palette).toBe(true);
    dispatch({ key: 'k', ctrlKey: true });
    expect(ui.palette).toBe(false);
  });

  it('keeps a plugin\'s clashing default key off until the owner chooses', () => {
    register('tasks', [cmd('core-n', { key: 'n' })]);
    const reg = (id: string, key: string) => ({ ...cmd(`plugin:fx/${id}`, { key, plugin: 'fx' }), scope: 'tasks' as const });
    const same = reg('same', 'n');
    expect(clashed(same)).toBe(true);
    expect(keyOf(same)).toBeUndefined();
    rebind(same, 'Shift+n');
    expect(keyOf(same)).toBe('Shift+n');
    expect(clashed(same)).toBe(false);
    resetKey(same);
    expect(keyOf(same)).toBeUndefined();
    expect(clashed(reg('free', 'y'))).toBe(false);
    // Keys zenbot keeps for itself clash too.
    for (const k of RESERVED) expect(clashed({ ...cmd('plugin:fx/r', { key: k, plugin: 'fx' }), scope: 'plugin' }), k).toBe(true);
  });

  it('never takes a key held as another command\'s extra key', () => {
    register('tasks', [cmd('walk', { key: 'j', alt: ['ArrowDown'] }), cmd('want', { key: 'w' })], 911);
    const walk = { ...cmd('walk', { key: 'j', alt: ['ArrowDown'] }), scope: 'tasks' as const };
    const want = { ...cmd('want', { key: 'w' }), scope: 'tasks' as const };
    const holder = findClash(want, 'ArrowDown');
    expect(holder?.id).toBe('walk');
    expect(heldAsAlt(holder!, 'ArrowDown')).toBe(true);
    expect(heldAsAlt(holder!, 'j')).toBe(false);
    rebind(want, 'ArrowDown', walk);
    expect(keyOf(walk)).toBe('j');
    expect(keyOf(want)).toBe('w');
    resetKey(walk);
    resetKey(want);
  });

  it('finds the command holding a key, and takes it over on request', () => {
    const run = vi.fn();
    register('tasks', [cmd('hold', { key: 'q' }), cmd('want', { key: 'w', run })], 908);
    const [hold, want] = [{ ...cmd('hold', { key: 'q' }), scope: 'tasks' as const }, { ...cmd('want', { key: 'w', run }), scope: 'tasks' as const }];
    expect(findClash(want, 'q')?.id).toBe('hold');
    expect(findClash(want, 'y')).toBeNull();
    rebind(want, 'q', hold);
    expect(keyOf(hold)).toBeUndefined();
    expect(keyOf(want)).toBe('q');
    dispatch(ev('q'));
    expect(run).toHaveBeenCalledTimes(1);
    unbind(want);
    expect(keyOf(want)).toBeUndefined();
    resetKey(want);
    resetKey(hold);
  });
});
