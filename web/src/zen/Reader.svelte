<script lang="ts">
  // The file viewer (reader), as the terminal app's (crates/zen/src/tui/panel.rs, views.rs): prose
  // for Markdown and text, monospace for the rest; the front matter folded to a line, the changelog
  // in its own footer; reading aids (width, spacing, bionic, focus, dimming) shared with the TUI
  // through reader.json; links followed in place with a way back; and play, one word at a time.
  import { onDestroy, onMount, tick, untrack } from 'svelte';
  import { files, type FileRead, type ReaderSettings } from './api.ts';
  import { app, closeFile, editNote, notice, openDir, openFile, tilde, typing, type Pane } from './app.svelte.ts';
  import { bionic, render } from './md.ts';
  import { DIMS, MEASURES, WPM, dwell, isProse, keep, lines, orp, properties, takeChangelog, units, wrapWords } from './reader/text.ts';
  import { back, expected, scrolls } from './reader/nav.ts';
  import { trees } from './views/tree.svelte.ts';

  let { path, pane, active }: { path: string; pane: Pane; active: boolean } = $props();

  let doc = $state<FileRead | null>(null);
  let error = $state('');
  let loading = $state(true);
  let set = $state<ReaderSettings>({ wpm: 200, pauses: false, width: 72, spaced: false, bionic: false, focus: 'off', dim: 300 });

  let scroller = $state<HTMLDivElement>();
  let host = $state<HTMLDivElement>();
  let logScroller = $state<HTMLDivElement>();
  let logHost = $state<HTMLDivElement>();

  let fm = $state<[string, string][]>([]);
  let propsOpen = $state(false);
  let hasLog = $state(false);
  let showLog = $state(false);
  let logHint = $state('');
  let pct = $state(0);
  let wordsLeft = $state(0);
  let nWords = $state(0);
  let linkSel = $state(-1);
  let focusAt = $state(-1);
  let lineAt = $state(0);
  let mark = $state<{ top: number; height: number } | null>(null);
  let play = $state<{ at: number; playing: boolean } | null>(null);

  // The rendered document's parts (rebuilt with it, not reactive).
  let words: HTMLElement[] = [];
  let tops: number[] | null = null;
  let us: HTMLElement[] = [];
  let links: HTMLAnchorElement[] = [];
  let logUnits: HTMLElement[] = [];

  const name = $derived(path.split('/').pop() ?? path);
  const folder = $derived(tilde(path.slice(0, path.lastIndexOf('/')) || '/'));
  const prose = $derived(isProse(path));
  const minutes = $derived(Math.ceil(wordsLeft / WPM));
  const k = $derived(keep(set.dim));
  const focusing = $derived(set.focus !== 'off' && !play && prose);

  // ---- the folder's other files (previous / next, in the tree's order) ----

  const dirOf = (p: string) => p.slice(0, p.lastIndexOf('/')) || '/';
  const siblings = $derived.by(() => {
    const t = trees[pane];
    return (t.listing[dirOf(path)] ?? []).filter((e) => !e.dir && (!e.hidden || t.hidden)).map((e) => e.path);
  });
  const sibAt = $derived(siblings.indexOf(path));
  $effect(() => {
    const dir = dirOf(path);
    untrack(() => {
      const t = trees[pane];
      if (dir in t.listing) return;
      files.list(dir).then((r) => (t.listing[dir] = r.entries)).catch(() => {});
    });
  });
  function sibling(d: 1 | -1) {
    const to = siblings[sibAt + d];
    if (sibAt < 0 || !to) return;
    trees[pane].sel = to;
    openFile(to, pane);
  }

  // ---- loading ----

  let loaded = '';
  let seq = 0;
  let canBack = $state(false);
  $effect(() => {
    const p = path;
    untrack(() => {
      if (expected[pane] !== p) back[pane] = [];
      expected[pane] = null;
      canBack = back[pane].length > 0;
      stop();
      void load(p, false);
    });
  });

  async function load(p: string, again: boolean) {
    const n = ++seq;
    if (!again) {
      loading = true;
      files.reader().then((s) => (set = { ...set, ...s })).catch(() => {});
    }
    let d: FileRead;
    try {
      d = await files.read(p);
    } catch (e) {
      if (n !== seq) return;
      loading = false;
      error = (e as Error).message;
      doc = null;
      return;
    }
    if (n !== seq) return;
    if (d.dir) return void openDir(p, pane);
    const at = again ? (scroller?.scrollTop ?? 0) : (scrolls.get(p) ?? 0);
    error = '';
    doc = d;
    loading = false;
    if (!again) {
      linkSel = -1;
      focusAt = -1;
      showLog = false;
      propsOpen = false;
    }
    await tick();
    build();
    loaded = p;
    requestAnimationFrame(() => {
      if (!scroller) return;
      scroller.scrollTop = at;
      measure();
      if (focusing) pickFocus();
    });
  }

  /** Renders the file into the page: prose or monospace, words wrapped, bionic when on. */
  function build() {
    if (!host || !logHost) return;
    const d = doc;
    words = [];
    us = [];
    links = [];
    logUnits = [];
    tops = null;
    fm = [];
    hasLog = false;
    if (!d || d.text === undefined || d.binary || d.tooBig) {
      host.replaceChildren();
      logHost.replaceChildren();
      return;
    }
    let body: HTMLElement;
    let log: HTMLElement | null = null;
    if (prose) {
      const r = render(d.text);
      body = r.el;
      fm = r.front ? properties(r.front) : [];
      log = takeChangelog(body);
    } else {
      body = document.createElement('pre');
      body.className = 'plain';
      body.textContent = d.text;
    }
    words = wrapWords(body, !prose);
    nWords = words.length;
    if (set.bionic && prose) {
      bionic(body);
      if (log) bionic(log);
    }
    host.replaceChildren(body);
    logHost.replaceChildren(...(log ? [log] : []));
    hasLog = !!log && (log.textContent ?? '').trim() !== '';
    if (!hasLog) showLog = false;
    us = prose ? units(body) : [];
    us.forEach((u) => u.classList.add('u'));
    links = [...body.querySelectorAll('a')];
    logUnits = log ? units(log) : [];
    paintFocus();
    if (play) paintWord();
  }

  // Polls the file: newer on disk, read again where it was.
  const poll = setInterval(async () => {
    if (document.hidden || !doc || loaded !== path) return;
    try {
      const s = await files.stat(path);
      if (doc && s.mtime > doc.mtime && loaded === path) await load(path, true);
    } catch {}
  }, 1000);
  onDestroy(() => {
    clearInterval(poll);
    clearTimeout(timer);
  });

  let ro: ResizeObserver | undefined;
  onMount(() => {
    ro = new ResizeObserver(() => {
      tops = null;
      measure();
      paintFocus();
      logStats();
    });
    if (host) ro.observe(host);
    if (scroller) ro.observe(scroller);
    return () => ro?.disconnect();
  });

  // ---- where the reader is ----

  /** Each word's top in the scroller's content (cached until the layout changes). */
  function wordTops(): number[] {
    if (tops || !scroller) return tops ?? [];
    const base = scroller.getBoundingClientRect().top - scroller.scrollTop;
    tops = words.map((w) => w.getBoundingClientRect().top - base);
    return tops;
  }
  /** The first word at or below `y` (scroller content px). */
  function wordAt(y: number): number {
    const t = wordTops();
    let lo = 0;
    let hi = t.length;
    while (lo < hi) {
      const m = (lo + hi) >> 1;
      if (t[m] < y) lo = m + 1;
      else hi = m;
    }
    return lo;
  }
  /** An element's top and bottom in the scroller's content. */
  function box(e: Element): { top: number; bottom: number } {
    const r = e.getBoundingClientRect();
    const base = scroller!.getBoundingClientRect().top - scroller!.scrollTop;
    return { top: r.top - base, bottom: r.bottom - base };
  }

  let raf = 0;
  function onScroll() {
    if (!scroller) return;
    scrolls.set(path, scroller.scrollTop);
    cancelAnimationFrame(raf);
    raf = requestAnimationFrame(() => {
      measure();
      if (focusing && !stepping) keepFocusInView();
      stepping = false;
    });
  }
  function measure() {
    if (!scroller) return;
    const max = scroller.scrollHeight - scroller.clientHeight;
    pct = max <= 1 ? 100 : Math.round((scroller.scrollTop / max) * 100);
    wordsLeft = words.length - wordAt(scroller.scrollTop - 2);
  }

  function scrollBy(px: number) {
    scroller?.scrollBy({ top: px });
  }
  const pageH = () => (scroller ? scroller.clientHeight * 0.9 : 400);
  const lineH = () => (scroller ? parseFloat(getComputedStyle(scroller).fontSize) * 1.8 : 30);

  /** Brings `y` (scroller content px) into view, at a third of the height, unless it already is. */
  function reveal(top: number, bottom = top + 20, margin = 0.15) {
    if (!scroller) return;
    const h = scroller.clientHeight;
    const s = scroller.scrollTop;
    if (top < s + h * margin || bottom > s + h * (1 - margin)) {
      stepping = true;
      scroller.scrollTop = Math.max(0, top - h / 3);
    }
  }

  // ---- settings ----

  function change(c: Partial<ReaderSettings>, label = true) {
    const rebuild = c.bionic !== undefined && c.bionic !== set.bionic;
    set = { ...set, ...c };
    files.setReader(c).catch((e) => notice(`reader settings not saved: ${(e as Error).message}`));
    if (label) notice(`reading: ${describe()}`, 3000);
    if (rebuild) {
      const at = scroller?.scrollTop ?? 0;
      build();
      requestAnimationFrame(() => scroller && (scroller.scrollTop = at));
    }
    if (c.focus !== undefined) {
      focusAt = -1;
      requestAnimationFrame(() => (set.focus !== 'off' ? pickFocus() : paintFocus()));
    } else requestAnimationFrame(() => paintFocus());
  }
  function describe() {
    const w = set.width ? `${set.width} columns` : 'full width';
    return [w, set.spaced && 'spaced', set.bionic && 'bionic', set.focus !== 'off' && `${set.focus} focus · dim ${set.dim}`].filter(Boolean).join(' · ');
  }
  function widen(by: number) {
    const i = MEASURES.indexOf(set.width as (typeof MEASURES)[number]);
    const j = Math.max(0, Math.min(MEASURES.length - 1, (i < 0 ? 1 : i) + by));
    if (MEASURES[j] !== set.width) change({ width: MEASURES[j] as ReaderSettings['width'] });
  }
  const nextFocus = () => change({ focus: set.focus === 'off' ? 'paragraph' : set.focus === 'paragraph' ? 'line' : 'off' });
  const nextDim = () => change({ dim: DIMS[(DIMS.indexOf(set.dim) + 1) % DIMS.length] as ReaderSettings['dim'] });

  // ---- focus ----

  let stepping = false;

  /** Lights the first block in view (its first line). */
  function pickFocus() {
    if (!scroller || !us.length) return paintFocus();
    const s = scroller.scrollTop;
    const i = us.findIndex((u) => box(u).bottom > s + 8);
    focusAt = i < 0 ? 0 : i;
    lineAt = 0;
    if (set.focus === 'line' && i >= 0) {
      const ls = lines(us[focusAt]);
      const top = box(us[focusAt]).top;
      lineAt = Math.max(0, ls.findIndex(([t]) => top + t >= s));
    }
    paintFocus();
  }

  /** The lit block out of view after a scroll by hand: light what's in view instead. */
  function keepFocusInView() {
    if (focusAt < 0 || !us[focusAt] || !scroller) return;
    const b = box(us[focusAt]);
    const s = scroller.scrollTop;
    let top = b.top;
    let bottom = b.bottom;
    if (set.focus === 'line') {
      const l = lines(us[focusAt])[lineAt];
      if (l) [top, bottom] = [b.top + l[0], b.top + l[1]];
    }
    if (bottom < s || top > s + scroller.clientHeight) pickFocus();
  }

  /** Puts the focus classes and the line mask on the blocks. */
  function paintFocus() {
    for (const u of us) {
      u.classList.remove('lit');
      u.style.removeProperty('mask-image');
      u.style.removeProperty('-webkit-mask-image');
    }
    mark = null;
    if (!focusing || focusAt < 0 || !us[focusAt]) return;
    const u = us[focusAt];
    u.classList.add('lit');
    if (set.focus === 'line') {
      const ls = lines(u);
      const l = ls[Math.min(lineAt, ls.length - 1)];
      if (!l) return;
      const m = `rgba(0,0,0,${((1 + k) / 2).toFixed(2)})`;
      const g = `linear-gradient(to bottom, ${m} 0, ${m} ${l[0]}px, #000 ${l[0]}px, #000 ${l[1]}px, ${m} ${l[1]}px)`;
      u.style.setProperty('mask-image', g);
      u.style.setProperty('-webkit-mask-image', g);
      const b = box(u);
      const pageTop = host?.parentElement ? box(host.parentElement).top : 0;
      mark = { top: b.top - pageTop + l[0], height: l[1] - l[0] };
    }
  }

  function focusStep(by: number) {
    if (!us.length) return;
    if (focusAt < 0) return pickFocus();
    if (set.focus === 'line') {
      const n = lines(us[focusAt]).length;
      let i = focusAt;
      let j = lineAt + by;
      if (j < 0) {
        if (i === 0) return;
        i -= 1;
        j = lines(us[i]).length - 1;
      } else if (j >= n) {
        if (i === us.length - 1) return;
        i += 1;
        j = 0;
      }
      focusAt = i;
      lineAt = Math.max(0, j);
      const b = box(us[i]);
      const l = lines(us[i])[lineAt] ?? [0, 20];
      reveal(b.top + l[0], b.top + l[1], 0.2);
    } else {
      focusAt = Math.max(0, Math.min(us.length - 1, focusAt + by));
      lineAt = 0;
      const b = box(us[focusAt]);
      reveal(b.top, Math.min(b.bottom, b.top + (scroller?.clientHeight ?? 400) * 0.6), 0.12);
    }
    paintFocus();
  }

  // ---- headings and links ----

  function heading(by: number) {
    if (!host || !scroller) return;
    const hs = [...host.querySelectorAll('h1, h2, h3, h4, h5, h6')].map((h) => box(h).top);
    const s = scroller.scrollTop + 12;
    const t = by > 0 ? hs.find((y) => y > s + 4) : hs.filter((y) => y < s - 4).at(-1);
    if (t === undefined) return by > 0 ? notice('no heading below', 2000) : void (scroller.scrollTop = 0);
    scroller.scrollTop = Math.max(0, t - 12);
  }

  function selectLink(by: number) {
    if (!links.length || !scroller) return notice('no links here', 2000);
    const n = links.length;
    const s = scroller.scrollTop;
    const h = scroller.clientHeight;
    let i: number;
    if (linkSel >= 0) i = (linkSel + by + n) % n;
    else if (by > 0) i = Math.max(0, links.findIndex((a) => box(a).top >= s));
    else {
      i = n - 1;
      while (i > 0 && box(links[i]).top >= s + h) i--;
    }
    links[linkSel]?.classList.remove('sel');
    linkSel = i;
    links[i].classList.add('sel');
    const b = box(links[i]);
    reveal(b.top, b.bottom, 0.1);
  }

  async function follow(a: HTMLAnchorElement) {
    const ref = a.dataset.wiki ?? a.dataset.href;
    if (ref === undefined) {
      if (a.href) window.open(a.href, '_blank', 'noopener');
      return;
    }
    if (ref.startsWith('#')) return anchor(ref.slice(1));
    try {
      const { found } = await files.locate(ref, path);
      if (!found) return notice(`not found: ${ref}`);
      if (found.path === path) return;
      back[pane].push(path);
      expected[pane] = found.path;
      if (found.dir) {
        back[pane] = [];
        openDir(found.path, pane);
      } else openFile(found.path, pane);
    } catch (e) {
      notice(`${ref}: ${(e as Error).message}`);
    }
  }

  const slug = (s: string) => s.toLowerCase().trim().replace(/[^\p{L}\p{N}\s-]/gu, '').replace(/\s+/g, '-');
  function anchor(id: string) {
    const h = [...(host?.querySelectorAll('h1, h2, h3, h4, h5, h6') ?? [])].find((e) => slug(e.textContent ?? '') === slug(decodeURIComponent(id)));
    if (h && scroller) scroller.scrollTop = Math.max(0, box(h).top - 12);
  }

  function onClick(e: MouseEvent) {
    const a = (e.target as HTMLElement).closest('a');
    if (!a || (!a.dataset.wiki && a.dataset.href === undefined)) return;
    e.preventDefault();
    void follow(a as HTMLAnchorElement);
  }

  function goBack() {
    const prev = back[pane].pop();
    if (prev) {
      expected[pane] = prev;
      openFile(prev, pane);
    } else close();
  }
  function close() {
    back[pane] = [];
    stop();
    closeFile(pane);
  }
  function edit() {
    if (doc?.vault) editNote(doc.vault, pane);
    else notice('only a Markdown note of the vault can be written here', 3000);
  }

  // ---- the changelog ----

  function toggleLog() {
    if (!hasLog) return notice('this file has no changelog', 2500);
    showLog = !showLog;
    if (showLog) requestAnimationFrame(logStats);
  }
  function logStats() {
    if (!logScroller || !showLog) return;
    const n = logUnits.length;
    if (!n) return void (logHint = 'changelog');
    const r = logScroller.getBoundingClientRect();
    const vis = logUnits.map((u, i) => [u.getBoundingClientRect(), i] as const).filter(([b]) => b.bottom > r.top + 2 && b.top < r.bottom - 2);
    const a = vis.length ? vis[0][1] + 1 : 1;
    const b = vis.length ? vis.at(-1)![1] + 1 : 1;
    logHint = `changelog · ${a}–${b} of ${n}`;
  }
  function logKey(key: string) {
    const l = logScroller;
    if (!l) return;
    const line = lineH();
    const page = l.clientHeight * 0.9;
    if (key === 'ArrowUp') l.scrollBy({ top: -line });
    else if (key === 'ArrowDown') l.scrollBy({ top: line });
    else if (key === 'PageUp') l.scrollBy({ top: -page });
    else if (key === 'PageDown') l.scrollBy({ top: page });
    else if (key === 'Home') l.scrollTop = 0;
    else if (key === 'End') l.scrollTop = l.scrollHeight;
  }

  // ---- play (one word at a time) ----

  let timer: ReturnType<typeof setTimeout> | undefined;
  let due = 0;
  let painted: HTMLElement | null = null;

  function startPlay() {
    if (!words.length || !scroller) return notice('nothing to play', 2000);
    const i = Math.min(words.length - 1, wordAt(scroller.scrollTop - 2));
    play = { at: i, playing: false };
    paintFocus();
    resume();
  }
  function resume() {
    if (!play) return;
    if (play.at >= words.length - 1 && !play.playing && ended) play.at = 0;
    ended = false;
    play.playing = true;
    due = performance.now() + dwell(wordText(play.at), set.wpm, set.pauses);
    paintWord();
    schedule();
  }
  let ended = $state(false);
  function pause() {
    if (!play) return;
    play.playing = false;
    clearTimeout(timer);
  }
  function schedule() {
    clearTimeout(timer);
    if (!play?.playing) return;
    timer = setTimeout(advance, Math.max(0, due - performance.now()));
  }
  function advance() {
    if (!play?.playing) return;
    const now = performance.now();
    if (play.at + 1 >= words.length) {
      play.playing = false;
      ended = true;
      return;
    }
    play.at += 1;
    const next = due + dwell(wordText(play.at), set.wpm, set.pauses);
    due = next < now ? now + dwell(wordText(play.at), set.wpm, set.pauses) : next;
    paintWord();
    schedule();
  }
  function step(by: number) {
    if (!play) return;
    pause();
    ended = false;
    play.at = Math.max(0, Math.min(words.length - 1, play.at + by));
    paintWord();
  }
  function stop() {
    clearTimeout(timer);
    painted?.classList.remove('cur');
    painted = null;
    ended = false;
    if (play) {
      play = null;
      requestAnimationFrame(() => paintFocus());
    }
  }
  function speed(by: number) {
    const wpm = Math.max(100, Math.min(1000, set.wpm + by));
    if (wpm !== set.wpm) change({ wpm }, false);
  }
  const wordText = (i: number) => words[i]?.textContent ?? '';

  /** The playing word bold in the text, which follows it. */
  function paintWord() {
    if (!play) return;
    painted?.classList.remove('cur');
    painted = words[play.at] ?? null;
    if (!painted || !scroller) return;
    painted.classList.add('cur');
    const t = wordTops()[play.at];
    const h = scroller.clientHeight;
    if (t !== undefined && (t < scroller.scrollTop + h * 0.15 || t > scroller.scrollTop + h * 0.7)) {
      stepping = true;
      scroller.scrollTop = Math.max(0, t - h / 3);
    }
  }

  const shown = $derived.by(() => {
    if (!play) return null;
    const w = wordText(play.at);
    const o = Math.min(orp(w), Math.max(0, [...w].length - 1));
    const cs = [...w];
    const ctx = (a: number, b: number) => Array.from({ length: b - a }, (_, j) => wordText(a + j)).filter(Boolean).join(' ');
    return {
      before: ctx(Math.max(0, play.at - 2), play.at),
      pre: cs.slice(0, o).join(''),
      pivot: cs[o] ?? '',
      post: cs.slice(o + 1).join(''),
      after: ctx(play.at + 1, Math.min(words.length, play.at + 3)),
    };
  });

  // ---- the source: the first web link among the properties (link, url, source first) ----

  const source = $derived.by(() => {
    const web = (v: string) => /^https?:\/\/\S+$/.exec(v.split(' · ')[0].trim())?.[0];
    const pref = ['link', 'url', 'source'];
    const ranked = [...fm].sort(([a], [b]) => (pref.indexOf(a.toLowerCase()) + 1 || 99) - (pref.indexOf(b.toLowerCase()) + 1 || 99));
    for (const [, v] of ranked) {
      const u = web(v);
      if (u) return u;
    }
    return null;
  });
  function openSource() {
    if (source) window.open(source, '_blank', 'noopener,noreferrer');
    else notice('no web link in the properties', 2000);
  }

  // ---- keys ----

  function key(e: KeyboardEvent) {
    if (active && !typing(e) && e.altKey && !e.ctrlKey && !e.metaKey && e.code === 'KeyO') {
      e.preventDefault();
      return openSource();
    }
    if (!active || typing(e) || e.ctrlKey || e.metaKey || e.altKey) return;
    const k = e.key;
    const done = () => e.preventDefault();
    if (play) {
      if (k === ' ' || k === 'p') play.playing ? pause() : resume();
      else if (k === 'ArrowUp') step(-1);
      else if (k === 'ArrowDown') step(1);
      else if (k === '+' || k === '=') speed(50);
      else if (k === '-') speed(-50);
      else if (k === '.') change({ pauses: !set.pauses }, false);
      else if (k === 'Escape' || k === 'ArrowLeft' || k === 'Backspace') stop();
      else return;
      return done();
    }
    if (showLog && ['ArrowUp', 'ArrowDown', 'PageUp', 'PageDown', 'Home', 'End', 'Escape'].includes(k)) {
      if (k === 'Escape') showLog = false;
      else logKey(k);
      return done();
    }
    switch (k) {
      case 'e':
        edit();
        break;
      case 'i':
        propsOpen = !propsOpen;
        break;
      case 'h':
        toggleLog();
        break;
      case 'ArrowUp':
      case 'ArrowDown':
        if (focusing) focusStep(k === 'ArrowUp' ? -1 : 1);
        else scrollBy(k === 'ArrowUp' ? -lineH() : lineH());
        break;
      case 'PageUp':
        scrollBy(-pageH());
        break;
      case 'PageDown':
        scrollBy(pageH());
        break;
      case ' ':
        scrollBy(e.shiftKey ? -pageH() : pageH());
        break;
      case 'Home':
        if (scroller) scroller.scrollTop = 0;
        break;
      case 'End':
        if (scroller) scroller.scrollTop = scroller.scrollHeight;
        break;
      case '<':
        widen(-1);
        break;
      case '>':
        widen(1);
        break;
      case ':':
        change({ spaced: !set.spaced });
        break;
      case 'b':
        change({ bionic: !set.bionic });
        break;
      case 'f':
        nextFocus();
        break;
      case 'd':
        nextDim();
        break;
      case '.':
        change({ pauses: !set.pauses }, false);
        notice(`play: pauses ${set.pauses ? 'on' : 'off'}`, 2000);
        break;
      case 'p':
        startPlay();
        break;
      case '[':
        heading(-1);
        break;
      case ']':
        heading(1);
        break;
      case 'l':
        selectLink(1);
        break;
      case 'L':
        selectLink(-1);
        break;
      case 'n':
        sibling(1);
        break;
      case 'N':
        sibling(-1);
        break;
      case 'Enter':
        if (linkSel >= 0 && links[linkSel]) void follow(links[linkSel]);
        else return;
        break;
      case 'ArrowLeft':
      case 'Backspace':
        goBack();
        break;
      case 'Escape':
        close();
        break;
      default:
        return;
    }
    done();
  }
</script>

<svelte:window onkeydown={key} />

<div class="reader" class:prose class:spaced={set.spaced} style:--keep={k}>
  <div class="top">
    <div class="title">
      <span class="name">{name}</span>
      {#if doc?.text !== undefined}<span class="dim">&nbsp;·&nbsp;{pct}%{#if wordsLeft}&nbsp;·&nbsp;{minutes} min left{/if}</span>{/if}
      <span class="folder dim" title={folder}><bdi>{folder}</bdi></span>
    </div>
    <div class="tools" role="toolbar" aria-label="reader">
      <button onclick={goBack} title={canBack ? 'back (← or backspace)' : 'close (esc)'}>{canBack ? '←' : '✕'}</button>
      <button onclick={() => sibling(-1)} disabled={sibAt <= 0} title="previous file in this folder (N)">‹ prev</button>
      <button onclick={() => sibling(1)} disabled={sibAt < 0 || sibAt >= siblings.length - 1} title="next file in this folder (n)">next ›</button>
      {#if doc?.vault}<button onclick={edit} title="write (e)">Write</button>{/if}
      <span class="sep"></span>
      <button onclick={() => widen(-1)} title="narrower (<)" disabled={set.width === 60}>‹</button>
      <span class="val" title="line width">{set.width || 'full'}</span>
      <button onclick={() => widen(1)} title="wider (>)" disabled={set.width === 0}>›</button>
      <button class:on={set.spaced} onclick={() => change({ spaced: !set.spaced })} title="line spacing (:)">↕</button>
      {#if prose}
        <button class:on={set.bionic} onclick={() => change({ bionic: !set.bionic })} title="bionic (b)"><b>Bi</b>o</button>
        <button class:on={set.focus !== 'off'} onclick={nextFocus} title="focus: off → paragraph → line (f)">{set.focus === 'line' ? '― line' : set.focus === 'paragraph' ? '¶ para' : '¶ off'}</button>
      {/if}
      <button onclick={nextDim} title="dim level (d)">◐ {set.dim}</button>
      <span class="sep"></span>
      <button onclick={() => heading(-1)} title="previous heading ([)">§↑</button>
      <button onclick={() => heading(1)} title="next heading (])">§↓</button>
      <button onclick={() => selectLink(-1)} title="previous link (L)">‹link</button>
      <button onclick={() => selectLink(1)} title="next link (l)">link›</button>
      {#if linkSel >= 0}<button onclick={() => links[linkSel] && follow(links[linkSel])} title="follow the selected link (enter)">go ↵</button>{/if}
      <span class="sep"></span>
      {#if source}<button onclick={openSource} title={`open the source (Alt+O): ${source}`}>↗ source</button>{/if}
      {#if fm.length}<button class:on={propsOpen} onclick={() => (propsOpen = !propsOpen)} title="properties (i)">i</button>{/if}
      {#if hasLog}<button class:on={showLog} onclick={toggleLog} title="changelog (h)">log</button>{/if}
      <button class:on={!!play} onclick={() => (play ? stop() : startPlay())} title="play one word at a time (p)">{play ? '■ stop' : '▶ play'}</button>
      <button class:on={set.pauses} onclick={() => change({ pauses: !set.pauses }, false)} title="longer stays at punctuation and long words (.)">pauses</button>
    </div>
  </div>

  {#if loading && !doc}
    <p class="msg dim">reading…</p>
  {:else if error}
    <p class="msg err">{error}</p>
  {:else if doc?.binary}
    <p class="msg dim">{name} is a binary file ({doc.size} bytes): nothing to read here.</p>
  {:else if doc?.tooBig}
    <p class="msg dim">{name} is too big to show ({Math.round(doc.size / 1024)} KB).</p>
  {/if}

  <div class="scroll" bind:this={scroller} onscroll={onScroll} class:hidden={!doc || doc.text === undefined}>
    <div class="rpage" style:max-width={set.width ? `calc(${set.width}ch + 48px)` : 'none'} class:focusing class:playing={!!play}>
      {#if fm.length}
        {#if propsOpen}
          <button class="fm open" onclick={() => (propsOpen = false)} title="fold (i)">
            {#each fm as [key, v]}<span class="pk">{key}</span><span class="pv">{v}</span>{/each}
          </button>
        {:else}
          <button class="fm" onclick={() => (propsOpen = true)} title="one row per property (i)">
            {#each fm as [key, v], j}{j ? ' · ' : ''}<span class="pk">{key}:</span> {v}{/each}
          </button>
        {/if}
      {/if}
      <div class="body" bind:this={host} onclick={onClick} role="presentation">
      </div>
      {#if mark}<span class="mark" style:top="{mark.top}px" style:height="{mark.height}px"></span>{/if}
    </div>
  </div>

  <div class="clog" class:hidden={!showLog || !!play}>
    <div class="clog-head dim">
      <span>{logHint}</span>
      <button onclick={() => (showLog = false)} title="hide (h or esc)">✕</button>
    </div>
    <div class="clog-scroll" bind:this={logScroller} onscroll={logStats}>
      <div class="rpage" style:max-width={set.width ? `calc(${set.width}ch + 48px)` : 'none'}>
        <div class="body" bind:this={logHost} onclick={onClick} role="presentation"></div>
      </div>
    </div>
  </div>

  {#if play && shown}
    <div class="player">
      <div class="rsvp" aria-live="off">
        <span class="tick">▼</span>
        <div class="line">
          <span class="left"><span><span class="ctx">{shown.before}</span>{shown.before ? ' ' : ''}{shown.pre}</span></span>
          <span class="pivot">{shown.pivot}</span>
          <span class="right"><span>{shown.post}{shown.after ? ' ' : ''}<span class="ctx">{shown.after}</span></span></span>
        </div>
        <span class="tick">▲</span>
      </div>
      <div class="pstat dim">
        <button onclick={() => speed(-50)} title="slower (-)">−</button>
        <span>{set.wpm} wpm</span>
        <button onclick={() => speed(50)} title="faster (+)">+</button>
        <span>·</span>
        <button onclick={() => step(-1)} title="a word back (↑)">⏮</button>
        <button onclick={() => (play?.playing ? pause() : resume())} title="pause / resume (space)">{play.playing ? '⏸' : '▶'}</button>
        <button onclick={() => step(1)} title="a word on (↓)">⏭</button>
        <span>{play.playing ? 'playing' : ended ? 'the end' : 'paused'} · word {play.at + 1} of {nWords}</span>
        <button class:on={set.pauses} onclick={() => change({ pauses: !set.pauses }, false)} title="pauses (.)">pauses</button>
        <button onclick={stop} title="stop here (esc)">■</button>
      </div>
    </div>
  {/if}
</div>

<style>
  .reader { height: 100%; min-height: 0; display: flex; flex-direction: column; background: var(--paper); color: var(--ink); position: relative; }
  .top { flex: none; padding: 8px 14px 6px; border-bottom: 1px solid var(--faint); font-size: 13px; display: flex; flex-direction: column; gap: 4px; }
  .title { display: flex; align-items: baseline; gap: 0; min-width: 0; white-space: nowrap; }
  .title .name { font-weight: 600; overflow: hidden; text-overflow: ellipsis; flex: 0 1 auto; min-width: 3em; }
  .title > .dim { flex: none; }
  .title .folder { flex: 1 1 0; margin-left: auto; padding-left: 12px; overflow: hidden; text-overflow: ellipsis; direction: rtl; text-align: right; min-width: 0; }
  .tools { display: flex; flex-wrap: wrap; align-items: center; gap: 2px 4px; color: var(--dim); }
  .tools button, .pstat button { padding: 2px 7px; border-radius: 5px; color: var(--dim); white-space: nowrap; }
  .tools button:hover, .pstat button:hover { color: var(--ink); background: color-mix(in srgb, var(--faint) 45%, transparent); }
  .tools button:disabled { opacity: 0.35; cursor: default; background: none; }
  .tools button.on, .pstat button.on { color: var(--accent); }
  .tools .val { min-width: 2.2em; text-align: center; font-variant-numeric: tabular-nums; }
  .sep { width: 1px; height: 14px; background: var(--faint); margin: 0 4px; }
  .msg { padding: 24px 20px; margin: 0; }
  .err { color: var(--err); }
  .hidden { display: none !important; }

  .scroll { flex: 1; min-height: 0; overflow: auto; -webkit-overflow-scrolling: touch; overscroll-behavior: contain; }
  .rpage { margin: 0 auto; padding: 22px 24px 30vh; position: relative; font-family: var(--mono); font-size: 14px; line-height: 1.6; }
  .prose .rpage { font-family: var(--serif); font-size: 19px; line-height: 1.7; }
  .prose.spaced .rpage { line-height: 2.15; }
  .spaced .rpage { line-height: 2; }
  .clog .rpage { padding-top: 6px; padding-bottom: 12px; }

  .fm { display: block; width: 100%; text-align: left; color: var(--dim); font-family: var(--sans); font-size: 13px; line-height: 1.6; margin: 0 0 14px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .fm .pk { color: var(--dim); }
  .fm.open { display: grid; grid-template-columns: max-content 1fr; gap: 0 14px; white-space: normal; }
  .fm.open .pv { color: var(--ink); overflow-wrap: anywhere; }

  .mark { position: absolute; left: 6px; width: 3px; border-radius: 2px; background: var(--accent); }

  /* the rendered text */
  .body :global(pre.plain) { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; font: inherit; }
  .body :global(.md) { overflow-wrap: break-word; }
  .body :global(.md > :first-child) { margin-top: 0; }
  .body :global(h1), .body :global(h2), .body :global(h3), .body :global(h4), .body :global(h5), .body :global(h6) { font-family: var(--serif); line-height: 1.3; margin: 1.4em 0 0.5em; }
  .body :global(h1) { font-size: 1.6em; }
  .body :global(h2) { font-size: 1.3em; }
  .body :global(h3) { font-size: 1.12em; }
  .body :global(h4), .body :global(h5), .body :global(h6) { font-size: 1em; }
  .body :global(p) { margin: 0 0 0.9em; }
  .body :global(ul), .body :global(ol) { margin: 0 0 0.9em; padding-left: 1.4em; }
  .body :global(li > p) { margin: 0 0 0.25em; }
  .body :global(li.md-task) { list-style: none; }
  .body :global(li.md-task input) { margin: 0 0.4em 0 -1.3em; }
  .body :global(li.md-done) { color: var(--dim); }
  .body :global(blockquote) { margin: 0 0 0.9em; padding-left: 1em; border-left: 3px solid var(--faint); color: color-mix(in srgb, var(--ink) 80%, var(--paper)); }
  .body :global(.md-callout) { border-left-color: var(--accent); background: color-mix(in srgb, var(--accent) 6%, transparent); padding: 0.5em 0.9em; border-radius: 4px; }
  .body :global(.md-callout-title) { font-weight: 600; font-family: var(--sans); font-size: 0.85em; margin-bottom: 0.3em; }
  .body :global(code) { font-family: var(--mono); font-size: 0.82em; background: color-mix(in srgb, var(--faint) 45%, transparent); padding: 0.1em 0.3em; border-radius: 4px; }
  .body :global(pre:not(.plain)) { font-family: var(--mono); font-size: 0.78em; line-height: 1.55; background: color-mix(in srgb, var(--faint) 35%, transparent); padding: 0.8em 1em; border-radius: 6px; overflow-x: auto; margin: 0 0 1em; }
  .body :global(pre code) { background: none; padding: 0; font-size: 1em; }
  .body :global(table) { border-collapse: collapse; font-family: var(--sans); font-size: 0.78em; margin: 0 0 1em; display: block; overflow-x: auto; max-width: 100%; }
  .body :global(th), .body :global(td) { border: 1px solid var(--faint); padding: 0.3em 0.6em; text-align: left; vertical-align: top; }
  .body :global(td code) { white-space: nowrap; }
  .body :global(th) { background: color-mix(in srgb, var(--faint) 30%, transparent); }
  .body :global(hr) { border: 0; border-top: 1px solid var(--faint); margin: 1.5em 0; }
  .body :global(img) { max-width: 100%; }
  .body :global(a) { color: var(--accent); text-decoration: none; border-bottom: 1px solid color-mix(in srgb, var(--accent) 35%, transparent); cursor: pointer; }
  .body :global(a:hover) { border-bottom-color: var(--accent); }
  .body :global(a.sel) { background: color-mix(in srgb, var(--accent) 18%, transparent); outline: 2px solid color-mix(in srgb, var(--accent) 55%, transparent); border-radius: 3px; }
  .body :global(.md-html) { font-family: var(--mono); font-size: 0.8em; color: var(--dim); }
  .body :global(.md-display) { display: block; overflow-x: auto; overflow-y: hidden; margin: 0 0 1em; }
  .body :global(b.md-bionic) { font-weight: 700; }

  /* focus: the lit block in full, the rest dimmed */
  .focusing :global(.u) { transition: opacity 0.15s; }
  .focusing :global(.u:not(.lit)) { opacity: var(--keep); }

  /* play: the text dimmed, the word playing bold */
  .playing :global(.md), .playing :global(.md *), .playing :global(pre.plain) { color: color-mix(in srgb, var(--ink) calc(var(--keep) * 100%), var(--paper)) !important; }
  .playing :global(.w.cur), .playing :global(.w.cur *) { color: var(--ink) !important; font-weight: 700; }

  .clog { flex: none; display: flex; flex-direction: column; max-height: 33%; min-height: 7.5em; border-top: 1px solid var(--faint); background: color-mix(in srgb, var(--faint) 12%, var(--paper)); }
  .clog-head { display: flex; justify-content: space-between; align-items: center; padding: 4px 14px; font-size: 12px; }
  .clog-head button { color: var(--dim); }
  .clog-scroll { flex: 1; min-height: 0; overflow: auto; -webkit-overflow-scrolling: touch; overscroll-behavior: contain; }
  .clog .rpage { font-size: 16px; }

  .player { flex: none; border-top: 1px solid var(--faint); padding: 8px 12px 10px; background: var(--paper); }
  .rsvp { display: flex; flex-direction: column; align-items: stretch; font-family: var(--serif); }
  .tick { text-align: center; color: var(--dim); font-size: 10px; line-height: 1; }
  .line { display: grid; grid-template-columns: 1fr auto 1fr; font-size: 30px; line-height: 1.35; white-space: nowrap; }
  .line .left { overflow: hidden; display: flex; justify-content: flex-end; }
  .line .left > span { flex: none; white-space: pre; }
  .line .right > span { white-space: pre; }
  .line .right { overflow: hidden; }
  .line .ctx { color: var(--dim); opacity: 0.55; font-size: 0.7em; }
  .line .pivot { color: var(--err); }
  .pstat { display: flex; flex-wrap: wrap; justify-content: center; align-items: center; gap: 2px 6px; font-size: 12px; margin-top: 4px; font-variant-numeric: tabular-nums; }

  @media (max-width: 640px) {
    .rpage { padding: 16px 16px 30vh; }
    .prose .rpage { font-size: 18px; }
    .line { font-size: 26px; }
    .title .folder { display: none; }
  }
</style>
