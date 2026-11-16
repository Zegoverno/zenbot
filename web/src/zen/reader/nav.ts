// What the file viewer keeps for the page's life: where each file was left (scroll), and each
// pane's way back through the links followed.

import type { Pane } from '../app.svelte.ts';

/** Scroll position by path. */
export const scrolls = new Map<string, number>();

/** Per pane: the files a link was followed from (last is the latest), and the path the viewer
 *  expects next (set before a follow or a back, so an unrelated open clears the stack). */
export const back: Record<Pane, string[]> = { home: [], side: [] };
export const expected: Record<Pane, string | null> = { home: null, side: null };
