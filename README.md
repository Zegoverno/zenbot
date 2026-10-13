# zenbot

A maker tool that works like a chief of staff: the owner hands it jobs, operational work and building software alike, and it carries them end to end with Claude Code, Codex and API classifiers, so the owner's attention goes to the decisions that matter. A personal tool, developed in public.

Status: early, in daily use by the owner. What's built and what's next: [ROADMAP.md](ROADMAP.md); how it works: [DESIGN.md](DESIGN.md).

## Docs

| File | Read it for |
|---|---|
| [CONTEXT.md](CONTEXT.md) | What zenbot is for, how success is measured, principles, constraints. Read first |
| [ROADMAP.md](ROADMAP.md) | What's next, open decisions, technical debt, the finished phases |
| [DESIGN.md](DESIGN.md) | How the system works today, and the agreed target design |
| [SPEC.md](SPEC.md) | The modules zenbot is meant to have, long term |
| [MAP.md](MAP.md) | The code: files, routes, tables, settings, scripts. Read before changing anything |
| [DEVELOPMENT.md](DEVELOPMENT.md) | Build, test, run a dev kernel, upgrade, evals, CI |
| [DECISIONS.md](DECISIONS.md) | Decisions and why (D-001…) |
| [PROGRESS.md](PROGRESS.md) | What shipped, newest first |
| [AGENTS.md](AGENTS.md) | Rules for agents (and people) working on this repo |
| [INSTALL.md](INSTALL.md) | Installing on a fresh VM (written for a coding agent) |
| `docs/` | Deep dives: [context.md](docs/context.md) (what the model reads each turn), [brief.md](docs/brief.md) (briefs and verification), [worker-protocol.md](docs/worker-protocol.md), [client-protocol.md](docs/client-protocol.md), [matrix.md](docs/matrix.md) (the Matrix channel); `docs/research/` holds dated research snapshots (history) |
| [evals/README.md](evals/README.md) | Eval tasks and the report |

## Install

On Debian/Ubuntu with systemd and sudo ([INSTALL.md](INSTALL.md) has the steps and what the installer adds). Models run on your existing subscriptions: Claude through the Claude Code CLI and GPT through Codex, with zenbot's own prompt, tools and history; System One decisions call OpenRouter when configured.

```bash
git clone https://github.com/Zegoverno/zenbot.git ~/zenbot && ~/zenbot/install.sh
zen login      # signs in to Claude Code (Claude plan) and Codex (ChatGPT plan)
zen status
```

Update with `zen upgrade` (or `/upgrade` inside zen, which then restarts zen itself on the new version, back in the same session); `/restart` loads a version installed some other way. zen tells you when a new version is on GitHub ([INSTALL.md](INSTALL.md#updating)). Developing zenbot: [DEVELOPMENT.md](DEVELOPMENT.md).

Every session starts from the prompt files in `~/.zenbot/`: `agents/zenbot/SOUL.md` (who zenbot is, at bottom: yours to edit), `agents/zenbot/IDENTITY.md` (its character and how it works), `AGENTS.md` (its environment), `USER.md` (you: fill it in), and its short-term memory; then `AGENTS.md` (or `CLAUDE.md`) in each directory from `/` down to the workspace. zenbot writes default versions of its files when they're missing and never overwrites yours. What the model reads each turn: [docs/context.md](docs/context.md).

## Using zen

Run `zen` in your terminal. It opens on the sessions board: every session in Main, Jobs and Archived sections, running, idle or waiting on you (`?`: it asked questions you haven't answered; listed first and counted in the title), with the subagents working under each. ↑↓ choose, Enter opens one, `n` starts a new session, `/` filters, `a` shows archived ones, `r` refreshes, `q` quits; `/board` (or Esc on an empty input) comes back, and a running session keeps working meanwhile. Subagent sessions open read-only. In a session it's full screen: the conversation scrolls with PgUp/PgDn or the mouse wheel, and a side panel sits next to the chat: Ctrl+B opens it on a folder tree of `~/.zenbot` (`/files <dir>` shows another folder; ↑↓ move, →/Enter open a folder or file, ← fold, `.` dotfiles, Shift+Tab moves the keys between chat and panel, or click), and `/open <file>` shows a file there (reloaded as it changes; `/close` hides it). Runs of tool calls show as one line; Ctrl+O unfolds them.

```bash
zen              # the sessions board
zen -c           # continue the most recent session
zen -r [id]      # resume a session (picker when no id)
zen -m codex/gpt-6-sol     # default is claude/claude-opus-5-5
zen -e xhigh     # thinking level (default: the model's; `zen models` lists them)
zen --inline     # no full screen: the conversation goes to terminal scrollback, like Claude Code (or ZEN_INLINE=1)
```

Inside: `/new`, `/resume`, `/board`, `/model`, `/effort`, `/done`, `/rename <title>`, `/files`, `/open [file]`, `/close`, `/mouse` (wheel scrolling off, so the terminal can select text), `/archive`, `/jobs`, `/skills`, `/tools` (see below), `/trust` (see Web), `/upgrade`, `/restart`, `/help`, `/exit`. After each turn zenbot names the session (until you `/rename` it) and may suggest your next prompt in grey in the input: Tab takes it, then Enter sends it or you edit it first; typing your own replaces it (`zen suggestions` shows what became of them). Enter sends; Shift+Enter (or Alt+Enter, Ctrl+J) starts a new line; Esc interrupts zenbot, ↑↓ recall prompts, Ctrl-D exits.

`/done` records your verdict on the work so far: *accept* (done and good), *more* (same goal, keep working), *reshape* (the framing was wrong) or *drop*. Verdicts are the ground truth zenbot is measured and tuned by.

For scripts, every command accepts `--json` and exits non-zero on failure:

```bash
zen ask "Find large files in ~ and summarize"     # one task: streams the answer, tools on stderr; Ctrl-C stops it, twice leaves
echo "notes…" | zen ask "Summarize this" --json   # prompt from stdin, JSON result
zen ask -s 3f2a "And now fix it"                  # continue a session (id or prefix)
zen ask -m claude/claude-sonnet-5-5 -e low "…"    # model and thinking level for a new session
zen sessions ls | show <id> | new | archive <id> | restore <id> | rename <id> <title>
zen sessions decide <id> accept|more|reshape|drop [-n note]   # same as /done
zen memory [--tier short|long|archived|all]       # what zenbot remembers, and the last sleep
zen memory sleep                                  # tidy short-term memory now
zen suggestions                                   # suggested next prompts: accepted, edited, declined
zen jobs [runs [name]]                            # scheduled jobs, and their reports
zen jobs add <name> -s "0 7 * * 1-5" -p "…"       # run a prompt on a schedule (also: pause|resume|rm|run|set)
zen skills                                        # skills, drafts and made tools, with use and fingerprints
zen skills accept|reject <domain/name> <fingerprint>
zen tools accept|reject <name> <fingerprint>
zen policy                                        # which model gets delegated work, and the evidence
zen policy set <kind> <model> [--candidates a,b] | undo
zen models                                        # models and their thinking levels, [default]
zen status
zen upgrade [--check]
```

## What zenbot does for you

How each of these works is in [DESIGN.md](DESIGN.md) ("As built"); here is what you see and set.

- **Skills, `verify` and `ask`.** zenbot gets tools and skills rather than a fixed procedure ([docs/brief.md](docs/brief.md)). Skills live in `~/.zenbot/global/skills/<domain>/<name>/SKILL.md` ([agentskills.io](https://agentskills.io) format) and load when a job matches; it starts with `work/brief`, `work/verify` and `work/close`. `verify` has the kernel run a job's checks, with a fresh read-only verifier for judgment calls; `ask` brings you up to three questions, each with options and a recommendation.
- **Memory.** zenbot saves where things stand with `remember` (`MEMORY.md`, a fixed size, shown from the next session) and writes lasting traits into `IDENTITY.md` (about itself) or `USER.md` (about you), backed up in `~/.zenbot/backups/prompt-files/` on every change. A nightly sleep keeps what's likely needed, archives the rest and promotes lasting entries; the morning note says what moved. `search` finds anything said or done in earlier sessions. Settings: `ZEN_MEMORY_CHARS` (size, default 4000), `ZEN_S1_PRIVATE=0` (keep your content away from the System One model).
- **Skills and tools it improves itself.** A new skill stays a draft until you accept it (or a session that used it is accepted), and a new kind of work needs your OK; a tool it makes runs sandboxed, with no network, until you approve it. Accepting names the fingerprint `zen skills` showed and is refused if the content changed since.
- **Scheduled jobs.** Ask for one ("every weekday at 7, brief me on …") or `zen jobs add` it. Each run is a fresh session (`job: <name>`) that ends with a report, or stays silent when there's nothing new; `zen jobs runs` shows reports. A job zenbot creates goes live only when a fast model judges, from your own words, that you asked for it; otherwise it waits paused until you `zen jobs resume` it. The same commands work from the chat, in zen (`/jobs`, `/jobs run|resume|pause <name>`, `/jobs runs [name]`, `/skills`, `/skills accept|reject <domain/name> <sha>`, `/tools accept|reject <name> <sha>`) and in Matrix (`!jobs …`, `!skills …`, `!tools …`): the client runs them with your token, and they never reach zenbot, so nothing it reads can approve anything. Schedules: cron (`0 7 * * 1-5`, in America/Sao_Paulo unless `--tz` or `ZEN_TZ`), `every 2h`, `at 2026-10-12 07:00`, `in 30m`. The same scheduler runs zenbot's own nightly upkeep and engine updates (`zen jobs`).
- **Subagents.** zenbot hands side tasks to subagents and can run several at once; which model each uses follows a routing policy that learns from your verdicts (`zen policy`).
- **The wiki.** Lasting knowledge in `~/.zenbot/global/wiki/`, a git repository of markdown pages you can read and edit (`index.md` lists them, `log.md` every capture).
- **Matrix.** Talk to zen from your phone with any Matrix client: one encrypted room per session, answering only you, plus a "zen jobs" room for reports. Setup: [docs/matrix.md](docs/matrix.md).

## The web and connected services

- **Web**: `web_search` and `web_fetch` work out of the box through a SearXNG container next to Postgres; set `BRAVE_API_KEY` or `TAVILY_API_KEY` in `~/.zenbot/env` to use those instead. Everything read from the web is untrusted, and so is what a session that read it saves (`zen memory` and `zen skills` show `untrusted`). Once a session has read untrusted content its shell runs offline and writes only in the workspace (builds, tests and commits work; installs and pushes don't); zen marks it `untrusted` on the board and in the footer. `/trust` (Matrix: `!trust`) shows what it read; if you trust that, `/trust yes` lifts the limits from then on (the next untrusted read sets them again, and what it saved meanwhile stays marked). Or continue in a fresh session.
- **MCP servers** (email, calendar, documents, your own services): list them in `~/.zenbot/mcp.json`, the same shape Claude Code uses. Secrets go in `~/.zenbot/env` and are referenced as `${VAR}`; a variable named like a secret (`…TOKEN`, `…KEY`, `…SECRET`, `…PASSWORD`) is passed only to a server that lists it in `allow_env`:
  ```json
  { "mcpServers": {
      "files": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/docs"] },
      "issues": { "url": "https://mcp.example.com/mcp", "headers": { "Authorization": "Bearer ${ISSUES_TOKEN}" }, "allow_env": ["ISSUES_TOKEN"] } } }
  ```
  Per server: `enabled`, `timeout_s`, `include` / `exclude` (tool names), `allow_env`, `untrusted` (default true: its output is untrusted and taints the session; set `"untrusted": false` for a server you trust). Only you edit `mcp.json`. `curl -H "Authorization: Bearer $(cat ~/.zenbot/token)" localhost:8100/api/mcp` shows what loaded.

## Settings you might set

In `~/.zenbot/env` (every setting, with defaults: [MAP.md](MAP.md#environment-variables)):

- **Context:** `ZEN_CONTEXT_TOKENS` (budget, default 200000; past 70% older turns are summarized and stay readable with `history`), `ZEN_SUMMARY_MODEL` (default `claude/claude-sonnet-5-5`), `ZEN_COMPACT_SOFT` / `ZEN_COMPACT_HARD` / `ZEN_COMPACT_KEEP` / `ZEN_COMPACT_IDLE_SECS`, and `ZEN_CLAUDE_RESUME=0` / `ZEN_CODEX_RESUME=0` to run without engine sessions.
- **System One and live scoring** (optional): a fast classifier that reranks search, judges the sleep, gates jobs and answers fixed questions about each session (from your messages and the final answers only) into `session_scores`:
  ```
  ZEN_S1_MODEL=openrouter/typesafe/jev-1.13
  OPENROUTER_API_KEY=sk-or-…
  ```
  `ZEN_SCORE_IDLE_SECS` sets the quiet time before a session is scored (default 7200).
- **Trusted repositories:** `ZEN_TRUSTED_REPOS` lists repositories whose `AGENTS.md` zenbot follows; any other project's instructions come as untrusted content.

Every turn is recorded with what produced it (build, engine and version, model, thinking level), tokens, cost, time and cache use; evals compare two harness versions on fixed tasks ([evals/README.md](evals/README.md)).
