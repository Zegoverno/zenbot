<script lang="ts" module>
  import type { Pin } from '../preview.ts';
  /** Pins per session, kept while the tab is closed: the chip shows them all until it is sent or removed. */
  const pinned = new Map<string, { origin: string; pins: Pin[] }>();
</script>

<script lang="ts">
  // The Preview tab (side panel): a dev server on this host's own port, in a frame beside the chat,
  // at a screen size. With "Point" on, a click in the app pins that element: its source file and
  // line, box, key styles, recent console errors and a screenshot go to the chat as one chip
  // ("Preview"), numbered #1, #2… so the owner's comment can refer to them; they go with the next
  // prompt. The rules (which addresses, what a pin may say) are in ../preview.ts.
  //
  // The frame loads `/preview/<port>/…` on zen's own address (web/server/preview.ts): zen's web
  // server forwards it to the dev server and adds public/zen-preview.js to its pages, so any app
  // can be pointed at, and it works wherever zen does (no other port to sign in to or be blocked).
  import { onMount } from 'svelte';
  import { app, notice } from '../app.svelte.ts';
  import { upload } from '../api.ts';
  import { chat, contexts, dropContext, keepContext } from '../plugins.svelte.ts';
  import { pinsText, previewTarget, readPin, where } from '../preview.ts';

  const KEY = 'zen.preview.url';
  /** The chip's key: unlike a plugin's title, never ends in a plugin's name. */
  const NAME = 'Preview (zen)';
  const MAX_PINS = 8;
  const SIZES = { fit: null, phone: [390, 844], tablet: [820, 1180], desktop: [1280, 800] } as const;
  type Size = keyof typeof SIZES;

  const last = localStorage.getItem(KEY) ?? '';
  let address = $state(last);
  let target = $state<URL | null>(previewTarget(last, location));
  let size = $state<Size>('fit');
  let point = $state(false);
  /** Whether the app answered: it loads zen-preview.js, so pointing works. */
  let linked = $state(false);
  let loadedOnce = $state(false);
  let n = $state(0);
  let frame: HTMLIFrameElement | undefined = $state();
  let boxWidth = $state(0);
  /** The frame's address: zen's own, under /preview/<port>. */
  const frameSrc = $derived(target ? `/preview/${target.port}${target.pathname}${target.search}${target.hash}` : '');
  const frameOrigin = location.origin;
  const label = $derived(target ? `localhost:${target.port}` : '');

  const dims = $derived(SIZES[size]);
  const scale = $derived(dims && boxWidth ? Math.min(1, boxWidth / dims[0]) : 1);

  function go() {
    const t = previewTarget(address, location);
    if (!t) return notice(`Preview shows this host's own ports ${location.hostname}:3000–9999, e.g. 5173`);
    localStorage.setItem(KEY, address.trim());
    // Pins name the app they came from: another app starts again.
    const sid = app.session;
    if (sid && pinned.get(sid)?.origin !== `localhost:${t.port}` && contexts[sid]?.[NAME]) {
      dropContext(sid, NAME);
      pinned.delete(sid);
    }
    target = t;
    linked = false;
    loadedOnce = false;
    n++;
  }

  function reload() {
    linked = false;
    n++;
  }

  const post = (msg: Record<string, unknown>) => {
    if (frameOrigin && frame?.contentWindow) frame.contentWindow.postMessage({ zen: 'preview', ...msg }, frameOrigin);
  };

  function loaded() {
    loadedOnce = true;
    linked = false;
    post({ type: 'hello' });
    if (point) post({ type: 'pick', on: true });
  }

  function togglePoint() {
    point = !point;
    post({ type: 'pick', on: point });
  }

  /** Show a session's pins as its chip; a screenshot not yet saved shows as saving. */
  function showPins(sid: string, origin: string, pins: Pin[]) {
    const images = pins.filter((p) => p.image).map((p) => ({ mimeType: 'image/png', data: p.image!.split(',')[1], ...p.saved }));
    keepContext(sid, NAME, pinsText(origin, pins), images);
  }

  function addPin(pin: Pin) {
    const sid = app.session;
    if (!sid || !target) return notice('open a session to send what you point at');
    const origin = label;
    // A chip sent or removed starts the list again.
    const entry = contexts[sid]?.[NAME] && pinned.get(sid)?.origin === origin ? pinned.get(sid)! : { origin, pins: [] };
    if (entry.pins.length >= MAX_PINS) return notice(`${MAX_PINS} pins at most: send these first`);
    entry.pins = [...entry.pins, pin];
    pinned.set(sid, entry);
    showPins(sid, origin, entry.pins);
    // Each screenshot is saved once; the chip updates when it is.
    if (pin.image) {
      const bytes = Uint8Array.from(atob(pin.image.split(',')[1]), (ch) => ch.charCodeAt(0));
      upload(new File([bytes as BlobPart], `preview-${Date.now()}.png`, { type: 'image/png' }), () => {}).done.then(
        (r) => (pin.saved = { path: r.path }),
        (e) => (pin.saved = { error: (e as Error).message }),
      ).then(() => {
        if (pinned.get(sid) === entry && entry.pins.includes(pin) && contexts[sid]?.[NAME]) showPins(sid, origin, entry.pins);
      });
    }
    notice(`#${entry.pins.length} ${pin.tag}${pin.source ? ` · ${where(pin.source)}` : ''}: write your comment`, 3000);
    chat.focus?.();
  }

  onMount(() => {
    const onMessage = (e: MessageEvent) => {
      // Only the framed app, at the address shown, is heard.
      if (!target || !frame || e.source !== frame.contentWindow || e.origin !== frameOrigin) return;
      const d = e.data as { zen?: unknown; type?: unknown; on?: unknown } | null;
      if (!d || d.zen !== 'preview') return;
      if (d.type === 'ready') linked = true;
      else if (d.type === 'pick') point = d.on === true;
      else if (point) {
        // Only while the owner is pointing: the app can't send pins (or files) on its own.
        const pin = readPin(d);
        if (pin) addPin(pin);
      }
    };
    addEventListener('message', onMessage);
    return () => removeEventListener('message', onMessage);
  });
</script>

<div class="pv">
  <form class="pv-bar" onsubmit={(e) => { e.preventDefault(); go(); }}>
    <input class="pv-addr" bind:value={address} placeholder="port, e.g. 5173" aria-label="preview address" spellcheck="false" />
    <button type="submit" class="dim">Go</button>
    <button type="button" class="dim" onclick={reload} disabled={!target} title="reload" aria-label="reload">↻</button>
    <select bind:value={size} aria-label="screen size">
      <option value="fit">Fit</option>
      <option value="phone">Phone</option>
      <option value="tablet">Tablet</option>
      <option value="desktop">Desktop</option>
    </select>
    <button type="button" class="pv-point" class:on={point} onclick={togglePoint} disabled={!linked} aria-pressed={point} title={linked ? 'click an element in the app to pin it' : 'waiting for the page'}>Point</button>
    {#if target}<a class="dim" href={frameSrc} target="_blank" rel="noopener noreferrer" title="open in a tab">↗</a>{/if}
  </form>
  {#if target && loadedOnce && !linked}
    <p class="pv-hint dim">Point works on the app's pages (HTML). A blank or broken page? Start the dev server with the base <code>/preview/{target.port}/</code> (Vite: <code>--base /preview/{target.port}/</code>).</p>
  {/if}
  <div class="pv-box" bind:clientWidth={boxWidth}>
    {#if target}
      {#key n}
        <div class="pv-stage" style:width={dims ? `${dims[0] * scale}px` : '100%'} style:height={dims ? `${dims[1] * scale}px` : '100%'}>
          <iframe
            bind:this={frame}
            title="preview"
            src={frameSrc}
            sandbox="allow-scripts allow-same-origin allow-forms allow-modals"
            onload={loaded}
            style:width={dims ? `${dims[0]}px` : '100%'}
            style:height={dims ? `${dims[1]}px` : '100%'}
            style:transform={scale < 1 ? `scale(${scale})` : null}
          ></iframe>
        </div>
      {/key}
    {:else}
      <p class="dim pv-empty">Show a dev server running on this machine: type its port (3000–9999) and press Go.</p>
    {/if}
  </div>
</div>

<style>
  .pv { height: 100%; display: flex; flex-direction: column; min-height: 0; }
  .pv-bar { display: flex; gap: 6px; align-items: center; padding: 8px 10px; border-bottom: 1px solid var(--faint); flex: none; font-size: 13px; }
  .pv-addr { flex: 1; min-width: 6em; font: inherit; font-family: var(--mono); font-size: 12.5px; padding: 3px 8px; border: 1px solid var(--faint); border-radius: 6px; background: var(--paper); color: var(--ink); }
  .pv-bar select { font: inherit; background: var(--paper); color: var(--ink); border: 1px solid var(--faint); border-radius: 6px; padding: 2px 4px; }
  .pv-point { padding: 2px 10px; border-radius: 6px; border: 1px solid var(--faint); color: var(--dim); }
  .pv-point.on { color: var(--paper); background: var(--accent); border-color: var(--accent); }
  .pv-point:disabled { opacity: 0.5; }
  .pv-hint { margin: 0; padding: 6px 10px; font-size: 12.5px; border-bottom: 1px solid var(--faint); overflow-wrap: anywhere; }
  .pv-box { flex: 1; min-height: 0; overflow: auto; display: flex; justify-content: center; background: color-mix(in srgb, var(--faint) 25%, transparent); }
  .pv-stage { flex: none; overflow: hidden; }
  .pv-stage iframe { border: 0; background: white; transform-origin: 0 0; display: block; }
  .pv-empty { padding: 16px; margin: 0; }
</style>
