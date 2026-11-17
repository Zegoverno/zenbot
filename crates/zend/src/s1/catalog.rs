//! What each decision's caller sends and accepts, fixed in code. A decision file chooses how to
//! decide; the catalog fixes what it may decide: the facts the caller supplies, the exposure of
//! that evidence and the outcomes the caller understands.

use super::spec::Exposure;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// One state, the file's questions.
    Single,
    /// One question per item, in one call (`<prefix>_<i>` keys).
    Items,
}

pub struct Kind {
    pub name: &'static str,
    pub exposure: Exposure,
    pub mode: Mode,
    /// Facts the caller supplies (state keys, and lists for `options_from`).
    pub facts: &'static [&'static str],
    /// None: information only, the caller reads the answers itself.
    pub outcomes: Option<&'static [&'static str]>,
}

pub const KINDS: &[Kind] = &[
    Kind { name: "memory_capture", exposure: Exposure::Private, mode: Mode::Items, facts: &[], outcomes: Some(&["save", "skip"]) },
    Kind { name: "memory_durable", exposure: Exposure::Private, mode: Mode::Items, facts: &["memories"], outcomes: Some(&["promote", "lasting", "keep"]) },
    Kind { name: "memory_disposition", exposure: Exposure::Private, mode: Mode::Items, facts: &["memories"], outcomes: Some(&["archive", "keep"]) },
    Kind { name: "memory_long", exposure: Exposure::Private, mode: Mode::Items, facts: &[], outcomes: Some(&["long", "short"]) },
    Kind { name: "web_focus", exposure: Exposure::Outside, mode: Mode::Items, facts: &["need"], outcomes: Some(&["keep", "drop"]) },
    Kind { name: "web_rerank", exposure: Exposure::Outside, mode: Mode::Items, facts: &["query"], outcomes: None },
    Kind { name: "search_rerank", exposure: Exposure::Private, mode: Mode::Items, facts: &["query"], outcomes: Some(&["keep", "drop"]) },
    Kind { name: "skill_duplicate", exposure: Exposure::Private, mode: Mode::Single, facts: &["new_skill", "skills"], outcomes: Some(&["duplicate", "new"]) },
    Kind { name: "judge", exposure: Exposure::Information, mode: Mode::Items, facts: &["context"], outcomes: None },
];

pub fn get(name: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|k| k.name == name)
}
