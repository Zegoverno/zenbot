# zenbot

A personal agent harness: the owner hands it jobs, and it carries them out on the owner's own
Claude Code and Codex subscriptions, with its own prompt, tools, memory, scheduled jobs and
System One classifiers. In daily use by its owner.

You reach it from the web app (`web/`: the board of sessions, the agent chat, files, tasks and
agents), from Matrix, and from the `zen` command line (for scripts).

- [INSTALL.md](INSTALL.md): install, sign in, update, Matrix.
- [MAP.md](MAP.md): how it works and where everything is.
- [AGENTS.md](AGENTS.md): how to change it.
- [PLUGINS.md](PLUGINS.md): build a plugin for zenbot.

```bash
git clone https://github.com/Zegoverno/zenbot.git ~/zenbot && ~/zenbot/install.sh
zen login && zen status   # then open the web app (INSTALL.md)
```

## Plugins

zenbot installs plugins: MCP servers with an optional UI, sandboxed, with the permissions they ask
for shown before you approve them. [PLUGINS.md](PLUGINS.md) is the guide for writing and releasing
one. The [marketplace](https://zegoverno.github.io/zenbot/) lists them (the list is
[`marketplace/plugins.json`](marketplace/plugins.json)); install from Settings → Marketplace in the
web app or with `zen plugins add <name>`. To list yours, open a "List a plugin" issue or a pull
request; a maintainer reviews every listing.
