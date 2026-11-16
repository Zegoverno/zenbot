// The file viewer's text work, apart from the component: front matter, the changelog split, words
// for play and reading time, the blocks focus lights and the visual lines inside one. As the
// terminal app's viewer (crates/zen/src/tui/panel.rs) does it, adapted to the DOM.

/** Silent reading speed for non-fiction, words a minute (Brysbaert 2019), for the minutes left. */
export const WPM = 238;
/** Prose widths `<` and `>` go through, in characters; 0 is the whole pane. */
export const MEASURES = [60, 72, 90, 0] as const;
/** The dim levels, light to strong, and how much of the ink stays at each. */
export const DIMS = [100, 300, 500, 700, 900] as const;
export const keep = (dim: number) => ({ 100: 0.75, 300: 0.55, 500: 0.4, 700: 0.25, 900: 0.12 })[dim] ?? 0.55;

/** A file shown as prose (Markdown or plain text), else monospace. */
export const isProse = (path: string) => /\.(md|markdown|txt)$/i.test(path);
export const isMarkdown = (path: string) => /\.(md|markdown)$/i.test(path);

/** Front matter lines as properties, as the TUI reads them: `[a, b]` and `- a` lists as `a · b`,
 *  quotes off, empty ones left out. */
export function properties(front: string): [string, string][] {
  const unquote = (v: string) => v.trim().replace(/^["']|["']$/g, '');
  const props: [string, string[]][] = [];
  for (const l of front.split('\n')) {
    const item = /^[ -]/.test(l) ? /^\s*- (.*)$/.exec(l) : null;
    if (item) {
      props.at(-1)?.[1].push(unquote(item[1]));
      continue;
    }
    const i = l.indexOf(':');
    if (i <= 0 || l.startsWith(' ')) continue;
    const v = l.slice(i + 1).trim();
    let vs: string[];
    if (v.startsWith('[') && v.endsWith(']') && !v.startsWith('[[')) vs = v.slice(1, -1).split(',').map(unquote).filter(Boolean);
    else vs = v ? [unquote(v)] : [];
    props.push([l.slice(0, i).trim(), vs]);
  }
  return props.filter(([, vs]) => vs.length).map(([k, vs]) => [k, vs.join(' · ')]);
}

/** Takes the changelog (a top-level heading naming it, any level, and all after it) out of `md`
 *  into a new element; null when there is none. */
export function takeChangelog(md: HTMLElement): HTMLElement | null {
  const kids = [...md.children];
  const at = kids.findIndex((e) => /^H[1-6]$/.test(e.tagName) && /\bchangelog\b/i.test(e.textContent ?? ''));
  if (at < 0) return null;
  const log = document.createElement('div');
  log.className = 'md';
  for (const e of kids.slice(at + 1)) log.append(e);
  kids[at].remove();
  return log;
}

const SKIP = 'code, pre, .md-math, .katex';

/** Wraps each word (anything with a letter or digit) of the text under `root` in `span.w`, for
 *  play and the reading time; math is left alone. With `code`, words in code too (plain files). */
export function wrapWords(root: HTMLElement, code = false): HTMLElement[] {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => (n.parentElement?.closest(code ? '.md-math, .katex' : SKIP) ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
  });
  const nodes: Text[] = [];
  while (walker.nextNode()) nodes.push(walker.currentNode as Text);
  for (const t of nodes) {
    const parts = t.data.split(/(\S+)/);
    if (parts.length < 2) continue;
    const frag = document.createDocumentFragment();
    parts.forEach((p, i) => {
      if (!p) return;
      if (i % 2 === 0 || !/[\p{L}\p{N}]/u.test(p)) return void frag.append(p);
      const s = document.createElement('span');
      s.className = 'w';
      s.textContent = p;
      frag.append(s);
    });
    t.replaceWith(frag);
  }
  return [...root.querySelectorAll<HTMLElement>('span.w')];
}

const LEAF = 'p, h1, h2, h3, h4, h5, h6, pre, table, .md-html, .md-callout-title';

/** The blocks focus lights: paragraphs (with any math in them), headings, code, tables, display
 *  math on its own, list items with no blocks inside. Outermost of each, in order. */
export function units(root: HTMLElement): HTMLElement[] {
  const all = [...root.querySelectorAll<HTMLElement>(`${LEAF}, .md-display, li`)];
  return all.filter((e) => {
    if ((e.textContent ?? '').trim() === '' && !e.querySelector('.katex, img')) return false;
    if (e.parentElement?.closest(LEAF)) return false;
    if (e.tagName === 'LI') return !e.querySelector(`${LEAF}, ul, ol, .md-display`);
    return true;
  });
}

/** A block's visual lines, `[top, bottom]` in px from the block's top, from the boxes of its text. */
export function lines(unit: HTMLElement): [number, number][] {
  const r = document.createRange();
  r.selectNodeContents(unit);
  const base = unit.getBoundingClientRect().top;
  const rects = [...r.getClientRects()].filter((b) => b.width > 0 && b.height > 0).sort((a, b) => a.top - b.top);
  const out: [number, number][] = [];
  for (const b of rects) {
    const mid = (b.top + b.bottom) / 2 - base;
    const last = out.at(-1);
    if (last && mid >= last[0] && mid <= last[1]) {
      last[0] = Math.min(last[0], b.top - base);
      last[1] = Math.max(last[1], b.bottom - base);
    } else if (last && b.top - base < last[1] - 2) {
      // a box mostly inside the line before (math, a bold run): the same line
      last[1] = Math.max(last[1], b.bottom - base);
    } else out.push([b.top - base, b.bottom - base]);
  }
  // Lines touch: each runs to the next one's top so the band covers the leading.
  for (let i = 0; i + 1 < out.length; i++) out[i][1] = Math.max(out[i][1], out[i + 1][0]);
  return out;
}

/** The letter a word is recognized by (Spritz's optimal recognition point). */
export function orp(word: string): number {
  const n = [...word].length;
  return n <= 1 ? 0 : n <= 5 ? 1 : n <= 9 ? 2 : n <= 13 ? 3 : 4;
}

/** How long a word stays, in ms: with pauses, longer at the end of a sentence or clause and for
 *  long words, as Spritz does. */
export function dwell(word: string, wpm: number, pauses: boolean): number {
  const base = 60_000 / Math.max(1, wpm);
  if (!pauses) return base;
  let f = /[.!?:]$/.test(word) ? 2.2 : /[,;)]$/.test(word) ? 1.5 : 1;
  if ([...word].length > 8) f += 0.3;
  return base * f;
}
