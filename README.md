# zenbot

A maker tool that works like a chief of staff: the owner hands it jobs, operational work and building software alike, and it carries them end to end with Claude Code, Codex and API classifiers, so the owner's attention goes to the decisions that matter. A personal tool, developed in public.

Status: early. The kernel, the `zen` terminal app, Claude Code / Codex engines and context management are built and in use. The redesign around tools, skills and memory agreed on 2026-10-06 is being built ([ROADMAP.md](ROADMAP.md)): prompt files, short-term memory with a nightly sleep, skills, and briefs and verification as a skill and a tool are in.

## Docs

| File | Read it for |
|---|---|
| [CONTEXT.md](CONTEXT.md) | What zenbot is for, how success is measured, principles, constraints. Read first |
| [ROADMAP.md](ROADMAP.md) | Phases, the active one, next steps, debt, open decisions |
| [DESIGN.md](DESIGN.md) | How the system works today, and the agreed target design |
| [SPEC.md](SPEC.md) | The modules zenbot is meant to have, long term |
| [MAP.md](MAP.md) | The code: files, routes, tables, settings, scripts. Read before changing anything |
| [DEVELOPMENT.md](DEVELOPMENT.md) | Build, test, run a dev kernel, upgrade, evals, CI |
| [DECISIONS.md](DECISIONS.md) | Decisions and why (D-001…) |
| [PROGRESS.md](PROGRESS.md) | What shipped, newest first |
| [AGENTS.md](AGENTS.md) | Rules for agents (and people) working on this repo |
| [INSTALL.md](INSTALL.md) | Installing on a fresh VM (written for a coding agent) |
| `docs/` | Deep dives: [context.md](docs/context.md) (what the model reads each turn), [brief.md](docs/brief.md), [worker-protocol.md](docs/worker-protocol.md), [client-protocol.md](docs/client-protocol.md) |

## Install

Requirements: Debian/Ubuntu with systemd and sudo. The installer adds Docker, Node, and the Claude Code and Codex CLIs, and downloads prebuilt zenbot binaries (it installs Rust and compiles only when there are none for your platform or commit). See [INSTALL.md](INSTALL.md).

Models run on your existing subscriptions: Claude (Opus, Sonnet, Haiku) through the Claude Code CLI and GPT through Codex, with zenbot's own prompt, tools and history ([how](docs/worker-protocol.md)). System One typed decisions call OpenRouter directly when configured.

```bash
git clone https://github.com/Zegoverno/zenbot.git ~/zenbot && ~/zenbot/install.sh
zen login      # signs in to Claude Code (Claude plan) and Codex (ChatGPT plan)
zen status
```

Update with `zen upgrade` (or `/upgrade` inside zen, which then restarts zen itself on the new version, back in the same session); `/restart` loads a version installed some other way (zen notices it). zen tells you when a new version is on GitHub (see [INSTALL.md](INSTALL.md#updating)). The Claude Code and Codex CLIs update themselves daily, tested, with rollback ([engines](INSTALL.md#engines)).

For development: `./scripts/dev.sh` runs a kernel from the checkout next to the service, on port 18100 with its own `zen_dev` database (`ZEN_FAUX=1 ./scripts/dev.sh` adds `faux/smoke`, a scripted test model). See [DEVELOPMENT.md](DEVELOPMENT.md).

Every session starts from the prompt files in `~/.zenbot/`: `agents/zenbot/SOUL.md` (who zenbot is, at bottom: yours to edit), `agents/zenbot/IDENTITY.md` (its character and how it works), `AGENTS.md` (its environment), `USER.md` (you: fill it in), and its short-term memory; then `AGENTS.md` (or `CLAUDE.md`) in each directory from `/` down to the workspace. zenbot writes default versions of its files when they're missing and never overwrites yours. The home is split by scope, ready for more agents: system-wide files at the top, each agent's own under `agents/<name>/`, and the knowledge they share (memory copy, wiki, skills, tools) under `global/`.

## Using zen

Run `zen` for an interactive session in your terminal. It's full screen: the conversation scrolls with PgUp/PgDn or the mouse wheel, and a side panel sits next to the chat: Ctrl+B opens it on a folder tree of `~/.zenbot` (`/files <dir>` shows another folder; ↑↓ move, →/Enter open a folder or file, ← fold, `.` dotfiles, Tab moves the keys between chat and panel, or click), and `/open <file>` shows a file there (reloaded as it changes; `/close` hides it). Runs of tool calls show as one line; Ctrl+O unfolds them.

```bash
zen              # new session
zen -c           # continue the most recent session
zen -r [id]      # resume a session (picker when no id)
zen -m codex/gpt-6-sol     # default is claude/claude-opus-5-5
zen -e xhigh     # thinking level (default: the model's; `zen models` lists them)
zen --inline     # no full screen: the conversation goes to terminal scrollback, like Claude Code (or ZEN_INLINE=1)
```

Inside: `/new`, `/resume`, `/model`, `/effort`, `/done`, `/rename <title>`, `/files`, `/open [file]`, `/close`, `/mouse` (wheel scrolling off, so the terminal can select text), `/archive`, `/upgrade`, `/restart`, `/help`, `/exit`. Enter sends; Shift+Enter (or Alt+Enter, Ctrl+J) starts a new line or paragraph; Esc interrupts zenbot, ↑↓ recall prompts, Ctrl-D exits.

For scripts, every command accepts `--json` and exits non-zero on failure:

```bash
zen ask "Find large files in ~ and summarize"     # one task: streams the answer, tools on stderr
echo "notes…" | zen ask "Summarize this" --json   # prompt from stdin, JSON result
zen ask -s 3f2a "And now fix it"                  # continue a session (id or prefix)
zen sessions ls | show <id> | new | archive <id> | restore <id> | rename <id> <title>
zen sessions decide <id> accept|more|reshape|drop [-n note]   # same as /done
zen memory [--tier short|long|archived|all]       # what zenbot remembers, and the last sleep
zen memory sleep                                  # tidy short-term memory now
zen jobs [runs [name]]                            # scheduled jobs, and their reports
zen jobs add <name> -s "0 7 * * 1-5" -p "…"       # run a prompt on a schedule (also: pause|resume|rm|run|set)
zen ask -m claude/claude-sonnet-5-5 -e low "…"    # model and thinking level for a new session
zen models                                        # models and their thinking levels, [default]
zen status
zen upgrade [--check]
```

## How a session works

A session is one job. zenbot gets tools, skills and memory rather than a fixed procedure ([docs/brief.md](docs/brief.md)):

- **Skills** are how to do a kind of work well: `~/.zenbot/global/skills/<domain>/<name>/SKILL.md` ([agentskills.io](https://agentskills.io) format). Only their names and descriptions are in the instructions; zenbot loads one when a job matches it. It starts with `work/brief` (frame a big, risky or unclear job: the real goal, scope, assumptions, criteria as commands) and `work/verify` (prove it before saying it's done).
- **`verify`**: the kernel runs the criteria's commands itself, and when some need judgment a fresh verifier (no history, read-only) reads the diff and judges them.
- **`ask`**: up to three questions only you can answer, each with options and a recommendation; the turn ends until you reply.
- **Memory**: zenbot saves where things stand with `remember` (`MEMORY.md`, a fixed size, shown from the next session). Traits, guidance and preferences aren't memory: when one really matters, zenbot writes it straight into `IDENTITY.md` (about itself) or `USER.md` (about you) and keeps both compact (a full file is rewritten tighter, never cut); every version is backed up in `~/.zenbot/backups/prompt-files/`. A session that read the web, or a subagent, can't edit those files. Every night (the kernel's `sleep` job) a sleep keeps what's most likely needed, archives the rest (never deletes, but out of search) and promotes lasting entries on its own: about you into `USER.md`, about how it works into `IDENTITY.md` (backed up first, compacted when full), lasting knowledge into the wiki. The morning note lists what moved. `zen memory` shows it; `~/.zenbot/global/MEMORY.md` is a copy to read. `search` finds anything said or done in earlier sessions. Settings: `ZEN_MEMORY_CHARS` (size, default 4000), `ZEN_S1_PRIVATE=0` (keep memories away from the System One model; the sleep then ranks by recency).

`/done` records your verdict on the work so far.

## Skills and tools zenbot improves itself

zenbot keeps its know-how as skills (`~/.zenbot/global/skills/`, a git repository) and can write tools of its own (`~/.zenbot/global/tools/`). It improves a skill when a job shows what works, rather than adding near-duplicates; a new skill stays a draft until you accept it (or a session that used it is accepted), and a new kind of work needs your OK. A tool it makes runs sandboxed, with no network, until you approve it. `zen skills` shows each skill's use; `zen skills accept|reject <domain/name>` and `zen tools accept|reject <name>` decide.

## Scheduled jobs

zenbot runs work on a schedule without you in the conversation. Ask for it ("every weekday at 7, brief me on …") and it creates the job with its `schedule` tool, or add one yourself with `zen jobs add`. Each run is a fresh session (`job: <name>` in your sessions) with only the instructions the job picks (the soul always; by default your profile and memory) and the skills it names; it can't ask you, delegate or schedule more jobs, takes no outward action, and ends with a report, or stays silent when there's nothing new. `zen jobs runs` shows the reports. A job zenbot creates goes live only when a fast model, reading your own words in that conversation, judges you asked for it; otherwise it waits paused until you `zen jobs resume` it. The same scheduler runs zenbot's own upkeep: the nightly memory sleep and the daily engine updates. Schedules: cron (`0 7 * * 1-5`, in America/Sao_Paulo unless `--tz`), `every 2h`, `at 2026-10-12 07:00`, `in 30m`.

## Subagents

zenbot hands side tasks to subagents (fresh sessions with its tools and memory) and can run several at once. Which model a subagent uses follows a routing policy per kind of work that learns from your verdicts: `zen policy` shows the evidence, `zen policy set <kind> <model> [--candidates a,b]` routes, `zen policy undo` goes back.

## The wiki

zenbot keeps lasting knowledge in `~/.zenbot/global/wiki/` (a git repository of markdown pages you can read and edit): one page per concept, project, decision or person, each a short summary over a dated timeline with sources. It adds to it with `capture` (System One finds the right page and skips what's already there) and finds pages with `search`. `index.md` lists the pages; `log.md` lists every capture.

## The web and connected services

- **Web**: `web_search` and `web_fetch` work out of the box. Search goes through a SearXNG container the installer starts next to Postgres (only reachable from the VM); set `BRAVE_API_KEY` or `TAVILY_API_KEY` in `~/.zenbot/env` to use those instead. Fetching reaches public addresses only. Everything read from the web is marked untrusted, and a session that read it can't save memories that count as the owner's words.
- **MCP servers** (email, calendar, documents, your own services): list them in `~/.zenbot/mcp.json`, the same shape Claude Code uses. zenbot finds their tools with `find_tools` and runs them with `call_tool`, so its tool list (and the prompt cache) never changes. Secrets go in `~/.zenbot/env` and are referenced as `${VAR}`:
  ```json
  { "mcpServers": {
      "files": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/docs"] },
      "issues": { "url": "https://mcp.example.com/mcp", "headers": { "Authorization": "Bearer ${ISSUES_TOKEN}" } } } }
  ```
  Per server: `enabled`, `timeout_s`, `include` / `exclude` (tool names), `untrusted` (default true for remote servers). `curl -H "Authorization: Bearer $(cat ~/.zenbot/token)" localhost:8100/api/mcp` shows what loaded.

## Context

How zen builds what the model reads each turn is in [docs/context.md](docs/context.md). In short: the instructions are fixed for the session (an edited AGENTS.md applies from the next session), history is only ever appended to, and each session keeps a matching Claude Code or Codex session so earlier turns come from the provider's cache. When a session passes 70% of its context budget, older turns are summarized; the model can still read any of them with its `history` tool. Secrets in tool output are masked.

Settings in `~/.zenbot/env`: `ZEN_CONTEXT_TOKENS` (budget, default 200000), `ZEN_SUMMARY_MODEL` (default `claude/claude-sonnet-5-5`), `ZEN_COMPACT_SOFT` / `ZEN_COMPACT_HARD` / `ZEN_COMPACT_KEEP` / `ZEN_COMPACT_IDLE_SECS`, and `ZEN_CLAUDE_RESUME=0` / `ZEN_CODEX_RESUME=0` to run without engine sessions.

## Measuring zenbot

Every turn is recorded in the `turns` table with what produced it: the zenbot build (harness), the engine and its version, the model and the thinking level, plus tokens, cost, time and tool errors, what was sent (`context`) and whether the prompt cache could be reused (`cache_break`).

- **`/done`** records your verdict on a session's work so far: *accept* (done and good), *more* (same goal, keep working), *reshape* (the framing was wrong) or *drop*. These are the ground truth for everything else.
- **Live scoring** (optional): a System One model answers fixed questions about each session (kind of work, did you have to correct it, did it claim success without checking, outcome, how usable its answers were) once you decide or after two quiet hours, and the answers are stored in `session_scores`. Only your messages and the agent's final answers are sent, never tool output. To turn it on, add to `~/.zenbot/env`:
  ```
  ZEN_S1_MODEL=openrouter/typesafe/jev-1.13
  OPENROUTER_API_KEY=sk-or-…
  ```
  It calls OpenRouter directly; no extra worker is needed. `ZEN_SCORE_IDLE_SECS` changes the quiet time (default 7200).
- **Evals** compare two harness versions on fixed tasks with the same model: `scripts/eval.sh` (see [evals/README.md](evals/README.md)). Run them before changing the harness; the report is for you to decide on.

## Web UI (on hold)

The kernel still serves a minimal web chat at `http://<host>:8100/?token=$(cat ~/.zenbot/token)`. It works but is frozen while the CLI comes first.
