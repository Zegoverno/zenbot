<script lang="ts">
  // A plugin's `ui://` resource, running as an MCP App (ADR 0001, Host API): its HTML in a
  // sandboxed iframe with an opaque origin and a CSP from its `_meta.ui.csp` (frame.ts), talking to
  // the page only through `AppBridge` (postMessage, JSON-RPC). The host context carries the theme,
  // the host's colours and fonts, and zen's own `{ pane, path, mode, slot }`.
  //
  // What the app may ask, and where it goes:
  // - tool calls → `POST /api/plugins/<name>/call` (the kernel runs and records them);
  // - `openLink`: `zen://open?path=…&mode=read|edit` opens that vault file in this pane;
  //   `zen://close` returns this pane to the core Files tree,
  //   http(s) opens a new tab (noopener), anything else is refused;
  // - `sendMessage` queues a visible, attributed draft; only the owner can send it;
  // - `updateModelContext` is kept for the open session, shown as a chip above the chat box, and
  //   goes with its next prompt; its images are saved to ~/inbox and named by path.
  // Keys (PLUGINS.md, "Keys and commands"): the iframe swallows the keys pressed in it, so the app
  // sends `zen/commands` (its command list, registered for this pane in keys.svelte.ts: ring, Ctrl+K,
  // `?`) and `zen/key` (a key it didn't use, dispatched as if pressed in this pane, and only while
  // this pane is the focused one); the host sends `zen/command {id}` when one runs and
  // `zen/focus {focused}` when the pane gains or loses the keyboard (it then focuses the iframe).
  // All of it is checked in frame.ts and shown as text only.
  import { onMount } from 'svelte';
  import { AppBridge, PostMessageTransport, type McpUiHostContext } from '@modelcontextprotocol/ext-apps/app-bridge';
  import type { CallToolResult } from '@modelcontextprotocol/sdk/types.js';
  import { pluginApi, type ToolResult } from './api.ts';
  import { app, openFile, closeFile, setView, openSession, notice, textField, type Pane } from './app.svelte.ts';
  import { chat, contextImages, keepContextSaving, resultText } from './plugins.svelte.ts';
  import { SANDBOX, validCommands, validKeyEvent, webLink, withCsp, zenNavigation } from './frame.ts';
  import { dispatch, focused, focusPane, register, ui as keys, unregister, type PanelKind } from './keys.svelte.ts';

  let {
    plugin,
    ui,
    pane,
    path = null,
    mode = 'read',
    slot,
    title = plugin,
    input,
    result,
    scope,
  }: {
    plugin: string;
    ui: string;
    pane: Pane;
    /** The vault-relative file it shows (openers), or null. */
    path?: string | null;
    mode?: 'read' | 'edit';
    slot: 'view' | 'opener' | 'game';
    title?: string;
    /** A command's arguments and result, sent to the app once it is ready. */
    input?: Record<string, unknown>;
    result?: ToolResult;
    /** The panel it sits in, whose commands it adds to (default: by slot). */
    scope?: PanelKind;
  } = $props();
  const where = $derived<PanelKind>(scope ?? (slot === 'game' ? 'games' : slot === 'opener' ? (mode === 'edit' ? 'files-writer' : 'files-reader') : 'plugin'));

  let frame: HTMLIFrameElement | undefined = $state();
  let error = $state('');
  let bridge: AppBridge | null = null;
  const darkQuery = matchMedia('(prefers-color-scheme: dark)');
  let dark = $state(darkQuery.matches);
  let dimensions = $state({ width: 0, height: 0 });
  let phone = $state(false);
  let ready = $state(false);
  /** Registry handle for this frame's commands. */
  let cmds = 0;

  /** The host's colours and fonts, under MCP Apps' variable names. */
  function styles() {
    const css = getComputedStyle(document.documentElement);
    const v = (name: string) => css.getPropertyValue(name).trim();
    return {
      variables: {
        '--color-background-primary': v('--paper'),
        '--color-background-secondary': v('--soft'),
        '--color-border-secondary': v('--faint'),
        '--color-background-info': v('--accent'),
        '--border-radius-md': v('--radius'),
        '--color-text-primary': v('--ink'),
        '--color-text-secondary': v('--dim'),
        '--color-border-primary': v('--faint'),
        '--color-text-info': v('--accent'),
        '--color-text-danger': v('--err'),
        '--color-text-success': v('--ok'),
        '--color-text-warning': v('--warn'),
        '--font-sans': v('--sans'),
        '--font-mono': v('--mono'),
      } satisfies Partial<NonNullable<McpUiHostContext['styles']>['variables']>,
    };
  }

  function context(): McpUiHostContext {
    return {
      theme: dark ? 'dark' : 'light',
      // The SDK type requires every variable, although its runtime schema allows a subset.
      styles: styles() as McpUiHostContext['styles'],
      displayMode: 'inline',
      availableDisplayModes: ['inline'],
      // Host context crosses postMessage: Svelte's state proxy is not structured-cloneable.
      containerDimensions: { width: dimensions.width, height: dimensions.height },
      platform: 'web',
      locale: navigator.language,
      timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone,
      zen: { pane: pane === 'home' ? 'board' : 'side', path, mode, slot, phone, headingFont: getComputedStyle(document.documentElement).getPropertyValue('--serif').trim() },
    };
  }

  const session = () => (app.session && app.session !== 'new' ? app.session : null);

  /** `zen/commands`: replaces this frame's list in the registry; running one tells the app. */
  function setCommands(params: unknown) {
    const list = validCommands(params).map((c, i) => ({
      id: `plugin:${plugin}/${c.id}`,
      title: c.title,
      icon: c.icon ?? 'plugin',
      key: c.key,
      ring: c.ring ? 100 + i : undefined,
      plugin,
      run: () => void bridge?.notification({ method: 'zen/command', params: { id: c.id } } as never).catch(() => {}),
    }));
    cmds = register(where, list, cmds || undefined);
  }

  /** `zen/key`: a key the app didn't use goes through the window's handler, as if pressed in this pane. */
  function forwardKey(params: unknown) {
    const k = validKeyEvent(params);
    if (!k) return;
    // Only the frame that really has the keyboard may press keys: a click inside it never reaches the
    // page, so the frame having it is what focuses its pane. A frame in the background, or in a window
    // that lost the focus, is dropped (it could otherwise drive the app unseen).
    const i = keys.panes.indexOf(where);
    if (document.activeElement !== frame || !document.hasFocus() || i < 0) return;
    focusPane(i);
    dispatch(textField(document.activeElement) ? { ...k, typing: true } : k);
  }

  // The pane gained or lost the keyboard: tell the app, and put the keyboard in the frame.
  let wasFocused: boolean | null = null;
  $effect(() => {
    const on = focused() === where;
    if (!ready || on === wasFocused) return;
    wasFocused = on;
    void bridge?.notification({ method: 'zen/focus', params: { focused: on } } as never).catch(() => {});
    if (on) frame?.focus();
  });

  onMount(() => {
    let gone = false;
    const theme = () => {
      const selected = document.documentElement.dataset.theme;
      dark = selected === 'dark' || (selected !== 'light' && darkQuery.matches);
      bridge?.setHostContext(context());
    };
    const resize = () => {
      if (frame) dimensions = { width: frame.clientWidth, height: frame.clientHeight };
      phone = window.matchMedia('(max-width: 700px)').matches;
    };
    const observer = new MutationObserver(theme);
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme', 'class', 'style'] });
    const sizes = new ResizeObserver(resize);
    if (frame) sizes.observe(frame);
    window.addEventListener('resize', resize);
    // Focus moving into the frame (a click in it) makes this pane the focused one.
    const focusIn = () => setTimeout(() => { const i = keys.panes.indexOf(where); if (document.activeElement === frame && i >= 0) focusPane(i); }, 0);
    window.addEventListener('blur', focusIn);
    theme(); resize();
    darkQuery.addEventListener('change', theme);
    (async () => {
      try {
        const res = await pluginApi.resource(plugin, ui);
        if (gone || !frame?.contentWindow) return;
        const csp = res._meta?.ui?.csp;
        const b = new AppBridge(
          null,
          { name: 'zen', version: '0' },
          { openLinks: {}, serverTools: {}, logging: {}, message: { text: {} }, updateModelContext: { text: {}, image: {}, structuredContent: {} }, sandbox: { csp } },
          { hostContext: context() },
        );
        b.oncalltool = async (p) => {
          try {
            return (await pluginApi.call(plugin, p.name, p.arguments ?? {}, session())) as CallToolResult;
          } catch (e) {
            return { content: [{ type: 'text', text: (e as Error).message }], isError: true };
          }
        };
        b.onopenlink = async ({ url }) => {
          const nav = zenNavigation(url);
          if (nav?.action === 'close') {
            closeFile(pane);
            setView(pane, 'files');
            return {};
          }
          if (nav?.action === 'task') {
            try {
              const response = await fetch(`/api/zen/tasks/${encodeURIComponent(nav.id)}`, {
                method: 'PATCH', headers: { 'Content-Type': 'application/json', 'X-Zen-Web': '1' },
                body: JSON.stringify({ done: nav.done }),
              });
              if (!response.ok) throw new Error(`HTTP ${response.status}`);
              return {};
            } catch (e) {
              notice(`Could not update task: ${(e as Error).message}`);
              return { isError: true };
            }
          }
          if (nav?.action === 'open' && app.vault) {
            openFile(`${app.vault}/${nav.path}`, pane, nav.mode === 'edit');
            return {};
          }
          if (webLink(url)) {
            window.open(url, '_blank', 'noopener,noreferrer');
            return {};
          }
          return { isError: true };
        };
        b.onmessage = async ({ content }) => {
          const text = resultText({ content: content as { type: string; text?: string }[] }).trim();
          // Plugin text is untrusted: queue only, never invoke the prompt/command path.
          if (text && !app.session && text.length <= 100000) {
            app.chatDraft = `[From ${title}]\n${text}`;
            openSession('new', 'zenbot');
            return {};
          }
          if (text && chat.queue?.(plugin, text)) return {};
          notice(chat.queue ? `${title}: draft could not be queued (empty, read-only, or queue full)` : `${title} wants to write in the chat: open a session first`);
          return { isError: true };
        };
        b.onupdatemodelcontext = async (p) => {
          const text = resultText(p as { content?: { type: string; text?: string }[]; structuredContent?: unknown });
          keepContextSaving(app.session, title, text, contextImages(p.content));
          return {};
        };
        b.onloggingmessage = (m) => console.debug(`[${plugin}]`, m.level, m.data);
        b.fallbackNotificationHandler = async (n) => {
          if (n.method === 'zen/commands') setCommands(n.params);
          else if (n.method === 'zen/key') forwardKey(n.params);
        };
        b.oninitialized = () => {
          ready = true;
          // Plain copies: postMessage can't clone the page's reactive state.
          if (input) void b.sendToolInput({ arguments: $state.snapshot(input) });
          if (result) void b.sendToolResult($state.snapshot(result) as CallToolResult);
        };
        await b.connect(new PostMessageTransport(frame.contentWindow, frame.contentWindow));
        if (gone) return void b.close();
        bridge = b;
        frame.srcdoc = withCsp(res.text, csp);
      } catch (e) {
        if (!gone) error = (e as Error).message;
      }
    })();
    return () => {
      gone = true;
      if (cmds) unregister(cmds);
      darkQuery.removeEventListener('change', theme);
      window.removeEventListener('resize', resize);
      window.removeEventListener('blur', focusIn);
      observer.disconnect();
      sizes.disconnect();
      void bridge?.close();
      bridge = null;
    };
  });

  // The theme, the file or the mode changed: the app hears it as a host context change.
  $effect(() => {
    const c = context();
    bridge?.setHostContext(c);
  });
</script>

<div class="pframe">
  {#if error}<p class="dim err">{title}: {error}</p>{/if}
  <iframe bind:this={frame} {title} sandbox={SANDBOX} referrerpolicy="no-referrer" data-plugin={plugin}></iframe>
</div>

<style>
  .pframe { height: 100%; min-height: 0; display: flex; flex-direction: column; }
  iframe { flex: 1; width: 100%; min-height: 0; border: 0; background: transparent; color-scheme: normal; }
  .err { margin: 0; padding: 10px 16px; font-size: 13px; }
</style>
