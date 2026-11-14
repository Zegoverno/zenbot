# Installing zenbot

On a Linux VM (Debian or Ubuntu) with systemd and passwordless `sudo`. zenbot runs as
your user: its shell commands run on the host with your permissions.

1. Clone and install (safe to re-run):

   ```bash
   git clone https://github.com/Zegoverno/zenbot.git ~/zenbot
   ~/zenbot/install.sh
   ```

   It installs `git`, `curl`, `jq`, `python3`, `poppler-utils` (PDF pages), `bubblewrap`
   and `passt` (the sandbox of every agent other than zenbot and its private network, pasta:
   without them their `bash` refuses to run; `sudo apt-get install -y bubblewrap passt`; where
   AppArmor restricts user namespaces, as on stock Ubuntu 24.04, also
   `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`), Node.js 22 and the
   Claude Code and Codex CLIs if missing; installs Rust and builds zenbot (several minutes the first
   time); writes `~/.zenbot/env` and the API token; installs the `zenbot` service (database
   `~/.zenbot/zen.sqlite3`) and the native SearXNG web search (`zen-searxng`); and links `zen` into
   `~/.local/bin`.

2. Check it: `zen status`. `kernel`, `database` and `worker:engine` say `ok`; `claude` and `codex`
   say `NOT signed in` until step 3.

3. Sign in (opens a browser link): `zen login` signs in to Claude Code (Claude plan) and Codex
   (ChatGPT plan).

4. Start: `zen` opens the sessions board; `zen ask "…"` runs one task from a script.

Optional, in `~/.zenbot/env` (MAP.md lists every setting): `OPENROUTER_API_KEY` and
`ZEN_S1_MODEL=openrouter/typesafe/jev-1.13` turn on System One and search embeddings;
`BRAVE_API_KEY` or `TAVILY_API_KEY` switch web search to that API. MCP servers go in
`~/.zenbot/mcp.json` (Claude Code's `mcpServers` shape; secrets as `${VAR}` from `env`, given only
to a server that lists them in `allow_env`); `zen mcp login <server>` signs in to a remote one.
After changing `env`, restart the service from outside a zen session: `sudo systemctl restart zenbot`.

## Updating

zenbot checks GitHub for a newer `main` hourly; `zen` and `zen status` say when there is one.

```bash
zen upgrade          # or /upgrade in zen; `zen upgrade check` only checks
```

That pulls `main` and runs `scripts/upgrade.sh`, which builds the checkout and installs it once no
session is working: the database is backed up first, and if the new kernel isn't healthy the old
binaries and the database come back. Results go to `~/.zenbot/upgrade.log`. To run a branch,
check it out in `~/zenbot` and run `scripts/upgrade.sh` yourself.

The kernel's daily `engines` job keeps the Claude Code and Codex CLIs current, putting the previous
version back if a test turn fails (`scripts/update-engines.sh --check` compares versions).

## Matrix

Talk to zen from any Matrix client (Beeper, Element). `zen-matrix` is a separate service, a client
of the kernel like `zen`.

1. Create the bot's account (e.g. on matrix.org); save any recovery key; sign out.
2. Write `~/.zenbot/matrix.env` (mode 600):
   ```
   MATRIX_USER=@your-bot:matrix.org
   MATRIX_PASSWORD=…               # only for the first sign-in
   MATRIX_OWNER=@you:beeper.com     # the only account it answers
   # MATRIX_RECOVERY_KEY=…          # only if the account already has encryption set up
   # MATRIX_HOMESERVER=https://…    # only if discovery from the user id fails
   ```
3. Run `scripts/matrix.sh`: it builds `crates/zen-matrix` (about 15 minutes the first time), signs in
   with cross-signing and key backup (recovery key in `~/.zenbot/matrix/recovery-key`), creates the
   encrypted **zen** room and starts the `zen-matrix` service.
4. Accept the invite to **zen**; remove `MATRIX_PASSWORD` from `matrix.env`.

Each room is a session; **zen** is always up. Write to prompt; `!new [title]` opens a new room,
`!steer`, `!stop`, `!session`, and every shared command (`!help` lists them: `!model`, `!jobs`, …).
Job reports post to **zen**. The bridge answers only `MATRIX_OWNER`'s end-to-end encrypted messages
from cross-signed devices, in encrypted rooms shared with the owner alone, and pins the owner's
identity (`zen-matrix repin-owner` after the owner resets it, with the service stopped).
`scripts/upgrade.sh` rebuilds an enabled bridge when its sources change.

## Troubleshooting

`journalctl -u zenbot -n 50` (and `-u zen-matrix`, `-u zen-searxng`). The service listens on
127.0.0.1:8100; re-run the installer with `ZEN_PORT=…` to change it (other settings are kept).
