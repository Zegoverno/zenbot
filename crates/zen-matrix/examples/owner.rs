//! A scripted owner for the end-to-end test (scripts/matrix-e2e.sh): signs in to a test homeserver
//! as the owner, accepts the bot's invites, and talks to zen in the "zen" room through end-to-end
//! encryption, checking the answers.
//!
//!   cargo run --example owner -- <homeserver> <user> <password> <store-dir> [job-name]
//!
//! With a job name it then waits for that job's report in the "zen jobs" room (the caller runs it).

#![recursion_limit = "256"]

use anyhow::{bail, Context, Result};
use matrix_sdk::{
    config::SyncSettings,
    room::Room,
    ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent},
    Client, RoomState,
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
    let me = c.user_id().unwrap().to_owned();
    c.add_event_handler(move |ev: OriginalSyncRoomMessageEvent, r: Room| {
        let tx = tx.clone();
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
            tx.send((notice, body)).ok();
        }
    });
    let sync = c.clone();
    tokio::spawn(async move { sync.sync(SyncSettings::default()).await });
    tokio::time::sleep(Duration::from_secs(2)).await;

    room.send(RoomMessageEventContent::text_plain("!help")).await?;
    wait_for(&mut rx, "!new").await?;
    println!("ok: !help answered");

    room.send(RoomMessageEventContent::text_plain("hello zen")).await?;
    wait_for(&mut rx, "Smoke test passed").await?;
    println!("ok: a prompt went to zen and the answer came back encrypted");

    room.send(RoomMessageEventContent::text_plain("please ask me")).await?;
    wait_for(&mut rx, "Reply with option numbers").await?;
    room.send(RoomMessageEventContent::text_plain("2 1")).await?;
    wait_for(&mut rx, "you chose: Which color? → blue").await?;
    println!("ok: questions answered by number");
    if let Some(job) = job {
        // The caller runs the job now: its report comes to the jobs room within a minute.
        println!("waiting for the report of job {job}");
        wait_for(&mut rx, &format!("**{job}**")).await?;
        println!("ok: the job's report reached the jobs room");
    }
    Ok(())
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
