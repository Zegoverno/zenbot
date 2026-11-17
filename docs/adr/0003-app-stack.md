# ADR 0003: Plugin apps stay Svelte; thin shells per platform

Status: accepted (2026-11-17); the open points below are still open (proposed 2026-11-14)

## Context

The reader, the writer and Sudoku should each run in four ways:
- as a web app (hosted by us, or self-hosted for free);
- as a desktop app;
- as a mobile app on Android and iOS;
- as a zenbot plugin.

They are written today in Svelte 5 (with CodeMirror for the writer).

## Decision

- **Keep Svelte 5 + Vite + TypeScript.** The writer depends on CodeMirror, and the reader is
  typography and layout. Both are what the web platform does best. A rewrite in React Native or
  Flutter would mean rebuilding the editor for no gain. Svelte also compiles to small bundles,
  which suits an MCP App delivered as one HTML file.
- **One repo per app, one code base, thin shells:**

  ```
  core/     pure TS: the model and the rules (no UI, no I/O), unit-tested
  ui/       Svelte: the app
  mcp/      MCP server: tools + serves the built UI as ui://<app>/…
  shells/
    web/      PWA: static files, installable on desktop and Android, offline
    desktop/  Tauri 2 (Rust): local files, starts ACP agents
    mobile/   Capacitor: App Store and Play Store
  plugin.json
  ```

- **Two adapters** let the same UI run anywhere:
  - **storage:**
    - web: browser storage (OPFS/IndexedDB);
    - desktop and mobile: a folder;
    - in zenbot or Claude: the plugin's MCP tools.
  - **AI:**
    - desktop: ACP, where the user picks zenbot, Claude Code or Codex;
    - in zenbot: the chat it lives in;
    - web and mobile: none at first. Later, a connection to the user's zenbot.
- **Order of shells:** PWA first, for all three apps, since it is free and installs on PC and
  Android. Then Tauri desktop, when the writer needs ACP. Store builds last, when an app is
  ready to sell or list.
- **Hosting:** the web shell is static files, served free from Cloudflare Pages or GitHub Pages
  for our hosted copy. Anyone can self-host it with any static server.
- **Licence:** MIT, public repos from the start.

## Open points

- **Capacitor or Tauri for mobile.** Capacitor has the longer track record in the App Store. Tauri
  2 mobile would mean one shell for everything. To be decided by building Sudoku's iOS build both
  ways when we get there.
- **Sync between devices** for the writer and reader:
  - free: the user's own folder (iCloud, Drive, Syncthing, git);
  - paid: a hosted sync service later. That is the business model Obsidian proves.
- **App Store review** (guideline 4.2 on minimum functionality) can reject thin web wrappers.
  Sudoku works offline as a full game, which is the safest first listing.
