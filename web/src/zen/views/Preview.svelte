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
  // The apps to show come from the server's list of dev servers on this machine (web/server/servers.ts),
  // named by app, each with restart and stop; no port to type (the address box stays as a fallback).
  // Keys (keys.svelte.ts, for the focused Preview panel): r reloads, s cycles the screen size, o
  // opens it in a tab, l goes to the address box, a opens the list of apps, p turns Point on.
  import { onMount } from 'svelte';
  import { commands as registerCommands } from '../keys.svelte.ts';
  import { app, notice } from '../app.svelte.ts';
  import { files, upload, type DevServer } from '../api.ts';
  import { chat, contexts, dropContext, keepContext } from '../plugins.svelte.ts';
  import { pinsText, previewTarget, readPin, where } from '../preview.ts';

  const KEY = 'zen.preview.url';
  /** The chip's key: unlike a plugin's title, never ends in a plugin's name. */
  const NAME = 'Preview (zen)';
  const MAX_PINS = 8;
  /** A screenshot on its way: a blank 1×1 PNG, so the chip shows it saving and sending waits. */
  const PENDING = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=';
  const SIZES = { fit: null, phone: [390, 844], tablet: [820, 1180], desktop: [1280, 800] } as const;
  type Size = keyof typeof SIZES;

  const last = localStorage.getItem(KEY) ?? '';
  /** What the owner types as a fallback; picking an app leaves it empty (the bar shows the app's name). */
  let address = $state('');
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
  // The apps on this machine: null until the first answer.
  let servers = $state<DevServer[] | null>(null);
  let listOpen = $state(false);
  let listing = $state(false);
  let listError = $state('');
  /** Ports being restarted or stopped. */
  let busy = $state<Record<number, string>>({});
  const shown = $derived(target ? (servers?.find((s) => s.port === Number(target!.port)) ?? null) : null);
  const shownName = $derived(target ? (shown?.name ?? label) : '');

  const dims = $derived(SIZES[size]);
  const scale = $derived(dims && boxWidth ? Math.min(1, boxWidth / dims[0]) : 1);

  async function refresh() {
    listing = true;
    try {
      servers = (await files.previewServers()).servers;
      listError = '';
    } catch (e) {
      listError = (e as Error).message;
    } finally {
      listing = false;
    }
  }

  function go() {
    const t = previewTarget(address, location);
    if (!t) return notice(`Preview shows this host's own ports ${location.hostname}:3000–9999, e.g. 5173`);
    show(t, address.trim());
    address = '';
  }

  /** Shows an app (what was typed, or a listed one's port); the choice is kept for the next visit. */
  function show(t: URL, remember: string) {
    localStorage.setItem(KEY, remember);
    listOpen = false;
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

  const pick = (s: DevServer) => show(previewTarget(String(s.port), location)!, String(s.port));

  async function restartApp(s: DevServer) {
    busy[s.port] = 'restarting…';
    try {
      const r = await files.previewRestart(s.port);
      if (target && Number(target.port) === s.port) reload();
      if (servers) servers = servers.map((x) => (x.port === s.port ? r.server : x));
    } catch (e) {
      notice(`restart failed: ${(e as Error).message}`);
    } finally {
      delete busy[s.port];
      void refresh();
    }
  }

  async function stopApp(s: DevServer) {
    if (!confirm(`Stop ${s.name} (:${s.port})?`)) return;
    busy[s.port] = 'stopping…';
    try {
      await files.previewStop(s.port);
      if (target && Number(target.port) === s.port) {
        target = null;
        linked = false;
        loadedOnce = false;
        localStorage.removeItem(KEY);
      }
    } catch (e) {
      notice(`stop failed: ${(e as Error).message}`);
    } finally {
      delete busy[s.port];
      void refresh();
    }
  }

  function openList() {
    listOpen = true;
    void refresh().then(() => document.querySelector<HTMLElement>('.pv-pick')?.focus());
  }

  async function reload() {
    linked = false;
    // A plain reload can bring back a copy the browser kept (a dev server that allows caching, or one
    // kept before it stopped): fetch the page fresh first, which replaces that copy, then reframe it.
    if (frameSrc) await fetch(frameSrc, { cache: 'reload' }).then((r) => r.arrayBuffer()).catch(() => {});
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
    // The screenshot is taken on this machine (server/shot.ts): nothing is uploaded from the
    // browser, which some networks refuse. Until it's back the chip shows it saving; if it fails,
    // the browser's own capture is uploaded instead.
    const own = pin.image;
    pin.image = own ?? PENDING;
    entry.pins = [...entry.pins, pin];
    pinned.set(sid, entry);
    showPins(sid, origin, entry.pins);
    const refresh = () => {
      if (pinned.get(sid) === entry && entry.pins.includes(pin) && contexts[sid]?.[NAME]) showPins(sid, origin, entry.pins);
    };
    files
      .previewShot({ url: pin.url, selector: pin.selector, width: pin.viewport.w, height: pin.viewport.h })
      .then((r) => {
        pin.image = `data:image/png;base64,${r.data}`;
        pin.saved = { path: r.path };
      })
      .catch(async (e) => {
        if (!own) return void ((pin.image = null), (pin.saved = undefined), notice(`no screenshot: ${(e as Error).message}`));
        pin.image = own;
        const bytes = Uint8Array.from(atob(own.split(',')[1]), (ch) => ch.charCodeAt(0));
        await upload(new File([bytes as BlobPart], `preview-${Date.now()}.png`, { type: 'image/png' }), () => {}).done.then(
          (r) => (pin.saved = { path: r.path }),
          (err) => (pin.saved = { error: (err as Error).message }),
        );
      })
      .then(refresh);
    notice(`#${entry.pins.length} ${pin.tag}${pin.source ? ` · ${where(pin.source)}` : ''}: write your comment`, 3000);
    chat.focus?.();
  }

  const SIZE_ORDER = Object.keys(SIZES) as Size[];
  let addr = $state<HTMLInputElement>();
  registerCommands('preview', () => [
    { id: 'preview-reload', title: 'Reload', icon: 'reload', key: 'r', ring: 0, enabled: () => !!target, run: reload },
    { id: 'preview-size', title: 'Screen size', icon: 'size', key: 's', ring: 1, run: () => { size = SIZE_ORDER[(SIZE_ORDER.indexOf(size) + 1) % SIZE_ORDER.length]; notice(`Screen size: ${size}`, 2000); } },
    { id: 'preview-tab', title: 'Open in a tab', icon: 'external', key: 'o', ring: 2, enabled: () => !!target, run: () => window.open(frameSrc, '_blank', 'noopener,noreferrer') },
    { id: 'preview-address', title: 'Change address', icon: 'search', key: 'l', ring: 3, run: () => { addr?.focus(); addr?.select(); } },
    { id: 'preview-apps', title: 'Apps on this machine', icon: 'preview', key: 'a', ring: 4, run: openList },
    { id: 'preview-point', title: point ? 'Stop pointing' : 'Point at an element', icon: 'hand', key: 'p', enabled: () => linked, run: togglePoint },
  ]);

  onMount(() => {
    void refresh();
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

{#snippet apps()}
  {#if servers === null}
    <p class="dim pv-note">{listError ? `Couldn't list the dev servers: ${listError}` : 'Looking for dev servers…'}</p>
  {:else if !servers.length}
    <p class="dim pv-note">No dev servers running on this machine (ports 3000–9999).</p>
  {:else}
    <ul class="pv-apps" aria-label="dev servers">
      {#each servers as s (s.port)}
        <li class="pv-row" class:cur={target && Number(target.port) === s.port} title={s.cwd}>
          <button type="button" class="pv-pick" onclick={() => pick(s)} disabled={!!busy[s.port]}>
            <span class="pv-name">{s.name}</span><span class="dim pv-port">:{s.port}</span>
            <span class="dim pv-cmd">{busy[s.port] ?? s.command}</span>
          </button>
          <button type="button" class="dim pv-act" onclick={() => restartApp(s)} disabled={!!busy[s.port]} title="restart" aria-label="restart {s.name}">↻</button>
          <button type="button" class="dim pv-act" onclick={() => stopApp(s)} disabled={!!busy[s.port]} title="stop" aria-label="stop {s.name}">✕</button>
        </li>
      {/each}
    </ul>
    {#if listError}<p class="dim pv-note">{listError}</p>{/if}
  {/if}
{/snippet}

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="pv" onkeydown={(e) => { if (e.key === 'Escape' && listOpen) { listOpen = false; e.stopPropagation(); } }}>
  <form class="pv-bar" onsubmit={(e) => { e.preventDefault(); go(); }}>
    <button type="button" class="pv-apps-btn" class:on={listOpen} onclick={() => (listOpen ? (listOpen = false) : openList())} aria-expanded={listOpen} aria-label="dev servers" title="the apps running on this machine (a)">{target ? shownName : 'Apps'} ▾</button>
    <button type="button" class="dim" onclick={refresh} disabled={listing} title="refresh the list" aria-label="refresh dev servers">⟳</button>
    <input class="pv-addr" bind:this={addr} onkeydown={(e) => { if (e.key === 'Escape') (e.currentTarget as HTMLInputElement).blur(); }} bind:value={address} placeholder="or a port, e.g. 5173" aria-label="preview address" spellcheck="false" />
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
    {#if listOpen && target}
      <div class="pv-drop" role="region" aria-label="apps on this machine">{@render apps()}</div>
    {/if}
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
      <div class="pv-empty"><p class="dim pv-note">Pick a dev server running on this machine:</p>{@render apps()}</div>
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
  .pv-empty { padding: 8px 10px; margin: 0; width: 100%; align-self: flex-start; }
  .pv-box { position: relative; }
  .pv-drop { position: absolute; z-index: 5; left: 8px; right: 8px; top: 0; max-height: 70%; overflow: auto; background: var(--paper); border: 1px solid var(--faint); border-radius: 8px; box-shadow: 0 6px 24px rgba(0, 0, 0, 0.12); padding: 4px; }
  .pv-note { margin: 0; padding: 6px 8px; font-size: 13px; }
  .pv-apps { list-style: none; margin: 0; padding: 0; }
  .pv-row { display: flex; align-items: center; gap: 2px; border-radius: 6px; }
  .pv-row:hover, .pv-row.cur { background: color-mix(in srgb, var(--faint) 45%, transparent); }
  .pv-pick { flex: 1; min-width: 0; display: flex; align-items: baseline; gap: 6px; padding: 6px 8px; text-align: left; font-size: 13px; }
  .pv-name { font-weight: 600; flex: none; }
  .pv-port { flex: none; font-family: var(--mono); font-size: 12px; }
  .pv-cmd { font-family: var(--mono); font-size: 11.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .pv-act { padding: 4px 8px; border-radius: 6px; }
  .pv-act:hover:not(:disabled) { color: var(--ink); }
  .pv-apps-btn { padding: 2px 10px; border-radius: 6px; border: 1px solid var(--faint); font-weight: 600; max-width: 40%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: none; }
  .pv-apps-btn.on { background: color-mix(in srgb, var(--faint) 45%, transparent); }
</style>
