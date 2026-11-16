# Working on zen web

zen's web app: a Svelte page plus a Node server that relays the zen kernel. README.md says what it
does, its settings and its security rules; the code wins. The repo's own AGENTS.md applies too.

## Layout

- `server/`: the Node server, run with Node's type stripping (`node server/main.ts`). `main.ts`
  serves the page and the vault's notes; `zen.ts` relays the kernel (token added here, never sent
  to the page); `home.ts` the home folder (reads, and new files and folders and renames);
  `commands.ts` zen's kernel commands; `uploads.ts` chat attachments; `kernel.ts`/`ai.ts`/`check.ts`
  the writer's AI; `versions.ts` note versions. Tests: `*.test.ts`.
- `src/`: the page. `zen/` the zen app (board, chat, viewer), `editor/` the writer (CodeMirror),
  `doc/` the note format, `lib/` the API client.
- `scripts/zen.ts`: zenbot's side of note versions.
- `tests/e2e.mjs` (the writer), `tests/zen-e2e.mjs` (the zen app): the real server in dev mode,
  a stand-in kernel and headless Chromium (Playwright).

## Checks

```bash
npm ci
npm run check            # svelte-check + vitest
node tests/e2e.mjs
node tests/zen-e2e.mjs
```

`scripts/check.sh` at the repo root runs these too. UI changes come with an e2e assertion.

Keep the security rules in `server/main.ts`'s header: loopback only, the proxy's login header,
`X-Zen-Web: 1` on writes, paths inside the root, the kernel token never reaching the page.

## Install

The repo's `scripts/upgrade.sh` builds this folder with the kernel; `scripts/web-install.sh`
installs the build as the user unit `zen-web` on :4400 and rolls back if it doesn't answer.
Development runs separately (`ZEN_WEB_PORT=4401 npm run dev`), so the working copy never touches
the installed app.
