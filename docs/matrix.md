# The Matrix channel (`zen-matrix`)

Talk to zen from any Matrix client (Beeper, Element) as well as the terminal (D-049). `zen-matrix`
is a separate process, a client of [client-protocol.md](client-protocol.md) like `zen`; the kernel
doesn't know Matrix exists. Owner → E2EE → matrix.org → `zen-matrix` → localhost → `zend`.

## Setup

1. Create the bot's account on matrix.org in a browser (captcha); save any recovery key; sign out.
2. Write `~/.zenbot/matrix.env` (mode 600):
   ```
   MATRIX_USER=@your-bot:matrix.org
   MATRIX_PASSWORD=…               # only for the first sign-in
   MATRIX_OWNER=@you:beeper.com     # the only account it answers
   # MATRIX_RECOVERY_KEY=…          # only if the account already has encryption set up
   # MATRIX_HOMESERVER=https://…    # only if discovery from the user id fails
   ```
3. Run `scripts/matrix.sh`: builds and tests `crates/zen-matrix`, installs `~/.zenbot/bin/zen-matrix`,
   signs in (`zen-matrix login`: cross-signing, key backup with the recovery key in
   `~/.zenbot/matrix/recovery-key`, two encrypted rooms), installs and starts the `zen-matrix` service.
4. Accept the invite to **zen**. Remove `MATRIX_PASSWORD` from `matrix.env`.
5. Write only from sessions of your account that you verified: others are refused.

After that, `scripts/upgrade.sh` rebuilds and restarts an enabled bridge whenever its sources
changed, with rollback (D-077). `zen-matrix status` shows config, sign-in, encryption, rooms and the
pinned owner key; `journalctl -u zen-matrix -f` the log.

## Use

- **Each room is a session.** **zen** is a direct chat on a `channel` session, always up (archive or done starts a clean one); `!new [title]`
  opens a new room and session; a room you create and invite the bot to (encrypted, only you) gets a
  session on its first message.
- **Turns.** A message is the prompt; during a turn it queues, `!steer <text>` redirects at the
  next tool boundary, `!stop` aborts. The room shows typing; answers post as Markdown (tool calls
  don't), split past ~20 KB, cut after 8 parts (the rest via `zen -r`). Failures and prompts typed
  in the terminal show as notices.
- **Questions** from `ask` post numbered; reply `1 2` to pick options, or answer in words.
- **Nothing lost.** Per room the bridge keeps the last session `seq` it posted (`state.json`) and
  catches up (`?after=`) on every reconnect, so answers arriving while anything was down post once.
- **Owner commands** (`zen_client::owner`, the same as `/…` in zen and `zen …`; run against the
  kernel, never seen by the agent): `!session`, `!help`, `!queue`, `!approve [N]`, `!deny [N]` (no
  number: everything waiting), `!trust [add|rm <url|repo> [level]]`, `!jobs [run|resume|pause <name>
  | runs [name]]`, `!skills [accept|reject <domain/name> <sha>]`, `!tools accept|reject <name> <sha>`.
  Creating or removing jobs stays in the terminal.
- **Approval queue.** Every 5 s new held items post to **zen**, numbered. Answer with an encrypted
  `!approve N` or `!deny N`; plaintext reactions never approve (they carry no device identity).
- **Job reports.** Each finished run (`ok`, `error`, `missed`, `interrupted`; not `silent`) posts
  once to **zen**, except runs posted into a session (`post_to`, D-060).

## Security

The rules are pure, unit-tested functions in `crates/zen-matrix/src/trust.rs`.

- It acts only on end-to-end encrypted messages from `MATRIX_OWNER` (never the bot itself), sent
  from a device the owner's cross-signing identity signed. Others are refused and logged.
- The owner's master key is pinned on first contact. If it changes, everything is refused until you
  compare keys and reset: `sudo systemctl stop zen-matrix`, `~/.zenbot/bin/zen-matrix repin-owner`,
  `sudo systemctl start zen-matrix`.
- Rooms must be encrypted with only the owner and the bot; the bot leaves any room that isn't,
  checking before each post. Raw HTML in answers is shown as text.
- `~/.zenbot/matrix/` (mode 700) holds the access token, the store passphrase and the recovery key.
  The kernel token stays local (`ZEN_URL` overrides the address). The service
  (`deploy/zen-matrix.service`) is confined and runs no tools. Protect the Matrix account with 2FA.

## Limits

No emoji (SAS) verification of the bot; text only (attachments ignored); `ask` questions posted
while the link is down aren't caught up; messages sent while the bridge is down aren't prompts.
`crates/zen-matrix` is its own Cargo workspace (matrix-sdk's SQLite clashes with sqlx), built in its
own CI job and tested end to end by `scripts/matrix-e2e.sh`.
