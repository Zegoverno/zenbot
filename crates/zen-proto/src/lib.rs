//! Helpers shared by the kernel, the engine worker and the CLI, so they read messages the same way.
//! Messages use Pi's format (docs/worker-protocol.md): `content` is a string or a list of parts.
//! `client` types the client protocol's events (docs/client-protocol.md), `worker` the worker
//! protocol's messages (docs/worker-protocol.md).

use serde::{Deserialize, Deserializer};
use serde_json::Value;

pub mod client;
pub mod worker;

/// A field that may be missing or `null`: its default then.
pub(crate) fn or_default<'de, D: Deserializer<'de>, T: Default + Deserialize<'de>>(d: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// The text of a message's content: a string as is, or its text parts joined by newlines.
pub fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// What the kernel's provenance record of a result hashes: its text, then each image's media type
/// and data. A text-only result digests as its text, so older records still match.
pub fn digest_text(content: &Value) -> String {
    let mut text = text_of(content);
    for part in content.as_array().into_iter().flatten().filter(|p| p["type"] == "image") {
        text.push_str(&format!("\n[image {} {}]", part["mimeType"].as_str().unwrap_or(""), part["data"].as_str().unwrap_or("")));
    }
    text
}

/// The first `max` characters of `s`, saying how much was left out.
pub fn head(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max).collect();
    format!("{kept} [… {} more characters]", n - max)
}

/// The last `max` characters of `s` (where errors and results usually are).
pub fn tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    s.chars().skip(n - max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_and_cuts() {
        assert_eq!(text_of(&json!("hi")), "hi");
        assert_eq!(text_of(&json!([{ "type": "text", "text": "a" }, { "type": "thinking", "thinking": "x" }, { "type": "text", "text": "b" }])), "a\nb");
        assert_eq!(text_of(&json!(null)), "");
        assert_eq!(head("héllo", 2), "hé [… 3 more characters]");
        assert_eq!(head("hi", 5), "hi");
        assert_eq!(tail("héllo", 3), "llo");
    }
}
