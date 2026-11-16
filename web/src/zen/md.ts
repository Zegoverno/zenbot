// Markdown to DOM for the chat and the file viewer, built on Lezer's parser.
//
// Safe by construction: every piece of text becomes a text node and no HTML in the source is ever
// parsed as HTML (HTML shows as text, comments are hidden), so nothing a model or a file says can
// run in the page. Links never navigate by themselves except web links (http, https, mailto) in a
// new tab; the rest carry `data-href` or `data-wiki` for the caller's click handler.
//
// Beyond GFM (tables, task lists, strikethrough, autolinks): `[[wiki links]]`, `$…$` and `$$…$$`
// math (KaTeX, loaded on first use), Obsidian callouts (`> [!note] Title`), and a front matter
// block, which is taken off and handed back.

import { parser as base, GFM, type MarkdownConfig, type BlockContext, type Line, type InlineContext } from '@lezer/markdown';
import type { SyntaxNode } from '@lezer/common';

const WikiLink: MarkdownConfig = {
  defineNodes: ['WikiLink'],
  parseInline: [
    {
      name: 'WikiLink',
      before: 'Link',
      parse(cx: InlineContext, next: number, pos: number) {
        if (next !== 91 || cx.char(pos + 1) !== 91) return -1;
        const end = cx.slice(pos + 2, Math.min(cx.end, pos + 300)).indexOf(']]');
        if (end <= 0 || cx.slice(pos + 2, pos + 2 + end).includes('\n')) return -1;
        return cx.addElement(cx.elt('WikiLink', pos, pos + end + 4));
      },
    },
  ],
};

const MathExt: MarkdownConfig = {
  defineNodes: ['InlineMath', 'DisplayMath', 'MathBlock'],
  parseInline: [
    {
      name: 'InlineMath',
      before: 'Escape',
      parse(cx: InlineContext, next: number, pos: number) {
        if (next !== 36 /* $ */ || cx.char(pos - 1) === 92) return -1;
        const display = cx.char(pos + 1) === 36;
        const open = display ? 2 : 1;
        const rest = cx.slice(pos + open, cx.end);
        const close = display ? rest.indexOf('$$') : rest.search(/(?<!\\)\$/);
        if (close <= 0) return -1;
        const inner = rest.slice(0, close);
        if (!display && (/^\s|\s$/.test(inner) || /^\d/.test(rest.slice(close + 1)))) return -1; // "$5 and $6"
        return cx.addElement(cx.elt(display ? 'DisplayMath' : 'InlineMath', pos, pos + open + close + open));
      },
    },
  ],
  parseBlock: [
    {
      name: 'MathBlock',
      before: 'FencedCode',
      parse(cx: BlockContext, line: Line) {
        const text = line.text.slice(line.pos);
        if (!text.startsWith('$$')) return false;
        const start = cx.lineStart + line.pos;
        const after = text.slice(2);
        if (after.trimEnd().endsWith('$$') && after.trim().length > 2) {
          cx.nextLine();
          cx.addElement(cx.elt('MathBlock', start, start + text.length));
          return true;
        }
        let end = start + text.length;
        while (cx.nextLine()) {
          end = cx.lineStart + line.text.length;
          if (line.text.trimEnd().endsWith('$$')) {
            cx.nextLine();
            break;
          }
        }
        cx.addElement(cx.elt('MathBlock', start, end));
        return true;
      },
    },
  ],
};

const parser = base.configure([GFM, WikiLink, MathExt]);

let katex: typeof import('katex').default | null = null;
let loading: Promise<unknown> | null = null;
function drawMath(el: HTMLElement, tex: string, display: boolean) {
  el.className = display ? 'md-math md-display' : 'md-math';
  el.textContent = tex;
  const draw = () => {
    try {
      katex!.render(tex, el, { displayMode: display, throwOnError: false, trust: false });
    } catch {}
  };
  if (katex) return draw();
  loading ??= Promise.all([import('katex'), import('katex/dist/katex.min.css')]).then(([k]) => (katex = k.default));
  loading.then(draw);
}

export interface Rendered {
  el: HTMLElement;
  /** The front matter's lines (`key: value`), when the text had one. */
  front: string | null;
}

export interface Options {
  /** Keep the front matter in the text (the chat); default off: it's taken off and returned. */
  keepFront?: boolean;
}

/** `text` as an element (`div.md`) of rendered Markdown. */
export function render(text: string, opts: Options = {}): Rendered {
  let front: string | null = null;
  let src = text.replace(/\r\n?/g, '\n');
  if (!opts.keepFront) {
    const m = /^---\n([\s\S]*?)\n(---|\.\.\.)[ \t]*(\n|$)/.exec(src);
    if (m) {
      front = m[1];
      src = src.slice(m[0].length);
    }
  }
  const root = document.createElement('div');
  root.className = 'md';
  const tree = parser.parse(src);
  blocks(tree.topNode, src, root);
  return { el: root, front };
}

const el = <K extends keyof HTMLElementTagNameMap>(tag: K, cls?: string) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  return e;
};

const SKIP = new Set(['CommentBlock', 'LinkReference', 'ProcessingInstructionBlock']);

function blocks(node: SyntaxNode, src: string, out: HTMLElement) {
  for (let c = node.firstChild; c; c = c.nextSibling) block(c, src, out);
}

function block(n: SyntaxNode, src: string, out: HTMLElement) {
  const name = n.name;
  if (SKIP.has(name)) return;
  const at = (e: HTMLElement) => {
    e.dataset.from = String(n.from);
    out.append(e);
    return e;
  };
  const h = /^(ATX|Setext)Heading(\d)$/.exec(name);
  if (h) {
    const e = at(el(`h${h[2]}` as 'h1'));
    inline(n, src, e, new Set(['HeaderMark']));
    return;
  }
  switch (name) {
    case 'Paragraph':
      return void inline(n, src, at(el('p')), new Set());
    case 'Blockquote':
      return void quote(n, src, at(el('blockquote')));
    case 'BulletList':
    case 'OrderedList': {
      const list = at(el(name === 'BulletList' ? 'ul' : 'ol'));
      const first = n.getChild('ListItem')?.getChild('ListMark');
      if (name === 'OrderedList' && first) {
        const start = parseInt(src.slice(first.from, first.to), 10);
        if (start > 1) (list as HTMLOListElement).start = start;
      }
      for (let li = n.firstChild; li; li = li.nextSibling) {
        if (li.name !== 'ListItem') continue;
        const item = el('li');
        item.dataset.from = String(li.from);
        list.append(item);
        listItem(li, src, item);
      }
      return;
    }
    case 'FencedCode':
    case 'CodeBlock': {
      const pre = at(el('pre'));
      const code = el('code');
      const info = n.getChild('CodeInfo');
      if (info) code.dataset.lang = src.slice(info.from, info.to);
      const texts: string[] = [];
      for (let c = n.firstChild; c; c = c.nextSibling) if (c.name === 'CodeText') texts.push(src.slice(c.from, c.to));
      code.textContent = name === 'CodeBlock' ? src.slice(n.from, n.to).replace(/^ {4}/gm, '') : texts.join('');
      pre.append(code);
      return;
    }
    case 'MathBlock': {
      const tex = src.slice(n.from, n.to).trim().replace(/^\$\$/, '').replace(/\$\$$/, '');
      const d = at(el('div'));
      drawMath(d, tex.trim(), true);
      return;
    }
    case 'HorizontalRule':
      return void at(el('hr'));
    case 'Table':
      return void table(n, src, at(el('table')));
    case 'HTMLBlock': {
      const t = src.slice(n.from, n.to).replace(/<!--[\s\S]*?-->/g, '').trim();
      if (t) at(el('p', 'md-html')).textContent = t;
      return;
    }
    default:
      if (n.firstChild) blocks(n, src, out);
  }
}

function listItem(li: SyntaxNode, src: string, item: HTMLElement) {
  for (let c = li.firstChild; c; c = c.nextSibling) {
    if (c.name === 'ListMark') continue;
    if (c.name === 'Task') {
      const marker = c.getChild('TaskMarker');
      const done = marker ? /x/i.test(src.slice(marker.from, marker.to)) : false;
      item.classList.add('md-task');
      if (done) item.classList.add('md-done');
      const p = el('p');
      const box = el('input');
      box.type = 'checkbox';
      box.checked = done;
      box.disabled = true;
      p.append(box, ' ');
      inline(c, src, p, new Set(['TaskMarker']));
      item.append(p);
    } else block(c, src, item);
  }
}

/** A quote, or an Obsidian callout when its first line is `[!type] Title`. */
function quote(n: SyntaxNode, src: string, q: HTMLElement) {
  const first = n.getChild('Paragraph');
  const head = first ? /^\[!(\w+)\]([+-]?)[ \t]*(.*)/.exec(src.slice(first.from, first.to).replace(/^>\s?/gm, '')) : null;
  if (!head) {
    for (let c = n.firstChild; c; c = c.nextSibling) if (c.name !== 'QuoteMark') block(c, src, q);
    return;
  }
  q.className = `md-callout md-callout-${head[1].toLowerCase()}`;
  const title = el('div', 'md-callout-title');
  title.textContent = head[3].split('\n')[0] || head[1][0].toUpperCase() + head[1].slice(1).toLowerCase();
  q.append(title);
  // The first paragraph's lines after the title, then the rest.
  const rest = src.slice(first!.from, first!.to).split('\n').slice(1).map((l) => l.replace(/^\s*>\s?/, '')).join('\n');
  if (rest.trim()) q.append(render(rest, { keepFront: true }).el);
  for (let c = first!.nextSibling; c; c = c.nextSibling) if (c.name !== 'QuoteMark') block(c, src, q);
}

function table(n: SyntaxNode, src: string, t: HTMLElement) {
  const body = el('tbody');
  for (let r = n.firstChild; r; r = r.nextSibling) {
    if (r.name !== 'TableHeader' && r.name !== 'TableRow') continue;
    const tr = el('tr');
    for (let c = r.firstChild; c; c = c.nextSibling) {
      if (c.name !== 'TableCell') continue;
      const td = el(r.name === 'TableHeader' ? 'th' : 'td');
      inline(c, src, td, new Set());
      tr.append(td);
    }
    if (r.name === 'TableHeader') {
      const head = el('thead');
      head.append(tr);
      t.append(head);
    } else body.append(tr);
  }
  t.append(body);
}

const MARKS = new Set(['EmphasisMark', 'CodeMark', 'LinkMark', 'QuoteMark', 'StrikethroughMark', 'URL', 'LinkTitle', 'LinkLabel']);

/** The inline content of `n` into `out`, leaving out the `skip` children and every mark. */
function inline(n: SyntaxNode, src: string, out: HTMLElement, skip: Set<string>) {
  let pos = n.from;
  const text = (to: number) => {
    if (to > pos) out.append(src.slice(pos, to).replace(/\n[ \t]*>?[ \t]?/g, '\n').replace(/\n/g, ' '));
  };
  for (let c = n.firstChild; c; c = c.nextSibling) {
    text(c.from);
    pos = c.to;
    if (skip.has(c.name) || MARKS.has(c.name)) continue;
    inlineNode(c, src, out);
  }
  text(n.to);
  // A heading's or paragraph's trailing spaces.
  if (out.lastChild?.nodeType === 3) out.lastChild.textContent = out.lastChild.textContent!.replace(/[ \t]+$/, '');
  if (out.firstChild?.nodeType === 3 && /^(h\d|p|td|th)$/i.test(out.tagName)) out.firstChild.textContent = out.firstChild.textContent!.replace(/^[ \t]+/, '');
}

const WEB = /^(https?:\/\/|mailto:)/i;

function link(href: string): HTMLAnchorElement {
  const a = el('a', 'md-link');
  if (WEB.test(href)) {
    a.href = href;
    a.target = '_blank';
    a.rel = 'noopener noreferrer';
  } else {
    a.href = '#';
    a.dataset.href = href;
  }
  return a;
}

function inlineNode(c: SyntaxNode, src: string, out: HTMLElement) {
  const s = src.slice(c.from, c.to);
  switch (c.name) {
    case 'Emphasis':
      return void inline(c, src, out.appendChild(el('em')), new Set());
    case 'StrongEmphasis':
      return void inline(c, src, out.appendChild(el('strong')), new Set());
    case 'Strikethrough':
      return void inline(c, src, out.appendChild(el('del')), new Set());
    case 'InlineCode': {
      out.appendChild(el('code')).textContent = s.replace(/^`+\s?|\s?`+$/g, '');
      return;
    }
    case 'Link': {
      const url = c.getChild('URL');
      const href = url ? src.slice(url.from, url.to).replace(/^<|>$/g, '') : '';
      inline(c, src, out.appendChild(link(href)), new Set());
      return;
    }
    case 'Image': {
      // Images are shown as their alt text, linked: no request leaves the page by itself.
      const url = c.getChild('URL');
      const a = out.appendChild(link(url ? src.slice(url.from, url.to) : ''));
      a.classList.add('md-image');
      a.textContent = `🖼 ${s.replace(/^!\[|\]\(.*$/gs, '') || 'image'}`;
      return;
    }
    case 'Autolink':
    case 'URL': {
      const href = s.replace(/^<|>$/g, '');
      const a = out.appendChild(link(/^www\./i.test(href) ? `https://${href}` : href));
      a.textContent = href;
      return;
    }
    case 'WikiLink': {
      const inner = s.slice(2, -2);
      const [target, label] = inner.split('|');
      const a = el('a', 'md-link md-wiki');
      a.href = '#';
      a.dataset.wiki = target.trim();
      a.textContent = (label ?? target).trim();
      out.append(a);
      return;
    }
    case 'InlineMath':
    case 'DisplayMath': {
      const display = c.name === 'DisplayMath';
      drawMath(out.appendChild(el(display ? 'div' : 'span')), s.slice(display ? 2 : 1, display ? -2 : -1).trim(), display);
      return;
    }
    case 'HardBreak':
      return void out.append(el('br'));
    case 'Escape':
      return void out.append(s.slice(1));
    case 'Entity': {
      const t = el('textarea');
      t.innerHTML = /^&(#\d+|#x[0-9a-f]+|[a-z]+\d*);$/i.test(s) ? s : ''; // a textarea decodes entities without running anything
      return void out.append(t.value || s);
    }
    case 'Comment':
      return;
    default:
      if (c.firstChild) inline(c, src, out, new Set());
      else out.append(s);
  }
}
