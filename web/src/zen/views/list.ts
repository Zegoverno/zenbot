// Small helpers the views share: the spinner, a page's worth of rows, keeping the selected
// row in view, and plain keys (no modifiers).

export const SPINNER = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/** Rows a page key moves by: what fits in the list's scrolling box (≥1). */
export function pageRows(el: HTMLElement | undefined, row = 28): number {
  let box: HTMLElement | null = el ?? null;
  while (box && box.scrollHeight <= box.clientHeight && box.parentElement) box = box.parentElement;
  const h = box?.clientHeight ?? 400;
  return Math.max(1, Math.floor(h / row) - 2);
}

/** Scroll the selected row (`[data-sel]` inside `el`) into view once the DOM has it. */
export function follow(el: HTMLElement | undefined) {
  requestAnimationFrame(() => el?.querySelector<HTMLElement>('[data-sel="1"]')?.scrollIntoView({ block: 'nearest' }));
}

/** A key without ctrl, alt or meta (shift allowed). */
export const plain = (e: KeyboardEvent) => !e.ctrlKey && !e.altKey && !e.metaKey;

/** Whitespace folded to single spaces. */
export const clean = (s: string) => s.replace(/\s+/g, ' ').trim();

/** "512 B", "3.4 KB", "12 MB". */
export function human(n: number): string {
  if (n < 1024) return `${n} B`;
  const u = ['KB', 'MB', 'GB', 'TB'];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${u[i]}`;
}
