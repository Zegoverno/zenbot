//! What each decision's caller sends and accepts, fixed in code. A decision file chooses how to
//! decide; the catalog fixes what it may decide: the facts the caller supplies, the exposure of
//! that evidence, the outcomes the caller understands and what each does. So an owner's file can
//! tune any decision but can't, say, let a decision that reads web pages approve an action.

use super::spec::Exposure;

/// What an outcome does where its caller acts on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Toward caution: hold, pause, raise a risk, drop an item.
    Cautious,
    /// Changes nothing the caller would otherwise allow.
    Neutral,
    /// Allows or lowers: a job goes live, a source is rated below "unknown".
    Grant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// One state, the file's questions.
    Single,
    /// One question per item, in one call (`<prefix>_<i>` keys).
    Items,
    /// The file's questions for each of several subjects, in one call (`<i>_<question>` keys).
    Batch,
}

pub struct Kind {
    pub name: &'static str,
    pub exposure: Exposure,
    pub mode: Mode,
    /// Facts the caller supplies (state keys, and lists for `options_from`).
    pub facts: &'static [&'static str],
    /// None: information only, the caller reads the answers itself.
    pub outcomes: Option<&'static [(&'static str, Effect)]>,
    /// The outcome `unavailable` must be (a security decision fails toward caution).
    pub unavailable: Option<&'static str>,
    /// The caller can hand an unsure decision to the owner (`ask_owner`).
    pub owner_rung: bool,
}

use Effect::*;

pub const KINDS: &[Kind] = &[
    Kind {
        name: "job_approval", exposure: Exposure::Owner, mode: Mode::Single,
        facts: &["owner_messages", "job"],
        outcomes: Some(&[("live", Grant), ("paused", Cautious)]), unavailable: Some("paused"), owner_rung: true,
    },
    Kind {
        name: "owner_url", exposure: Exposure::Owner, mode: Mode::Single,
        facts: &["owner_message", "url", "base_rating"],
        outcomes: Some(&[("lower_one", Grant), ("keep", Neutral), ("raise", Cautious)]), unavailable: Some("keep"), owner_rung: false,
    },
    Kind {
        name: "source_risk", exposure: Exposure::Metadata, mode: Mode::Batch,
        facts: &["host", "scope", "path", "path_depth", "reached"],
        outcomes: Some(&[("2", Grant), ("3", Grant), ("4", Neutral)]), unavailable: Some("4"), owner_rung: false,
    },
    Kind {
        name: "content_risk", exposure: Exposure::Outside, mode: Mode::Single,
        facts: &["untrusted_text"],
        outcomes: Some(&[("raise", Cautious), ("keep", Neutral)]), unavailable: Some("raise"), owner_rung: false,
    },
    Kind {
        name: "action_gate", exposure: Exposure::Outside, mode: Mode::Single,
        facts: &["job_data", "proposed_action_UNTRUSTED_DATA", "effect", "context", "trusted"],
        outcomes: Some(&[("run", Neutral), ("hold", Cautious), ("lock", Cautious)]), unavailable: Some("hold"), owner_rung: true,
    },
    Kind {
        name: "web_focus", exposure: Exposure::Outside, mode: Mode::Items,
        facts: &["need"],
        outcomes: Some(&[("keep", Neutral), ("drop", Cautious)]), unavailable: Some("keep"), owner_rung: false,
    },
    Kind {
        name: "egress", exposure: Exposure::Metadata, mode: Mode::Single,
        facts: &["context", "rating"],
        outcomes: Some(&[("allow", Grant), ("hold", Cautious)]), unavailable: Some("hold"), owner_rung: false,
    },
    Kind { name: "web_rerank", exposure: Exposure::Outside, mode: Mode::Items, facts: &["query"], outcomes: None, unavailable: None, owner_rung: false },
    Kind { name: "search_rerank", exposure: Exposure::Private, mode: Mode::Items, facts: &["query"], outcomes: None, unavailable: None, owner_rung: false },
    Kind {
        name: "wiki_capture", exposure: Exposure::Private, mode: Mode::Single,
        facts: &["note", "pages", "candidates"],
        outcomes: Some(&[("add", Neutral), ("skip", Cautious), ("refuse", Cautious)]), unavailable: Some("add"), owner_rung: false,
    },
    Kind {
        name: "skill_duplicate", exposure: Exposure::Private, mode: Mode::Single,
        facts: &["new_skill", "skills"],
        outcomes: Some(&[("duplicate", Cautious), ("new", Neutral)]), unavailable: None, owner_rung: false,
    },
    Kind {
        name: "sleep_judge", exposure: Exposure::Private, mode: Mode::Single,
        facts: &["memory", "source", "age_days", "days_since_used", "other_memories", "user_profile", "agent_identity", "covers"],
        outcomes: None, unavailable: None, owner_rung: false,
    },
    Kind {
        name: "sleep_recheck", exposure: Exposure::Private, mode: Mode::Single,
        facts: &["memory", "source", "age_days", "days_since_used", "other_memories", "user_profile", "agent_identity"],
        outcomes: None, unavailable: None, owner_rung: false,
    },
    Kind { name: "judge", exposure: Exposure::Information, mode: Mode::Items, facts: &["context"], outcomes: None, unavailable: None, owner_rung: false },
];

pub fn get(name: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|k| k.name == name)
}

impl Kind {
    pub fn effect(&self, outcome: &str) -> Option<Effect> {
        if outcome == "ask_owner" && self.owner_rung {
            return Some(Cautious);
        }
        self.outcomes?.iter().find(|(o, _)| *o == outcome).map(|(_, e)| *e)
    }
}
