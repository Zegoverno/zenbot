//! The System One models the owner approved (`~/.zenbot/s1/models.toml`, owner-only). A decision's
//! ladder names models from this list; a name that isn't on it is a checker error, so no decision
//! sends data to a model the owner didn't approve. Without the file, one model named `default`
//! comes from `ZEN_S1_MODEL` (and System One is off when that's unset too).
//!
//! ```toml
//! default = "jev"
//! [model.jev]
//! id = "typesafe/jev-1.13"
//! note = "fast general classifier"
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    default: String,
    #[serde(default)]
    model: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Default)]
pub struct Models {
    pub default: Option<String>,
    pub list: BTreeMap<String, Entry>,
    /// Where they came from: the file, or `ZEN_S1_MODEL`.
    pub source: String,
}

pub fn path() -> PathBuf {
    super::dir().join("models.toml")
}

impl Models {
    /// Parse the owner's file. Errors carry the toml crate's message (with its position).
    pub fn parse(src: &str) -> Result<Models, String> {
        let f: File = toml::from_str(src).map_err(|e| super::check::position(src, &e))?;
        if !f.model.contains_key(&f.default) {
            return Err(format!("default `{}` isn't one of the listed models ({})", f.default, f.model.keys().cloned().collect::<Vec<_>>().join(", ")));
        }
        if let Some((name, _)) = f.model.iter().find(|(_, e)| e.id.trim().is_empty()) {
            return Err(format!("model `{name}` has an empty id"));
        }
        Ok(Models { default: Some(f.default), list: f.model, source: path().display().to_string() })
    }

    /// From `ZEN_S1_MODEL` alone (no file).
    pub fn from_env() -> Models {
        match crate::config::get().s1_model.clone() {
            Some(id) => Models {
                default: Some("default".into()),
                list: BTreeMap::from([("default".into(), Entry { id, note: "ZEN_S1_MODEL".into() })]),
                source: "ZEN_S1_MODEL".into(),
            },
            None => Models { source: "none (ZEN_S1_MODEL unset, no models.toml)".into(), ..Default::default() },
        }
    }

    /// The OpenRouter id behind a ladder name (`default` is the list's default).
    pub fn id(&self, name: &str) -> Option<&str> {
        let name = if name == "default" { self.default.as_deref()? } else { name };
        self.list.get(name).map(|e| e.id.as_str())
    }

    pub fn knows(&self, name: &str) -> bool {
        self.id(name).is_some()
    }

    pub fn on(&self) -> bool {
        self.default.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_owner_lists_models_and_a_default() {
        let m = Models::parse("default = \"jev\"\n[model.jev]\nid = \"typesafe/jev-1.13\"\n[model.big]\nid = \"x/y\"\n").unwrap();
        assert_eq!(m.id("default"), Some("typesafe/jev-1.13"));
        assert_eq!(m.id("big"), Some("x/y"));
        assert!(!m.knows("other"));
        assert!(Models::parse("default = \"nope\"\n[model.jev]\nid = \"a\"\n").unwrap_err().contains("isn't one of"));
        assert!(Models::parse("default = \"jev\"\n[model.jev]\nidd = \"a\"\n").unwrap_err().contains("idd"));
    }
}
