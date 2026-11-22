<script lang="ts">
  // The command list (Ctrl+K): one box, fuzzy search over what the focused panel can do now, the
  // global commands, the plugins' commands (runtime `zen/commands` of the focused plugin frame, and
  // the manifests' slash commands, which run like the chat's `/command`), the sessions, and files to
  // open (the server's name search, after a pause). Every row shows its shortcut. On a command row
  // Ctrl+E captures a new key (Backspace unbinds, Esc cancels); a key another command that can be
  // active at the same time holds asks "Take it from <X>? Enter / Esc" and unbinds X. A plugin
  // command whose default key clashes with a core key has no shortcut and a "clash" badge: the
  // same flow rebinds it. Overrides live in keys.svelte.ts (localStorage `zen.keys`).
  import { tick } from 'svelte';
  import Icon from './Icon.svelte';
  import { ui, active, focused, kindName, keyOf, clashed, findClash, isReserved, heldAsAlt, rebind, unbind, resetKey, changed, type Registered } from './keys.svelte.ts';
  import { chordOf, browserOwned, fuzzy, keyLabel } from './keys.ts';
  import { app, openSession, showFile, openDir, notice } from './app.svelte.ts';
  import { board } from './live.svelte.ts';
  import { chat, plugins } from './plugins.svelte.ts';
  import { files, type Found, type PluginCommand } from './api.ts';

  type Row = { id: string; kind: 'command' | 'slash' | 'session' | 'file'; title: string; sub?: string; icon: string; key?: string; badge?: string; cmd?: Registered; score: number; run: () => void };

  let q = $state('');
  let at = $state(0);
  let box = $state<HTMLInputElement>();
  let listEl = $state<HTMLElement>();
  let found = $state<Found[]>([]);
  /** Rebinding: waiting for the key, or asking whether to take it from another command. */
  let capture = $state<null | { cmd: Registered; chord?: string; other?: Registered }>(null);
  let say = $state('');

  $effect(() => {
    if (!ui.palette) return;
    q = ''; at = 0; capture = null; say = ''; found = [];
    tick().then(() => box?.focus());
  });

  // Files by name, under the vault, after a pause.
  $effect(() => {
    const t = q.trim();
    found = [];
    if (!ui.palette || t.length < 2 || !app.vault) return;
    const h = setTimeout(() => {
      files.find(app.vault, t).then((r) => { if (q.trim() === t) found = r.results.slice(0, 6); }).catch(() => {});
    }, 200);
    return () => clearTimeout(h);
  });

  const slash = $derived(plugins.list.filter((p) => p.enabled).flatMap((p) => (p.contributes.commands ?? []).map((c) => ({ ...c, plugin: p.name, title: p.title }))));

  function runSlash(c: PluginCommand) {
    const text = `/${c.name}`;
    const needsArg = !!c.args?.startsWith('<') && !c.choices?.length;
    if (chat.run) chat.run(needsArg ? `${text} ` : text, !needsArg);
    else {
      app.chatDraft = needsArg || c.choices?.length ? `${text} ` : text;
      openSession('new', app.agent);
    }
  }
  function open(path: string, dir: boolean) {
    if (dir) openDir(path); else showFile(path, { open: true });
  }

  const rows = $derived.by((): Row[] => {
    void ui.palette;
    const t = q.trim();
    const out: Row[] = [];
    const score = (text: string, bonus = 0) => { const s = fuzzy(t, text); return s === null ? null : s + bonus; };
    for (const c of active()) {
      if (c.quiet) continue;
      const s = score(c.title, 2);
      if (s !== null) out.push({ id: c.id, kind: 'command', title: c.title, sub: c.plugin ? `${plugins.list.find((p) => p.name === c.plugin)?.title ?? c.plugin}` : undefined, icon: c.icon, key: [c.hint, keyLabel(keyOf(c))].filter(Boolean).join(' · '), badge: clashed(c) ? 'clash' : undefined, cmd: c, score: s + (c.scope === 'global' ? 0 : 1), run: c.run });
    }
    for (const c of slash) {
      const s = score(`/${c.name} ${c.description ?? ''}`, 0.5);
      if (s !== null) out.push({ id: `slash:${c.plugin}/${c.name}`, kind: 'slash', title: `/${c.name}${c.args ? ' ' + c.args : ''}`, sub: c.description, icon: 'plugin', score: s, run: () => runSlash(c) });
    }
    const sessions = [...board.channels, ...board.sessions].flatMap((s) => { const sc = score(s.title); return sc === null ? [] : [{ s, sc }]; });
    sessions.sort((a, b) => b.sc - a.sc).slice(0, t ? 6 : 4).forEach(({ s, sc }) =>
      out.push({ id: `session:${s.id}`, kind: 'session', title: s.title, sub: s.state === 'waiting' ? 'needs you' : s.state === 'working' ? 'working' : s.when, icon: 'chat', score: sc - 1, run: () => openSession(s.id) }));
    for (const f of found) out.push({ id: `file:${f.path}`, kind: 'file', title: f.name, sub: f.rel, icon: f.dir ? 'files' : 'read', score: -5, run: () => open(f.path, f.dir) });
    // Typing sorts by match; with no query the panel's commands come first, then the global ones, then sessions.
    return (t ? out.sort((a, b) => b.score - a.score) : out).slice(0, 40);
  });

  $effect(() => { void rows; if (at >= rows.length) at = Math.max(0, rows.length - 1); });
  $effect(() => { void at; tick().then(() => listEl?.querySelector('.row.on')?.scrollIntoView({ block: 'nearest' })); });

  function close() { ui.palette = false; }
  function pick(i = at) {
    const r = rows[i];
    if (!r) return;
    close();
    // After the list has gone, so what the command focuses keeps it.
    setTimeout(() => { try { r.run(); } catch (e) { notice(String((e as Error).message ?? e)); } }, 0);
  }

  /** Ctrl+E on a row: wait for the new key. */
  function startCapture() {
    const c = rows[at]?.cmd;
    if (!c) return void (say = 'Only commands have keys to change');
    if (c.reserved) return void (say = `${keyLabel(c.key)} is zenbot's own and stays`);
    capture = { cmd: c };
    say = '';
  }
  function captureKey(e: KeyboardEvent) {
    const cap = capture!;
    e.preventDefault();
    e.stopPropagation();
    if (cap.other) {
      if (e.key === 'Enter') { rebind(cap.cmd, cap.chord!, cap.other); say = `${keyLabel(cap.chord)} is now ${cap.cmd.title}'s (taken from ${cap.other.title})`; capture = null; }
      else if (e.key === 'Escape') { capture = null; say = ''; }
      return;
    }
    if (e.key === 'Escape') { capture = null; return; }
    if (e.key === 'Backspace' || e.key === 'Delete') { unbind(cap.cmd); capture = null; say = `${cap.cmd.title} has no key now`; return; }
    if (e.key === 'Enter' && !e.ctrlKey && !e.metaKey && !e.altKey) { resetKey(cap.cmd); capture = null; say = `${cap.cmd.title} is back to its own key`; return; }
    const chord = chordOf(e);
    if (!chord) return;
    if (isReserved(chord)) { say = `${keyLabel(chord)} is zenbot's own: pick another`; return; }
    if (browserOwned(chord)) { say = `${keyLabel(chord)} belongs to the browser: pick another`; return; }
    const other = findClash(cap.cmd, chord);
    if (other && heldAsAlt(other, chord)) { say = `${keyLabel(chord)} is fixed for ${other.title}: pick another`; return; }
    if (other) { capture = { cmd: cap.cmd, chord, other }; return; }
    rebind(cap.cmd, chord);
    say = `${cap.cmd.title}: ${keyLabel(chord)}`;
    capture = null;
  }

  function onkey(e: KeyboardEvent) {
    if (capture) return captureKey(e);
    if (e.isComposing) return;
    const n = rows.length;
    if (e.key === 'ArrowDown' || (e.key === 'n' && e.ctrlKey)) { e.preventDefault(); at = n ? (at + 1) % n : 0; }
    else if (e.key === 'ArrowUp' || (e.key === 'p' && e.ctrlKey)) { e.preventDefault(); at = n ? (at - 1 + n) % n : 0; }
    else if (e.key === 'Enter') { e.preventDefault(); pick(); }
    else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); close(); }
    else if (e.key.toLowerCase() === 'e' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); startCapture(); }
  }
</script>

{#if ui.palette}
  <button class="scrim" aria-label="Close" onclick={close}></button>
  <div class="pal" role="dialog" aria-label="Command list" aria-modal="true">
    <div class="in">
      <Icon name="search" size={18} />
      <input bind:this={box} bind:value={q} onkeydown={onkey} oninput={() => (at = 0)} placeholder="Search commands, sessions and files in {kindName(focused())}…" aria-label="Command list search" spellcheck="false" autocomplete="off" />
      <kbd>Esc</kbd>
    </div>
    <div class="list" bind:this={listEl} role="listbox" aria-label="Commands">
      {#each rows as r, i (r.id)}
        <button class="row" class:on={i === at} role="option" aria-selected={i === at} onclick={() => pick(i)} onpointermove={(e) => { if (e.movementX || e.movementY) at = i; }} data-kind={r.kind}>
          <span class="ri"><Icon name={r.icon} size={16} /></span>
          <span class="rt">{r.title}{#if r.sub}<small>{r.sub}</small>{/if}</span>
          {#if r.badge}<span class="badge" title="Its default key is one zenbot already uses: Ctrl+E picks one">{r.badge}</span>{/if}
          {#if r.key}<kbd class:ch={r.cmd && changed(r.cmd)}>{r.key}</kbd>{:else if r.kind !== 'command'}<span class="kind">{r.kind === 'slash' ? 'plugin command' : r.kind}</span>{/if}
        </button>
      {:else}
        <div class="none">Nothing matches “{q}”.</div>
      {/each}
    </div>
    <div class="foot" role="status">
      {#if capture?.other}
        <span>Take {keyLabel(capture.chord)} from <b>{capture.other.title}</b>? <kbd>Enter</kbd> / <kbd>Esc</kbd></span>
      {:else if capture}
        <span>Press the new key for <b>{capture.cmd.title}</b> · <kbd>Backspace</kbd> unbinds · <kbd>Enter</kbd> resets · <kbd>Esc</kbd> cancels</span>
      {:else if say}{say}
      {:else}<span><kbd>↑</kbd><kbd>↓</kbd> move · <kbd>Enter</kbd> run · <kbd>Ctrl+E</kbd> change its key</span>{/if}
    </div>
  </div>
{/if}

<style>
  .scrim { position: fixed; inset: 0; z-index: 40; background: color-mix(in srgb, var(--bg) 45%, transparent); backdrop-filter: blur(3px); cursor: default; }
  .pal { position: fixed; z-index: 41; left: 50%; top: 14vh; transform: translateX(-50%); width: min(620px, calc(100vw - 24px)); max-height: 70vh; display: flex; flex-direction: column;
    background: var(--paper); border: 1px solid var(--faint); border-radius: 18px; box-shadow: 0 24px 70px var(--shadow); overflow: hidden; }
  .in { display: flex; align-items: center; gap: 10px; padding: 14px 16px; border-bottom: 1px solid var(--faint); color: var(--dim); }
  .in input { flex: 1; min-width: 0; border: 0; outline: 0; background: transparent; font-size: 16px; color: var(--ink); }
  .list { overflow: auto; padding: 6px; display: flex; flex-direction: column; gap: 1px; }
  .row { display: flex; align-items: center; gap: 10px; padding: 9px 10px; border-radius: 11px; text-align: left; font-size: 14px; }
  .row.on { background: color-mix(in srgb, var(--accent) 14%, var(--paper)); }
  .ri { display: grid; place-items: center; width: 26px; height: 26px; border-radius: 8px; background: var(--soft); color: var(--dim); flex: none; }
  .row.on .ri { color: var(--accent); }
  .rt { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .rt small { margin-left: 8px; color: var(--dim); font-size: 12px; }
  .kind { font-size: 11px; color: var(--dim); text-transform: uppercase; letter-spacing: 0.06em; }
  .badge { font-size: 11px; font-weight: 650; color: var(--warn); background: color-mix(in srgb, var(--warn) 16%, transparent); padding: 1px 8px; border-radius: 99px; }
  kbd { font-family: var(--sans); font-size: 12px; border: 1px solid var(--faint); border-radius: 6px; padding: 1px 6px; color: var(--dim); background: var(--paper); }
  kbd.ch { color: var(--accent); border-color: color-mix(in srgb, var(--accent) 45%, var(--faint)); }
  .none { padding: 18px; color: var(--dim); font-size: 14px; }
  .foot span { display: inline; }
  .foot { padding: 9px 16px; border-top: 1px solid var(--faint); font-size: 12.5px; color: var(--dim); display: flex; gap: 6px; align-items: center; flex-wrap: wrap; }
  .foot b { color: var(--ink); font-weight: 600; }
</style>
