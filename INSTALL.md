# Installing zenbot

These steps are written so a coding agent (or a person) can follow them on a fresh Linux VM: Ubuntu or Debian, with systemd and passwordless `sudo`. `sudo` is needed because `install.sh` installs packages, Docker and the systemd service, and `scripts/upgrade.sh` restarts the service and runs Docker later without asking; it also means the agent's shell, which runs as that user, can do anything on the VM (DESIGN.md, "Security today").

1. Clone the repository into the home directory and run the installer:

   ```bash
   git clone https://github.com/Zegoverno/zenbot.git ~/zenbot
   ~/zenbot/install.sh
   ```

   The installer is safe to re-run. It installs Docker, Node.js 22, `git`, `curl`, `jq`, bubblewrap (for the sandboxed shells and tools), `pdftotext`, and the Claude Code and Codex CLIs if they're missing, downloads zenbot's prebuilt binaries for the checked-out commit, writes `~/.zenbot/env` and the API token, installs the `zenbot` systemd service (starts on boot, restarts on failure; it brings up Postgres and the SearXNG search container in Docker before the kernel), and links the `zen` command into `~/.local/bin`.

   Prebuilt binaries exist for every commit on `main` that passed CI's checks (built for x86_64 Linux after the tests, clippy and end-to-end scenarios pass). On other platforms, for commits CI hasn't published yet, or with `ZEN_BUILD_FROM_SOURCE=1`, the installer installs Rust and compiles instead, which takes several minutes on a small VM.

2. Check that it's healthy:

   ```bash
   ~/.local/bin/zen status
   ```

   `kernel`, `database` and `worker:engine` should say `ok`; `claude` and `codex` say `NOT signed in` until step 3. The other lines (engines, model, memory, jobs, version) are information.

3. Sign in to the model engines. This step needs the owner, because it opens a browser:

   ```bash
   zen login
   ```

   It signs in to Claude Code (your Claude plan) and Codex (your ChatGPT plan) in turn: open each printed link, approve, and paste back any code it asks for.

4. Start zenbot:

   ```bash
   zen
   ```

## Updating

zenbot checks GitHub for a newer `main` about once an hour. `zen` mentions it when you start a session, and `zen status` shows it. To update:

```bash
zen upgrade            # or /upgrade inside zen; `zen upgrade --check` only checks
```

It pulls `main` (refusing local changes or another branch) and runs `scripts/upgrade.sh`, which tests the new version, restarts zenbot once no session is working and rolls back if it isn't healthy; the result is in `~/.zenbot/upgrade.log`. After a healthy install it also trims oversized cargo build directories and old Docker leftovers, and `zen status` warns when the root filesystem is over 80% full. Every step: [DEVELOPMENT.md, "Applying a change"](DEVELOPMENT.md#applying-a-change-scriptsupgradesh).

### Engines

The Claude Code and Codex CLIs are kept on their latest versions by the kernel's daily `engines` job: each new version must pass a real test turn, else the previous one is put back ([DEVELOPMENT.md, "Engine updates"](DEVELOPMENT.md#engine-updates)). Results are in `~/.zenbot/upgrade.log` (`engines:` lines), and `zen status` shows the versions.

```bash
~/zenbot/scripts/update-engines.sh --check   # installed vs latest, changes nothing
~/zenbot/scripts/update-engines.sh           # update now
zen jobs                                     # next run of every scheduled job
```

Troubleshooting: `journalctl -u zenbot -n 50` shows the service logs. The service listens on port 8100 (change it by re-running the installer with `ZEN_PORT=…`; a re-run keeps your other settings in `~/.zenbot/env`).
