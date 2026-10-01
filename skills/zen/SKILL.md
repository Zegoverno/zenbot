---
name: zen
description: Delegate a task to zenbot, the owner's personal agent on this machine, or read its past sessions. Use when the user asks to hand work to zenbot, or to check what zenbot did.
---

# Using zenbot from another agent

zenbot runs as a service on this machine. Talk to it with the `zen` CLI. Always pass `--json` and check the exit code.

- Run a task: `zen ask --json "<task>"` → `{session_id, text, tools, usage, error}`. Long input can go on stdin: `cat file | zen ask --json "Summarize this"`.
- Continue a task: `zen ask --json -s <session_id> "<follow-up>"`.
- List sessions: `zen sessions ls --json` (add `--archived` for archived ones).
- Read a session: `zen sessions show <id> --json` (messages include tool calls and results).
- Check health: `zen status --json` (`ok: false` means zenbot can't run tasks right now).

zenbot can run shell commands and edit files on this machine. Ask the user before delegating anything destructive or outward-facing.
