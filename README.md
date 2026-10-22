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
| `docs/` | Deep dives: [context.md](docs/context.md) (what the model reads each turn), [brief.md](docs/brief.md) (briefs and verification), [worker-protocol.md](docs/worker-protocol.md), [client-protocol.md](docs/client-protocol.md), [matrix.md](docs/matrix.md) (the Matrix channel); `docs/archive/research/` holds dated research snapshots (history) |
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

## Source allowlist

Only you change trusted sources: `zen sources add docs.rs --note "Rust docs"`,
`zen sources add x.com/paulg --risk 3`, `zen sources list`, `zen sources rm docs.rs`.
Entries cover an exact HTTPS host or whole path segments, not subdomains; levels are 2 (default)
or 3. Chat: `/sources add docs.rs 2 Rust docs`; Matrix uses `!sources`.
System One caches metadata-only source judgments for 30 days using assessed host/account/surface
scopes (page-only when uncertain). Public contributions never inherit maintained-code trust.
These judgments cannot authorize unseen URL requests; only your entries can.
Content checks can raise but never lower a rating and honor private-scoring opt-out. Levels 2–3
allow safe local work and labelled instructions; project code stays sandboxed or held from level 2. `zen sources rerate` revisits stored
authenticated web bytes; missing historical provenance stays conservative.
Fetching an allowlisted target is permitted in a risky session, including model-built URLs:
only add destinations you accept receiving those requests. Redirect destinations are checked too.

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

Inside: `/new`, `/resume`, `/board`, `/model`, `/effort`, `/done`, `/rename <title>`, `/files`, `/open [file]`, `/close`, `/mouse` (wheel scrolling off, so the terminal can select text), `/archive`, `/jobs`, `/skills`, `/tools` (see below), `/trust` (see Web), `/allow` and `/deny` (see Safety), `/upgrade`, `/restart`, `/help`, `/exit`. After each turn zenbot names the session (until you `/rename` it) and may suggest your next prompt in grey in the input: Tab takes it, then Enter sends it or you edit it first; typing your own replaces it (`zen suggestions` shows what became of them). Enter sends; Shift+Enter (or Alt+Enter, Ctrl+J) starts a new line; Esc interrupts zenbot, ↑↓ recall prompts, Ctrl-D exits.

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
zen memory sleep                                  # move lasting memories to long-term memory now
zen brain list --args '{"store":"entity"}'        # records: people, projects, knowledge, long-term memory
zen brain get --id ana-partner                    # one record, with its links (also: put, schema, lint, index)
zen suggestions                                   # suggested next prompts: accepted, edited, declined
zen jobs [runs [name]]                            # scheduled jobs, and their reports
zen jobs add <name> -s "0 7 * * 1-5" -p "…"       # run a prompt on a schedule (also: pause|resume|rm|run|set)
zen jobs add <name> -s "every 1h" --command "…"  # a script job: a shell command, no model; its output is the report
zen skills                                        # skills, drafts and retired tool files, with use and fingerprints
zen skills accept|reject <domain/name> <fingerprint>
zen tools accept|reject <name> <fingerprint>
zen mcp-login <server>                            # sign in to a remote MCP server through your browser (also /mcp login)
zen mcp-pending                                   # forms an MCP server is waiting on you to answer
zen mcp-respond <id> --action accept --data '{…}' # answer one (or --action decline|cancel)
zen policy                                        # which model gets delegated work, and the evidence
zen policy set <kind> <model> [--candidates a,b] | undo
zen trust [add <url|api|mcp|repo> <match> --level trusted|known|partly-open|blocked | rm …]   # what keeps a session low
zen s1                                            # System One: each decision's calls, outcomes, latency, cost
zen s1 check | models                             # check your decision files (line and column); approved models
zen s1 replay <decision> [--file edited.toml]     # what an edited decision would have decided on recorded answers
zen s1 compare <decision> --models a,b            # recorded decisions asked again on other approved models
zen s1 proposals | approve <id> | reject <id>     # changes the learner proposes to your decision files
zen models                                        # models and their thinking levels, [default]
zen status                                        # health, versions, jobs; warns when the disk is over 80% full
zen upgrade [--check]
```

## What zenbot does for you

How each of these works is in [DESIGN.md](DESIGN.md) ("As built"); here is what you see and set.

- **Skills, `verify` and `ask`.** zenbot gets tools and skills rather than a fixed procedure ([docs/brief.md](docs/brief.md)). Skills live in `~/.zenbot/global/skills/<domain>/<name>/SKILL.md` ([agentskills.io](https://agentskills.io) format) and load when a job matches; it starts with `work/brief`, `work/verify`, `work/close` and `work/search`. `verify` has the kernel run a job's checks, with a fresh read-only verifier for judgment calls; `ask` brings you up to three questions, each with options and a recommendation.
- **Memory and knowledge.** zenbot saves where things stand with `remember` into short-term memory (`~/.zenbot/global/MEMORY.md`, a fixed size, shown from the next session; edit it yourself if you like), and picks lasting sentences from what you say. A sleep (nightly, or when memory is full) moves lasting memories into long-term memory and archives finished ones. Long-term memories, knowledge (with how we know it) and people, organizations and projects are Markdown files you own under `~/.zenbot/global/` (`memory/`, `entities/`, the wiki); `search` finds them and follows their links. It writes lasting traits into `IDENTITY.md` (about itself) or `USER.md` (about you), backed up in `~/.zenbot/backups/prompt-files/`. Settings: `ZEN_MEMORY_CHARS` (size, default 4000), `ZEN_S1_PRIVATE=0` (keep your content away from the System One model). [Formats and commands](docs/second-brain.md).
- **Skills it improves itself.** A new skill, or a change to an active one, stays a draft until you accept its fingerprint. Repeatable procedures live in skills with supporting scripts. Old made-tool files remain inspectable; authoring, execution and approval are retired.
- **Scheduled jobs.** Ask for one ("every weekday at 7, brief me on …") or `zen jobs add` it. Each run is a fresh session (`job: <name>`) that ends with a report, or stays silent when there's nothing new; `zen jobs runs` shows reports. When a script builds a report that must reach you exactly (say, texts checked word for word), `--report-file out/message.md` makes the run write it there and zenbot posts the file, not its own answer; a missing, stale or out-of-place file fails the run instead. `--post-to <session>` sends a job's reports into one of your sessions instead of the jobs room, as zenbot's message you can answer there (in Matrix, `!session` shows a room's session id). A job zenbot creates goes live only when System One judges, from your own words, that you asked for it (`job_approval`); otherwise it waits paused until you `zen jobs resume` it. The same commands work from the chat, in zen (`/jobs`, `/jobs run|resume|pause <name>`, `/jobs runs [name]`, `/skills`, `/skills accept|reject <domain/name> <sha>`, `/tools accept|reject <name> <sha>`) and in Matrix (`!jobs …`, `!skills …`, `!tools …`): the client runs them with your token, and they never reach zenbot, so nothing it reads can approve anything. Schedules: cron (`0 7 * * 1-5`, in America/Sao_Paulo unless `--tz` or `ZEN_TZ`), `every 2h`, `at 2026-10-12 07:00`, `in 30m`. The same scheduler runs zenbot's own nightly upkeep and engine updates (`zen jobs`). A job that only runs a script can skip the model: `zen jobs add <name> -s … --command "python3 devotion.py angelus"` runs the command in the same contained shell zenbot uses, and its output (or `--report-file`) is the report; `zen jobs runs` shows each run's command and exit code. zenbot can write one too, but it waits paused until you've read the command and `zen jobs resume`d it, and again after zenbot changes it.
- **System One (S1).** Fast decisions (which parts of a page matter, how relevant a result is, whether you asked for a job, how risky a source is, whether an action follows the job) go to a cheap, calibrated classifier you approve, never to the main model. Each decision is a small file you can read and change: copy one from `crates/zend/s1/` into `~/.zenbot/s1/` and edit its questions, what each mistake costs (`false_yes = 9`, `false_no = 1` means "yes from 0.9") or its models; `zen s1 check` points at any mistake by line, and a broken file never takes effect. `~/.zenbot/s1/models.toml` lists the models you approve. zenbot itself asks it quick questions over many items with the `judge` tool. Your answers to held actions and paused jobs teach it; `zen s1 proposals` shows what it suggests changing.
- **Subagents.** zenbot hands side tasks to subagents and can run several at once; which model each uses follows a routing policy that learns from your verdicts (`zen policy`).
- **Knowledge.** Markdown records in `~/.zenbot/global/wiki/`, organized by kind, with evidence and typed links. For old flat pages, preview `zen brain migrate_legacy`, then apply with `zen brain migrate_legacy --args '{"apply":true}'`. Exact originals stay in `wiki/_legacy/`; conflicts are held without overwriting either version. `capture` is retired.
- **Matrix.** Talk to zen from your phone with any Matrix client: one encrypted room per session, answering only you, plus a "zen jobs" room for reports. Setup: [docs/matrix.md](docs/matrix.md).

## The web and connected services

- **Web**: `web_search` and `web_fetch` work out of the box through a SearXNG container next to Postgres; set `BRAVE_API_KEY` or `TAVILY_API_KEY` in `~/.zenbot/env` to use those instead. Everything read from the web is untrusted, and so is what a session that read it saves (`zen memory` and `zen skills` show `untrusted`). Once a session has read untrusted content project code runs offline with a read-only workspace (only private `/tmp` is writable); builds or commits that need more access require exact `/allow` approval; zen marks it `untrusted` on the board and in the footer. `/trust` (Matrix: `!trust`) shows what it read; if you trust that, `/trust yes` lifts the limits from then on (the next untrusted read sets them again, and what it saved meanwhile stays marked). Or continue in a fresh session.
- **MCP servers** (email, calendar, documents, your own services): list them in `~/.zenbot/mcp.json`, the same shape Claude Code uses. Secrets go in `~/.zenbot/env` and are referenced as `${VAR}`; a variable named like a secret (`…TOKEN`, `…KEY`, `…SECRET`, `…PASSWORD`) is passed only to a server that lists it in `allow_env`:
  ```json
  { "mcpServers": {
      "files": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/docs"] },
      "issues": { "url": "https://mcp.example.com/mcp", "headers": { "Authorization": "Bearer ${ISSUES_TOKEN}" }, "allow_env": ["ISSUES_TOKEN"] } } }
  ```
  Per server: `enabled`, `timeout_s`, `include` / `exclude` (tool names), `allow_env`, `untrusted` (default true: its output is untrusted and taints the session; set `"untrusted": false` for a server you trust)., `oauth` (only when a server needs it: `client_id` and `client_secret` of an app you registered there with the redirect `http://127.0.0.1:33418/callback`, or `token_url`, `client_id` and `refresh_token` you got elsewhere). A server that asks for sign-in: run `zen mcp-login <server>` (or `/mcp login <server>` in zen), open the link on any machine, approve, and paste back the address your browser ended on (its "can't connect" page is expected); zenbot keeps the tokens in `~/.zenbot/mcp-auth/` and renews them. `/mcp` shows each server's tools and sign-in. A server's resources and prompts show up in `find_tools` as `<server>_zen_resources_read`, `<server>_zen_prompts_get` and so on. When a server asks you something mid-call (a form), the session shows it as waiting: answer with `zen mcp-respond` within 5 minutes; zenbot never answers for you. Only you edit `mcp.json`. `curl -H "Authorization: Bearer $(cat ~/.zenbot/token)" localhost:8100/api/mcp` shows what loaded.

### Safety: let it run at low risk, stop when it's dangerous

Every shell runs contained: your secrets, the zen home's config, sudo, Docker and local services are
out of reach, and the network goes through zenbot's proxy (so builds and downloads work). Each
session has a context risk, the highest of what it read: **low** (your words, your repos, sources on
your trust list), **medium** (reputable sources System One rated), **high** (the open web, unknown).
Before an action with effect, zenbot weighs that, what the action does and, when needed, whether
System One reads it as following your job:

- low risk: everything runs, including pushes and PRs that follow the job (`host: true` commands);
- medium: local work and on-job memory run; pushes and new hosts wait unless trusted and on-job;
- high: shells run read-only; local file edits that follow the job run, other effects wait for you;
- an action that doesn't follow your job after outside content **locks** the session (local work
  only, offline) until you look.

What waits is one item in `/queue` (zen), `!queue` (Matrix) or `zen queue`. `zen trust add url docs.rs`
(or `repo github.com/you/*`, `mcp calendar`) keeps those sources low; `/deny` stops a session and
everything that read its output; `/trust` lifts its marks. `zen trust suggest` shows repeated clean
hosts to review and add explicitly. Owner-approved jobs keep their named sources and own folder;
a held job waits up to two hours for your queue decision and resumes when approved.

Messages sent during a turn queue in the kernel, including from Matrix. `/steer message` (Matrix `!steer message`) stops at the next tool boundary and continues with your correction.

Private history mirrors appear in `~/.zenbot/sessions/<session-id>.jsonl` within a few seconds and rebuild on restart. MCP stdio servers run contained with their own writable folder under `~/.cache/zenbot/mcp/`; network follows their trust level. `/api/mcp` reports cached state without starting servers.

## Settings you might set

In `~/.zenbot/env` (every setting, with defaults: [MAP.md](MAP.md#environment-variables)):

- **Context:** `ZEN_CONTEXT_TOKENS` (budget, default 200000; past 70% older turns are summarized and stay readable with `history`), `ZEN_SUMMARY_MODEL` (default `claude/claude-sonnet-5-5`), `ZEN_COMPACT_SOFT` / `ZEN_COMPACT_HARD` / `ZEN_COMPACT_KEEP` / `ZEN_COMPACT_IDLE_SECS`, and `ZEN_CLAUDE_RESUME=0` / `ZEN_CODEX_RESUME=0` to run without engine sessions.
- **System One** (optional): the fast classifier behind zenbot's quick decisions (see above). One model:
  ```
  ZEN_S1_MODEL=openrouter/typesafe/jev-1.13
  OPENROUTER_API_KEY=sk-or-…
  ```
  or several in `~/.zenbot/s1/models.toml` (`default = "jev"`, then `[model.jev] id = "typesafe/jev-1.13"`, one table per model), which decision files name in their `ladder`. `ZEN_S1_CONCURRENCY` (8) caps calls in flight; `ZEN_JUDGE_TOOL=0` hides `judge`.
- **Trusted repositories:** `ZEN_TRUSTED_REPOS` lists repositories whose `AGENTS.md` zenbot follows (their git worktrees too); any other project's instructions come as untrusted content.

Every turn is recorded with what produced it (build, engine and version, model, thinking level), tokens, cost, time and cache use; evals compare two harness versions on fixed tasks ([evals/README.md](evals/README.md)).
