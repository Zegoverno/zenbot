# The Matrix channel (`zen-matrix`)

The owner talks to zen from any Matrix client (Beeper, Element), on the phone or the desktop, as
well as from the terminal (D-049). `zen-matrix` is a separate process and a client of the kernel's
client protocol (docs/client-protocol.md), like `zen`: the kernel doesn't know Matrix exists.

```
 Beeper / Element ──(E2EE)── matrix.org ── zen-matrix ──HTTP+WS, localhost── zend
```

## What it does

- **Each room is one zen session.** `zen-matrix login` makes two encrypted rooms and invites the
  owner: **zen** (a direct chat on a session titled "Matrix") and **zen jobs**. `!new [title]` starts a
  session in a new room. A room the owner creates and invites the bot to gets a session on its
  first message.
- **Turns.** The owner's message is the prompt. While zen works the room shows it typing; each
  assistant message with text is posted as Markdown (tool calls aren't). A failed turn, a kernel
  error or a prompt typed in the terminal in the same session (while the room's link is open)
  shows as a notice. Answers over 12,000 characters are cut, with a pointer to `zen -r`.
- **Questions.** An `ask` is posted numbered, recommended option first. Replying with one number
  per question (`1 2`) sends the chosen options' text as the answer; anything else goes as is.
- **Commands:** `!new [title]`, `!stop` (abort the turn), `!session` (the session id, for `zen -r`),
  `!help`.
- **Job reports.** Every minute it reads `GET /api/jobs/runs` and posts each finished run (`ok` with
  its report, `error`, `missed`, `interrupted`; not `silent`) once to **zen jobs**. Runs from before
  the bridge first started aren't replayed.

## Security

- **One owner.** It acts only on messages from `MATRIX_OWNER` and joins only rooms that account
  invites it to; other invites are declined, other senders ignored.
- **End-to-end encryption.** Every room it creates is encrypted (Megolm, recommended defaults).
  `login` sets up cross-signing, so the bot's device is verified by its own identity, and server-side
  key backup with a recovery key saved to `~/.zenbot/matrix/recovery-key`. Room keys are shared with
  all the owner's devices (matrix-sdk's default), verified or not.
- **Secrets stay local.** `~/.zenbot/matrix/` is mode 700 and its files 600: `session.json` (the
  access token), `store.key` (the passphrase that encrypts the SQLite store of keys and sync state),
  `recovery-key`. `matrix.env` holds the password only until the first sign-in; remove it after.
- **The kernel token** never leaves the machine: the bridge talks to `127.0.0.1` with it in a header.
- **The service is confined** (`deploy/zen-matrix.service`): `ProtectSystem=strict`, writable only
  `~/.zenbot/matrix`, `NoNewPrivileges`. It runs no tools itself; everything zen does still goes
  through the kernel.
- **Message content** reaches the bot decrypted on this machine, and is then a prompt like one typed
  in the terminal. Anyone holding the owner's Matrix account can drive zen: protect that account (2FA
  at the provider, verified sessions).

## Setup

1. Create the bot's account on matrix.org (in a browser: it has a captcha). Pick a neutral name.
   If the client offers to set up a recovery key, save it. Sign out.
2. Write `~/.zenbot/matrix.env` (mode 600):
   ```
   MATRIX_USER=@your-bot:matrix.org
   MATRIX_PASSWORD=…               # only for the first sign-in
   MATRIX_OWNER=@you:beeper.com     # the only account it answers
   # MATRIX_RECOVERY_KEY=…          # only if the account already has encryption set up
   # MATRIX_HOMESERVER=https://…    # only if discovery from the user id doesn't work
   ```
3. `scripts/matrix.sh`: builds `crates/zen-matrix`, installs `~/.zenbot/bin/zen-matrix`, signs in
   (`zen-matrix login`), installs and starts the `zen-matrix` service.
4. Accept the two invites in your client. Remove `MATRIX_PASSWORD` from `matrix.env`.

`zen-matrix status` shows the configuration, the sign-in, encryption and rooms;
`journalctl -u zen-matrix -f` the log. After pulling a new version, run `scripts/matrix.sh` again.

## Code

`crates/zen-matrix` is its own Cargo workspace: matrix-sdk's SQLite (`libsqlite3-sys` 0.38) clashes
with sqlx's `links = "sqlite3"` in the kernel's graph, and its few hundred dependencies stay out of
the kernel's build and lockfile. CI builds, tests and lints it in its own job; it isn't in the
release tarball.

| File | What |
|---|---|
| `src/main.rs` | Commands (`login`, `run`, `status`); the Matrix client (SQLite store, sign-in, cross-signing and backup), rooms, invites, the message handler, the per-room link task (session WebSocket, reconnects, typing) and the job reporter |
| `src/relay.rs` | Pure: commands, kernel events → room posts, numbered answers, job reports, cuts. Unit-tested |
| `src/kernel.rs` | The client-protocol calls it uses |
| `src/state.rs` | `matrix.env` and `~/.zenbot/env`, `state.json` (rooms ↔ sessions, job cursor), private files |

## Limits and next steps

- No interactive verification (emoji/SAS) of the bot yet: the owner's client may show the bot as
  unverified until the owner verifies it by hand.
- Only text messages; attachments and images are ignored.
- A session's room learns of terminal prompts only while its link is open (after its first message
  since the bridge started).
