# zenbot

A personal agent harness: the owner hands it jobs, and it carries them out on the owner's own
Claude Code and Codex subscriptions, with its own prompt, tools, memory, scheduled jobs and
System One classifiers. In daily use by its owner.

You reach it from the web app (`web/`: the board of sessions, the agent chat, files, tasks and
agents), from Matrix, and from the `zen` command line (for scripts).

- [INSTALL.md](INSTALL.md): install, sign in, update, Matrix.
- [MAP.md](MAP.md): how it works and where everything is.
- [AGENTS.md](AGENTS.md): how to change it.

```bash
git clone https://github.com/Zegoverno/zenbot.git ~/zenbot && ~/zenbot/install.sh
zen login && zen status   # then open the web app (INSTALL.md)
```
