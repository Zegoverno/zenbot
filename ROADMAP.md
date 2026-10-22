# zenbot — Roadmap

> What's next, open decisions and real technical debt. What shipped goes to
> [PROGRESS.md](PROGRESS.md); why, to [DECISIONS.md](DECISIONS.md); how it works, to
> [DESIGN.md](DESIGN.md).

Rules for every step: follow the best known practice, checked in reference projects' code, and say
where it comes from; use what ships on real work the week it ships; evaluate harness changes against
the installed version (`scripts/eval.sh`) and let the owner decide; record every load and call per
turn.

## Where things stand

`main` is what runs. The first redesign (D-025 to D-038: prompt files, memory, skills, web, MCP,
search, wiki, self-improvement, delegation) and the follow-ups (jobs D-046, names and suggestions
D-047, board D-048, Matrix D-049) are done. The second redesign, phases 1 to 10 (D-066 to D-077:
System One decisions, contained sessions and the owner queue, the second brain, rich tools, MCP
sign-in, script jobs, the learning router, engine accounting, SQLite storage, visible-text clients,
self-upgrading Matrix, aarch64 releases, and the docs rewrite), is done too.

## What's next

Dogfood zenbot on real work and let sessions, verdicts and what the owner reviews (promotions,
skill drafts, tools, routes, queue decisions) drive what changes. Candidates, each only when use
calls for it:

- **MCP:** sampling, roots, subscriptions; open the HTTP event stream (see debt).
- **Matrix:** emoji (SAS) verification of the bot; streamed answers, if waiting feels slow.
- **Jobs:** pre-run checks that skip the model when nothing changed, if job costs call for it.
- **Board:** a kernel-wide event stream instead of the 2 s poll, if that feels slow.
- **Suggestions:** improve the prompt from `zen suggestions` data after a few weeks.

## Open decisions

1. **Pilot project.** Which side project zenbot serves after zenbot itself.
2. **Privilege separation** (needs the owner's yes; see debt): `zend` as its own Unix user with a
   narrow sudoers drop-in.

## Technical debt

- **Host privileges.** Session shells are contained (bubblewrap, D-067), but `zend`, the kernel's
  upkeep scripts and owner-approved `host: true` commands run as the owner's Unix user, which on the
  standard install has passwordless `sudo`. Plan, not built: run `zend` as its own user without the
  owner's files, and replace blanket `sudo` with a drop-in allowing only
  `systemd-run … <repo>/scripts/apply-upgrade.sh` and the `systemctl` calls the upgrade needs.
- **Contamination reach** (`crates/zend/src/fsview.rs`): what a host-capable shell writes outside the
  workspace or in build and cache folders (`target`, `node_modules`, `.cache`, …) isn't registered,
  so a later deny can't mask it.
- **Permission checks are spread out.** What each kind of session may call (`agent::refusal`) and
  what a risky session may do (`taint::limited`, the gate) are decided centrally, but each tool asks
  and applies the answer itself.
- **Reading records through `bash`** bypasses the `brain` tool's provenance handover; the shell
  sandbox is the only boundary there.
- **Settings outside `crates/zend/src/config.rs`:** boot settings (`main.rs`), paths (`ZEN_HOME`,
  `ZEN_*_DIR`, `ZEN_MCP_CONFIG`), workers (`ZEN_WORKERS`, `ZEN_ENGINE_CMD`, `ZEN_WORKER_<NAME>_CMD`,
  `ZEN_MODELS`) and `ZEN_FAUX`; tests that set `ZEN_WORKERS` must pass values first.
- **Untyped client events:** the kernel builds them with `json!`, not `zen_proto::client`'s types.
- **Legacy schema carried into SQLite** (`crates/zend/sqlite-migrations/0001_init.sql`, expand-only):
  `sessions.state` and old tape kinds; the index `search_docs_unembedded` (replaced by
  `search_docs_to_embed`) and the old `memories`/`wiki` keys of `search_state`; `memories.tier =
  'long'` and `sleep_runs.proposed` (unused since D-045). Drop in a later release.
- **Search watermark** assumes tape events commit within 5 seconds of their timestamp
  (`crates/zend/src/search.rs`); a queue like the memories' would remove that assumption.
- **Web limits:** page and search caches hold 32 entries each, 15 minutes; a page keeps 2 MB of
  text from at most 5 MB downloaded.
- **`read`** refuses files over 20 MiB instead of streaming bounded lines.
- **MCP over HTTP** doesn't open the server's event stream, so `tools/list_changed` is missed until
  it reconnects; a timed-out stdio server restarts only when no call to it runs.
- **Untested with real models:** `remember`, `load_skill`, `verify`, `ask` on Codex; a model switch
  mid-session; a failed verification followed by a real fix.

## Long-term milestones

From the original plan; each is used on a real side project the week it ships.

| | Milestone | Status |
|---|---|---|
| M0 | Walking skeleton: kernel, sessions and tape, tools, worker, token, tracing | done (terminal app instead of a web UI) |
| M1 | Mind: sources, wiki, memory, review queue, search, `taste.record`, zenbot MCP server | built but taste and the MCP server (D-069) |
| M2 | Build loop: sandboxes, `run` in any language, services, dev preview, screenshots | contained shells (D-067); the rest open |
| M3 | Work: goals, tasks, board, agent definitions, `delegate`, inbox with push | board, `delegate`; the rest open |
| M4 | Router, context, budgets, bench | System One, router (D-074), context built; bench open |
| M5 | Closed loop: crons, runs and evals, reviewer, taste distillation | jobs built; taste open |
| M6 | Research and reach: web with taint rules, agent browser, Matrix, publish with approval | web, Matrix done; browser, publishing open |
| M7 | Ship to agents: project template with its own MCP server, skills, API, metering | open |

## Open questions (long-term)

1. License for the public repo (Apache-2.0, MIT or AGPL-3.0).
2. Web UI framework (Svelte, React, Lit), if a web UI comes back.
3. Sandbox granularity (per project or per session); when to move to gVisor.
4. Own minimal workflow engine or Windmill.
5. Public handle and devlog cadence.
