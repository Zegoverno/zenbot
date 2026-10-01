# zenbot

A personal + company operating system for niche builders: a machine for thinking, analyzing and building, with LLMs at the center and agents doing the work.

Status: design phase. See [SPEC.md](SPEC.md).

## Install

Requirements: Linux with systemd, Docker, Rust, Node 22+.

```bash
cd packages/mind && npm install && cd ../..
mkdir -p ~/.zenbot && (cd ~/.zenbot && npx --prefix ../zenbot/packages/mind pi-ai login openai)   # Sign in with ChatGPT
./scripts/install-service.sh   # builds, installs the systemd service (starts on boot), links `zen` into ~/.local/bin
zen status
```

For development without the service: `./scripts/dev.sh` (`ZEN_FAUX=1` adds a scripted test model).

## Using zen

Run `zen` for an interactive session in your terminal (inline, like Claude Code, Codex or Pi):

```bash
zen              # new session
zen -c           # continue the most recent session
zen -r [id]      # resume a session (picker when no id)
zen -m openai/gpt-6-sol
```

Inside: `/new`, `/resume`, `/model`, `/rename <title>`, `/archive`, `/help`, `/exit`. Enter sends, Alt+Enter adds a line, Esc interrupts zenbot, ↑↓ recall prompts, Ctrl-D exits.

For scripts, every command accepts `--json` and exits non-zero on failure:

```bash
zen ask "Find large files in ~ and summarize"     # one task: streams the answer, tools on stderr
echo "notes…" | zen ask "Summarize this" --json   # prompt from stdin, JSON result
zen ask -s 3f2a "And now fix it"                  # continue a session (id or prefix)
zen sessions ls | show <id> | new | archive <id> | restore <id> | rename <id> <title>
zen models
zen status
```

## Web UI (on hold)

The kernel still serves a minimal web chat at `http://<host>:8100/?token=$(cat ~/.zenbot/token)`. It works but is frozen while the CLI comes first.
