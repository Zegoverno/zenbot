# zenbot

A maker tool that works like a chief of staff: the owner hands it jobs, operational work and building
software alike, and it carries them end to end with Claude Code, Codex and small API classifiers, so
the owner's attention goes to the decisions that matter. A personal tool, developed in public.

Status: early, in daily use by the owner. What's next: [ROADMAP.md](ROADMAP.md).

## Docs

| File | Read it for |
|---|---|
| [CONTEXT.md](CONTEXT.md) | What zenbot is for, how success is measured, principles. Read first |
| [ROADMAP.md](ROADMAP.md) | What's next, open decisions, technical debt |
| [DESIGN.md](DESIGN.md) | How the system works, and the target design |
| [MAP.md](MAP.md) | The code: files, routes, tables, settings, scripts |
| [DEVELOPMENT.md](DEVELOPMENT.md) | Build, test, dev kernel, upgrade, CI |
| [DECISIONS.md](DECISIONS.md) | The rules in force (D-001…) |
| [PROGRESS.md](PROGRESS.md) | What shipped, newest first |
| [AGENTS.md](AGENTS.md) | Rules for agents (and people) changing this repo |
| [INSTALL.md](INSTALL.md) | Installing on a fresh VM |
| `docs/` | [context.md](docs/context.md) (what the model reads each turn), [worker-protocol.md](docs/worker-protocol.md), [client-protocol.md](docs/client-protocol.md), [matrix.md](docs/matrix.md); `docs/archive/` is history |
| [evals/README.md](evals/README.md) | Eval tasks and the report |

## Install

On Debian/Ubuntu (x86_64 or aarch64) with systemd and sudo ([INSTALL.md](INSTALL.md)). Models run on
your existing subscriptions: Claude through the Claude Code CLI, GPT through Codex, with zenbot's own
prompt, tools and history; System One decisions use OpenRouter when configured.

```bash
git clone https://github.com/Zegoverno/zenbot.git ~/zenbot && ~/zenbot/install.sh
zen login      # Claude Code (Claude plan) and Codex (ChatGPT plan)
zen status
```

Update with `zen upgrade` (or `/upgrade` inside zen, which then restarts zen on the new version in
the same session); `/restart` loads a version installed another way.

Every session starts from the prompt files in `~/.zenbot/`: `agents/zenbot/SOUL.md` (who zenbot is:
yours to edit), `agents/zenbot/IDENTITY.md` (its character), `AGENTS.md` (its environment), `USER.md`
(you: fill it in) and its short-term memory, then `AGENTS.md` (or `CLAUDE.md`) in each directory down
to the workspace. Missing files get defaults; yours are never overwritten. Details:
[docs/context.md](docs/context.md).

## Using zen

`zen` opens the sessions board (Main, Jobs, Archived; `?` marks a session waiting on your answers):
↑↓ choose, Enter opens, `n` new, `/` filter, `a` archived, `q` quit. In a session: PgUp/PgDn or the
wheel scroll, Enter sends, Shift+Enter (or Alt+Enter, Ctrl+J) adds a line, Esc interrupts, Tab takes
the grey suggested prompt, Ctrl+O unfolds tool calls, Ctrl+B opens the file panel, Ctrl-D exits.
Messages sent during a turn queue; `/steer message` corrects course at the next tool boundary.

```bash
zen -c | -r [id]           # continue the latest / resume a session
zen -m codex/gpt-6-sol -e xhigh   # model (default claude/claude-opus-5-5) and thinking level
zen --inline               # no full screen (or ZEN_INLINE=1)
```

Inside: `/new`, `/resume`, `/board`, `/model`, `/effort`, `/done`, `/rename`, `/files`, `/open`,
`/close`, `/mouse`, `/archive`, `/jobs`, `/skills`, `/trust`, `/queue`, `/allow`, `/deny`, `/upgrade`,
`/restart`, `/help`, `/exit`. `/done` records your verdict (*accept*, *more*, *reshape*, *drop*): the
ground truth zenbot is measured and tuned by.

Every command accepts `--json` and exits non-zero on failure:

```bash
zen ask "Summarize ~/notes"               # one task; stdin works too; -s <id> continues a session
zen sessions ls | show | new | archive | restore | rename | decide <id> accept|more|reshape|drop
zen memory [--tier short|long|archived|all] | zen memory sleep
zen brain list --args '{"store":"entity"}' | zen brain get --id <id>   # records and links
zen jobs [runs [name]] | add | pause | resume | rm | run | set
zen skills [accept|reject <domain/name> <fingerprint>]
zen queue                                 # held actions and source proposals
zen sources add docs.rs --note "Rust docs" | list | rm   # your source allowlist
zen trust [add|rm|suggest]                # sources that keep a session low risk
zen policy [set|undo]                     # which model each kind of task gets, and the evidence
zen s1 [check|models|replay|compare|proposals|approve|reject]   # System One decisions
zen mcp-login <server> | mcp-pending | mcp-respond <id> --action accept --data '{…}'
zen models | suggestions | status | upgrade [--check]
```

## What zenbot does for you

How each works: [DESIGN.md](DESIGN.md).

- **Skills, `verify` and `ask`.** Tools and skills rather than a fixed procedure. Skills live in
  `~/.zenbot/global/skills/<domain>/<name>/SKILL.md` ([agentskills.io](https://agentskills.io)
  format) and load when a job matches. A new or changed skill stays a draft until you accept its
  fingerprint. `verify` runs a job's checks (with a fresh read-only verifier for judgment calls);
  `ask` brings you up to three questions with options and a recommendation.
- **Memory and knowledge.** Short-term memory is `~/.zenbot/global/MEMORY.md` (size
  `ZEN_MEMORY_CHARS`, default 4000). A nightly sleep moves lasting memories to long-term memory.
  Long-term memories, knowledge, people, organizations and projects are Markdown files you own under
  `~/.zenbot/global/`; `search` finds them. Old flat wiki pages: preview `zen brain migrate_legacy`,
  apply with `--args '{"apply":true}'`.
- **Scheduled jobs.** Ask for one or `zen jobs add <name> -s "0 7 * * 1-5" -p "…"` (cron in
  America/Sao_Paulo unless `--tz` or `ZEN_TZ`; also `every 2h`, `at 2026-10-12 07:00`, `in 30m`).
  Each run is a fresh session ending in a report. `--command "…"` runs a script with no model;
  `--report-file` posts a file verbatim; `--post-to <session>` sends reports to a session. A job
  zenbot creates runs only when System One judges you asked for it, and a script job zenbot writes
  waits paused until you've read it and `zen jobs resume` it.
- **System One.** Fast decisions (relevance, source risk, job approval, on-job actions) go to a cheap
  classifier you approve, never the main model. Each decision is a file: copy one from
  `crates/zend/s1/` to `~/.zenbot/s1/` to edit; `zen s1 check` validates it; approved models are in
  `~/.zenbot/s1/models.toml`.
- **Subagents and routing.** zenbot delegates side tasks to subagents. Unless you pick a model, it
  routes each session, job run and subtask by kind of task, and switches routes on evidence from your
  verdicts; a switch to another provider or a much costlier model waits for you.
- **Matrix.** One encrypted room per session from your phone, plus a jobs room: [docs/matrix.md](docs/matrix.md).

## The web, MCP and safety

`web_fetch` reads public pages. For `web_search` set `ZEN_SEARXNG_URL` (an external SearXNG), or
`BRAVE_API_KEY` or `TAVILY_API_KEY`, in `~/.zenbot/env`.

MCP servers go in `~/.zenbot/mcp.json`, the shape Claude Code uses. Secrets live in `~/.zenbot/env`
as `${VAR}`; a secret-named variable reaches only a server listing it in `allow_env`. Per server:
`enabled`, `timeout_s`, `include`/`exclude`, `untrusted` (default true), `oauth`. `zen mcp-login
<server>` signs in to one; `/mcp` shows servers and tools. Only you edit `mcp.json`.

Every shell runs contained: no secrets, zen config, sudo, Docker or local services, network through
zenbot's proxy. Each session's risk is the highest of what it read: **low** (your words, repos,
trusted sources), **medium** (sources System One rated reputable), **high** (the open web). Before an
action with effect zenbot weighs that risk, the action and whether it follows your job: at low risk
everything on-job runs, including pushes (`host: true`); at medium pushes and new hosts wait; at high
shells are read-only; an off-job action after outside content locks the session until you look. What
waits is in `/queue` (`!queue` in Matrix, `zen queue`). `/deny` stops a session and what read its
output; `/trust yes` lifts its marks. `zen trust add url docs.rs` (or `repo github.com/you/*`, `mcp
calendar`) keeps sources low; `zen sources` is the allowlist of fetchable sources (levels 2–3).

## Settings you might set

In `~/.zenbot/env` (all settings: [MAP.md](MAP.md#settings)):

- **Context:** `ZEN_CONTEXT_TOKENS` (default 200000; older turns are summarized past 70%),
  `ZEN_SUMMARY_MODEL`, `ZEN_COMPACT_*`, `ZEN_CLAUDE_RESUME=0` / `ZEN_CODEX_RESUME=0`.
- **System One:** `ZEN_S1_MODEL=openrouter/typesafe/jev-1.13` with `OPENROUTER_API_KEY`, or several
  models in `~/.zenbot/s1/models.toml`; `ZEN_S1_PRIVATE=0` keeps your content from it.
- **Trusted repositories:** `ZEN_TRUSTED_REPOS`, whose `AGENTS.md` zenbot follows.

State lives in the private SQLite file `~/.zenbot/zen.sqlite3`. `scripts/storage.py` exports and
restores it as a versioned archive; back up prompt files and Markdown separately
([DEVELOPMENT.md](DEVELOPMENT.md#database-changes-and-existing-data-migration)).
