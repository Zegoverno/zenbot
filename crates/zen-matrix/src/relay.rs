//! What the bridge says, without Matrix or the network: the owner's messages read as commands or
//! prompts, the kernel's session events turned into messages for the room, and job runs turned
//! into reports. Everything here is pure, so it is tested without a homeserver or a kernel.

use serde_json::Value;
use zen_proto::text_of;

/// Longest Markdown body of one message, in bytes. A Matrix event may be 65,535 bytes; an
/// encrypted one carries its content in base64 (a third larger), and the HTML body goes next to
/// the plain one, so a longer answer is split into several messages.
pub const MAX_BODY_BYTES: usize = 20_000;
/// At most this many messages for one answer; the rest stays in the session (`zen -r`).
pub const MAX_PARTS: usize = 8;
/// Largest event content the bridge sends, as JSON before encryption: base64 makes it 4/3 larger,
/// and the encrypted event's envelope needs room too. A message over it goes without its HTML.
pub const MAX_CONTENT_JSON: usize = 46_000;

/// What the owner wrote in a room.
#[derive(Debug, PartialEq)]
pub enum Input {
    /// `!new [title]`: a new session in a new room.
    New(Option<String>),
    /// `!stop`: abort the running turn.
    Stop,
    /// `!session`: which session this room is.
    Which,
    /// `!help` (or an unknown command).
    Help,
    /// Anything else: a prompt for the room's session.
    Prompt(String),
}

pub fn parse(text: &str) -> Input {
    let t = text.trim();
    let Some(cmd) = t.strip_prefix('!') else { return Input::Prompt(t.to_string()) };
    let (word, rest) = cmd.split_once(char::is_whitespace).unwrap_or((cmd, ""));
    let rest = rest.trim();
    match word.to_ascii_lowercase().as_str() {
        "new" => Input::New((!rest.is_empty()).then(|| rest.to_string())),
        "stop" => Input::Stop,
        "session" => Input::Which,
        _ => Input::Help,
    }
}

pub const HELP: &str = "**zen in Matrix**\n\n\
- Write anything: it goes to this room's zen session.\n\
- `!new [title]`: start a new session in a new room.\n\
- `!stop`: stop the running turn.\n\
- `!session`: which session this room is.\n\
- When zen asks questions, answer with the option numbers (`1 2`), or in your own words.";

/// One thing to post in the room.
#[derive(Debug, PartialEq)]
pub enum Out {
    /// zen's answer (Markdown).
    Answer(String),
    /// A side note from the bridge (Markdown), sent as a notice.
    Note(String),
    /// zen is (or stopped) working: the typing indicator.
    Typing(bool),
}

/// The questions an `ask` left open in a room, for answering them by number.
pub type Questions = Vec<(String, Vec<String>)>;

/// The state of one room's session stream, between events.
#[derive(Default)]
pub struct Stream {
    /// Prompts this bridge sent that the kernel hasn't echoed yet: their echo isn't repeated.
    pub sent: Vec<String>,
    /// The questions of the last `ask`, until the owner answers.
    pub questions: Questions,
    pub busy: bool,
}

impl Stream {
    /// The owner's message, ready to send as a prompt: digits answering open questions become the
    /// options' text, so zen gets the answer in words. Clears the open questions either way.
    pub fn prompt(&mut self, text: &str) -> String {
        let qs = std::mem::take(&mut self.questions);
        let text = answers(&qs, text).unwrap_or_else(|| text.to_string());
        self.sent.push(text.clone());
        text
    }

    /// What one session message means for the room. `live` is false when catching up after a
    /// reconnect or a restart: prompts this bridge sent before it restarted aren't known any more,
    /// so user messages aren't noted then, only answers posted.
    pub fn on_message(&mut self, m: &Value, live: bool) -> Vec<Out> {
        let mut out = vec![];
        match m["role"].as_str() {
            Some("user") if m["kernel"] != true => {
                let text = text_of(&m["content"]);
                if let Some(i) = self.sent.iter().position(|s| s.trim() == text.trim()) {
                    self.sent.remove(i);
                } else if live && !text.trim().is_empty() {
                    // A prompt typed somewhere else (the terminal) in the same session.
                    out.push(Out::Note(format!("⌨️ *from the terminal:* {}", one_line(&text, 300))));
                }
            }
            Some("assistant") => {
                let text = text_of(&m["content"]);
                if !text.trim().is_empty() {
                    out.push(Out::Answer(text));
                }
                if m["stopReason"] == "error" {
                    if let Some(e) = m["errorMessage"].as_str() {
                        out.push(Out::Note(format!("⚠️ {}", one_line(e, 500))));
                    }
                }
            }
            _ => {}
        }
        out
    }

    /// What one kernel event means for the room.
    pub fn on_event(&mut self, ev: &Value) -> Vec<Out> {
        let mut out = vec![];
        match ev["type"].as_str().unwrap_or("") {
            "busy" => {
                self.busy = true;
                out.push(Out::Typing(true));
            }
            "message" => out = self.on_message(&ev["message"], true),
            "questions" => {
                self.questions = ev["questions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|q| {
                        let opts = q["options"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
                        (q["question"].as_str().unwrap_or("").to_string(), opts)
                    })
                    .collect();
                out.push(Out::Answer(render_questions(&self.questions)));
            }
            "end" => {
                if let Some(e) = ev["error"].as_str().filter(|e| !e.is_empty()) {
                    out.push(Out::Note(format!("⚠️ the turn failed: {}", one_line(e, 500))));
                }
                if ev["next"] != true {
                    self.busy = false;
                    out.push(Out::Typing(false));
                }
            }
            "idle" => {
                self.busy = false;
                out.push(Out::Typing(false));
            }
            "error" => out.push(Out::Note(format!("⚠️ {}", one_line(ev["error"].as_str().unwrap_or("error"), 500)))),
            "resync" if ev["busy"] == false && self.busy => {
                self.busy = false;
                out.push(Out::Typing(false));
            }
            _ => {}
        }
        out
    }
}

fn render_questions(qs: &Questions) -> String {
    let mut s = String::from("**zen asks:**\n");
    for (i, (q, opts)) in qs.iter().enumerate() {
        s.push_str(&format!("\n**{}. {}**\n", i + 1, q));
        for (j, o) in opts.iter().enumerate() {
            s.push_str(&format!("{}. {}{}\n", j + 1, o, if j == 0 { " *(recommended)*" } else { "" }));
        }
    }
    let example = vec!["1"; qs.len()].join(" ");
    s.push_str(&format!("\nReply with option numbers in order (`{example}`), or in your own words."));
    s
}

/// The owner's numbers as answers to the open questions: one number per question (separated by
/// spaces or commas), each a valid option. Anything else is a reply in words (None).
pub fn answers(qs: &Questions, text: &str) -> Option<String> {
    if qs.is_empty() {
        return None;
    }
    let nums: Vec<usize> = text.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()).map(|s| s.parse().ok()).collect::<Option<_>>()?;
    if nums.len() != qs.len() {
        return None;
    }
    let mut lines = vec![];
    for ((q, opts), n) in qs.iter().zip(nums) {
        let opt = opts.get(n.checked_sub(1)?)?;
        lines.push(format!("{q} → {opt}"));
    }
    Some(lines.join("\n"))
}

/// A job run as a report for the jobs room, or None when there's nothing to say (silent, or
/// still running).
pub fn job_report(run: &Value) -> Option<String> {
    let job = run["job"].as_str().unwrap_or("job");
    match run["status"].as_str()? {
        "ok" => Some(format!("**{job}**\n\n{}", run["output"].as_str().unwrap_or("(no report)"))),
        "error" => Some(format!("⚠️ **{job}** failed: {}", one_line(run["error"].as_str().unwrap_or("unknown error"), 1000))),
        "missed" => Some(format!("⏭️ **{job}** missed its run (the kernel was down past its grace)")),
        "interrupted" => Some(format!("⚠️ **{job}** was interrupted (the kernel restarted mid-run)")),
        _ => None,
    }
}

/// `s` on one line, at most `max` characters.
fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    zen_proto::head(&flat, max)
}

/// `md` split into messages of at most `max` bytes each, at a paragraph or line break where there
/// is one in the second half of the budget. A code fence open at a split is closed there and
/// reopened in the next part. After `max_parts` the rest is cut, with a pointer to the session.
pub fn split(md: &str, max: usize, max_parts: usize) -> Vec<String> {
    const CUT: &str = "\n\n… *(cut here: the whole answer is in the session, `zen -r`)*";
    let mut out = vec![];
    let mut rest = md;
    let mut reopen = String::new();
    while !rest.is_empty() {
        // Room for the reopened fence, a closing one and the cut note.
        let budget = max.saturating_sub(reopen.len() + CUT.len() + 16).max(1);
        let end = if rest.len() <= budget { rest.len() } else { break_before(rest, budget) };
        let mut part = std::mem::take(&mut reopen);
        part.push_str(rest[..end].trim_end_matches('\n'));
        rest = rest[end..].trim_start_matches('\n');
        if !rest.is_empty() {
            if let Some(fence) = open_fence(&part) {
                part.push('\n');
                part.push_str(&fence.chars().take_while(|c| *c == '`' || *c == '~').collect::<String>());
                reopen = format!("{fence}\n");
            }
            if out.len() + 1 == max_parts {
                part.push_str(CUT);
                out.push(part);
                break;
            }
        }
        out.push(part);
    }
    out
}

/// Where to end a part of at most `budget` bytes of `s`: after the last blank line, else the last
/// line, in the second half of the budget; else at a character boundary.
fn break_before(s: &str, budget: usize) -> usize {
    let mut end = budget.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let head = &s[..end];
    let late = |i: &usize| *i >= end / 2;
    head.rfind("\n\n").filter(late).or_else(|| head.rfind('\n').filter(late)).unwrap_or(end)
}

/// The opening line of a code fence left open at the end of `md`, if any.
fn open_fence(md: &str) -> Option<String> {
    let mut open: Option<String> = None;
    for line in md.lines() {
        let t = line.trim_start();
        let marker = if t.starts_with("```") { '`' } else if t.starts_with("~~~") { '~' } else { continue };
        match &open {
            None => open = Some(t.trim_end().to_string()),
            Some(o) if o.starts_with(marker) && t.trim_end().chars().all(|c| c == marker) => open = None,
            Some(_) => {}
        }
    }
    open
}

/// Markdown as the HTML of a message. Raw HTML in it (from the model, a web page it quoted) is
/// shown as text, never passed on as markup; the rest renders as matrix-sdk's own Markdown does
/// (tables, strikethrough, a line break for each newline).
pub fn html(md: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser};
    let events = Parser::new_ext(md, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH).map(|e| match e {
        Event::Html(s) | Event::InlineHtml(s) => Event::Text(s),
        Event::SoftBreak => Event::HardBreak,
        e => e,
    });
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, events);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn commands_and_prompts() {
        assert_eq!(parse("  hello there "), Input::Prompt("hello there".into()));
        assert_eq!(parse("!new"), Input::New(None));
        assert_eq!(parse("!NEW  Tax return 2026 "), Input::New(Some("Tax return 2026".into())));
        assert_eq!(parse("!stop"), Input::Stop);
        assert_eq!(parse("!session"), Input::Which);
        assert_eq!(parse("!what"), Input::Help);
        assert_eq!(parse("!help"), Input::Help);
    }

    fn qs() -> Questions {
        vec![("Homeserver?".into(), vec!["matrix.org".into(), "self-host".into()]), ("E2EE?".into(), vec!["yes".into(), "no".into()])]
    }

    #[test]
    fn numbers_answer_the_open_questions() {
        assert_eq!(answers(&qs(), "1 2").as_deref(), Some("Homeserver? → matrix.org\nE2EE? → no"));
        assert_eq!(answers(&qs(), " 2, 1 ").as_deref(), Some("Homeserver? → self-host\nE2EE? → yes"));
        assert_eq!(answers(&qs(), "1"), None, "one number for two questions is a reply in words");
        assert_eq!(answers(&qs(), "3 1"), None, "no option 3");
        assert_eq!(answers(&qs(), "0 1"), None);
        assert_eq!(answers(&qs(), "1 and 2"), None);
        assert_eq!(answers(&vec![], "1"), None);
    }

    #[test]
    fn a_turn_becomes_typing_an_answer_and_done() {
        let mut s = Stream::default();
        let p = s.prompt("hi");
        assert_eq!(p, "hi");
        assert_eq!(s.on_event(&json!({ "type": "busy" })), vec![Out::Typing(true)]);
        assert_eq!(s.on_event(&json!({ "type": "message", "message": { "role": "user", "content": "hi" } })), vec![], "our own prompt isn't echoed");
        assert_eq!(s.on_event(&json!({ "type": "delta", "delta": "Hel" })), vec![]);
        let tool_only = json!({ "type": "message", "message": { "role": "assistant", "content": [{ "type": "toolCall", "name": "bash" }] } });
        assert_eq!(s.on_event(&tool_only), vec![]);
        let answer = json!({ "type": "message", "message": { "role": "assistant", "content": [{ "type": "text", "text": "Hello **Ze**" }] } });
        assert_eq!(s.on_event(&answer), vec![Out::Answer("Hello **Ze**".into())]);
        assert_eq!(s.on_event(&json!({ "type": "end", "next": true })), vec![], "the kernel goes on: still working");
        assert!(s.busy);
        assert_eq!(s.on_event(&json!({ "type": "idle" })), vec![Out::Typing(false)]);
        assert!(!s.busy);
    }

    #[test]
    fn catching_up_posts_answers_but_not_old_prompts() {
        let mut s = Stream::default();
        let prompt = json!({ "role": "user", "content": "from before the restart", "seq": 4 });
        assert_eq!(s.on_message(&prompt, false), vec![], "not taken for a terminal prompt");
        let answer = json!({ "role": "assistant", "content": [{ "type": "text", "text": "done" }], "seq": 5 });
        assert_eq!(s.on_message(&answer, false), vec![Out::Answer("done".into())]);
        s.prompt("again");
        assert_eq!(s.on_message(&json!({ "role": "user", "content": "again" }), false), vec![]);
        assert!(s.sent.is_empty(), "a prompt this bridge sent is matched either way");
    }

    #[test]
    fn prompts_from_the_terminal_and_failures_are_noted() {
        let mut s = Stream::default();
        let out = s.on_event(&json!({ "type": "message", "message": { "role": "user", "content": "check the build" } }));
        assert_eq!(out, vec![Out::Note("⌨️ *from the terminal:* check the build".into())]);
        let kernel = json!({ "type": "message", "message": { "role": "user", "kernel": true, "content": "verifier instructions" } });
        assert_eq!(s.on_event(&kernel), vec![]);
        let out = s.on_event(&json!({ "type": "end", "error": "engine crashed" }));
        assert_eq!(out, vec![Out::Note("⚠️ the turn failed: engine crashed".into()), Out::Typing(false)]);
        assert_eq!(s.on_event(&json!({ "type": "error", "error": "session is busy" })), vec![Out::Note("⚠️ session is busy".into())]);
    }

    #[test]
    fn questions_are_numbered_and_answered_by_number() {
        let mut s = Stream::default();
        let ev = json!({ "type": "questions", "questions": [
            { "question": "Homeserver?", "options": ["matrix.org", "self-host"] },
            { "question": "E2EE?", "options": ["yes", "no"] } ] });
        let [Out::Answer(text)] = &s.on_event(&ev)[..] else { panic!() };
        assert!(text.contains("**1. Homeserver?**\n1. matrix.org *(recommended)*\n2. self-host"), "{text}");
        assert!(text.contains("(`1 1`)"), "{text}");
        assert_eq!(s.prompt("1 2"), "Homeserver? → matrix.org\nE2EE? → no");
        assert!(s.questions.is_empty());
        assert_eq!(s.prompt("1 2"), "1 2", "answered once: numbers are plain text again");
        // The echo of the translated answer isn't repeated in the room.
        let echo = json!({ "type": "message", "message": { "role": "user", "content": "Homeserver? → matrix.org\nE2EE? → no" } });
        assert_eq!(s.on_event(&echo), vec![]);
    }

    #[test]
    fn job_runs_become_reports() {
        assert_eq!(job_report(&json!({ "job": "brief", "status": "ok", "output": "3 new emails" })).as_deref(), Some("**brief**\n\n3 new emails"));
        assert_eq!(job_report(&json!({ "job": "x", "status": "silent" })), None);
        assert_eq!(job_report(&json!({ "job": "x", "status": "running" })), None);
        assert_eq!(job_report(&json!({ "job": "x", "status": "error", "error": "boom\nline 2" })).as_deref(), Some("⚠️ **x** failed: boom line 2"));
        assert!(job_report(&json!({ "job": "x", "status": "missed" })).is_some());
    }

    #[test]
    fn long_answers_are_split_in_bytes() {
        assert_eq!(split("short", 100, 3), vec!["short"]);
        // Multi-byte text: every part within the budget, cut at a character boundary.
        let long = "é".repeat(150);
        let parts = split(&long, 200, 10);
        assert!(parts.len() > 1 && parts.iter().all(|p| p.len() <= 200), "{parts:?}");
        assert_eq!(parts.concat(), long);
        // Paragraphs are kept whole where they fit.
        let paras = format!("{}\n\n{}", "a".repeat(60), "b".repeat(60));
        assert_eq!(split(&paras, 200, 10), vec!["a".repeat(60), "b".repeat(60)]);
        // Past the last part the rest is cut, with a pointer to the session.
        let parts = split(&"word ".repeat(500), 200, 3);
        assert_eq!(parts.len(), 3);
        assert!(parts[2].contains("cut here") && !parts[1].contains("cut here"));
    }

    #[test]
    fn a_code_fence_split_across_parts_is_closed_and_reopened() {
        let code = format!("Here:\n\n```rust\n{}```\n\nDone.", "let x = 1;\n".repeat(30));
        let parts = split(&code, 200, 10);
        assert!(parts.len() > 2);
        for p in &parts {
            assert_eq!(open_fence(p), None, "every part is balanced: {p}");
        }
        assert!(parts[1].starts_with("```rust\n"), "{}", parts[1]);
    }

    #[test]
    fn raw_html_from_the_model_is_shown_as_text() {
        let h = html("**hi** <img src=x onerror=alert(1)> & <b>bold</b>\n\n<script>x()</script>\n\n`a<b`");
        assert!(h.contains("<strong>hi</strong>"), "{h}");
        assert!(!h.contains("<img") && !h.contains("<b>") && !h.contains("<script"), "{h}");
        assert!(h.contains("&lt;img") && h.contains("&amp;") && h.contains("&lt;script"), "{h}");
        assert!(h.contains("<code>a&lt;b</code>"), "code shows its own text: {h}");
        assert!(html("a\nb").contains("a<br />\nb"));
    }
}
