//! How script commands print: JSON with `--json`, else lines for a person, with text from the
//! kernel sanitized on a terminal.

use std::io::IsTerminal;

use serde_json::{json, Value};
use zen_proto::text_of;

use crate::client::{dim, short, tool_summary};
use crate::md;

/// A model's thinking levels for `zen models`, the default in brackets: "  effort: low [high] max".
pub(crate) fn effort_list(model: &Value) -> String {
    let default = model["default_effort"].as_str();
    let levels: Vec<String> = model["efforts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|e| if Some(e) == default { format!("[{e}]") } else { e.to_string() })
        .collect();
    if levels.is_empty() { String::new() } else { format!("  effort: {}", levels.join(" ")) }
}

pub(crate) fn print_sessions(list: &Value) {
    let rows = list.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("No sessions.");
        return;
    }
    for s in rows {
        let model = s["model"].as_str().unwrap_or("");
        println!(
            "{}  {:<16}  {}  {}",
            short(s["id"].as_str().unwrap_or("")),
            for_stdout(model.split('/').next_back().unwrap_or(model)),
            s["updated_at"].as_str().unwrap_or("").get(..16).unwrap_or("").replace('T', " "),
            for_stdout(s["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("(untitled)"))
        );
    }
}

/// Text for stdout: sanitized when it is a terminal (escape sequences in a model's reply or a
/// tool's output must not reach it), as is when piped.
pub(crate) fn for_stdout(s: &str) -> std::borrow::Cow<'_, str> {
    if std::io::stdout().is_terminal() { md::sanitize(s) } else { s.into() }
}

pub(crate) fn for_stderr(s: &str) -> std::borrow::Cow<'_, str> {
    if std::io::stderr().is_terminal() { md::sanitize(s) } else { s.into() }
}

pub(crate) fn print_messages(s: &Value) {
    println!("{}  ({})", for_stdout(s["title"].as_str().unwrap_or("")), for_stdout(s["model"].as_str().unwrap_or("")));
    for m in s["messages"].as_array().into_iter().flatten() {
        match m["role"].as_str() {
            Some("user") => println!("\n› {}", for_stdout(&text_of(&m["content"]))),
            Some("assistant") => {
                for c in m["content"].as_array().into_iter().flatten() {
                    match c["type"].as_str() {
                        Some("text") => println!("\n{}", for_stdout(c["text"].as_str().unwrap_or(""))),
                        Some("toolCall") => println!("{}", dim(&format!("  ▸ {}", tool_summary(c["name"].as_str().unwrap_or(""), &c["arguments"])))),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// Print `v` as JSON with `--json`, else the human-readable `text`.
pub(crate) fn emit(json: bool, v: &Value, text: impl FnOnce() -> String) {
    if json {
        println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
    } else {
        println!("{}", for_stdout(&text()));
    }
}

/// What a sleep did, in one line.
pub(crate) fn sleep_counts(r: &Value) -> String {
    if let Some(e) = r["error"].as_str() {
        return format!("failed: {e}");
    }
    let n = |k: &str| r[k].as_i64().unwrap_or(0);
    let scorer = r["scorer"].as_str().map(|s| format!(" (judged by {s})")).unwrap_or_else(|| " (by recency: no System One model)".into());
    format!("{} entries: {} kept, {} archived, {} promoted{scorer}", n("entries"), n("kept"), n("dropped"), n("promoted"))
}

/// Short-term memory's size and the last sleep, from `/api/memory`.
pub(crate) fn memory_summary(m: &Value) -> Value {
    let list = m["memories"].as_array().cloned().unwrap_or_default();
    let chars: usize = list.iter().map(|x| x["text"].as_str().map_or(0, str::len) + 10).sum();
    json!({ "entries": list.len(), "chars": chars, "size": m["size"], "last_sleep": m["last_sleep"] })
}

/// The jobs in a few words: how many, the next run, and anything failing or paused.
pub(crate) fn jobs_summary(r: &Value) -> String {
    let all: Vec<&Value> = r["jobs"].as_array().into_iter().flatten().collect();
    if r["scheduler"] == false {
        return format!("{} jobs; the scheduler is OFF in this kernel (ZEN_JOBS=0)", all.len());
    }
    let paused = all.iter().filter(|j| j["enabled"] != true).count();
    let failing: Vec<&str> = all.iter().filter(|j| j["last_status"] == "error").filter_map(|j| j["name"].as_str()).collect();
    let next = all.iter().filter(|j| j["enabled"] == true).filter_map(|j| Some((j["next_run_at"].as_str()?, j["name"].as_str()?))).min();
    let mut s = format!("{} jobs ({paused} paused)", all.len());
    if let Some((t, n)) = next {
        s.push_str(&format!("; next: {n} at {} UTC", t.get(..16).unwrap_or(t).replace('T', " ")));
    }
    if !failing.is_empty() {
        s.push_str(&format!("; last run FAILED: {} (`zen jobs runs`)", failing.join(", ")));
    }
    s
}

/// One job on one line: name, kind, schedule, and when it runs next (or why it's paused).
pub(crate) fn job_line(j: &Value) -> String {
    let t = |k: &str| j[k].as_str().map(|t| t.get(..16).unwrap_or(t).replace('T', " ")).unwrap_or_default();
    let state = if j["enabled"] == true {
        match j["next_run_at"].as_str() {
            Some(_) => format!("next {} UTC", t("next_run_at")),
            None => "nothing ahead".into(),
        }
    } else {
        format!("paused: {}", j["paused_reason"].as_str().unwrap_or("paused"))
    };
    let last = j["last_status"].as_str().map(|s| format!("; last {s} {} UTC", t("last_run_at"))).unwrap_or_default();
    let when = format!("{} ({})", j["schedule"].as_str().unwrap_or(""), j["tz"].as_str().unwrap_or(""));
    format!("{:<16} {:<6} {:<34} {state}{last}", j["name"].as_str().unwrap_or(""), j["kind"].as_str().unwrap_or(""), when)
}

pub(crate) fn memory_line(m: &Value) -> String {
    let s = memory_summary(m);
    let sleep = &s["last_sleep"];
    let last = if sleep.is_null() {
        "no sleep yet".to_string()
    } else {
        let when = sleep["ended_at"].as_str().or(sleep["started_at"].as_str()).unwrap_or("").get(..16).unwrap_or("").replace('T', " ");
        format!("last sleep {when} UTC: {}", sleep_counts(sleep))
    };
    format!("{} entries, {}/{} characters; {last}", s["entries"], s["chars"], s["size"])
}

/// `zen suggestions`: outcomes per prompt version and model, then the latest suggestions.
pub(crate) fn print_suggestions(st: &Value) {
    let n = |v: &Value| v.as_i64().unwrap_or(0);
    for v in st["by_version"].as_array().into_iter().flatten() {
        let decided = n(&v["accepted"]) + n(&v["edited"]) + n(&v["declined"]);
        let rate = if decided > 0 { format!("{:.0}% taken", 100.0 * (n(&v["accepted"]) + n(&v["edited"])) as f64 / decided as f64) } else { "no outcomes yet".into() };
        println!(
            "{} {}  {} shown: {} accepted, {} edited, {} declined, {} unseen, {} open  ({rate}; {:.1}s, ${:.4})",
            for_stdout(v["prompt_version"].as_str().unwrap_or("")),
            dim(&for_stdout(v["model"].as_str().unwrap_or(""))),
            n(&v["shown"]), n(&v["accepted"]), n(&v["edited"]), n(&v["declined"]), n(&v["unseen"]), n(&v["open"]),
            v["latency_ms"].as_f64().unwrap_or(0.0) / 1000.0,
            v["cost_usd"].as_f64().unwrap_or(0.0),
        );
    }
    if st["by_version"].as_array().is_none_or(|a| a.is_empty()) {
        println!("no suggestions yet");
        return;
    }
    println!();
    for r in st["recent"].as_array().into_iter().flatten().take(10) {
        let outcome = r["outcome"].as_str().unwrap_or("open");
        let fin = r["final"].as_str().map(|f| format!(" → {}", for_stdout(&zen_proto::head(f, 60)))).unwrap_or_default();
        println!("{:<9} {}{}", outcome, for_stdout(&zen_proto::head(r["suggested"].as_str().unwrap_or(""), 60)), dim(&fin));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn memory_line_shows_size_and_the_last_sleep() {
        let m = json!({ "size": 4000, "memories": [{ "text": "abc" }, { "text": "defgh" }],
            "last_sleep": { "ended_at": "2026-10-07T04:01:02Z", "entries": 3, "kept": 2, "dropped": 1, "promoted": 0, "proposed": 0, "scorer": null } });
        assert_eq!(memory_line(&m), "2 entries, 28/4000 characters; last sleep 2026-10-07 04:01 UTC: 3 entries: 2 kept, 1 archived, 0 promoted (by recency: no System One model)");
        assert!(memory_line(&json!({ "size": 4000, "memories": [], "last_sleep": null })).ends_with("no sleep yet"));
    }
}
