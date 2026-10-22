# Installing zenbot

For a coding agent (or a person) on a fresh Linux VM: Ubuntu or Debian, systemd, passwordless `sudo`
(the installer adds packages and the service). Shells run contained, but a `host: true` command that passes
the gate runs as that user on the host, sudo included (DESIGN.md).

1. Clone and install (safe to re-run):

   ```bash
   git clone https://github.com/Zegoverno/zenbot.git ~/zenbot
   ~/zenbot/install.sh
   ```

   It installs Node.js 22, `git`, `curl`, `jq`, `python3`, bubblewrap, `pdftotext` and the Claude
   Code and Codex CLIs if missing; downloads CI's prebuilt binaries for the checked-out commit
   (x86_64 or aarch64 Linux, e.g. a Raspberry Pi with 64-bit OS), or installs Rust and compiles
   (several minutes; forced with `ZEN_BUILD_FROM_SOURCE=1`); writes `~/.zenbot/env` and the API
   token; installs the `zenbot` systemd service, with its SQLite database in `~/.zenbot/zen.sqlite3`
   (no Docker); and links `zen` into `~/.local/bin`.

2. Check it: `~/.local/bin/zen status`. `kernel`, `database` and `worker:engine` should say `ok`;
   `claude` and `codex` say `NOT signed in` until step 3.

3. Sign in (needs the owner: it opens a browser): `zen login` signs in to Claude Code (Claude plan)
   and Codex (ChatGPT plan); open each link, approve, paste back any code.

4. Start: `zen`.

## Updating

zenbot checks GitHub for a newer `main` about once an hour; `zen` and `zen status` mention it.

```bash
zen upgrade            # or /upgrade inside zen; `zen upgrade --check` only checks
```

It pulls `main` (refusing local changes or another branch) and runs `scripts/upgrade.sh`: test,
restart once no session is working, roll back if unhealthy; results in `~/.zenbot/upgrade.log`
([DEVELOPMENT.md](DEVELOPMENT.md#applying-a-change-scriptsupgradesh)). An existing PostgreSQL
installation is migrated to SQLite by its first upgrade (`install.sh` refuses it); PostgreSQL and
the archive are kept.

The kernel's daily `engines` job keeps the Claude Code and Codex CLIs current, putting back the
previous version if a test turn fails ([DEVELOPMENT.md](DEVELOPMENT.md#engine-updates));
`scripts/update-engines.sh --check` compares versions.

Troubleshooting: `journalctl -u zenbot -n 50`. The service listens on port 8100; re-run the
installer with `ZEN_PORT=…` to change it (other settings in `~/.zenbot/env` are kept). Web
search runs on SearXNG, installed natively (`systemctl status zen-searxng`; `scripts/searxng.sh check`).
