//! Whom and where the bridge answers, without Matrix or the network: the decisions on invites,
//! rooms, messages and the owner's identity. The bridge gathers the facts (membership, encryption,
//! the sending device's trust, the owner's master key) and acts on what these return, so every
//! rule is unit-tested here.
//!
//! The rules (docs/matrix.md, Security):
//! - Only the owner (`MATRIX_OWNER`) is answered; the bot's own messages and anyone else's are
//!   ignored.
//! - A message counts only when it was end-to-end encrypted from one of the owner's devices that
//!   the owner's cross-signing identity signed. matrix-sdk refuses to decrypt anything else
//!   (`TrustRequirement::CrossSigned`); this checks it again on the decrypted event.
//! - The owner's identity (master key) is pinned the first time a message is accepted; a different
//!   one is refused until the owner resets the pin on the machine (`zen-matrix repin-owner`).
//! - Rooms are encrypted and hold only the owner and the bot (joined or invited). An invite is
//!   joined only from the owner to such a room; a room that gains anyone else is left.

/// How a room stands, from its encryption and its active (joined or invited) members.
#[derive(Debug, PartialEq)]
pub enum RoomCheck {
    Ok,
    NotEncrypted,
    /// Someone other than the owner and the bot is in the room (or invited).
    Stranger(String),
}

/// A room is fine when it is encrypted and every active member is the owner or the bot. The owner
/// not being there (yet, or any more) is fine: nobody else reads what the bot posts.
pub fn check_room<'a>(owner: &str, bot: &str, encrypted: bool, members: impl IntoIterator<Item = &'a str>) -> RoomCheck {
    if let Some(m) = members.into_iter().find(|m| *m != owner && *m != bot) {
        return RoomCheck::Stranger(m.to_string());
    }
    if !encrypted {
        return RoomCheck::NotEncrypted;
    }
    RoomCheck::Ok
}

/// What to do with an invite.
#[derive(Debug, PartialEq)]
pub enum Invite {
    /// Join, then check the room again with its full state (`check_room`).
    Join,
    Decline(String),
}

/// An invite is joined only from the owner, to a room that isn't known to be unencrypted
/// (`encrypted` is None when the invite's state doesn't say: the room is checked after joining).
pub fn invite(owner: &str, inviter: Option<&str>, encrypted: Option<bool>) -> Invite {
    match inviter {
        Some(i) if i == owner => {}
        other => return Invite::Decline(format!("the invite is from {}, not the owner", other.unwrap_or("someone unknown"))),
    }
    if encrypted == Some(false) {
        return Invite::Decline("the room isn't encrypted".into());
    }
    Invite::Join
}

/// How the message reached the bot.
#[derive(Debug, PartialEq)]
pub enum Sealed {
    /// Not encrypted at all.
    Plain,
    /// Encrypted from a device the sender's cross-signing identity signed.
    CrossSigned,
    /// Encrypted, but the device isn't cross-signed (or its identity changed, or the key came from
    /// an insecure source): why, as matrix-sdk says it.
    Untrusted(String),
}

/// What to do with a message.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    /// The owner's message: act on it (after checking the owner's identity, `identity`).
    Act,
    /// Not for the bridge (its own messages, other people's): drop it quietly.
    Ignore,
    /// The owner's account, but not trustworthy: drop it and log why.
    Refuse(String),
}

pub fn message(owner: &str, bot: &str, sender: &str, sealed: &Sealed, room: &RoomCheck) -> Verdict {
    if sender == bot || sender != owner {
        return Verdict::Ignore;
    }
    match room {
        RoomCheck::Ok => {}
        RoomCheck::NotEncrypted => return Verdict::Refuse("the room isn't encrypted".into()),
        RoomCheck::Stranger(m) => return Verdict::Refuse(format!("{m} is in the room")),
    }
    match sealed {
        Sealed::CrossSigned => Verdict::Act,
        Sealed::Plain => Verdict::Refuse("the message wasn't encrypted".into()),
        Sealed::Untrusted(why) => Verdict::Refuse(format!("the sending device isn't trusted: {why}")),
    }
}

/// The owner's identity against the pinned one.
#[derive(Debug, PartialEq)]
pub enum Identity {
    /// Nothing pinned yet: pin this master key.
    Pin(String),
    Same,
    /// The owner's master key isn't the pinned one: refuse until the owner resets the pin.
    Changed { pinned: String, now: String },
    /// The owner has no cross-signing identity the bot can see.
    Missing,
}

pub fn identity(pinned: Option<&str>, now: Option<&str>) -> Identity {
    match (pinned, now) {
        (_, None) => Identity::Missing,
        (None, Some(now)) => Identity::Pin(now.to_string()),
        (Some(p), Some(now)) if p == now => Identity::Same,
        (Some(p), Some(now)) => Identity::Changed { pinned: p.to_string(), now: now.to_string() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "@ze:beeper.com";
    const BOT: &str = "@zen-bot:matrix.org";

    #[test]
    fn rooms_hold_only_the_owner_and_the_bot_and_are_encrypted() {
        assert_eq!(check_room(OWNER, BOT, true, [BOT, OWNER]), RoomCheck::Ok);
        assert_eq!(check_room(OWNER, BOT, true, [BOT]), RoomCheck::Ok, "the owner not (yet) there is fine");
        assert_eq!(check_room(OWNER, BOT, true, [BOT, OWNER, "@eve:x.org"]), RoomCheck::Stranger("@eve:x.org".into()));
        assert_eq!(check_room(OWNER, BOT, false, [BOT, OWNER]), RoomCheck::NotEncrypted);
        assert_eq!(check_room(OWNER, BOT, false, [BOT, "@eve:x.org"]), RoomCheck::Stranger("@eve:x.org".into()), "a stranger is the louder reason");
    }

    #[test]
    fn invites_only_from_the_owner_to_rooms_not_known_plain() {
        assert_eq!(invite(OWNER, Some(OWNER), Some(true)), Invite::Join);
        assert_eq!(invite(OWNER, Some(OWNER), None), Invite::Join, "checked again after joining");
        assert!(matches!(invite(OWNER, Some(OWNER), Some(false)), Invite::Decline(_)));
        assert!(matches!(invite(OWNER, Some("@eve:x.org"), Some(true)), Invite::Decline(_)));
        assert!(matches!(invite(OWNER, None, Some(true)), Invite::Decline(_)));
    }

    #[test]
    fn only_the_owners_cross_signed_encrypted_messages_in_good_rooms_count() {
        let ok = RoomCheck::Ok;
        assert_eq!(message(OWNER, BOT, OWNER, &Sealed::CrossSigned, &ok), Verdict::Act);
        assert_eq!(message(OWNER, BOT, BOT, &Sealed::CrossSigned, &ok), Verdict::Ignore, "never answers itself");
        assert_eq!(message(OWNER, BOT, "@eve:x.org", &Sealed::CrossSigned, &ok), Verdict::Ignore);
        assert!(matches!(message(OWNER, BOT, OWNER, &Sealed::Plain, &ok), Verdict::Refuse(_)));
        assert!(matches!(message(OWNER, BOT, OWNER, &Sealed::Untrusted("unsigned device".into()), &ok), Verdict::Refuse(_)));
        assert!(matches!(message(OWNER, BOT, OWNER, &Sealed::CrossSigned, &RoomCheck::NotEncrypted), Verdict::Refuse(_)));
        assert!(matches!(message(OWNER, BOT, OWNER, &Sealed::CrossSigned, &RoomCheck::Stranger("@eve:x.org".into())), Verdict::Refuse(_)));
    }

    #[test]
    fn the_owners_identity_is_pinned_on_first_use() {
        assert_eq!(identity(None, Some("K1")), Identity::Pin("K1".into()));
        assert_eq!(identity(Some("K1"), Some("K1")), Identity::Same);
        assert_eq!(identity(Some("K1"), Some("K2")), Identity::Changed { pinned: "K1".into(), now: "K2".into() });
        assert_eq!(identity(Some("K1"), None), Identity::Missing);
        assert_eq!(identity(None, None), Identity::Missing);
    }
}
