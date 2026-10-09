//! zen-matrix: the owner talks to zen from any Matrix client (Beeper, Element) as well as the
//! terminal. A separate process and a client of the kernel's client protocol, like `zen`: each
//! Matrix room is one zen session, and job reports go to a "zen jobs" room. It answers only the
//! owner's Matrix ID (`MATRIX_OWNER`); every room it makes is end-to-end encrypted.
//!
//!   zen-matrix login    sign in once: a device, cross-signing and key backup (prints where the
//!                       recovery key was saved), then the rooms
//!   zen-matrix run      the service: sync, relay, report jobs (deploy/zen-matrix.service)
//!   zen-matrix status   what's configured, signed in and mapped
//!
//! Design: docs/matrix.md, D-049.

mod kernel;
mod relay;
mod state;

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use kernel::Kernel;
use matrix_sdk::{
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    encryption::{recovery::RecoveryState, EncryptionSettings},
    room::Room,
    ruma::{
        api::client::{
            room::create_room::v3::{Request as CreateRoom, RoomPreset},
            uiaa,
        },
        events::{
            room::{
                encryption::RoomEncryptionEventContent,
                member::StrippedRoomMemberEvent,
                message::{MessageType, OriginalSyncRoomMessageEvent, Relation, RoomMessageEventContent},
            },
            InitialStateEvent,
        },
        OwnedRoomId, OwnedUserId, RoomId, UserId,
    },
    Client, RoomState,
};
use relay::{Input, Out, Stream};
use serde_json::{json, Value};
use state::{write_private, Config, State};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;

const DEVICE_NAME: &str = "zenbot";
/// How often the jobs room checks for finished runs.
const JOBS_EVERY: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into());
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).with_ansi(false).init();
    let cmd = std::env::args().nth(1).unwrap_or_default();
    match cmd.as_str() {
        "login" => login().await,
        "run" => run().await,
        "status" => status().await,
        "--version" | "-V" => {
            println!("zen-matrix {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            eprintln!("usage: zen-matrix login | run | status   (settings in ~/.zenbot/matrix.env; see docs/matrix.md)");
            std::process::exit(2);
        }
    }
}

fn log(msg: impl AsRef<str>) {
    eprintln!("{}", msg.as_ref());
}

/// The key that encrypts the local store (sync state, Olm/Megolm keys), made once.
fn store_passphrase(cfg: &Config) -> Result<String> {
    let path = cfg.dir.join("store.key");
    if let Ok(k) = std::fs::read_to_string(&path) {
        return Ok(k.trim().to_string());
    }
    let mut buf = [0u8; 32];
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    let key: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    write_private(&path, &key)?;
    Ok(key)
}

async fn client(cfg: &Config) -> Result<Client> {
    let passphrase = store_passphrase(cfg)?;
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(cfg.dir.join("store"))?;
    }
    let builder = Client::builder()
        .sqlite_store(cfg.dir.join("store"), Some(&passphrase))
        .with_encryption_settings(EncryptionSettings { auto_enable_cross_signing: false, auto_enable_backups: true, ..Default::default() });
    let builder = match &cfg.homeserver {
        Some(url) => builder.homeserver_url(url),
        None => builder.server_name_or_homeserver_url(cfg.server_name()),
    };
    builder.build().await.context("connecting to the homeserver")
}

/// A client signed in with the saved session (from `zen-matrix login`).
async fn signed_in(cfg: &Config) -> Result<Client> {
    let text = std::fs::read_to_string(cfg.dir.join("session.json")).context("not signed in: run `zen-matrix login` first")?;
    let session: MatrixSession = serde_json::from_str(&text).context("reading ~/.zenbot/matrix/session.json")?;
    if session.meta.user_id.as_str() != cfg.user {
        bail!("the saved session is for {}, but MATRIX_USER is {}: run `zen-matrix login` again", session.meta.user_id, cfg.user);
    }
    let c = client(cfg).await?;
    c.restore_session(session).await?;
    Ok(c)
}

async fn login() -> Result<()> {
    let cfg = Config::load()?;
    let c = if cfg.dir.join("session.json").exists() {
        log("already signed in; checking encryption");
        signed_in(&cfg).await?
    } else {
        let password = cfg.password.clone().context("MATRIX_PASSWORD is not set (in ~/.zenbot/matrix.env); it's only needed to sign in")?;
        let c = client(&cfg).await?;
        c.matrix_auth().login_username(&cfg.user, &password).initial_device_display_name(DEVICE_NAME).send().await.context("signing in")?;
        let session = c.matrix_auth().session().context("no session after signing in")?;
        write_private(&cfg.dir.join("session.json"), &serde_json::to_string(&session)?)?;
        log(format!("signed in as {} (device {})", session.meta.user_id, session.meta.device_id));
        c
    };
    c.sync_once(SyncSettings::default()).await?;
    encryption(&cfg, &c).await?;
    let st = Arc::new(Mutex::new(State::load(&cfg.dir)?));
    ensure_rooms(&cfg, &c, &st).await?;
    log("done: accept the invites from the bot in your Matrix client, then start the service (docs/matrix.md)");
    Ok(())
}

/// Cross-signing (so the bot's device is verified by its own identity) and key backup with a
/// recovery key. A fresh account gets new ones; an account that already has them needs
/// MATRIX_RECOVERY_KEY.
async fn encryption(cfg: &Config, c: &Client) -> Result<()> {
    let enc = c.encryption();
    enc.wait_for_e2ee_initialization_tasks().await;
    let recovery = enc.recovery();
    if let Some(key) = &cfg.recovery_key {
        recovery.recover(key).await.context("recovering with MATRIX_RECOVERY_KEY")?;
        log("recovered the account's keys with MATRIX_RECOVERY_KEY; this device is verified");
        return Ok(());
    }
    // Fresh accounts: the first cross-signing keys need no password (MSC3967); older servers ask.
    if let Err(e) = enc.bootstrap_cross_signing_if_needed(None).await {
        let Some(uiaa) = e.as_uiaa_response() else { return Err(e).context("setting up cross-signing") };
        let password = cfg.password.clone().context("the server wants the password to set up cross-signing: put MATRIX_PASSWORD back in matrix.env")?;
        let mut auth = uiaa::Password::new(uiaa::UserIdentifier::Matrix(uiaa::MatrixUserIdentifier::new(cfg.user.clone())), password);
        auth.session = uiaa.session.clone();
        enc.bootstrap_cross_signing(Some(uiaa::AuthData::Password(auth))).await.context("setting up cross-signing")?;
    }
    let status = enc.cross_signing_status().await;
    if !status.is_some_and(|s| s.is_complete()) {
        bail!(
            "this Matrix account already has encryption keys (set up by another client, e.g. Element at sign-up) and this device can't use them.\n\
             Put the account's recovery key in ~/.zenbot/matrix.env as MATRIX_RECOVERY_KEY=… and run `zen-matrix login` again\n\
             (or reset the account's cryptographic identity in Element, then run it again)."
        );
    }
    match recovery.state() {
        RecoveryState::Enabled => log("cross-signing and key backup are set up"),
        _ => {
            let key = recovery.enable().wait_for_backups_to_upload().await.context("setting up key backup")?;
            let path = cfg.dir.join("recovery-key");
            write_private(&path, &format!("{key}\n"))?;
            log(format!("cross-signing and key backup are set up; the recovery key is in {} (mode 600): keep a copy somewhere safe", path.display()));
        }
    }
    Ok(())
}

/// An encrypted, private room with the owner invited.
async fn create_room(c: &Client, owner: &UserId, name: &str, topic: &str, direct: bool) -> Result<Room> {
    let mut req = CreateRoom::new();
    req.name = Some(name.to_string());
    req.topic = Some(topic.to_string());
    req.invite = vec![owner.to_owned()];
    req.is_direct = direct;
    req.preset = Some(RoomPreset::TrustedPrivateChat);
    req.initial_state = vec![InitialStateEvent::with_empty_state_key(RoomEncryptionEventContent::with_recommended_defaults()).to_raw_any()];
    Ok(c.create_room(req).await?)
}

/// The main room (a direct chat with the owner, on its own session) and the jobs room.
async fn ensure_rooms(cfg: &Config, c: &Client, st: &Arc<Mutex<State>>) -> Result<()> {
    let kernel = Kernel::new(&cfg.kernel_url, cfg.kernel_token.clone())?;
    let owner = UserId::parse(&cfg.owner)?;
    let mut s = st.lock().await;
    let alive = |id: &Option<String>| id.as_deref().and_then(|r| RoomId::parse(r).ok()).and_then(|r| c.get_room(&r)).is_some_and(|r| r.state() == RoomState::Joined);
    if !alive(&s.main_room) {
        let room = create_room(c, &owner, "zen", "Your zen. Write here as in the terminal; !help for commands.", true).await?;
        let session = kernel.new_session(Some("Matrix")).await?;
        s.rooms.insert(room.room_id().to_string(), session.clone());
        s.main_room = Some(room.room_id().to_string());
        log(format!("created the main room {} for session {session}", room.room_id()));
    }
    if !alive(&s.jobs_room) {
        let room = create_room(c, &owner, "zen jobs", "Reports from zen's scheduled jobs.", false).await?;
        s.jobs_room = Some(room.room_id().to_string());
        log(format!("created the jobs room {}", room.room_id()));
    }
    s.save(&cfg.dir)
}

async fn status() -> Result<()> {
    let cfg = Config::load()?;
    println!("bot      {}", cfg.user);
    println!("owner    {}", cfg.owner);
    println!("kernel   {}", cfg.kernel_url);
    let k = Kernel::new(&cfg.kernel_url, cfg.kernel_token.clone())?;
    println!("         {}", if k.job_runs(1).await.is_ok() { "reachable" } else { "NOT reachable" });
    if !cfg.dir.join("session.json").exists() {
        println!("matrix   not signed in (zen-matrix login)");
        return Ok(());
    }
    let c = signed_in(&cfg).await?;
    c.sync_once(SyncSettings::default().timeout(Duration::from_secs(0))).await?;
    let cs = c.encryption().cross_signing_status().await;
    println!("matrix   signed in, device {}", c.device_id().map(|d| d.to_string()).unwrap_or_default());
    println!("         cross-signing {}, key backup {:?}", if cs.is_some_and(|s| s.is_complete()) { "complete" } else { "INCOMPLETE" }, c.encryption().recovery().state());
    let s = State::load(&cfg.dir)?;
    println!("rooms    main {}, jobs {}, {} mapped", s.main_room.as_deref().unwrap_or("-"), s.jobs_room.as_deref().unwrap_or("-"), s.rooms.len());
    Ok(())
}

/// What a room's link to its session takes.
enum Cmd {
    Prompt(String),
    Abort,
}

struct Bridge {
    cfg: Config,
    client: Client,
    kernel: Kernel,
    owner: OwnedUserId,
    state: Arc<Mutex<State>>,
    links: Mutex<HashMap<OwnedRoomId, mpsc::UnboundedSender<Cmd>>>,
}

async fn run() -> Result<()> {
    let cfg = Config::load()?;
    let c = signed_in(&cfg).await?;
    // The first sync catches up without handlers: messages from before the bridge started are
    // not taken as prompts.
    let first = c.sync_once(SyncSettings::default()).await.context("first sync")?;
    let st = Arc::new(Mutex::new(State::load(&cfg.dir)?));
    ensure_rooms(&cfg, &c, &st).await?;
    let bridge = Arc::new(Bridge {
        kernel: Kernel::new(&cfg.kernel_url, cfg.kernel_token.clone())?,
        owner: UserId::parse(&cfg.owner)?,
        cfg,
        client: c.clone(),
        state: st,
        links: Mutex::new(HashMap::new()),
    });
    // Invites the owner sent while the bridge was down.
    for room in c.invited_rooms() {
        let b = bridge.clone();
        tokio::spawn(async move { b.on_invite(room).await });
    }

    let b = bridge.clone();
    c.add_event_handler(move |ev: StrippedRoomMemberEvent, room: Room| {
        let b = b.clone();
        async move {
            if b.client.user_id() == Some(&*ev.state_key) && room.state() == RoomState::Invited {
                b.on_invite(room).await;
            }
        }
    });
    let b = bridge.clone();
    c.add_event_handler(move |ev: OriginalSyncRoomMessageEvent, room: Room| {
        let b = b.clone();
        async move {
            if let Err(e) = b.clone().on_message(ev, room.clone()).await {
                log(format!("message in {}: {e:#}", room.room_id()));
                b.note(&room, &format!("⚠️ {e}")).await;
            }
        }
    });

    let b = bridge.clone();
    tokio::spawn(async move { b.report_jobs().await });

    log(format!("zen-matrix running as {} for {}", bridge.cfg.user, bridge.owner));
    c.sync(SyncSettings::default().token(first.next_batch)).await?;
    Ok(())
}

impl Bridge {
    /// Join rooms the owner invites the bot to (each becomes a session); turn down anyone else.
    async fn on_invite(self: Arc<Self>, room: Room) {
        let inviter = room.invite_details().await.ok().and_then(|d| d.inviter).map(|m| m.user_id().to_owned());
        if inviter.as_deref() != Some(&*self.owner) {
            log(format!("declining an invite to {} from {:?}", room.room_id(), inviter));
            room.leave().await.ok();
            return;
        }
        // Joining can fail right after the invite (the server hasn't caught up): retry a bit.
        for delay in [0, 2, 4, 8, 16] {
            tokio::time::sleep(Duration::from_secs(delay)).await;
            if room.join().await.is_ok() {
                log(format!("joined {} at the owner's invite", room.room_id()));
                return;
            }
        }
        log(format!("could not join {}", room.room_id()));
    }

    async fn on_message(self: Arc<Self>, ev: OriginalSyncRoomMessageEvent, room: Room) -> Result<()> {
        if ev.sender != self.owner || room.state() != RoomState::Joined {
            return Ok(());
        }
        if matches!(ev.content.relates_to, Some(Relation::Replacement(_))) {
            return Ok(()); // an edit: the original already went to zen
        }
        let MessageType::Text(text) = &ev.content.msgtype else { return Ok(()) };
        let rid = room.room_id().to_owned();
        let is_jobs = self.state.lock().await.jobs_room.as_deref() == Some(rid.as_str());
        match relay::parse(&text.body) {
            Input::Help => self.note(&room, relay::HELP).await,
            Input::New(title) => {
                let name = title.clone().unwrap_or_else(|| "zen".into());
                let session = self.kernel.new_session(title.as_deref()).await?;
                let new = create_room(&self.client, &self.owner, &name, "A zen session. !help for commands.", false).await?;
                let mut s = self.state.lock().await;
                s.rooms.insert(new.room_id().to_string(), session.clone());
                s.save(&self.cfg.dir)?;
                drop(s);
                self.note(&room, &format!("New session `{}` in the room **{name}**: accept the invite.", &session[..8.min(session.len())])).await;
            }
            _ if is_jobs => self.note(&room, "This room only carries job reports; write to zen in the **zen** room.").await,
            Input::Which => {
                let s = self.state.lock().await.rooms.get(rid.as_str()).cloned();
                let msg = match s {
                    Some(id) => format!("Session `{id}`: in the terminal, `zen -r {}`.", &id[..8.min(id.len())]),
                    None => "No session yet: the first message starts one.".into(),
                };
                self.note(&room, &msg).await;
            }
            Input::Stop => {
                let _ = self.link(&room).await?.send(Cmd::Abort);
            }
            Input::Prompt(p) => {
                if !p.is_empty() {
                    let link = self.link(&room).await?;
                    if link.send(Cmd::Prompt(p)).is_err() {
                        self.links.lock().await.remove(&rid);
                        bail!("lost the link to the session; send it again");
                    }
                }
            }
        }
        Ok(())
    }

    /// The room's link to its session, made on first use (a room the owner made gets a session).
    async fn link(self: &Arc<Self>, room: &Room) -> Result<mpsc::UnboundedSender<Cmd>> {
        let rid = room.room_id().to_owned();
        let mut links = self.links.lock().await;
        if let Some(tx) = links.get(&rid).filter(|tx| !tx.is_closed()) {
            return Ok(tx.clone());
        }
        let session = {
            let mut s = self.state.lock().await;
            match s.rooms.get(rid.as_str()) {
                Some(id) => id.clone(),
                None => {
                    let title = room.name();
                    let id = self.kernel.new_session(title.as_deref()).await?;
                    s.rooms.insert(rid.to_string(), id.clone());
                    s.save(&self.cfg.dir)?;
                    id
                }
            }
        };
        if !kernel::valid_id(&session) {
            bail!("bad session id in state.json for this room");
        }
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(link_task(self.kernel.clone(), room.clone(), session, rx));
        links.insert(rid, tx.clone());
        Ok(tx)
    }

    async fn note(&self, room: &Room, md: &str) {
        post(room, Out::Note(md.to_string())).await;
    }

    async fn report_jobs(self: Arc<Self>) {
        loop {
            match self.kernel.job_runs(50).await {
                Ok(runs) => {
                    let (reports, room) = {
                        let mut s = self.state.lock().await;
                        let reports: Vec<String> = s.new_runs(&runs).into_iter().filter_map(relay::job_report).collect();
                        if let Err(e) = s.save(&self.cfg.dir) {
                            log(format!("saving state: {e:#}"));
                        }
                        (reports, s.jobs_room.clone())
                    };
                    let room = room.and_then(|r| RoomId::parse(r).ok()).and_then(|r| self.client.get_room(&r));
                    for r in reports {
                        match &room {
                            Some(room) => post(room, Out::Answer(r)).await,
                            None => log("a job report, but no jobs room"),
                        }
                    }
                }
                Err(e) => log(format!("job runs: {e:#}")),
            }
            tokio::time::sleep(JOBS_EVERY).await;
        }
    }
}

/// Post one thing in a room; a failure is logged (the owner can't be told in the same room).
async fn post(room: &Room, out: Out) {
    let res = match out {
        Out::Answer(md) => room.send(RoomMessageEventContent::text_markdown(md)).await.map(|_| ()),
        Out::Note(md) => room.send(RoomMessageEventContent::notice_markdown(md)).await.map(|_| ()),
        Out::Typing(on) => room.typing_notice(on).await,
    };
    if let Err(e) = res {
        log(format!("posting to {}: {e}", room.room_id()));
    }
}

/// One room's link to its session: prompts in, the session's events out to the room. Reconnects
/// when the kernel restarts; ends when the bridge drops the room's sender.
async fn link_task(kernel: Kernel, room: Room, session: String, mut rx: mpsc::UnboundedReceiver<Cmd>) {
    let mut stream = Stream::default();
    let mut down_noted = false;
    loop {
        let ws = match kernel.connect(&session).await {
            Ok(ws) => ws,
            Err(e) => {
                log(format!("session {session}: {e:#}"));
                if !down_noted {
                    post(&room, Out::Note("⚠️ zen isn't reachable right now; retrying.".into())).await;
                    down_noted = true;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        down_noted = false;
        let (mut sink, mut src) = ws.split();
        // The kernel's typing notice lasts a few seconds: renew it while zen works.
        let mut typing = tokio::time::interval(Duration::from_secs(3));
        loop {
            tokio::select! {
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { return };
                    let msg = match cmd {
                        Cmd::Prompt(text) => json!({ "type": "prompt", "text": stream.prompt(&text) }),
                        Cmd::Abort => json!({ "type": "abort" }),
                    };
                    if sink.send(Message::text(msg.to_string())).await.is_err() {
                        post(&room, Out::Note("⚠️ lost the connection to zen before that was sent; send it again.".into())).await;
                        break;
                    }
                }
                msg = src.next() => {
                    let text = match msg {
                        Some(Ok(Message::Text(t))) => t,
                        Some(Ok(_)) => continue,
                        _ => break,
                    };
                    let Ok(ev) = serde_json::from_str::<Value>(&text) else { continue };
                    for out in stream.on_event(&ev) {
                        post(&room, out).await;
                    }
                }
                _ = typing.tick(), if stream.busy => { room.typing_notice(true).await.ok(); }
            }
        }
        if stream.busy {
            stream.busy = false;
            room.typing_notice(false).await.ok();
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
