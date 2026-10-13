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
  session in a new room. A room the owner creates and invites the bot to (encrypted, with no one
  else in it) gets a session on its first message.
- **Turns.** The owner's message is the prompt. While zen works the room shows it typing; each
  assistant message with text is posted as Markdown (tool calls aren't). A failed turn, a kernel
  error or a prompt typed in the terminal in the same session (while the room's link is open)
  shows as a notice. An answer longer than about 20 KB goes in several messages (split at a
  paragraph or line, a code block closed and reopened across the split); past 8 messages the rest
  is cut, with a pointer to `zen -r`. When a message can't be sent, a short notice says so.
- **Nothing is lost on a reconnect or a restart.** The bridge keeps, per room, the `seq` of the last
  session message the room has had (`state.json`). On every connection to the kernel, on `resync`
  and on each new message it reads what came after it (`GET /api/sessions/{id}?after=`) and posts
  it, so answers that came while the WebSocket, the kernel or the bridge was down are posted when
  it is back, and none twice. At start it links every mapped room, not only those written to since.
  A room seen for the first time starts at the session's latest message.
- **Questions.** An `ask` is posted numbered, recommended option first. Replying with one number
  per question (`1 2`) sends the chosen options' text as the answer; anything else goes as is.
- **Commands:** `!new [title]`, `!stop` (abort the turn), `!session` (the session id, for `zen -r`, and whether it's untrusted),
  `!help`, and the owner commands `!jobs [run|resume|pause <name> | runs [name]]`,
  `!skills [accept|reject <domain/name> <sha>]`, `!tools accept|reject <name> <sha>` and
  `!allow [yes]` (review and approve the exact latest held action once) and
  `!trust [yes]` (what made the room's session untrusted; `yes` trusts it again, D-054)
  (`zen_client::owner`, the same as zen's `/jobs`, `/skills`, `/tools`, `/allow`, `/trust`). They work in every room,
  **zen jobs** included; the bridge runs them against the kernel with the owner's token and posts
  the answer as a notice, so they never reach the agent and nothing it read can approve a job, a
  skill or a tool. Removing or creating a job stays in the terminal.
- **Job reports.** Every minute it reads `GET /api/jobs/runs` and posts each finished run (`ok` with
  its report, `error`, `missed`, `interrupted`; not `silent`) once to **zen jobs**. Runs from before
  the bridge first started aren't replayed.

## Security

The rules are pure functions in `src/trust.rs`, unit-tested; the bridge only gathers the facts.

- **One owner, through end-to-end encryption, from verified devices.** It acts only on a message
  that is from `MATRIX_OWNER` (never from itself: `MATRIX_OWNER` can't be `MATRIX_USER`), was
  end-to-end encrypted, and was sent from a device the owner's cross-signing identity signed, i.e. a
  session the owner verified in their client. matrix-sdk refuses to decrypt anything else (sender
  device trust requirement `CrossSigned`), and the bridge checks it again on the decrypted event.
  Plaintext messages and messages from the owner's unverified sessions are refused and logged
  (`REFUSED …`, or "wasn't decrypted" for the latter); someone else's are ignored.
- **The owner's identity is pinned.** The first message accepted pins the owner's cross-signing
  master key in `state.json` (`zen-matrix status` shows it). If it changes (the owner reset their
  identity, or someone took the account over and made a new one), every message is refused, with a
  loud log and a notice in the room, until the owner resets the pin on the machine:
  `sudo systemctl stop zen-matrix`, `~/.zenbot/bin/zen-matrix repin-owner`,
  `sudo systemctl start zen-matrix`; the next message pins the new identity. Compare the key it
  logs with the one your client shows before you do.
- **Rooms hold only the owner and the bot, and are encrypted.** Every room it creates is encrypted
  (Megolm, recommended defaults). It joins a room only at the owner's invite, and only if the room
  isn't known to be unencrypted; once joined it checks the whole room and leaves if it isn't
  encrypted or anyone else is in it (joined or invited). Before every post it checks again, and it
  leaves a room as soon as anyone else joins or is invited (logged `LEAVING …`). At start it leaves
  rooms that break these rules.
- **Formatting is not markup from the model.** Answers are Markdown turned into the message's HTML
  with raw HTML in them shown as text, so model output (or a web page it quotes) can't inject tags.
- **Keys.** `login` sets up cross-signing, so the bot's device is verified by its own identity, and
  server-side key backup with a recovery key saved to `~/.zenbot/matrix/recovery-key`. Room keys
  are shared with all the owner's devices (matrix-sdk's default), verified or not: an unverified
  session of the owner's can read the bot's answers, but can't drive zen.
- **Secrets stay local.** `~/.zenbot/matrix/` is mode 700 and its files 600: `session.json` (the
  access token), `store.key` (the passphrase that encrypts the SQLite store of keys and sync state),
  `recovery-key`. `matrix.env` holds the password only until the first sign-in; remove it after.
- **The kernel token** never leaves the machine: the bridge talks to `127.0.0.1` (the port from
  `~/.zenbot/env`; `ZEN_URL` overrides it) with it in a header.
- **The service is confined** (`deploy/zen-matrix.service`): the home folder is an empty tmpfs with
  only its binary, `~/.zenbot/token`, `~/.zenbot/env` and `~/.zenbot/matrix.env` bound in read-only
  and `~/.zenbot/matrix` read-write; `ProtectSystem=strict`, `PrivateTmp`, `PrivateDevices`, no
  capabilities, `NoNewPrivileges`, only Unix and IP sockets, the `@system-service` system calls,
  kernel tunables, modules and logs protected. It stays inert until `session.json` exists. It runs
  no tools itself; everything zen does still goes through the kernel. `scripts/matrix.sh` installs
  a changed unit.
- **Message content** reaches the bot decrypted on this machine, and is then a prompt like one typed
  in the terminal. Anyone holding the owner's Matrix account *and* a verified session of it can
  drive zen: protect that account (2FA at the provider) and verify only your own sessions.

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
3. `scripts/matrix.sh`: builds and tests `crates/zen-matrix`, installs `~/.zenbot/bin/zen-matrix`, signs in
   (`zen-matrix login`), installs and starts the `zen-matrix` service.
4. Accept the two invites in your client. Remove `MATRIX_PASSWORD` from `matrix.env`.
5. Make sure the client you write from is a verified session of your account (Element and Beeper
   ask you to verify each new sign-in): messages from unverified sessions are refused.

`zen-matrix status` shows the configuration, the sign-in, encryption and rooms;
`journalctl -u zen-matrix -f` the log. After pulling a new version, run `scripts/matrix.sh` again.

## Code

`crates/zen-matrix` is its own Cargo workspace: matrix-sdk's SQLite (`libsqlite3-sys` 0.38) clashes
with sqlx's `links = "sqlite3"` in the kernel's graph, and its few hundred dependencies stay out of
the kernel's build and lockfile. CI builds, tests and lints it in its own job; it isn't in the
release tarball.

| File | What |
|---|---|
| `src/main.rs` | Commands (`login`, `run`, `status`, `repin-owner`); the Matrix client (SQLite store, sign-in, cross-signing and backup, decryption trust), rooms, invites, membership watch, the message handler, posting (room check, parts, failure notice), the per-room link task (session WebSocket, catch-up, reconnects, typing) and the job reporter |
| `src/trust.rs` | Pure: who and where it answers (sender, device trust, room members and encryption, invites, the pinned owner identity). Unit-tested |
| `src/relay.rs` | Pure: commands, kernel events (`zen_proto::client::Event`, its end-of-turn rule and echo matching) and session messages → room posts, numbered answers, job reports, splitting long answers, Markdown → HTML without raw HTML. Unit-tested |
| `src/kernel.rs` | The client-protocol calls it uses, over the client `zen` uses too (`crates/zen-client`, a path dependency) |
| `src/state.rs` | `matrix.env` and `~/.zenbot/env` (an owner that is the bot is refused), `state.json` (rooms ↔ sessions, per-room `seen` seq, the pinned owner key, job cursor; bad ids dropped on load), private files |

## Limits and next steps

- No interactive verification (emoji/SAS) of the bot yet: the owner's client may show the bot as
  unverified until the owner verifies it by hand.
- Only text messages; attachments and images are ignored.
- Questions an `ask` posts while the room's link is down are not caught up (they aren't session
  messages); the turn's answer is.
- Messages the owner sends while the bridge is down are not taken as prompts when it starts.
