//! Masking secrets in tool output before the model or the tape sees it (SPEC 5.18, docs/context.md).
//!
//! Two kinds are masked: the values of the kernel's own secrets (environment variables named like a
//! token, key, secret or password, zenbot's API token and the Claude Code and
//! Codex sign-ins, reloaded when those files change), and text in well-known token
//! formats (API keys, GitHub, GitLab, Slack, AWS, Google and Hugging Face tokens, private key blocks).
//! The prefix stays visible so the model knows what was there. A model that needs a secret's value
//! should move it with the shell without printing it.

use std::sync::{Arc, Mutex};

/// Token prefixes and the shortest run of token characters that must follow.
const PREFIXES: [(&str, usize); 18] = [
    ("sk-ant-", 20),
    ("sk-or-", 20),
    ("sk-proj-", 20),
    ("sk-", 32),
    ("ghp_", 30),
    ("gho_", 30),
    ("ghu_", 30),
    ("ghs_", 30),
    ("ghr_", 30),
    ("github_pat_", 30),
    ("glpat-", 20),
    ("xoxb-", 20),
    ("xoxp-", 20),
    ("xoxa-", 20),
    ("AKIA", 16),
    ("ASIA", 16),
    ("AIza", 30),
    ("hf_", 30),
];

const MASK: &str = "…[masked]";

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Whether an environment variable holds a secret: named like a token, key, secret or password, or
/// a connection URL (which carries a password). Its value is masked in output and kept out of the
/// shells the agent runs.
pub fn is_secret_var(name: &str) -> bool {
    let k = name.to_uppercase();
    k == "DATABASE_URL" || ["TOKEN", "KEY", "SECRET", "PASSWORD", "PASSWD"].iter().any(|w| k.contains(w))
}

/// The files and folders that hold secrets: under the zen home zenbot's API token, the settings
/// file, the MCP config and the Matrix channel's sign-in; the engines' sign-ins. Hidden from a
/// verifier's sandboxed shell and refused to its `read` (tools.rs). Only those that exist.
pub fn secret_files() -> Vec<std::path::PathBuf> {
    let home = crate::zen_home();
    let user = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let mut paths: Vec<std::path::PathBuf> = ["token", "env", "mcp.json", "matrix.env", "matrix"].iter().map(|f| home.join(f)).collect();
    paths.push(crate::mcp::config_path());
    paths.extend([user.join(".codex/auth.json"), user.join(".claude/.credentials.json")]);
    paths.retain(|p| p.exists());
    paths
}

/// Files whose secret values are masked: zenbot's token and the engines' sign-ins.
fn known_files() -> Vec<std::path::PathBuf> {
    let home = crate::zen_home();
    let user = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
    vec![home.join("token"), user.join(".codex/auth.json"), user.join(".claude/.credentials.json")]
}

/// Secret values the kernel knows, longest first (so a value containing another is masked whole).
fn load_known(files: &[std::path::PathBuf]) -> Vec<String> {
    let mut values: Vec<String> = std::env::vars().filter(|(k, _)| is_secret_var(k)).map(|(_, v)| v).collect();
    if let Ok(t) = std::fs::read_to_string(&files[0]) {
        values.push(t.trim().to_string());
    }
    for f in &files[1..] {
        if let Ok(text) = std::fs::read_to_string(f) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                collect_strings(&v, &mut values);
            }
        }
    }
    values.retain(|v| v.len() >= 12 && !v.contains(char::is_whitespace));
    values.sort_by_key(|v| std::cmp::Reverse(v.len()));
    values.dedup();
    values
}

type Known = (Vec<Option<std::time::SystemTime>>, Arc<Vec<String>>);

/// The known values, read again whenever one of their files changes (a new sign-in or a rotated
/// token is masked without a restart).
fn known() -> Arc<Vec<String>> {
    static KNOWN: Mutex<Option<Known>> = Mutex::new(None);
    let files = known_files();
    let stamps: Vec<_> = files.iter().map(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok()).collect();
    let mut cache = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    match cache.as_ref() {
        Some((at, values)) if *at == stamps => values.clone(),
        _ => {
            let values = Arc::new(load_known(&files));
            *cache = Some((stamps, values.clone()));
            values
        }
    }
}

fn collect_strings(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) if s.len() >= 20 => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_strings(x, out)),
        serde_json::Value::Object(o) => o.values().for_each(|x| collect_strings(x, out)),
        _ => {}
    }
}

/// `text` with known secret values and token-shaped strings masked.
pub fn mask(text: &str) -> String {
    mask_with(text, &known())
}

/// `mask` for text that may be large (up to 16 MB of command output), off the runtime's threads.
pub async fn mask_off_thread(text: String) -> String {
    if text.len() <= 64 * 1024 {
        return mask(&text);
    }
    tokio::task::spawn_blocking(move || mask(&text)).await.unwrap_or_else(|e| format!("(the output couldn't be masked: {e})"))
}

fn mask_with(text: &str, known: &[String]) -> String {
    let mut out = text.to_string();
    for v in known {
        if out.contains(v.as_str()) {
            out = out.replace(v.as_str(), &format!("{}{MASK}", &v[..v.floor_char_boundary(4)]));
        }
    }
    out = mask_private_keys(&out);
    mask_prefixed(&out)
}

/// The bytes that start one of PREFIXES: the scan skips ahead to the next such byte.
const STARTS: [bool; 256] = {
    let mut t = [false; 256];
    let mut i = 0;
    while i < PREFIXES.len() {
        t[PREFIXES[i].0.as_bytes()[0] as usize] = true;
        i += 1;
    }
    t
};

fn may_start_token(b: u8) -> bool {
    STARTS[b as usize]
}

fn mask_prefixed(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    'scan: while !rest.is_empty() {
        // Only ASCII bytes start a prefix, so the skip ends on a char boundary.
        let skip = rest.bytes().position(may_start_token).unwrap_or(rest.len());
        out.push_str(&rest[..skip]);
        rest = &rest[skip..];
        if rest.is_empty() {
            break;
        }
        for (prefix, min) in PREFIXES {
            if let Some(after) = rest.strip_prefix(prefix) {
                let preceded_by_token = out.chars().last().is_some_and(token_char);
                let run = after.chars().take_while(|c| token_char(*c)).count();
                if !preceded_by_token && run >= min {
                    out.push_str(prefix);
                    out.push_str(MASK);
                    rest = &after[after.char_indices().nth(run).map(|(i, _)| i).unwrap_or(after.len())..];
                    continue 'scan;
                }
            }
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Private key blocks keep their BEGIN and END lines; what's between is masked.
fn mask_private_keys(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("-----BEGIN ") {
        let Some(close) = rest[start + 11..].find("-----") else { break };
        let header_end = start + 11 + close + 5;
        let is_key = rest[start..header_end].contains("PRIVATE KEY");
        out.push_str(&rest[..header_end]);
        rest = &rest[header_end..];
        if is_key {
            out.push('\n');
            out.push_str(MASK);
            out.push('\n');
            rest = rest.find("-----END ").map(|e| &rest[e..]).unwrap_or("");
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_token_formats_but_not_ordinary_text() {
        let gh = format!("ghp_{}", "a1B2".repeat(9));
        let text = format!("export GITHUB_TOKEN={gh}\nAKIAIOSFODNN7EXAMPLE and sk-learn and task-1234 and my-sk-key");
        let masked = mask_with(&text, &[]);
        assert!(!masked.contains(&gh[4..]));
        assert!(masked.contains("ghp_…[masked]"));
        assert!(masked.contains("AKIA…[masked]"));
        assert!(masked.contains("sk-learn and task-1234 and my-sk-key"), "short or embedded matches stay: {masked}");
        let wide = format!("é→ {gh} ü{gh}\n{}", "ß".repeat(1000));
        let masked = mask_with(&wide, &[]);
        assert!(masked.starts_with("é→ ghp_…[masked] ü"), "{masked}");
        assert_eq!(masked.matches("[masked]").count(), 2, "a token after a non-token character is masked");
        assert!(masked.ends_with(&"ß".repeat(1000)));
    }

    #[test]
    fn secret_variables_by_name() {
        for k in ["ZEN_TOKEN", "OPENROUTER_API_KEY", "DATABASE_URL", "aws_secret_access_key", "PGPASSWORD"] {
            assert!(is_secret_var(k), "{k}");
        }
        for k in ["PATH", "HOME", "ZEN_PORT", "LANG"] {
            assert!(!is_secret_var(k), "{k}");
        }
        // A known value starting with a multibyte character is masked without panicking.
        assert_eq!(mask_with("x ééééééééééééé y", &["ééééééééééééé".into()]), "x éé…[masked] y");
    }

    #[test]
    fn masks_known_values_and_private_keys() {
        let known = vec!["zen-api-token-0123456789".to_string()];
        let text = "token: zen-api-token-0123456789\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----\nafter\n-----BEGIN CERTIFICATE-----\nMIIB\n";
        let masked = mask_with(text, &known);
        assert!(!masked.contains("0123456789"));
        assert!(!masked.contains("b3BlbnNzaC1rZXktdjEAAAAA"));
        assert!(masked.contains("-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(masked.contains("-----END OPENSSH PRIVATE KEY-----\nafter"));
        assert!(masked.contains("-----BEGIN CERTIFICATE-----\nMIIB"), "certificates are public");
    }
}
