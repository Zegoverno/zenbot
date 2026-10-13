//! Events from the kernel (a session's stream) and from background tasks (update checks, upgrades).

use super::*;

impl App {
    /// A turn starts (sent from here, or started by the kernel): reset its counters.
    pub(super) fn begin_turn(&mut self) {
        self.busy = true;
        self.status = "Working".into();
        self.turn_started = Instant::now();
        self.turn_tokens = 0;
        self.turn_tools = 0;
        self.turn_files.clear();
    }

    /// The installed zen changed since this one started.
    pub(super) fn newer_installed(&self) -> bool {
        let Some((path, started)) = &self.installed else { return false };
        std::fs::metadata(path).and_then(|m| m.modified()).is_ok_and(|now| now != *started)
    }

    pub(super) fn start_upgrade(&mut self) {
        if self.upgrading {
            self.note("an upgrade is already running", Sty::Warn);
            return;
        }
        self.upgrading = true;
        self.commit(vec![line("checking for updates…", Sty::Dim)]);
        let (c, tx) = (self.c.clone(), self.tx.clone());
        tokio::spawn(async move {
            let log_tx = tx.clone();
            let res = crate::client::upgrade(&c, move |l| {
                let _ = log_tx.send((String::new(), 0, json!({ "type": "upgrade_log", "line": l })));
            });
            let ev = match res.await {
                Ok(msg) => json!({ "type": "upgrade_done", "ok": true, "text": msg }),
                Err(e) => json!({ "type": "upgrade_done", "ok": false, "text": format!("{e:#}") }),
            };
            let _ = tx.send((String::new(), 0, ev));
        });
    }

    /// An event from the kernel or a background task. Session events count only for the current
    /// session on the current connection.
    pub(super) async fn on_incoming(&mut self, (sid, conn, ev): Incoming) {
        if sid.is_empty() {
            self.on_app_event(ev).await;
        } else if self.session.as_deref() == Some(sid.as_str()) && conn == self.conn {
            self.on_event(ev);
        }
    }

    /// Events that aren't tied to a session (sent with an empty session id).
    pub(super) async fn on_app_event(&mut self, ev: Value) {
        let text = clean(ev["text"].as_str().or(ev["line"].as_str()).unwrap_or(""));
        match ev["type"].as_str().unwrap_or("") {
            "update_available" => self.commit(vec![line(text, Sty::Warn), Vec::new()]),
            "board" => self.board_loaded(ev),
            "upgrade_log" => self.commit(vec![line(text, Sty::Dim)]),
            "upgrade_done" => {
                self.upgrading = false;
                let ok = ev["ok"] == true;
                self.commit(vec![line(text, if ok { Sty::Accent } else { Sty::Err }), Vec::new()]);
                // The upgrade installed a new zen too: load it, back in this session.
                if ok && self.newer_installed() {
                    self.restart = true;
                    self.quit = true;
                    return;
                }
                // Back on the restarted kernel: fetch the session again, with what happened meanwhile.
                if let Some(id) = self.session.clone() {
                    if self.sink.is_none() && self.switch_session(id).await.is_err() {
                        self.note("lost connection to zenbot; send a message to reconnect", Sty::Warn);
                    }
                }
                self.draw();
            }
            _ => {}
        }
    }

    /// A session event (docs/client-protocol.md), or `disconnected` from the stream's reader.
    pub(super) fn on_event(&mut self, ev: Value) {
        use zen_proto::client::{echoes, Event as Ev};
        let w = self.width();
        let ev = Ev::from_value(ev);
        let ends = ev.ends_turn();
        match ev {
            Ev::Message { message: m } => match m["role"].as_str() {
                Some("user") if m["kernel"] == true => {
                    // The kernel talking to the model (a verifier's instructions), not the owner.
                    let text = clean(&text_of(&m["content"]));
                    let mut out: Vec<Line> = text.lines().map(|l| line(format!("  zen › {l}"), Sty::Dim)).collect();
                    out.push(Vec::new());
                    self.commit(out);
                }
                Some("user") => {
                    if self.pending_prompt.as_deref().is_some_and(|p| echoes(&m, p)) {
                        self.pending_prompt = None;
                    } else {
                        self.push(Entry::User(clean(&text_of(&m["content"]))));
                    }
                }
                Some("assistant") => {
                    let mut entries = Vec::new();
                    let full = clean(&assistant_text(&m));
                    if self.inline {
                        // Inline, the streamed lines are already in scrollback: print the rest.
                        let mut out = Vec::new();
                        if self.stream.is_empty() {
                            if !full.trim().is_empty() {
                                out.extend(self.md.render(full.trim_end(), w));
                            }
                        } else if self.committed < self.stream.len() {
                            let rest = self.stream[self.committed..].to_string();
                            out.extend(self.md.render(rest.trim_end(), w));
                        }
                        if !full.trim().is_empty() {
                            out.push(Vec::new());
                        }
                        entries.push(Entry::Raw(out));
                    } else {
                        let text = if full.trim().is_empty() { self.stream.clone() } else { full };
                        if !text.trim().is_empty() {
                            entries.push(Entry::Md(text));
                        }
                    }
                    self.reset_stream();
                    for c in m["content"].as_array().into_iter().flatten().filter(|c| c["type"] == "toolCall") {
                        entries.push(Entry::ToolCall(c["name"].as_str().unwrap_or("").to_string(), c["arguments"].clone()));
                    }
                    if m["stopReason"] == "error" && !self.aborting {
                        entries.push(Entry::Raw(vec![line(clean(m["errorMessage"].as_str().unwrap_or("error")), Sty::Err)]));
                    }
                    self.turn_tokens += usage_total(&m["usage"]);
                    self.turn_model = m["model"].as_str().unwrap_or("").to_string();
                    self.push_all(entries);
                }
                Some("toolResult") => self.push(Self::tool_result_entry(&m)),
                _ => {}
            },
            Ev::Delta { delta } => {
                self.stream.push_str(&md::sanitize(&delta));
                self.status = "Writing".into();
                self.tool_since = None;
                if !self.inline {
                    self.draw_soon(); // the whole reply so far shows in the conversation
                    return;
                }
                let mut out = Vec::new();
                while let Some(pos) = self.stream[self.committed..].find('\n') {
                    let l = self.stream[self.committed..self.committed + pos].to_string();
                    out.extend(self.md.render_line(&l, w));
                    self.committed += pos + 1;
                }
                if out.is_empty() {
                    self.draw_soon();
                } else {
                    self.commit(out);
                }
            }
            Ev::Busy { effort, .. } => {
                self.suggestion = None;
                // A turn the kernel started also counts.
                if !self.busy {
                    self.begin_turn();
                }
                self.turn_effort = effort;
            }
            Ev::Questions { questions } => {
                let mut out = vec![line("── Questions ──", Sty::Dim)];
                for (i, q) in questions.iter().enumerate() {
                    out.push(line(format!("{}. {}", i + 1, clean(&q.question)), Sty::Plain));
                    for (j, o) in q.options.iter().enumerate() {
                        let rec = if j == 0 { "  (recommended)" } else { "" };
                        out.push(line(format!("   {}) {}{rec}", (b'a' + j as u8) as char, clean(o)), Sty::Plain));
                    }
                }
                out.push(Vec::new());
                self.commit(out);
            }
            // The kernel named the session (only while the owner hasn't).
            Ev::Title { title } => {
                self.title = clean(&title);
                self.draw();
            }
            // A suggested next prompt, after a turn: grey text in the empty input until a prompt is sent.
            Ev::Suggestion { id, text } if !self.busy => {
                self.suggestion = Some(Suggestion { id, text: text.trim().to_string(), taken: false });
                self.draw();
            }
            // Marked as having read untrusted content, or trusted again by the owner.
            Ev::Taint { tainted, .. } => {
                self.tainted = tainted;
                self.draw();
            }
            Ev::Status { text } => {
                self.status = clean(if text.is_empty() { "Working" } else { &text });
                self.draw();
            }
            Ev::ChildEnd { turn } => {
                self.session_tokens += record_total(&turn);
            }
            Ev::Idle {} => {
                self.busy = false;
                self.draw();
            }
            Ev::Thinking { .. } => {
                self.status = "Thinking".into();
            }
            Ev::ToolStart { name, args, .. } => {
                self.status = format!("Running {}", tool_summary(&name, &args));
                self.tool_since = Some(Instant::now());
                self.turn_tools += 1;
                if matches!(name.rsplit("__").next(), Some("write" | "edit")) {
                    if let Some(path) = args["path"].as_str() {
                        self.turn_files.insert(path.to_string());
                    }
                }
                // Remember the file, for `/open` with no path.
                if matches!(name.rsplit("__").next(), Some("read" | "write" | "edit")) {
                    if let Some(path) = args["path"].as_str() {
                        self.last_file = Some(path.to_string());
                    }
                }
                self.draw();
            }
            Ev::ToolEnd { .. } => {
                self.status = "Working".into();
                self.tool_since = None;
                // A tool may have changed the file in the side panel: show it now, not on the next poll.
                if self.panel.as_mut().is_some_and(Panel::reload_if_changed) {
                    self.draw();
                }
            }
            Ev::End { .. } if !self.busy => {} // already ended (e.g. after a resync)
            Ev::Resync { skipped, .. } => {
                self.note(format!("missed {skipped} updates from zenbot (the terminal fell behind)"), Sty::Warn);
                if ends && self.busy {
                    self.on_event(serde_json::json!({ "type": "end", "error": null }));
                }
            }
            Ev::End { error, turn: r, .. } => {
                let mut entries = Vec::new();
                let mut out = Vec::new();
                self.tool_since = None;
                if self.aborting {
                    if !self.inline {
                        // The text so far stays markdown, so it re-wraps at the chat width.
                        if !self.stream.trim().is_empty() {
                            entries.push(Entry::Md(self.stream.clone()));
                        }
                    } else if self.committed < self.stream.len() {
                        let rest = self.stream[self.committed..].to_string();
                        out.extend(self.md.render(rest.trim_end(), w));
                    }
                    out.push(line("interrupted", Sty::Warn));
                } else if let Some(e) = error {
                    out.push(line(clean(&e), Sty::Err));
                }
                self.aborting = false;
                self.reset_stream();
                // The kernel's totals cover the whole turn, including calls the stream never showed.
                if r.is_object() {
                    self.turn_tokens = record_total(&r);
                }
                let secs = self.turn_started.elapsed().as_secs_f32();
                let effort = self.turn_effort.take().map(|e| format!(" · {e}")).unwrap_or_default();
                out.push(line(format!("  {}{effort} · {} tokens · {:.1}s", self.turn_model, fmt_tokens(self.turn_tokens), secs), Sty::Dim));
                out.push(Vec::new());
                self.session_tokens += self.turn_tokens;
                // The kernel may go on with a turn of its own (`next`): wait for `idle`.
                self.busy = !ends;
                if self.busy {
                    self.status = "Continuing".into();
                }
                entries.push(Entry::Raw(out));
                self.push_all(entries);
            }
            Ev::Error { error } => {
                self.busy = false;
                self.commit(vec![line(clean(&error), Sty::Err), Vec::new()]);
            }
            Ev::Other(v) if v["type"] == "disconnected" => {
                self.busy = false;
                self.sink = None;
                // While upgrading it's expected: zenbot is restarting; we reconnect when it's back.
                if !self.upgrading {
                    self.note("lost connection to zenbot; send a message to reconnect", Sty::Warn);
                    self.draw();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::*;

    #[test]
    fn end_of_turn_line_shows_the_level_the_turn_ran_with() {
        let mut a = app_with_models(80, 20);
        a.busy = true;
        a.on_event(json!({ "type": "busy", "busy": true, "model": "claude/claude-opus-5-5", "effort": "xhigh" }));
        a.turn_tokens = 50; // from the streamed messages, which miss the engine's side calls
        let record = json!({ "input_tokens": 1000, "output_tokens": 200, "cache_read": 3000, "cache_write": 0 });
        a.on_event(json!({ "type": "end", "error": null, "turn": record }));
        let out = a.capture.take().unwrap();
        assert!(out.contains(" · xhigh · "), "{out}");
        assert_eq!(a.session_tokens, 4200, "the kernel's turn totals replace the streamed estimate");
    }

    #[test]
    fn an_interrupted_reply_stays_markdown_and_rewraps_at_the_chat_width() {
        let mut a = app(100, 30);
        a.busy = true;
        a.on_event(json!({ "type": "delta", "delta": "Some **bold** words ".repeat(8) }));
        a.aborting = true;
        a.on_event(json!({ "type": "end", "error": null, "turn": {} }));
        assert!(matches!(&a.entries[0], Entry::Md(t) if t.starts_with("Some **bold**")));
        assert!(texts(&a.view).iter().any(|l| l == "interrupted"));
        a.panel = Some(Panel { path: "x.txt".into(), text: "x".into(), modified: None, scroll: 0, lines: Vec::new(), lines_w: 0 });
        a.draw();
        assert!(a.view.iter().all(|l| l.iter().map(|(t, _)| width(t)).sum::<usize>() <= a.columns().0), "re-wrapped to the chat");
        assert!(a.view[0].iter().any(|(t, s)| t == "bold" && *s == Sty::Bold), "still rendered as markdown");
    }

    #[test]
    fn questions_render_with_the_recommended_option_first() {
        let mut a = app_with_models(80, 30);
        a.busy = true;
        a.on_event(json!({ "type": "questions", "questions": [{ "question": "Which flag name?", "options": ["--json", "--format json"] }] }));
        a.on_event(json!({ "type": "status", "text": "verifying: running 1 check(s)" }));
        assert_eq!(a.status, "verifying: running 1 check(s)");
        a.on_event(json!({ "type": "end", "error": null, "turn": {} }));
        assert!(!a.busy);
        let out = a.capture.take().unwrap();
        for want in ["── Questions ──", "1. Which flag name?", "a) --json  (recommended)", "b) --format json"] {
            assert!(out.contains(want), "missing {want:?} in {out}");
        }
    }

    #[test]
    fn the_echo_of_a_sent_prompt_shows_once_and_a_prompt_from_elsewhere_shows() {
        let mut a = app(80, 20);
        a.pending_prompt = Some("fix the build".into());
        a.on_event(json!({ "type": "message", "message": { "role": "user", "content": "fix the build\n" } }));
        assert!(a.pending_prompt.is_none() && a.entries.is_empty(), "already shown when sent");
        a.on_event(json!({ "type": "message", "message": { "role": "user", "content": "from Matrix" } }));
        assert!(matches!(&a.entries[..], [Entry::User(t)] if t == "from Matrix"));
        a.on_event(json!({ "type": "some_future_event", "x": 1 }));
        assert_eq!(a.entries.len(), 1, "an unknown event is ignored");
    }

    #[test]
    fn a_kernel_started_turn_shows_as_busy() {
        let mut a = app_with_models(80, 20);
        assert!(!a.busy);
        a.on_event(json!({ "type": "busy", "busy": true, "model": "claude/claude-opus-5-5", "effort": "high" }));
        assert!(a.busy, "a turn started elsewhere shows here too");
        a.on_event(json!({ "type": "idle" }));
        assert!(!a.busy);
    }

    #[tokio::test]
    async fn a_newer_install_is_noticed_and_an_upgrade_that_brings_one_restarts() {
        let dir = TempDir::new("install");
        let bin = dir.file("zen", "old");
        let mut a = app(80, 24);
        let started = std::fs::metadata(&bin).unwrap().modified().unwrap();
        a.installed = Some((bin.clone(), started));
        assert!(!a.newer_installed());
        // An upgrade that didn't change zen itself doesn't restart it.
        a.on_app_event(json!({ "type": "upgrade_done", "ok": true, "text": "zenbot upgraded" })).await;
        assert!(!a.restart && !a.quit);
        let f = std::fs::File::options().write(true).open(&bin).unwrap();
        f.set_modified(started + Duration::from_secs(60)).unwrap();
        assert!(a.newer_installed());
        // A failed upgrade doesn't restart; a successful one that installed a new zen does.
        a.on_app_event(json!({ "type": "upgrade_done", "ok": false, "text": "rolled back" })).await;
        assert!(!a.restart);
        a.on_app_event(json!({ "type": "upgrade_done", "ok": true, "text": "zenbot upgraded" })).await;
        assert!(a.restart && a.quit);
    }

    #[tokio::test]
    async fn events_from_an_old_connection_are_dropped() {
        let mut a = app(80, 20);
        a.session = Some("s1".into());
        a.conn = 2;
        a.busy = true;
        a.on_incoming(("s1".into(), 1, json!({ "type": "delta", "delta": "stale" }))).await;
        a.on_incoming(("s2".into(), 2, json!({ "type": "delta", "delta": "other session" }))).await;
        assert!(a.stream.is_empty());
        a.on_incoming(("s1".into(), 2, json!({ "type": "delta", "delta": "live" }))).await;
        assert_eq!(a.stream, "live");
        // A disconnect seen on the old connection doesn't drop the new one.
        a.on_incoming(("s1".into(), 1, json!({ "type": "disconnected" }))).await;
        assert!(a.busy);
    }

    #[test]
    fn the_footer_marks_an_untrusted_session_until_it_is_trusted() {
        let mut a = app(100, 20);
        assert!(!footer(&a).contains("untrusted"), "{}", footer(&a));
        a.on_event(json!({ "type": "taint", "tainted": true, "source": "web_search", "about": "harbour" }));
        assert!(footer(&a).contains(" · untrusted (/trust) · "), "{}", footer(&a));
        a.on_event(json!({ "type": "taint", "tainted": false }));
        assert!(!footer(&a).contains("untrusted"), "{}", footer(&a));
    }
}
