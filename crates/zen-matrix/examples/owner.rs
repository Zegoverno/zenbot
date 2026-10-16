//! A scripted owner for the end-to-end test (scripts/matrix-e2e.sh): signs in to a test homeserver
//! as the owner with a cross-signed device, accepts the bot's invites, and talks to zen in the "zen"
//! room through end-to-end encryption, checking the answers; then checks that the bot refuses a
//! plaintext message and an unverified session's, posts an answer that came while it was down
//! (the caller restarts it when this prints `ready for the restart`), declines an unencrypted room
//! and leaves a room a stranger is invited to.
//!
//!   cargo run --example owner -- <homeserver> <user> <password> <store-dir> [job-name]
//!
//! With a job name it then waits for that job's report in the "zen jobs" room (the caller runs it
//! after this prints `rooms checked`).

#![recursion_limit = "256"]

use anyhow::{bail, Context, Result};
use matrix_sdk::{
    config::SyncSettings,
    room::Room,
    ruma::{
        api::client::{message::send_message_event, room::create_room::v3::Request as CreateRoom, uiaa},
        events::{
            room::{
                encryption::RoomEncryptionEventContent,
                message::{MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent},
            },
            InitialStateEvent,
        },
        OwnedUserId, TransactionId, UserId,
    },
    Client, RoomMemberships, RoomState,
};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (hs, user, password, dir, job) = match &args[..] {
        [_, hs, user, password, dir] => (hs, user, password, dir, None),
        [_, hs, user, password, dir, job] => (hs, user, password, dir, Some(job.clone())),
        _ => bail!("usage: owner <homeserver> <user> <password> <store-dir> [job-name]"),
    };
    let c = Client::builder().homeserver_url(hs).sqlite_store(dir, None).build().await?;
    c.matrix_auth().login_username(user, password).initial_device_display_name("owner-test").send().await?;
    c.sync_once(SyncSettings::default()).await?;
    // A cross-signing identity that signs this device: the bot answers only cross-signed devices.
    let enc = c.encryption();
    enc.wait_for_e2ee_initialization_tasks().await;
    if let Err(e) = enc.bootstrap_cross_signing_if_needed(None).await {
        let Some(u) = e.as_uiaa_response() else { return Err(e).context("cross-signing") };
        let mut auth = uiaa::Password::new(uiaa::UserIdentifier::Matrix(uiaa::MatrixUserIdentifier::new(user.clone())), password.clone());
        auth.session = u.session.clone();
        enc.bootstrap_cross_signing(Some(uiaa::AuthData::Password(auth))).await.context("cross-signing")?;
    }
    println!("ok: the owner's device is cross-signed");

    // Accept the bot's invites and find the main room (encrypted, named "zen").
    let deadline = Instant::now() + Duration::from_secs(60);
    let room = loop {
        for r in c.invited_rooms() {
            r.join().await.ok();
        }
        c.sync_once(SyncSettings::default().timeout(Duration::from_secs(2))).await?;
        if let Some(r) = c.joined_rooms().into_iter().find(|r| r.name().as_deref() == Some("zen")) {
            break r;
        }
        if Instant::now() > deadline {
            bail!("no invite to the zen room");
        }
    };
    if !room.latest_encryption_state().await?.is_encrypted() {
        bail!("the zen room is not encrypted");
    }
    let jobs = c.joined_rooms().into_iter().any(|r| r.name().as_deref() == Some("zen jobs"));
    println!("ok: joined the encrypted zen room{}", if jobs { " and the jobs room" } else { "" });

    // Everything the bot posts, decrypted.
    let (tx, mut rx) = mpsc::unbounded_channel::<(bool, String)>();
    let (post_tx, mut post_rx) = mpsc::unbounded_channel::<String>();
    let (queue_tx, mut queue_rx) = mpsc::unbounded_channel();
    let me = c.user_id().unwrap().to_owned();
    c.add_event_handler(move |ev: OriginalSyncRoomMessageEvent, r: Room| {
        let tx = tx.clone();
        let post_tx = post_tx.clone();
        let queue_tx = queue_tx.clone();
        let me = me.clone();
        async move {
            if ev.sender == me || r.state() != RoomState::Joined {
                return;
            }
            let (notice, body) = match &ev.content.msgtype {
                MessageType::Text(t) => (false, t.body.clone()),
                MessageType::Notice(n) => (true, n.body.clone()),
                _ => return,
            };
            if body.contains("⏰ *matrix-e2e-main*") {
                post_tx.send(r.room_id().to_string()).ok();
            }
            if body.contains("Approval queue action/") && body.contains("MATRIX-QUEUE-") {
                queue_tx.send((r, ev.event_id, body.clone())).ok();
            }
            tx.send((notice, body)).ok();
        }
    });
    let sync = c.clone();
    tokio::spawn(async move { sync.sync(SyncSettings::default()).await });
    tokio::time::sleep(Duration::from_secs(2)).await;

    room.send(RoomMessageEventContent::text_plain("!help")).await?;
    wait_for(&mut rx, "!new").await?;
    println!("ok: !help answered");
    room.send(RoomMessageEventContent::text_plain("!sources add example.test/docs 3 test docs")).await?;
    wait_for(&mut rx, "Allowlisted").await?;
    room.send(RoomMessageEventContent::text_plain("!sources")).await?;
    wait_for(&mut rx, "example.test/docs").await?;
    room.send(RoomMessageEventContent::text_plain("!sources rm example.test/docs")).await?;
    wait_for(&mut rx, "Removed").await?;
    room.send(RoomMessageEventContent::text_plain("!sources")).await?;
    wait_for(&mut rx, "No allowlisted sources").await?;
    println!("ok: encrypted owner source CRUD bypasses the model");

    room.send(RoomMessageEventContent::text_plain("hello zen")).await?;
    wait_for(&mut rx, "Smoke test passed").await?;
    println!("ok: a prompt went to zen and the answer came back encrypted");

    room.send(RoomMessageEventContent::text_plain("!deny")).await?;
    wait_for(&mut rx, "Stopped. This session and its products").await?;
    room.send(RoomMessageEventContent::text_plain("!trust yes")).await?;
    wait_for(&mut rx, "Look first:").await?;
    room.send(RoomMessageEventContent::text_plain("!trust")).await?;
    wait_for(&mut rx, "owner /deny").await?;
    room.send(RoomMessageEventContent::text_plain("!trust yes")).await?;
    wait_for(&mut rx, "Contamination caused by this session is reversed").await?;
    room.send(RoomMessageEventContent::text_plain("hello after trust")).await?;
    wait_for(&mut rx, "Smoke test passed").await?;
    println!("ok: encrypted !deny stops; !trust requires review and resumes the session");

    room.send(RoomMessageEventContent::text_plain("please ask me")).await?;
    wait_for(&mut rx, "Reply with option numbers").await?;
    room.send(RoomMessageEventContent::text_plain("2 1")).await?;
    wait_for(&mut rx, "you chose: Which color? → blue").await?;
    println!("ok: questions answered by number");

    // A plaintext message in the encrypted room (sent past the client's encryption) is refused.
    let plain = send_message_event::v3::Request::new(room.room_id().to_owned(), TransactionId::new(), &RoomMessageEventContent::text_plain("hello in plaintext"))?;
    c.send(plain).await?;
    wait_for(&mut rx, "zen ignored that message: the message wasn't encrypted").await?;
    println!("ok: a plaintext message was refused");

    // A second session of the owner's account that nobody verified: the bot doesn't answer it.
    let other = Client::builder().homeserver_url(hs).sqlite_store(format!("{dir}-unverified"), None).build().await?;
    other.matrix_auth().login_username(user, password).initial_device_display_name("owner-unverified").send().await?;
    other.sync_once(SyncSettings::default()).await?;
    let other_room = other.get_room(room.room_id()).context("the unverified session doesn't see the zen room")?;
    other_room.send(RoomMessageEventContent::text_plain("!help")).await?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    room.send(RoomMessageEventContent::text_plain("!session")).await?;
    let mut seen = vec![];
    let fut = async {
        while let Some((_, body)) = rx.recv().await {
            if body.starts_with("Session `") {
                return Ok(());
            }
            seen.push(body);
        }
        bail!("stream ended")
    };
    tokio::time::timeout(Duration::from_secs(60), fut).await.context("no answer to !session")??;
    tokio::time::sleep(Duration::from_secs(3)).await;
    while let Ok((_, body)) = rx.try_recv() {
        seen.push(body);
    }
    if seen.iter().any(|b| b.contains("zen in Matrix")) {
        bail!("the bot answered an unverified session: {seen:?}");
    }
    println!("ok: an unverified session of the owner's was not answered");

    // The caller stops the bridge, runs a turn in the room's session from the terminal, and
    // starts the bridge again: the answer comes to the room.
    println!("ready for the restart");
    wait_for(&mut rx, "Smoke test passed").await?;
    println!("ok: an answer from while the bridge was down was posted after its restart");

    // Rooms: an unencrypted one is declined; an encrypted one is joined, and left once a
    // stranger is invited.
    let bot = room.members(RoomMemberships::JOIN).await?.into_iter().map(|m| m.user_id().to_owned()).find(|u| Some(&**u) != c.user_id()).context("no bot in the zen room")?;
    let plain_room = make_room(&c, &bot, "plain", false).await?;
    wait_membership(&plain_room, &bot, "leave").await.context("the bot didn't decline an unencrypted room")?;
    println!("ok: an unencrypted room was declined");
    let side = make_room(&c, &bot, "side", true).await?;
    wait_membership(&side, &bot, "join").await.context("the bot didn't join an encrypted room")?;
    let stranger = UserId::parse(format!("@stranger:{}", bot.server_name()))?;
    side.invite_user_by_id(&stranger).await?;
    wait_membership(&side, &bot, "leave").await.context("the bot stayed in a room a stranger was invited to")?;
    println!("ok: the bot left a room a stranger was invited to");
    println!("rooms checked");

    // Seeded queue actions: plaintext reactions must leave them pending; encrypted commands decide.
    for _ in 0..3 {
        let (r, event, body) = tokio::time::timeout(Duration::from_secs(90), queue_rx.recv())
            .await.context("no queue notice")?.context("queue stream ended")?;
        let emoji = if body.contains("MATRIX-QUEUE-approve") { "✅" }
            else if body.contains("MATRIX-QUEUE-reject") { "❌" } else { "🛑" };
        // Ordinary SDK reactions are plaintext and must have no authority.
        r.send(matrix_sdk::ruma::events::reaction::ReactionEventContent::new(
            matrix_sdk::ruma::events::relation::Annotation::new(event, emoji.into()))).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let key=body.split("Approval queue ").nth(1).context("queue key")?
            .split_whitespace().next().context("queue id")?.trim_end_matches('*');
        let decision=match emoji {"✅"=>"approve","❌"=>"reject",_=>"deny"};
        r.send(RoomMessageEventContent::text_plain("!queue")).await?;
        let pending=key.replace('/'," #");
        wait_for(&mut rx,&pending).await.context("plaintext reaction must leave action pending")?;
        r.send(RoomMessageEventContent::text_plain(format!("!queue {decision} {}",key.replace('/'," ")))).await?;
    }
    println!("ok: plaintext queue reactions ignored; encrypted verified owner commands sent");

    if let Some(job) = job {
        // The caller runs the job now: its report comes to the jobs room within a minute.
        println!("waiting for the report of job {job}");
        wait_for(&mut rx, &format!("**{job}**")).await?;
        println!("ok: the job's report reached the jobs room");
        let rid = tokio::time::timeout(Duration::from_secs(90), post_rx.recv())
            .await.context("no report posted into the main room")?
            .context("posted-report stream ended")?;
        if rid != room.room_id().as_str() {
            bail!("the targeted report went to {rid}, not the main room");
        }
        println!("ok: a targeted job report reached the encrypted main room");
    }
    Ok(())
}

/// A room the owner makes, with the bot invited.
async fn make_room(c: &Client, bot: &OwnedUserId, name: &str, encrypted: bool) -> Result<Room> {
    let mut req = CreateRoom::new();
    req.name = Some(name.into());
    req.invite = vec![bot.clone()];
    if encrypted {
        req.initial_state = vec![InitialStateEvent::with_empty_state_key(RoomEncryptionEventContent::with_recommended_defaults()).to_raw_any()];
    }
    Ok(c.create_room(req).await?)
}

/// Wait (60 s at most) until `user`'s membership in `room` is `want` ("join", "leave").
async fn wait_membership(room: &Room, user: &UserId, want: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(m) = room.get_member_no_sync(user).await? {
            if m.membership().as_str() == want {
                return Ok(());
            }
        }
        if Instant::now() > deadline {
            bail!("{user} never got to {want}");
        }
        // The background sync keeps the room's members current.
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// The bot's messages until one contains `want` (90 s at most).
async fn wait_for(rx: &mut mpsc::UnboundedReceiver<(bool, String)>, want: &str) -> Result<String> {
    let fut = async {
        while let Some((notice, body)) = rx.recv().await {
            println!("  bot{}: {}", if notice { " (notice)" } else { "" }, body.lines().next().unwrap_or(""));
            if body.contains(want) {
                return Ok(body);
            }
        }
        bail!("stream ended")
    };
    tokio::time::timeout(Duration::from_secs(90), fut).await.with_context(|| format!("no message containing `{want}`"))?
}
