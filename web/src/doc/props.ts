// A note's properties: its YAML front matter, read into rows and written back. Untouched, the
// front matter is kept byte for byte; an edit changes only that property (comments stay).

import { isMap, isScalar, isSeq, parseDocument, type Document } from 'yaml';

export interface Prop {
  key: string;
  /** What the row shows: a scalar as it is, a list joined with ', '. */
  value: string;
  list: boolean;
  /** A nested map or anything else a row can't edit: shown, not changed. */
  fixed: boolean;
}

/** The types of note the vault uses (its AGENTS.md), offered for `type`. */
export const NOTE_TYPES = ['idea', 'source', 'primitive', 'fact', 'solution', 'decision', 'hub', 'project-note', 'goal', 'daily'];

const OUT = { flowCollectionPadding: false, lineWidth: 0 } as const;

function inner(frontmatter: string): string {
  const m = /^---\r?\n([\s\S]*?)\r?\n?---[ \t]*(\r?\n|$)/.exec(frontmatter);
  return m ? m[1] : '';
}

function doc(frontmatter: string): Document | null {
  const d = parseDocument(inner(frontmatter));
  return d.errors.length || (d.contents !== null && !isMap(d.contents)) ? null : d;
}

/** The rows, in file order; null when the YAML is broken (left alone, shown raw). */
export function readProps(frontmatter: string): Prop[] | null {
  if (!frontmatter) return [];
  const d = doc(frontmatter);
  if (!d) return null;
  if (!isMap(d.contents)) return [];
  return d.contents.items.map((pair) => {
    const key = String(isScalar(pair.key) ? pair.key.value : pair.key);
    const v = pair.value;
    if (v === null || v === undefined || (isScalar(v) && v.value === null)) return { key, value: '', list: false, fixed: false };
    if (isScalar(v)) return { key, value: String(v.value), list: false, fixed: false };
    if (isSeq(v) && v.items.every((i) => isScalar(i))) {
      return { key, value: v.items.map((i) => String((i as { value: unknown }).value)).join(', '), list: true, fixed: false };
    }
    return { key, value: String(v).trim(), list: false, fixed: true };
  });
}

function write(d: Document): string {
  const body = isMap(d.contents) && d.contents.items.length ? d.toString(OUT) : '';
  return body ? `---\n${body.endsWith('\n') ? body : body + '\n'}---\n` : '';
}

/** Sets a property (added at the end when new). A list's value is comma separated. */
export function setProp(frontmatter: string, key: string, value: string, list: boolean): string {
  const d = doc(frontmatter || '---\n---\n') ?? null;
  if (!d) return frontmatter;
  if (d.contents === null) d.contents = d.createNode({}) as never;
  const old = d.get(key, true);
  let node;
  if (list) {
    const items = value.split(',').map((s) => s.trim()).filter(Boolean);
    node = d.createNode(items);
    // Keep the list's style: one line `[a, b]` or one item per line.
    (node as { flow?: boolean }).flow = isSeq(old) ? !!old.flow : true;
  } else {
    const v = value.trim();
    // true, false and plain numbers stay what they were in YAML, not quoted text.
    const typed = v === 'true' ? true : v === 'false' ? false : /^-?\d+(\.\d+)?$/.test(v) ? Number(v) : v;
    node = v === '' ? null : d.createNode(typed);
  }
  d.set(key, node);
  return write(d);
}

export function removeProp(frontmatter: string, key: string): string {
  const d = doc(frontmatter);
  if (!d) return frontmatter;
  d.delete(key);
  return write(d);
}
