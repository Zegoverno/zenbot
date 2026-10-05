# zenbot

A personal + company operating system for niche builders: a machine for thinking, analyzing and building, with LLMs at the center and agents doing the work.

Status: design phase. See [SPEC.md](SPEC.md).

## Install

Requirements: Debian/Ubuntu with systemd and sudo. The installer adds Docker, Rust, Node, and the Claude Code and Codex CLIs. See [INSTALL.md](INSTALL.md).

Models run on your existing subscriptions: Claude (Opus, Sonnet, Haiku) through the Claude Code CLI and GPT through Codex, with zenbot's own prompt, tools and history ([how](docs/worker-protocol.md)). Pi is available as an optional worker (`ZEN_WORKERS=engine,pi`).

```bash
git clone https://github.com/Zegoverno/zenbot.git ~/zenbot && ~/zenbot/install.sh
zen login      # signs in to Claude Code (Claude plan) and Codex (ChatGPT plan)
zen status
```

Update with `zen upgrade` (or `/upgrade` inside zen); zen tells you when a new version is on GitHub (see [INSTALL.md](INSTALL.md#updating)).

For development without the service: `./scripts/dev.sh` (`ZEN_FAUX=1` adds `faux/smoke`, a scripted test model; see [docs/worker-protocol.md](docs/worker-protocol.md#testing-without-a-model)).

zenbot adds instruction files to every session's system prompt: `~/.zenbot/AGENTS.md` (global), then `AGENTS.md` (or `CLAUDE.md`) in each directory from `/` down to the workspace.

## Using zen

Run `zen` for an interactive session in your terminal (inline, like Claude Code, Codex or Pi):

```bash
zen              # new session
zen -c           # continue the most recent session
zen -r [id]      # resume a session (picker when no id)
zen -m codex/gpt-6-sol     # default is claude/claude-opus-5-5
zen -e xhigh     # thinking level (default: the model's; `zen models` lists them)
zen --inline     # no full-screen layout: the input follows the conversation (or ZEN_INLINE=1)
```

Inside: `/new`, `/resume`, `/model`, `/effort`, `/done`, `/go`, `/brief`, `/quick`, `/verify`, `/rename <title>`, `/archive`, `/upgrade`, `/help`, `/exit`. Enter sends; Shift+Enter (or Alt+Enter, Ctrl+J) starts a new line or paragraph; Esc interrupts zenbot, ↑↓ recall prompts, Ctrl-D exits.

For scripts, every command accepts `--json` and exits non-zero on failure:

```bash
zen ask "Find large files in ~ and summarize"     # one task: streams the answer, tools on stderr
echo "notes…" | zen ask "Summarize this" --json   # prompt from stdin, JSON result
zen ask -s 3f2a "And now fix it"                  # continue a session (id or prefix)
zen sessions ls | show <id> | new | archive <id> | restore <id> | rename <id> <title>
zen sessions decide <id> accept|more|reshape|drop [-n note]   # same as /done
zen sessions flow <id> go|brief|quick|verify                 # same as /go, /brief, …
zen ask -m claude/claude-sonnet-5-5 -e low "…"    # model and thinking level for a new session
zen models                                        # models and their thinking levels, [default]
zen status
zen upgrade [--check]
```

## How a session works

A request is framed before anything changes ([docs/brief.md](docs/brief.md)). zen first looks around read-only (the shell can't write) and either answers, or asks up to three questions, or proposes a short **brief**: the goal, scope, must-nots, assumptions and success criteria, written as commands where possible. Small briefs are approved automatically; bigger ones wait for you (`/go`, or reply "yes"; reply with changes to reshape it). The work then continues with the brief (small work in the same context, architectural work in a fresh one), and when it's submitted zen runs the criteria's commands itself and a fresh verifier checks the rest. Failures go back to work twice at most, then you get a report: criteria results, the decisions it made on its own, and its assumptions. Where allowed it closes the session with its own verdict; yours (`/done`) always replaces it.

You can take any step yourself: `/brief` (frame the next request), `/quick` (skip the brief), `/go`, `/verify`, `/done`. Settings in `~/.zenbot/env`: `ZEN_AUTO_APPROVE` and `ZEN_AUTO_CLOSE` (routes, default `quick,bounded`; `all`), `ZEN_VERIFY_ROUNDS` (default 2), `ZEN_DECIDE_TOOL=0`, and `ZEN_BRIEFS=0` to work without briefs.

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
  It needs the `pi` worker. `ZEN_SCORE_IDLE_SECS` changes the quiet time (default 7200).
- **Evals** compare two harness versions on fixed tasks with the same model: `scripts/eval.sh` (see [evals/README.md](evals/README.md)). Run them before changing the harness; the report is for you to decide on.

## Web UI (on hold)

The kernel still serves a minimal web chat at `http://<host>:8100/?token=$(cat ~/.zenbot/token)`. It works but is frozen while the CLI comes first.
