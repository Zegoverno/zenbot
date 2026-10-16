//! zen-matrix: the owner talks to zen from any Matrix client (Beeper, Element) as well as the
//! terminal. A separate process and a client of the kernel's client protocol, like `zen`: each
//! Matrix room is one zen session, and job reports go to a "zen jobs" room. It answers only the
//! owner's Matrix ID (`MATRIX_OWNER`), only end-to-end encrypted messages from the owner's
//! cross-signed devices, and only in encrypted rooms it shares with the owner alone (the rules
//! are in trust.rs). Queue reactions require the same verified encrypted device authentication.
//! Ordinary plaintext reactions cannot authorize; use encrypted !queue commands (docs/matrix.md).
//!
//!   zen-matrix login         sign in once: a device, cross-signing and key backup (prints where
//!                            the recovery key was saved), then the rooms
//!   zen-matrix run           the service: sync, relay, report jobs (deploy/zen-matrix.service)
//!   zen-matrix status        what's configured, signed in, pinned and mapped
//!   zen-matrix repin-owner   forget the owner's pinned identity, after the owner reset it (with
//!                            the service stopped)
//!
//! Design: docs/matrix.md, D-049.

mod kernel;
mod relay;
mod state;
mod trust;

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use kernel::Kernel;
use matrix_sdk::{
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    deserialized_responses::{EncryptionInfo, VerificationLevel, VerificationState},
    encryption::{recovery::RecoveryState, EncryptionSettings},
    room::Room,
    ruma::{
        api::client::{
            room::create_room::v3::{Request as CreateRoom, RoomPreset},
            uiaa,
        },
        events::{
            room::{
                encrypted::OriginalSyncRoomEncryptedEvent,
                encryption::RoomEncryptionEventContent,
                member::{MembershipState, OriginalSyncRoomMemberEvent, StrippedRoomMemberEvent},
                message::{MessageType, OriginalSyncRoomMessageEvent, Relation, RoomMessageEventContent},
            },
            reaction::OriginalSyncReactionEvent,
            InitialStateEvent,
        },
        OwnedRoomId, OwnedUserId, RoomId, UserId,
    },
    Client, EncryptionState, RoomMemberships, RoomState,
};
use matrix_sdk_crypto::{DecryptionSettings, TrustRequirement};
use relay::{Input, Out, Stream};
use state::{write_private, Config, State};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;
use zen_proto::client::Event;

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
        "repin-owner" => repin_owner(),
        "--version" | "-V" => {
            println!("zen-matrix {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            eprintln!("usage: zen-matrix login | run | status | repin-owner   (settings in ~/.zenbot/matrix.env; see docs/matrix.md)");
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
        .with_encryption_settings(EncryptionSettings { auto_enable_cross_signing: false, auto_enable_backups: true, ..Default::default() })
        // Decrypt only what a cross-signed device sent: a device the owner's identity hasn't
        // signed (a new sign-in nobody verified, or one an intruder added) can't drive zen.
        .with_decryption_settings(DecryptionSettings { sender_device_trust_requirement: TrustRequirement::CrossSigned });
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

/// The lock `run` holds while it runs, so `login` and `repin-owner` (which share its store and
/// state) refuse to run next to the service.
fn run_lock(cfg: &Config) -> Result<std::fs::File> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&cfg.dir)?;
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(cfg.dir.join("run.lock"))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => bail!("zen-matrix is running (the zen-matrix service?): stop it first"),
        Err(std::fs::TryLockError::Error(e)) => Err(e).context("locking ~/.zenbot/matrix/run.lock"),
    }
}

async fn login() -> Result<()> {
    let cfg = Config::load()?;
    let _lock = run_lock(&cfg)?;
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
    println!("owner    identity {}", s.owner_key.as_deref().map(|k| format!("pinned, master key {k}")).unwrap_or_else(|| "not pinned yet (the owner's first message pins it)".into()));
    Ok(())
}

/// Forget the owner's pinned identity: the next message the owner sends from a cross-signed
/// device pins the identity it comes from. For when the owner reset their Matrix identity
/// (new cross-signing keys) themselves; the service must be stopped.
fn repin_owner() -> Result<()> {
    let cfg = Config::load()?;
    let _lock = run_lock(&cfg)?;
    let mut s = State::load(&cfg.dir)?;
    match s.owner_key.take() {
        Some(k) => {
            s.save(&cfg.dir)?;
            log(format!("forgot the owner's pinned identity (master key {k}); the owner's next message pins the new one. Start the service again."));
        }
        None => log("no identity is pinned: the owner's next message pins one"),
    }
    Ok(())
}

/// What a room's link to its session takes.
enum Cmd {
    Prompt(String),
    Abort,
}

type Link = mpsc::UnboundedSender<Cmd>;

struct Bridge {
    cfg: Config,
    client: Client,
    kernel: Kernel,
    owner: OwnedUserId,
    bot: OwnedUserId,
    state: Arc<Mutex<State>>,
    links: Mutex<HashMap<OwnedRoomId, Link>>,
    /// Held while a room gets its session, so two first messages don't make two sessions (the
    /// state and links locks are never held across a kernel call).
    making: Mutex<()>,
}

async fn run() -> Result<()> {
    let cfg = Config::load()?;
    let _lock = run_lock(&cfg)?;
    let c = signed_in(&cfg).await?;
    // The first sync catches up without handlers: messages from before the bridge started are
    // not taken as prompts.
    let first = c.sync_once(SyncSettings::default()).await.context("first sync")?;
    let st = Arc::new(Mutex::new(State::load(&cfg.dir)?));
    ensure_rooms(&cfg, &c, &st).await?;
    let bridge = Arc::new(Bridge {
        kernel: Kernel::new(&cfg.kernel_url, cfg.kernel_token.clone())?,
        owner: UserId::parse(&cfg.owner)?,
        bot: c.user_id().context("no user id after signing in")?.to_owned(),
        cfg,
        client: c.clone(),
        state: st,
        links: Mutex::new(HashMap::new()),
        making: Mutex::new(()),
    });
    // The rooms it is in: leave any that break the rules (from before them, or changed while
    // the bridge was down), and link the mapped ones to their sessions now, so answers that
    // came while the bridge was down are posted.
    for room in c.joined_rooms() {
        let b = bridge.clone();
        tokio::spawn(async move { b.on_start(room).await });
    }
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
    c.add_event_handler(move |ev: OriginalSyncRoomMemberEvent, room: Room| {
        let b = b.clone();
        async move {
            // Someone joins or is invited: anyone but the owner and the bot, and the bot leaves.
            let active = matches!(ev.content.membership, MembershipState::Join | MembershipState::Invite);
            if active && room.state() == RoomState::Joined {
                let check = trust::check_room(b.owner.as_str(), b.bot.as_str(), true, [ev.state_key.as_str()]);
                if check != RoomCheck::Ok {
                    b.leave(&room, &check).await;
                }
            }
        }
    });
    let b = bridge.clone();
    c.add_event_handler(move |ev: OriginalSyncRoomMessageEvent, room: Room, enc: Option<EncryptionInfo>| {
        let b = b.clone();
        async move {
            if let Err(e) = b.clone().on_message(ev, room.clone(), enc).await {
                log(format!("message in {}: {e:#}", room.room_id()));
                b.note(&room, &format!("⚠️ {e}")).await;
            }
        }
    });
    let b = bridge.clone();
    c.add_event_handler(move |ev: OriginalSyncRoomEncryptedEvent, room: Room| {
        let b = b.clone();
        async move {
            // Still encrypted after the sync: matrix-sdk couldn't, or wouldn't, decrypt it.
            if ev.sender == b.owner {
                log(format!(
                    "a message from the owner's account in {} wasn't decrypted: it came from a device the owner's identity hasn't signed \
                     (verify that session in your client), or its key hasn't arrived",
                    room.room_id()
                ));
            }
        }
    });

    let b = bridge.clone();
    c.add_event_handler(move |ev: OriginalSyncReactionEvent, room: Room, enc: Option<EncryptionInfo>| {
        let b=b.clone();
        async move {
            if let Err(e)=b.on_queue_reaction(ev,room,enc).await { log(format!("queue reaction: {e:#}")); }
        }
    });
    let b=bridge.clone();
    tokio::spawn(async move { b.report_queue().await });
    let b = bridge.clone();
    tokio::spawn(async move { b.report_jobs().await });

    log(format!("zen-matrix running as {} for {}", bridge.bot, bridge.owner));
    c.sync(SyncSettings::default().token(first.next_batch)).await?;
    Ok(())
}

use trust::{RoomCheck, Sealed, Verdict};

/// How a decrypted event reached the bot, from what matrix-sdk says about it.
fn sealed(sender: &UserId, enc: Option<&EncryptionInfo>) -> Sealed {
    let Some(info) = enc else { return Sealed::Plain };
    if info.sender != sender {
        return Sealed::Untrusted("the event's sender isn't the device's owner".into());
    }
    match &info.verification_state {
        // Verified (the bot verified the owner), or signed by the owner's identity, which the
        // bridge pins itself (trust::identity).
        VerificationState::Verified | VerificationState::Unverified(VerificationLevel::UnverifiedIdentity) => Sealed::CrossSigned,
        VerificationState::Unverified(level) => Sealed::Untrusted(level.to_string()),
    }
}

/// A session id, short (ids are checked when read: kernel::valid_id).
fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// A message's content: the Markdown as the plain body, its HTML as the formatted one, or the
/// plain body alone when both together would be too large for one event.
fn content(md: &str, notice: bool) -> RoomMessageEventContent {
    let html = relay::html(md);
    let c = if notice { RoomMessageEventContent::notice_html(md, html) } else { RoomMessageEventContent::text_html(md, html) };
    if serde_json::to_vec(&c).is_ok_and(|j| j.len() <= relay::MAX_CONTENT_JSON) {
        return c;
    }
    if notice {
        RoomMessageEventContent::notice_plain(md)
    } else {
        RoomMessageEventContent::text_plain(md)
    }
}

impl Bridge {
    async fn on_queue_reaction(&self,ev:OriginalSyncReactionEvent,room:Room,enc:Option<EncryptionInfo>) -> Result<()> {
        if room.state()!=RoomState::Joined || ev.sender!=self.owner {return Ok(())}
        let check=self.room_check(&room).await?;
        // Plain reactions carry no authenticated device identity. Never authorize from them.
        if check!=RoomCheck::Ok || !self.owner_identity_holds(&room).await? {return Ok(())}
        if trust::message(self.owner.as_str(),self.bot.as_str(),ev.sender.as_str(),
            &sealed(&ev.sender,enc.as_ref()),&check)!=trust::Verdict::Act {return Ok(())}
        let decision=match ev.content.relates_to.key.as_str() {"✅"=>"approve","❌"=>"reject","🛑"=>"deny",_=>return Ok(())};
        let mapkey=serde_json::to_string(&(room.room_id().as_str(),ev.content.relates_to.event_id.as_str()))?;
        let key=self.state.lock().await.queue_messages.get(&mapkey).cloned();
        if let Some(key)=key {
            match self.kernel.queue_decide(&key,decision).await {
                Ok(v)=>self.note(&room,&format!("Queue {key}: {v}")).await,
                Err(e)=>self.note(&room,&format!("Queue {key}: {e} (already decided, stale or busy items cannot be approved again).")).await,
            }
        }
        Ok(())
    }
    async fn report_queue(self:Arc<Self>) {
        loop {
            if let Err(e)=self.post_queue().await {log(format!("queue: {e:#}"))}
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
    async fn post_queue(&self) -> Result<()> {
        let rid={
            let s=self.state.lock().await;
            s.main_room.clone().or_else(||s.jobs_room.clone())
        };
        let Some(rid)=rid else {return Ok(())};
        let rid=RoomId::parse(rid)?;
        let Some(room)=self.client.get_room(&rid) else {return Ok(())};
        if room.state()!=RoomState::Joined || self.room_check(&room).await?!=RoomCheck::Ok {return Ok(())}
        for item in self.kernel.queue().await? {
            let key=format!("{}/{}",item["kind"].as_str().unwrap_or(""),item["id"]);
            if self.state.lock().await.queue_posted.contains(&key) {continue}
            let command_key=key.replace('/'," ");
            let md=format!("**Approval queue {key}** · {} · risk {}\n\n{}\n\nReason: {}\n\nSend encrypted !queue approve {command_key}, !queue reject {command_key}, or !queue deny {command_key} (actions only). Plain reactions cannot authorize.\n\n!queue lists pending items.",
                item["title"].as_str().unwrap_or("source proposal"),item["risk"],item["summary"].as_str().unwrap_or(""),item["reason"].as_str().unwrap_or(""));
            let sent=room.send(content(&md,true)).await?;
            let mut s=self.state.lock().await;
            s.queue_messages.insert(serde_json::to_string(&(room.room_id().as_str(),sent.response.event_id.as_str()))?,key.clone());
            s.queue_posted.insert(key);
            s.save(&self.cfg.dir)?;
        }
        Ok(())
    }

    /// Whether a room is one the bot may act and post in (trust::check_room).
    async fn room_check(&self, room: &Room) -> Result<RoomCheck> {
        let encryption = match room.encryption_state() {
            EncryptionState::Unknown => room.latest_encryption_state().await?,
            known => known,
        };
        let members = room.members(RoomMemberships::ACTIVE).await?;
        Ok(trust::check_room(self.owner.as_str(), self.bot.as_str(), encryption.is_encrypted(), members.iter().map(|m| m.user_id().as_str())))
    }

    /// Leave a room that broke the rules, loudly.
    async fn leave(&self, room: &Room, why: &RoomCheck) {
        let reason = match why {
            RoomCheck::Stranger(m) => format!("{m} is in it"),
            RoomCheck::NotEncrypted => "it isn't encrypted".into(),
            RoomCheck::Ok => "".into(),
        };
        log(format!("LEAVING room {}: {reason}. The bot stays only in encrypted rooms with the owner alone.", room.room_id()));
        self.links.lock().await.remove(room.room_id());
        if let Err(e) = room.leave().await {
            log(format!("leaving {}: {e}", room.room_id()));
        }
    }

    /// A room the bot is in when the bridge starts.
    async fn on_start(self: Arc<Self>, room: Room) {
        match self.room_check(&room).await {
            Ok(RoomCheck::Ok) => {
                let mapped = self.state.lock().await.rooms.contains_key(room.room_id().as_str());
                if mapped {
                    if let Err(e) = self.link(&room).await {
                        log(format!("linking {}: {e:#}", room.room_id()));
                    }
                }
            }
            Ok(bad) => self.leave(&room, &bad).await,
            Err(e) => log(format!("checking {}: {e}", room.room_id())),
        }
    }

    /// Join rooms the owner invites the bot to (each becomes a session) if they are encrypted
    /// and hold only the owner; turn down anything else (trust::invite).
    async fn on_invite(self: Arc<Self>, room: Room) {
        let inviter = room.invite_details().await.ok().and_then(|d| d.inviter).map(|m| m.user_id().to_owned());
        let encrypted = match room.encryption_state() {
            EncryptionState::Unknown => None,
            known => Some(known.is_encrypted()),
        };
        if let trust::Invite::Decline(why) = trust::invite(self.owner.as_str(), inviter.as_deref().map(|u| u.as_str()), encrypted) {
            log(format!("declining an invite to {}: {why}", room.room_id()));
            room.leave().await.ok();
            return;
        }
        // Joining can fail right after the invite (the server hasn't caught up): retry a bit.
        for delay in [0, 2, 4, 8, 16] {
            tokio::time::sleep(Duration::from_secs(delay)).await;
            if room.join().await.is_ok() {
                // Now the whole room is visible: check it again.
                match self.room_check(&room).await {
                    Ok(RoomCheck::Ok) => log(format!("joined {} at the owner's invite", room.room_id())),
                    Ok(bad) => self.leave(&room, &bad).await,
                    Err(e) => log(format!("joined {}, but couldn't check it: {e}", room.room_id())),
                }
                return;
            }
        }
        log(format!("could not join {}", room.room_id()));
    }

    async fn on_message(self: Arc<Self>, ev: OriginalSyncRoomMessageEvent, room: Room, enc: Option<EncryptionInfo>) -> Result<()> {
        if room.state() != RoomState::Joined || ev.sender != self.owner {
            return Ok(()); // the bot's own messages and anyone else's: nothing to check
        }
        let check = self.room_check(&room).await?;
        match trust::message(self.owner.as_str(), self.bot.as_str(), ev.sender.as_str(), &sealed(&ev.sender, enc.as_ref()), &check) {
            Verdict::Act => {}
            Verdict::Ignore => return Ok(()),
            Verdict::Refuse(why) => {
                log(format!("REFUSED a message from the owner's account in {}: {why}", room.room_id()));
                self.note(&room, &format!("⚠️ zen ignored that message: {why}.")).await;
                return Ok(());
            }
        }
        if !self.owner_identity_holds(&room).await? {
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
                if !kernel::valid_id(&session) {
                    bail!("the kernel returned a bad session id");
                }
                let new = create_room(&self.client, &self.owner, &name, "A zen session. !help for commands.", false).await?;
                let mut s = self.state.lock().await;
                s.rooms.insert(new.room_id().to_string(), session.clone());
                s.save(&self.cfg.dir)?;
                drop(s);
                self.note(&room, &format!("New session `{}` in the room **{name}**: accept the invite.", short(&session))).await;
            }
            // Read here, run with the owner's token, answered by the bridge: never sent to the agent.
            // Allowed in every room, the jobs room included (where its reports arrive).
            Input::Owner(cmd) => {
                let session = self.state.lock().await.rooms.get(rid.as_str()).cloned();
                let msg = self.kernel.owner(&cmd, session.as_deref()).await.unwrap_or_else(|e| format!("⚠️ {e:#}"));
                self.note(&room, &msg).await;
            }
            _ if is_jobs => self.note(&room, "This room only carries job reports; write to zen in the **zen** room.").await,
            Input::Which => {
                let s = self.state.lock().await.rooms.get(rid.as_str()).cloned();
                let msg = match s {
                    Some(id) => {
                        let limited = if self.kernel.tainted(&id).await.unwrap_or(false) {
                            "\nAn action was held, so it runs limited: `!queue` reviews held actions and proposals; `!trust` shows the session-wide override."
                        } else {
                            ""
                        };
                        format!("Session `{id}`: in the terminal, `zen -r {}`.{limited}", short(&id))
                    }
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

    /// The owner's cross-signing identity is the pinned one (pinned now if none is): a changed
    /// identity is refused until the owner resets the pin (`zen-matrix repin-owner`).
    async fn owner_identity_holds(&self, room: &Room) -> Result<bool> {
        let identity = self.client.encryption().get_user_identity(&self.owner).await?;
        let now = identity.and_then(|i| i.master_key().get_first_key()).map(|k| k.to_base64());
        let mut s = self.state.lock().await;
        match trust::identity(s.owner_key.as_deref(), now.as_deref()) {
            trust::Identity::Same => Ok(true),
            trust::Identity::Pin(key) => {
                log(format!("pinned the owner's identity: master key {key}"));
                s.owner_key = Some(key);
                s.save(&self.cfg.dir)?;
                Ok(true)
            }
            trust::Identity::Changed { pinned, now } => {
                drop(s);
                log(format!(
                    "REFUSED the owner's message in {}: the owner's Matrix identity CHANGED (pinned master key {pinned}, now {now}). \
                     If the owner reset it themselves: stop the service, run `zen-matrix repin-owner`, start it. Otherwise the account may be taken over.",
                    room.room_id()
                ));
                self.note(room, "⚠️ zen ignored that message: this account's encryption identity changed. If you reset it yourself, re-pin it on the machine (docs/matrix.md).").await;
                Ok(false)
            }
            trust::Identity::Missing => {
                drop(s);
                log(format!("REFUSED the owner's message in {}: the owner's cross-signing identity isn't known", room.room_id()));
                Ok(false)
            }
        }
    }

    /// The room's link to its session, made on first use (a room the owner made gets a session).
    async fn link(self: &Arc<Self>, room: &Room) -> Result<Link> {
        let rid = room.room_id().to_owned();
        if let Some(tx) = self.links.lock().await.get(&rid).filter(|tx| !tx.is_closed()) {
            return Ok(tx.clone());
        }
        let session = {
            let _making = self.making.lock().await;
            let known = self.state.lock().await.rooms.get(rid.as_str()).cloned();
            match known {
                Some(id) => id,
                None => {
                    let id = self.kernel.new_session(room.name().as_deref()).await?;
                    let mut s = self.state.lock().await;
                    s.rooms.insert(rid.to_string(), id.clone());
                    s.save(&self.cfg.dir)?;
                    id
                }
            }
        };
        if !kernel::valid_id(&session) {
            bail!("bad session id for this room");
        }
        let mut links = self.links.lock().await;
        if let Some(tx) = links.get(&rid).filter(|tx| !tx.is_closed()) {
            return Ok(tx.clone()); // another message linked it meanwhile
        }
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(self.clone().link_task(room.clone(), session, rx));
        links.insert(rid, tx.clone());
        Ok(tx)
    }

    async fn note(&self, room: &Room, md: &str) {
        self.post(room, Out::Note(md.to_string())).await;
    }

    /// Post one thing in a room, if the room still holds only the owner and is encrypted (else
    /// leave it). A long message goes in parts; when one can't be sent, a short notice says so
    /// (and if that fails too, the log).
    async fn post(&self, room: &Room, out: Out) {
        let (md, notice) = match out {
            Out::Typing(on) => {
                room.typing_notice(on).await.ok();
                return;
            }
            Out::Answer(md) => (md, false),
            Out::Note(md) => (md, true),
        };
        match self.room_check(room).await {
            Ok(RoomCheck::Ok) => {}
            Ok(bad) => return self.leave(room, &bad).await,
            Err(e) => return log(format!("not posting to {}: couldn't check the room: {e}", room.room_id())),
        }
        for part in relay::split(&md, relay::MAX_BODY_BYTES, relay::MAX_PARTS) {
            if let Err(e) = room.send(content(&part, notice)).await {
                log(format!("posting to {}: {e}", room.room_id()));
                let warn = "⚠️ a message from zen couldn't be posted here; it is in the session (`!session`, then `zen -r` in the terminal).";
                if let Err(e) = room.send(content(warn, true)).await {
                    log(format!("posting the failure notice to {}: {e}", room.room_id()));
                }
                return;
            }
        }
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
                            Some(room) => self.post(room, Out::Answer(r)).await,
                            None => log("a job report, but no jobs room"),
                        }
                    }
                }
                Err(e) => log(format!("job runs: {e:#}")),
            }
            tokio::time::sleep(JOBS_EVERY).await;
        }
    }

    /// Post the session's messages the room hasn't had yet (after its `seen` seq) and move
    /// `seen` on. A room seen for the first time starts at the session's latest message. Every
    /// message the room gets comes through here, so a reconnect, a resync or a restart loses none
    /// and posts none twice. `live` as in Stream::on_message.
    async fn catch_up(&self, room: &Room, session: &str, stream: &mut Stream, live: bool) {
        let rid = room.room_id().as_str();
        let seen = self.state.lock().await.seen.get(rid).copied();
        let Some(seen) = seen else {
            match self.kernel.last_seq(session).await {
                Ok(seq) => self.mark_seen(rid, seq).await,
                Err(e) => log(format!("session {session}: {e:#}")),
            }
            return;
        };
        let (messages, busy) = match self.kernel.messages_after(session, seen).await {
            Ok(m) => m,
            Err(e) => return log(format!("session {session}: catching up: {e:#}")),
        };
        for m in messages {
            let Some(seq) = m["seq"].as_i64().filter(|s| *s > seen) else { continue };
            for out in stream.on_message(&m, live) {
                self.post(room, out).await;
            }
            self.mark_seen(rid, seq).await;
        }
        if !live && busy != stream.busy {
            stream.busy = busy;
            self.post(room, Out::Typing(busy)).await;
        }
    }

    async fn mark_seen(&self, room: &str, seq: i64) {
        let mut s = self.state.lock().await;
        s.seen.insert(room.to_string(), seq);
        if let Err(e) = s.save(&self.cfg.dir) {
            log(format!("saving state: {e:#}"));
        }
    }

    /// One room's link to its session: prompts in, the session's events out to the room.
    /// Reconnects when the kernel restarts; ends when the bridge drops the room's sender.
    async fn link_task(self: Arc<Self>, room: Room, session: String, mut rx: mpsc::UnboundedReceiver<Cmd>) {
        let mut stream = Stream::default();
        let mut down_noted = false;
        // The first connection follows a bridge start: prompts sent before it aren't known.
        let mut live = false;
        loop {
            let ws = match self.kernel.connect(&session).await {
                Ok(ws) => ws,
                Err(e) => {
                    log(format!("session {session}: {e:#}"));
                    if !down_noted {
                        self.note(&room, "⚠️ zen isn't reachable right now; retrying.").await;
                        down_noted = true;
                    }
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            down_noted = false;
            // What the session got while this link was down; later messages arrive on the stream.
            self.catch_up(&room, &session, &mut stream, live).await;
            live = true;
            let (mut sink, mut src) = ws.split();
            // The kernel's typing notice lasts a few seconds: renew it while zen works.
            let mut typing = tokio::time::interval(Duration::from_secs(3));
            loop {
                tokio::select! {
                    cmd = rx.recv() => {
                        let Some(cmd) = cmd else { return };
                        let msg = match cmd {
                            Cmd::Prompt(text) => serde_json::json!({ "type": "prompt", "text": stream.prompt(&text) }),
                            Cmd::Abort => serde_json::json!({ "type": "abort" }),
                        };
                        if sink.send(Message::text(msg.to_string())).await.is_err() {
                            self.note(&room, "⚠️ lost the connection to zen before that was sent; send it again.").await;
                            break;
                        }
                    }
                    msg = src.next() => {
                        let text = match msg {
                            Some(Ok(Message::Text(t))) => t,
                            Some(Ok(_)) => continue,
                            _ => break,
                        };
                        let Some(ev) = Event::parse(&text) else { continue };
                        match ev {
                            // A message event carries no seq: it's the cue to fetch what's new.
                            Event::Message { .. } => self.catch_up(&room, &session, &mut stream, true).await,
                            Event::Resync { .. } => {
                                for out in stream.on_event(&ev) {
                                    self.post(&room, out).await;
                                }
                                self.catch_up(&room, &session, &mut stream, true).await;
                            }
                            _ => {
                                for out in stream.on_event(&ev) {
                                    self.post(&room, out).await;
                                }
                            }
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
}
