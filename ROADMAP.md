# zenbot — Roadmap

> What's being built next, open decisions and technical debt. What shipped goes to
> [PROGRESS.md](PROGRESS.md); why, to [DECISIONS.md](DECISIONS.md); how it works, to
> [DESIGN.md](DESIGN.md).

Rules for every step: each choice follows the best known practice, checked in reference projects'
code, and says where it comes from (security included); what ships is used on real work the week it
ships; harness changes are evaluated against the installed version (`scripts/eval.sh`) and the owner
decides; every load and call is recorded per turn; System One is built into each tool as it lands.

## Where things stand

`main` is what runs. The kernel, the `zen` terminal app, the Claude Code / Codex engines (through
`zen-engine`; the Pi worker is retired, D-043) and context v2 (`docs/context.md`) came first. The
redesign agreed with the owner on 2026-10-06 (D-025 to D-030) was built in the phases below, all
done; scheduled jobs (D-046), session names and suggestions (D-047), the sessions board (D-048) and
the Matrix channel (D-049) followed, then a weekend of hardening (D-050 to D-053; PROGRESS.md,
2026-10-10 and 2026-10-11).

| Phase | Name | Delivered | Where it's recorded |
|---|---|---|---|
| 0 | Ground | The reference projects studied in their code; the open fix PRs (#8, #9, #10, #13) merged | `docs/research/`, PROGRESS.md 2026-10-06 |
| 1 | Foundation | Prompt files, tool descriptions, short-term memory with sleep, skills (#17; eval 12/12 → 12/12, cost +27%) | D-027, D-028, D-029, PROGRESS.md |
| 2 | Reach | `web_search`, `web_fetch`, MCP client, `find_tools` / `load_tool` / `call_tool` (#18) | D-033, D-034 |
| 3 | Recall | `search` (exact, full-text and semantic) over sessions and memories (#19) | D-035 |
| 4 | Knowledge | The wiki and `capture` (#20) | D-036 |
| 5 | Self-improvement | `save_skill`, `save_tool`, without sprawl (#21) | D-037 |
| 6 | Delegation | `delegate`: subagents; model choice from real usage (#22) | D-030, D-038 |

## What's next

Keep dogfooding zenbot on real work and let sessions, verdicts and what the owner reviews (the
sleep's promotions into `IDENTITY.md` and `USER.md`, skill drafts, tools, routes) drive what
changes. Candidate follow-ups, each only when use calls for it:

- **Action gate (D-056), in order:** a session that reads outside content becomes *risky*, not
  limited; its actions are checked before they run by fixed rules over parsed facts (run, run in
  the offline sandbox, or ask the owner, with a one-tap `/allow`); then System One reads only the
  owner's own request to grant the turn's outward actions (push, PR, deploy). Step 0's replay
  backs the rules (none of 36 attacks escaped; 71% of recorded commands run unchecked) and found
  System One scoring the actions themselves too weak to gate them. The first step, search not
  spreading the mark and worktrees of trusted repositories trusted, is built.
- **Matrix:** interactive (emoji) verification of the bot; streaming answers by editing a message,
  if waiting for the whole answer feels slow.
- **Jobs:** reports can now be posted into an existing owner session (D-060), with a durable
  delivery queue; pre-run checks that skip the model when nothing changed, if job costs call for it.
- **Board:** a kernel-wide event stream instead of the 2 s poll, if that feels slow.
- **Suggestions:** improve the suggestion prompt from `zen suggestions` data after a few weeks.
- **Prefix cost:** shorter descriptions for `verify`, `remember` and `decide`, if the fixed prefix's
  cost (Phase 1's +27%) calls for it.

## Open decisions

1. ~~What System One may see~~: decided (D-032): private content allowed by default,
   `ZEN_S1_PRIVATE=0` turns it off.
2. **Pilot project.** Which side project zenbot serves after zenbot itself.

## Technical debt

- Web page/search caches cap at 32 entries each; a page's text caps at 2 MB (5 MB downloaded) and
  entries expire after 15 minutes. The untrusted-content boundary and SSRF filtering remain security-critical.
- The kernel `read` tool refuses files over 16 MiB before loading them; it should eventually stream
  bounded lines so large text files can be paged without `bash`.
- The kernel's tunable settings go through `config.rs` (read once, typed, defaults in one place),
  but these are still read from the environment where they're used: the boot settings in
  `main.rs` (`ZEN_TOKEN`, `ZEN_PORT`, `ZEN_BIND`, `ZEN_WORKSPACE`, `ZEN_REPO`, `ZEN_DEFAULT_MODEL`,
  `ZEN_HARNESS`), the paths (`ZEN_HOME`, `ZEN_SKILLS_DIR`, `ZEN_WIKI_DIR`, `ZEN_TOOLS_DIR`,
  `ZEN_MCP_CONFIG`, read by `layout.rs` and their modules), the workers' (`ZEN_WORKERS`,
  `ZEN_ENGINE_CMD`, `ZEN_WORKER_<NAME>_CMD`, `ZEN_MODELS`) and `ZEN_FAUX`. `zen-engine`, `zen`
  and `zen-matrix` read their own settings directly. Moving the rest into `config.rs` needs the
  tests that set `ZEN_WORKERS` to pass values instead.
- The kernel still builds its client events with `json!` rather than from `zen_proto::client`'s
  types. Messages (Pi's format) and usage reports are untyped JSON in both protocols.
- The old workflow's schema stays (expand-only): `sessions.state` and old tape block kinds.
  Drop them in a later release; `policies` is in use by model routing.
- Unused since migration 0021 (expand-only): the index `search_docs_unembedded` (replaced by
  `search_docs_to_embed`) and the `memories` and `wiki` keys of `search_state`; drop them in a later
  release. The tape watermark still relies on events being committed within 5 seconds of their id
  (each is its own short transaction, `tape::append`); a queue like the memories' would remove that
  assumption.
- Prompt files (`SOUL`, `IDENTITY`, `AGENTS`, `USER`), active skills and made tools are guarded
  at `edit`/`write` for child and kernel sessions (subagents, verifiers, jobs) and tainted sessions
  (D-045, D-051), and a tainted session's `bash` can't write them (offline sandbox, D-053); the
  owner's config and secrets (`mcp.json`, `env`, `env.prev`, `token`, `db-password`, `matrix*`, `bin/`) for every session. A
  clean session's `bash` can still write all of them. A tainted session's `edit`/`write` can still
  change files outside the workspace that aren't guarded (shell start-up files, other repositories'
  hooks), which a later clean session may run. Unused since D-045: new `memories.tier = 'long'` rows (old ones stay
  searchable) and `sleep_runs.proposed` (always 0); drop them in a later release.
- Tools run as the same Unix user as the owner. Shell subprocesses no longer inherit kernel secrets,
  and the verifier's shell has its own processes, no network and an empty home (D-051), but an
  ordinary agent shell can still read the owner's files, `/proc/<zend pid>/environ` included.
  Strong isolation requires a separate Unix user and a credential the agent cannot read. Worse, on
  the standard install the service user has passwordless `sudo` and Docker access, so an untainted
  session's shell is effectively root. The plan, **not built and needing the owner's yes**: run
  `zend` as its own user without the owner's files; replace blanket `sudo` with a sudoers drop-in
  allowing only `systemd-run … <repo>/scripts/apply-upgrade.sh`, `systemctl restart zenbot` and
  `docker compose -f <repo>/deploy/compose.yaml …`; take the user out of the `docker` group (the
  scripts fall back to `sudo -n docker`, `scripts/db.sh`).
- What each kind of session may call is decided in one place (`agent::refusal`, an allow-list per
  kind), and what a tainted session may do in another (`taint::limited`, D-053), but each tool still
  asks it and applies the answer itself; the shell sandbox is chosen at the dispatcher.
- A draft activated because a session that loaded it was accepted (`workshop::on_accept`) isn't
  tied to the content that session loaded; the owner's own accept is (D-051). Reading a project's
  files with `read` or `bash` isn't taint, only its AGENTS.md is; reading stored tainted content
  (a wiki page with `web` entries, a marked draft) with `bash` (`cat`) isn't either, only with
  `read`, `search`, `history`, `load_skill` and `schedule runs`. The wiki's `index.md` lists a
  page's title even when the title came from untrusted text.
- MCP over HTTP doesn't open the server's event stream, so only stdio servers' `tools/list_changed`
  is seen; an HTTP server's tools are listed again only when it reconnects or `mcp.json` changes. A
  stdio server that times out is started again only when no other call to it is running.
- Paths never run with real models: the new tools (`remember`, `load_skill`, `verify`, `ask`) on
  Codex, a model switch mid-session, a failed verification followed by a real fix.

## Long-term milestones

The original milestones (SPEC v0.2). Each is used on a real side project the week it ships. The
phases above delivered parts of them; what remains isn't planned yet.

| | Milestone | Scope | Status |
|---|---|---|---|
| M0 | Walking skeleton | `zend` API/WS, Postgres, sessions and tape, file and shell tools, a model worker, owner token, tracing | done, with the `zen` terminal app instead of the web UI |
| M1 | Mind v0 over MCP | Projects and scopes on disk; sources; wiki and review queue; memory; `taste.record`; full-text search; zenbot MCP server | partly in Phases 1, 3, 4 |
| M2 | Build loop | Sandboxes; `run` in any language; services; dev preview, point & comment, screenshots; MCP client; skills | MCP and skills in Phases 1–2 |
| M3 | Work v0 | Goals, tasks, board; agent definitions; `delegate`; inbox with push; device pairing | `delegate` in Phase 6 |
| M4 | Router, context, bench | System One and embeddings; model selection; budgets; context manager; hybrid search; zen-bench | context done; search in Phase 3; model choice in Phase 6 |
| M5 | Closed loop | Crons; runs and evals; reviewer; taste distillation; crystallization | skills loop in Phase 5; crons (scheduled jobs, D-046) built |
| M6 | Research & reach | `web_search` / `web_fetch` with taint rules; agent browser; Matrix; publish with approval | web in Phase 2; Matrix built (D-049) |
| M7 | Ship to agents | Project template exposing a project's own MCP server, skills, API and metering | |

## Open questions (long-term)

1. License for the public repo (Apache-2.0, MIT or AGPL-3.0).
2. Web UI framework (Svelte, React, Lit), if the web UI comes back.
3. Sandbox granularity (per project or per session); when to move to gVisor.
4. ~~Search provider for `web_search`; first embedding model~~: decided (D-034: SearXNG keyless,
   Brave or Tavily with a key; D-035: `openai/text-embedding-3-small`).
5. Own minimal workflow engine or Windmill.
6. Public handle and devlog cadence.
