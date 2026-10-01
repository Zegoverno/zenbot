# Installing zenbot

These steps are written so a coding agent (or a person) can follow them on a fresh Linux VM: Ubuntu or Debian, with systemd and passwordless `sudo`.

1. Clone the repository into the home directory and run the installer:

   ```bash
   git clone https://github.com/Zegoverno/zenbot.git ~/zenbot
   ~/zenbot/install.sh
   ```

   The installer is safe to re-run. It installs Docker, Rust and Node.js 22 if they're missing, builds zenbot, starts Postgres in Docker, installs the `zenbot` systemd service (starts on boot, restarts on failure), and links the `zen` command into `~/.local/bin`. The first build takes a few minutes.

2. Check that it's healthy:

   ```bash
   ~/.local/bin/zen status
   ```

   Every line should say `ok`, except `chatgpt`, which says `NOT signed in` until step 3.

3. Sign in to ChatGPT. This step needs the owner, because it opens a browser:

   ```bash
   zen login
   ```

   Open the printed link and approve. The browser then lands on a `127.0.0.1` page that won't load: copy that full address and paste it into the terminal.

4. Start zenbot:

   ```bash
   zen
   ```

Troubleshooting: `journalctl -u zenbot -n 50` shows the service logs. The service listens on port 8100 (change with `ZEN_PORT` before installing).
