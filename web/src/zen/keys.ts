// Keys, as plain functions (no state; keys.svelte.ts holds the registry). The key form is the one
// PLUGINS.md ("Keys and commands") gives plugin UIs: modifiers `Mod` (Ctrl, or Cmd on a Mac), `Alt`,
// `Shift` in that order, then `KeyboardEvent.key` with letters in lower case and `Space` for the
// space bar (`n`, `Shift+n`, `Mod+Enter`, `Alt+ArrowUp`). Shift is written with letters and named
// keys; with a symbol only next to Mod or Alt (`Mod+Shift+\`), because `?` is simply `?`.
// Everything here is shared by the window's key handler and by what plugin UIs forward (frame.ts
// checks it), so a plugin can never press a key a real keyboard couldn't.

export type KeyLike = { key: string; code?: string; ctrlKey?: boolean; metaKey?: boolean; altKey?: boolean; shiftKey?: boolean };

const NAMED = new Set([
  'Enter', 'Escape', 'Tab', 'Backspace', 'Delete', 'Space', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight',
  'Home', 'End', 'PageUp', 'PageDown', 'Insert', ...Array.from({ length: 12 }, (_, i) => `F${i + 1}`),
]);
const MODIFIERS = new Set(['Control', 'Meta', 'Alt', 'Shift', 'AltGraph', 'CapsLock', 'Dead', 'Unidentified', 'Process', 'Compose']);
/** The symbol a physical key makes without Shift, for chords where `key` would be its shifted form. */
const CODE_CHAR: Record<string, string> = {
  Backslash: '\\', Period: '.', Comma: ',', Slash: '/', Semicolon: ';', Quote: "'", BracketLeft: '[', BracketRight: ']',
  Minus: '-', Equal: '=', Backquote: '`',
  // Brazilian ABNT2: the extra key left of the right Shift (/ and ?) and the one beside the left Shift (\ and |).
  IntlBackslash: '\\', IntlRo: '/',
};

const isLetter = (k: string) => k.length === 1 && k.toLowerCase() !== k.toUpperCase();

/** The key form of a key event (or a forwarded one), or null for a modifier alone or an unknown key. */
export function chordOf(e: KeyLike): string | null {
  let key = e.key;
  if (typeof key !== 'string' || !key || MODIFIERS.has(key)) return null;
  const mod = !!(e.ctrlKey || e.metaKey);
  const alt = !!e.altKey;
  const shift = !!e.shiftKey;
  const code = typeof e.code === 'string' ? e.code : '';
  if (key === ' ') key = 'Space';
  // With Alt (a Mac turns it into other letters) or with Shift and a modifier, the physical key tells.
  if (alt && /^Key[A-Z]$/.test(code)) key = code.slice(3).toLowerCase();
  else if ((mod || alt) && shift && CODE_CHAR[code]) key = CODE_CHAR[code];
  else if ((mod || alt) && /^Digit\d$/.test(code)) key = code.slice(5);
  let writeShift = false;
  if (isLetter(key)) {
    key = key.toLowerCase();
    writeShift = shift;
  } else if (NAMED.has(key)) writeShift = shift;
  else if (key.length === 1) writeShift = shift && (mod || alt) && !!CODE_CHAR[code];
  else return null;
  return `${mod ? 'Mod+' : ''}${alt ? 'Alt+' : ''}${writeShift ? 'Shift+' : ''}${key}`;
}

/** Whether a string is a key in the form above (letters lower case, modifiers in order). */
export function validKey(s: unknown): s is string {
  if (typeof s !== 'string' || s.length > 24) return false;
  const m = /^(Mod\+)?(Alt\+)?(Shift\+)?(.+)$/.exec(s);
  if (!m) return false;
  const [, mod, alt, shift, key] = m;
  if (NAMED.has(key)) return true;
  if (key.length !== 1 || !/^[\x21-\x7e]$/.test(key) || /[A-Z]/.test(key)) return false;
  if (/[a-z]/.test(key)) return true;
  // A digit or symbol: Shift only beside Mod or Alt (`?` is `?`, never `Shift+?`).
  return !shift || !!(mod || alt);
}

/** What zenbot keeps for itself: plugins asking for one of these get no shortcut until the owner chooses. */
export const RESERVED = ['Mod+k', 'Mod+.', 'Mod+\\', 'Mod+Shift+\\', '?', 'Escape', 'g'];
/** Keys the shell handles before any command: the tab digits (the go-to's `g` is in RESERVED). */
export const FIXED = [...RESERVED, '1', '2', '3', '4', '5', '6', '7', '8', '9'];

/** Chords the browser keeps for itself (tabs, address bar, find, print, reload): a page can't reliably take them. */
export function browserOwned(chord: string): boolean {
  return /^Mod\+[1-9]$/.test(chord) || /^Mod\+[twnlfpr]$/.test(chord) || chord === 'Mod+Tab' || /^Mod\+Shift\+[tnw]$/.test(chord);
}

const MAC = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform || '');
const GLYPH: Record<string, string> = { ArrowUp: '↑', ArrowDown: '↓', ArrowLeft: '←', ArrowRight: '→', Escape: 'Esc', Space: 'Space' };

/** A key as the owner reads it: `Mod+Shift+n` → `Ctrl+Shift+N` (⌘ on a Mac). */
export function keyLabel(k: string | undefined | null): string {
  if (!k) return '';
  const m = /^(Mod\+)?(Alt\+)?(Shift\+)?(.+)$/.exec(k);
  if (!m) return k;
  const [, mod, alt, shift, key] = m;
  const name = GLYPH[key] ?? (key.length === 1 ? key.toUpperCase() : key);
  return [mod && (MAC ? '⌘' : 'Ctrl'), alt && (MAC ? '⌥' : 'Alt'), shift && 'Shift', name].filter(Boolean).join('+');
}

/**
 * A fuzzy match of `query` in `text`: null when the letters don't appear in order, else a score
 * (higher is better: consecutive letters and word starts count, a shorter text wins a tie).
 */
export function fuzzy(query: string, text: string): number | null {
  const q = query.toLowerCase().replace(/\s+/g, '');
  const t = text.toLowerCase();
  if (!q) return 0;
  let score = 0;
  let at = -1;
  let run = 0;
  for (const ch of q) {
    const i = t.indexOf(ch, at + 1);
    if (i < 0) return null;
    run = i === at + 1 ? run + 1 : 0;
    score += 1 + run * 2 + (i === 0 || /[\s\-_/.:]/.test(t[i - 1]) ? 4 : 0);
    at = i;
  }
  return score - t.length * 0.01;
}
