# Installing zenbot

These steps are written so a coding agent (or a person) can follow them on a fresh Linux VM: Ubuntu or Debian, with systemd and passwordless `sudo`.

1. Clone the repository into the home directory and run the installer:

   ```bash
   git clone https://github.com/Zegoverno/zenbot.git ~/zenbot
   ~/zenbot/install.sh
   ```

   The installer is safe to re-run. It installs Docker, Node.js 22, and the Claude Code and Codex CLIs if they're missing, downloads zenbot's prebuilt binaries for the checked-out commit, starts Postgres in Docker, installs the `zenbot` systemd service (starts on boot, restarts on failure), and links the `zen` command into `~/.local/bin`.

   Prebuilt binaries exist for every commit on `main` (built by CI on x86_64 Linux). On other platforms, for commits CI hasn't built yet, or with `ZEN_BUILD_FROM_SOURCE=1`, the installer installs Rust and compiles instead, which takes several minutes on a small VM.

2. Check that it's healthy:

   ```bash
   ~/.local/bin/zen status
   ```

   Every line should say `ok`, except `claude` and `codex`, which say `NOT signed in` until step 3.

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

This runs `scripts/self-update.sh`: it pulls `main` (refusing if the checkout has local changes or is on another branch) and runs `scripts/upgrade.sh`, showing progress until zenbot is back on the new version. Doing it by hand is the same thing:

```bash
cd ~/zenbot && git pull && ./scripts/upgrade.sh
```

`upgrade.sh` uses the prebuilt binaries for the new commit (or builds them if there are none yet, or if you changed the code locally), checks them, runs one scripted test turn against them, and then restarts zenbot as soon as no session is working. If the new version isn't healthy it rolls back by itself. The result is in `~/.zenbot/upgrade.log`, and `~/.zenbot/version` holds the running commit.

### Engines

The model engines are kept on their latest versions by a daily timer (`zen-engines.timer`, around 04:00 UTC): `scripts/update-engines.sh` updates the Claude Code and Codex CLIs and, with the `pi` worker, Pi. Each new version must pass a real test turn; if it doesn't, the previous version is put back. The CLIs need no restart. The updates make no commits: a newer Pi goes into `packages/mind/node_modules` only (the repo pins its minimum version) and is applied through `upgrade.sh`. It is skipped while the checkout has local changes or isn't on `main`. Results are in `~/.zenbot/upgrade.log` (`engines:` lines), and `zen status` shows the versions.

```bash
~/zenbot/scripts/update-engines.sh --check   # installed vs latest, changes nothing
~/zenbot/scripts/update-engines.sh           # update now
systemctl list-timers zen-engines.timer      # next run
```

Right after a push to `main`, CI needs a few minutes to publish the binaries; an upgrade started before that compiles locally instead.

Troubleshooting: `journalctl -u zenbot -n 50` shows the service logs. The service listens on port 8100 (change with `ZEN_PORT` before installing).
