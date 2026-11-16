// Each pane's Files tree, kept for the page's life (the view comes and goes with its tab and the
// file viewer): its root, which folders are open, the selected path, whether dotfiles show, and
// the folders' listings (read lazily, only while open).

import type { Pane } from '../app.svelte.ts';
import type { Entry } from '../api.ts';

export interface Tree {
  root: string;
  /** Open folders (the root always is). */
  open: Record<string, boolean>;
  /** The selected row's path ('..' the row that goes up). */
  sel: string | null;
  hidden: boolean;
  /** Folder → its entries; a missing key: not read yet. */
  listing: Record<string, Entry[]>;
  /** Folder → why it couldn't be read. */
  failed: Record<string, string>;
}

const blank = (): Tree => ({ root: '', open: {}, sel: null, hidden: false, listing: {}, failed: {} });

export const trees: Record<Pane, Tree> = $state({ home: blank(), side: blank() });
