// Markdown as it reads, the way Obsidian's live preview does it: the marks (`#`, `**`, `>`, `- `,
// a link's `](url)`, `$` around math) are hidden and what they mean is drawn instead. A line's own
// marks (`#`, `>`, `- `) show on the lines the caret or selection is on; inline marks (`**`, `*`,
// a link's, math's) only on the piece the caret touches. Reading hides them everywhere.
//
// Links work: a click on a drawn link opens it (a web link in a new tab, a `[[wiki link]]` or a
// link to a .md file as a note in the writer). On a link showing its source, a click only places the
// caret; Ctrl/Cmd+click opens from there too.

import { StateField, type EditorState, type Range } from '@codemirror/state';
import { Decoration, EditorView, ViewPlugin, WidgetType, type DecorationSet, type ViewUpdate } from '@codemirror/view';
import { syntaxTree } from '@codemirror/language';
import type { SyntaxNode } from '@lezer/common';

/** What a click on a link asks for: a web address, or a note by name or path. */
export type Target = { url: string } | { note: string };

class Bullet extends WidgetType {
  eq() {
    return true;
  }
  toDOM() {
    const el = document.createElement('span');
    el.className = 'zw-bullet';
    el.textContent = '•';
    return el;
  }
}

class Check extends WidgetType {
  readonly done: boolean;
  readonly at: number;
  constructor(done: boolean, at: number) {
    super();
    this.done = done;
    this.at = at;
  }
  eq(o: Check) {
    return o.done === this.done && o.at === this.at;
  }
  toDOM(view: EditorView) {
    const el = document.createElement('input');
    el.type = 'checkbox';
    el.className = 'zw-task';
    el.checked = this.done;
    el.addEventListener('mousedown', (e) => {
      e.preventDefault();
      // Ticking is writing: only with the editor on.
      if (view.state.readOnly) return;
      view.dispatch({ changes: { from: this.at + 1, to: this.at + 2, insert: this.done ? ' ' : 'x' }, userEvent: 'input.task' });
    });
    return el;
  }
  ignoreEvent() {
    return false;
  }
}

// KaTeX is loaded only once a note has math, so a page without any stays small.
let katex: typeof import('katex').default | null = null;
let loading: Promise<unknown> | null = null;

/** KaTeX can't set accented letters in math (`Orçamento`): such a word goes in as text. */
function accented(tex: string) {
  return tex.replace(/(\\text\{[^}]*\})|([A-Za-z\u00C0-\u024F]*[\u00C0-\u024F][A-Za-z\u00C0-\u024F]*)/g, (m, kept) => kept ?? `\\text{${m}}`);
}

class Tex extends WidgetType {
  readonly tex: string;
  readonly display: boolean;
  constructor(tex: string, display: boolean) {
    super();
    this.tex = tex;
    this.display = display;
  }
  eq(o: Tex) {
    return o.tex === this.tex && o.display === this.display;
  }
  toDOM(view: EditorView) {
    const el = document.createElement('span');
    el.className = this.display ? 'zw-math zw-math-block' : 'zw-math';
    const draw = () => {
      if (!katex) return;
      katex.render(accented(this.tex), el, { displayMode: this.display, throwOnError: false });
      // A formula wider than the column shrinks to fit, to 60% at most; past that it scrolls.
      // Measured again once KaTeX's fonts are in, which changes the width.
      if (this.display) {
        let size = 1;
        const fit = () =>
          view.requestMeasure({
            read: () => (el.scrollWidth > el.clientWidth + 1 ? el.clientWidth / el.scrollWidth : 1),
            write: (k) => {
              if (k >= 1) return;
              size = Math.max(0.6, size * k * 0.98);
              el.style.fontSize = `${size}em`;
            },
          });
        fit();
        document.fonts.ready.then(fit);
      } else view.requestMeasure();
    };
    if (katex) draw();
    else {
      el.textContent = this.tex;
      loading ??= Promise.all([import('katex'), import('katex/dist/katex.min.css')]).then(([k]) => (katex = k.default));
      loading.then(draw);
    }
    return el;
  }
}

class Rule extends WidgetType {
  eq() {
    return true;
  }
  toDOM() {
    const el = document.createElement('span');
    el.className = 'zw-hr';
    return el;
  }
}

// Hidden by style, not replaced: a replaced range lets the line break where it stood
// ("Swan" | "):"), a hidden span doesn't.
const hide = Decoration.mark({ class: 'zw-hide' });
const bullet = Decoration.replace({ widget: new Bullet() });
const WIKI = /\[\[([^\]|#\n]+)(#[^\]|\n]*)?(?:\|([^\]\n]+))?\]\]/g;
const MATH = /\$\$([^$\n]+?)\$\$|\$([^\s$](?:[^$\n]*?[^\s$\\])?)\$(?!\d)/g;
const IN_CODE = new Set(['InlineCode', 'FencedCode', 'CodeBlock', 'CodeText']);

/** The lines a selection touches: there the source shows. None while reading. */
function activeLines(state: EditorState): Set<number> {
  const lines = new Set<number>();
  if (state.readOnly) return lines;
  for (const r of state.selection.ranges) {
    for (let n = state.doc.lineAt(r.from).number; n <= state.doc.lineAt(r.to).number; n++) lines.add(n);
  }
  return lines;
}

/** Whether the caret or selection touches from..to: there inline marks show. Never while reading. */
function touches(state: EditorState, from: number, to: number) {
  return !state.readOnly && state.selection.ranges.some((r) => r.from <= to && r.to >= from);
}

function isCode(state: EditorState, pos: number) {
  for (let node: SyntaxNode | null = syntaxTree(state).resolveInner(pos, 1); node; node = node.parent) if (IN_CODE.has(node.name)) return true;
  return false;
}

/** `$$…$$` running over several lines, outside code. */
function mathBlocks(state: EditorState) {
  const found: { from: number; to: number; tex: string }[] = [];
  for (const m of state.doc.toString().matchAll(/\$\$([^$]+?)\$\$/g)) {
    if (m[1].includes('\n') && !isCode(state, m.index)) found.push({ from: m.index, to: m.index + m[0].length, tex: m[1] });
  }
  return found;
}

// A block that runs over lines is replaced across its line breaks, which only a state field may
// do (a view plugin may not), so these have their own.
const blockMath = StateField.define<DecorationSet>({
  create: (state) => drawBlocks(state),
  update: (deco, tr) =>
    tr.docChanged || tr.selection || tr.startState.readOnly !== tr.state.readOnly || syntaxTree(tr.startState) !== syntaxTree(tr.state)
      ? drawBlocks(tr.state)
      : deco,
  provide: (f) => EditorView.decorations.from(f),
});

/**
 * Display math one formula per row, so a page column holds it: the block's own line breaks (which
 * TeX ignores) and the `\qquad` that sets formulas side by side, outside braces, become breaks.
 */
function stacked(tex: string) {
  if (/\\\\|\\begin\{/.test(tex)) return tex;
  const rows: string[] = [];
  let depth = 0, row = '';
  for (let i = 0; i < tex.length; i++) {
    const c = tex[i];
    if (c === '{') depth++;
    else if (c === '}') depth--;
    const gap = depth === 0 && (c === '\n' || (tex.startsWith('\\qquad', i) && !/[a-zA-Z]/.test(tex[i + 6] ?? '')));
    if (!gap) row += c;
    else {
      rows.push(row);
      row = '';
      if (c !== '\n') i += 5;
    }
  }
  rows.push(row);
  const lines = rows.map((l) => l.trim()).filter(Boolean);
  return lines.length > 1 ? `\\begin{gathered}${lines.join('\\\\')}\\end{gathered}` : tex;
}

function drawBlocks(state: EditorState): DecorationSet {
  const out = mathBlocks(state)
    .filter((k) => !touches(state, k.from, k.to))
    .map((k) => Decoration.replace({ widget: new Tex(stacked(k.tex), true) }).range(k.from, k.to));
  return Decoration.set(out);
}

function build(view: EditorView): DecorationSet {
  const { state } = view;
  const active = activeLines(state);
  const isActive = (pos: number) => active.has(state.doc.lineAt(pos).number);
  const out: Range<Decoration>[] = [];
  const mark = (from: number, to: number, cls: string, attrs?: Record<string, string>) =>
    to > from && out.push(Decoration.mark({ class: cls, attributes: attrs }).range(from, to));
  const hideIf = (from: number, to: number) => to > from && !isActive(from) && out.push(hide.range(from, to));
  // Inline marks show only while the caret or selection touches their piece.
  const touched = (from: number, to: number) => touches(state, from, to);
  const blocks = mathBlocks(state);
  const hideOff = (from: number, to: number, piece: { from: number; to: number }) =>
    to > from && !touched(piece.from, piece.to) && out.push(hide.range(from, to));

  const inCode = (pos: number) => isCode(state, pos);

  for (const { from, to } of view.visibleRanges) {
    syntaxTree(state).iterate({
      from,
      to,
      enter: (n) => {
        const name = n.name;
        if (name === 'HeaderMark') {
          // `# ` before a heading's text, the space too; a closing `#` run as well.
          const after = state.sliceDoc(n.to, n.to + 1) === ' ' ? 1 : 0;
          hideIf(n.from, n.to + after);
        } else if (name === 'EmphasisMark' || name === 'StrikethroughMark' || (name === 'CodeMark' && n.node.parent?.name === 'InlineCode')) {
          hideOff(n.from, n.to, n.node.parent!);
        } else if (name === 'Escape') {
          // `\$` reads as `$`.
          hideOff(n.from, n.from + 1, n);
        } else if ((name === 'CodeMark' || name === 'CodeInfo') && n.node.parent?.name === 'FencedCode') {
          // A code block's fences and language, off the lines being edited.
          hideIf(n.from, n.to);
        } else if (name === 'Strikethrough') {
          mark(n.from, n.to, 'zw-strike');
        } else if (name === 'QuoteMark') {
          out.push(Decoration.line({ class: 'zw-quote-line' }).range(state.doc.lineAt(n.from).from));
          hideIf(n.from, n.to + (state.sliceDoc(n.to, n.to + 1) === ' ' ? 1 : 0));
        } else if (name === 'ListMark') {
          const text = state.sliceDoc(n.from, n.to);
          const line = state.doc.lineAt(n.from);
          if (!isActive(n.from) && line.from === state.doc.lineAt(n.node.parent!.from).from) {
            // Wrapped lines hang under the item's text, the nesting drawn by padding.
            let depth = 0;
            for (let p = n.node.parent; p; p = p.parent) if (p.name === 'BulletList' || p.name === 'OrderedList') depth++;
            out.push(Decoration.line({ class: 'zw-li', attributes: { style: `--depth: ${depth}` } }).range(line.from));
            if (n.from > line.from) out.push(hide.range(line.from, n.from));
          }
          if (/^[-*+]$/.test(text) && !isActive(n.from)) {
            const task = n.node.nextSibling?.name === 'Task';
            // A task's own box stands in for the bullet.
            out.push((task ? hide : bullet).range(n.from, n.to + (state.sliceDoc(n.to, n.to + 1) === ' ' ? 1 : 0)));
          }
        } else if (name === 'TaskMarker') {
          if (!isActive(n.from)) {
            const done = /x/i.test(state.sliceDoc(n.from, n.to));
            out.push(Decoration.replace({ widget: new Check(done, n.from) }).range(n.from, n.to));
          }
        } else if (name === 'HorizontalRule') {
          if (!isActive(n.from)) out.push(Decoration.replace({ widget: new Rule() }).range(n.from, n.to));
        } else if (name === 'FencedCode') {
          for (let pos = n.from; pos <= n.to; ) {
            const line = state.doc.lineAt(pos);
            out.push(Decoration.line({ class: 'zw-codeblock' }).range(line.from));
            pos = line.to + 1;
          }
        } else if (name === 'Link') {
          const marks = n.node.getChildren('LinkMark');
          const url = n.node.getChild('URL');
          // Only inline links ([text](url)); `[x]` alone is text (and inside a [[wiki link]]).
          if (marks.length < 2 || !url) return;
          const textFrom = marks[0].to, textTo = marks[1].from;
          const href = state.sliceDoc(url.from, url.to);
          mark(textFrom, textTo, 'zw-link', { 'data-href': href });
          hideOff(marks[0].from, marks[0].to, n);
          hideOff(marks[1].from, n.to, n);
          return false;
        } else if (name === 'URL' && n.node.parent?.name !== 'Link' && n.node.parent?.name !== 'Image') {
          // A bare address (GFM autolink) or one in <…>.
          mark(n.from, n.to, 'zw-link', { 'data-href': state.sliceDoc(n.from, n.to) });
        }
      },
    });
    // Wiki links (Obsidian's, not CommonMark's) and math are found by hand, outside code.
    const text = state.sliceDoc(from, to);
    // Math, $…$ inline and $$…$$ on one line, drawn by KaTeX. `$5 and $10` is not math: the
    // inside may not start or end with a space, nor the closing `$` come before a digit.
    for (const m of text.matchAll(MATH)) {
      const a = from + m.index, b = a + m[0].length;
      if (inCode(a) || touched(a, b) || blocks.some((k) => k.from < b && k.to > a) || /\\$/.test(state.sliceDoc(a - 1, a))) continue;
      out.push(Decoration.replace({ widget: new Tex(m[1] !== undefined ? stacked(m[1]) : m[2], m[1] !== undefined) }).range(a, b));
    }
    for (const m of text.matchAll(WIKI)) {
      const a = from + m.index, b = a + m[0].length;
      if (inCode(a)) continue;
      const target = m[1].trim();
      if (touched(a, b)) {
        mark(a + 2, a + 2 + m[1].length, 'zw-link zw-wiki', { 'data-note': target });
        continue;
      }
      // [[Target|alias]] shows the alias; [[Target#Heading]] the target.
      const shown = m[3] !== undefined ? b - 2 - m[3].length : a + 2;
      const shownTo = m[3] !== undefined ? b - 2 : a + 2 + m[1].length;
      out.push(hide.range(a, shown));
      mark(shown, shownTo, 'zw-link zw-wiki', { 'data-note': target });
      out.push(hide.range(shownTo, b));
    }
  }
  return Decoration.set(out, true);
}

/** Where a link element points, or null. */
function targetOf(el: HTMLElement | null): Target | null {
  const a = el?.closest<HTMLElement>('[data-href], [data-note]');
  if (!a) return null;
  const note = a.dataset.note;
  if (note) return { note };
  const href = a.dataset.href ?? '';
  if (/^(https?:|mailto:)/i.test(href)) return { url: href };
  if (/^www\./i.test(href)) return { url: `https://${href}` };
  // A relative link to a note, Obsidian style: [text](Other%20note.md).
  if (href && !/^[a-z]+:/i.test(href)) return { note: decodeURIComponent(href.replace(/#.*$/, '')) };
  return null;
}

export function preview(open: (t: Target) => void) {
  return [
    blockMath,
    ViewPlugin.fromClass(
      class {
        decorations: DecorationSet;
        constructor(view: EditorView) {
          this.decorations = build(view);
        }
        update(u: ViewUpdate) {
          const readOnly = u.startState.readOnly !== u.state.readOnly;
          if (u.docChanged || u.viewportChanged || u.selectionSet || readOnly || syntaxTree(u.startState) !== syntaxTree(u.state)) {
            this.decorations = build(u.view);
          }
        }
      },
      { decorations: (p) => p.decorations },
    ),
    EditorView.domEventHandlers({
      mousedown(e, view) {
        if (e.button !== 0) return false;
        const t = targetOf(e.target as HTMLElement);
        if (!t) return false;
        const pos = view.posAtDOM(e.target as Node);
        // On a link showing its source a plain click edits; a drawn link is followed.
        const len = (e.target as HTMLElement).textContent?.length ?? 0;
        const shown = !view.state.readOnly && view.state.selection.ranges.some((r) => r.from <= pos + len && r.to >= pos);
        if (shown && !(e.ctrlKey || e.metaKey)) return false;
        e.preventDefault();
        open(t);
        return true;
      },
    }),
  ];
}

// Exported for the tests.
export { stacked, accented };
