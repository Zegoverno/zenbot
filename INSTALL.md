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

```bash
cd ~/zenbot && git pull && ./scripts/upgrade.sh
```

`upgrade.sh` uses the prebuilt binaries for the new commit (or builds them if there are none yet, or if you changed the code locally), checks them, runs one scripted test turn against them, and then restarts zenbot as soon as no session is working. If the new version isn't healthy it rolls back by itself. The result is in `~/.zenbot/upgrade.log`, and `~/.zenbot/version` holds the running commit.

Right after a push to `main`, CI needs a few minutes to publish the binaries; an upgrade started before that compiles locally instead.

Troubleshooting: `journalctl -u zenbot -n 50` shows the service logs. The service listens on port 8100 (change with `ZEN_PORT` before installing).
