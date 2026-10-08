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
            let res = c.upgrade(move |l| {
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

    pub(super) fn on_event(&mut self, ev: Value) {
        let w = self.width();
        match ev["type"].as_str().unwrap_or("") {
            "message" => {
                let m = &ev["message"];
                match m["role"].as_str() {
                    Some("user") if m["kernel"] == true => {
                        // The kernel talking to the model (a verifier's instructions), not the owner.
                        let text = clean(&text_of(&m["content"]));
                        let mut out: Vec<Line> = text.lines().map(|l| line(format!("  zen › {l}"), Sty::Dim)).collect();
                        out.push(Vec::new());
                        self.commit(out);
                    }
                    Some("user") => {
                        let text = clean(&text_of(&m["content"]));
                        if self.pending_prompt.as_deref() == Some(text.as_str()) {
                            self.pending_prompt = None;
                        } else {
                            self.push(Entry::User(text));
                        }
                    }
                    Some("assistant") => {
                        let mut entries = Vec::new();
                        let full = clean(&assistant_text(m));
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
                    Some("toolResult") => self.push(Self::tool_result_entry(m)),
                    _ => {}
                }
            }
            "delta" => {
                self.stream.push_str(&md::sanitize(ev["delta"].as_str().unwrap_or("")));
                self.status = "Writing".into();
                self.tool_since = None;
                if !self.inline {
                    self.draw(); // the whole reply so far shows in the conversation
                    return;
                }
                let mut out = Vec::new();
                while let Some(pos) = self.stream[self.committed..].find('\n') {
                    let l = self.stream[self.committed..self.committed + pos].to_string();
                    out.extend(self.md.render_line(&l, w));
                    self.committed += pos + 1;
                }
                if out.is_empty() {
                    self.draw();
                } else {
                    self.commit(out);
                }
            }
            "busy" => {
                // A turn the kernel started also counts.
                if !self.busy {
                    self.begin_turn();
                }
                self.turn_effort = ev["effort"].as_str().map(String::from);
            }
            "questions" => {
                let mut out = vec![line("── Questions ──", Sty::Dim)];
                for (i, q) in ev["questions"].as_array().into_iter().flatten().enumerate() {
                    out.push(line(format!("{}. {}", i + 1, clean(q["question"].as_str().unwrap_or(""))), Sty::Plain));
                    for (j, o) in q["options"].as_array().into_iter().flatten().enumerate() {
                        let rec = if j == 0 { "  (recommended)" } else { "" };
                        out.push(line(format!("   {}) {}{rec}", (b'a' + j as u8) as char, clean(o.as_str().unwrap_or(""))), Sty::Plain));
                    }
                }
                out.push(Vec::new());
                self.commit(out);
            }
            "status" => {
                self.status = clean(ev["text"].as_str().unwrap_or("Working"));
                self.draw();
            }
            "child_end" => {
                self.session_tokens += record_total(&ev["turn"]);
            }
            "idle" => {
                self.busy = false;
                self.draw();
            }
            "thinking" => {
                self.status = "Thinking".into();
            }
            "tool_start" => {
                let name = ev["name"].as_str().unwrap_or("");
                self.status = format!("Running {}", tool_summary(name, &ev["args"]));
                self.tool_since = Some(Instant::now());
                self.turn_tools += 1;
                if matches!(name.rsplit("__").next(), Some("write" | "edit")) {
                    if let Some(path) = ev["args"]["path"].as_str() {
                        self.turn_files.insert(path.to_string());
                    }
                }
                // Remember the file, for `/open` with no path.
                if matches!(name.rsplit("__").next(), Some("read" | "write" | "edit")) {
                    if let Some(path) = ev["args"]["path"].as_str() {
                        self.last_file = Some(path.to_string());
                    }
                }
                self.draw();
            }
            "tool_end" => {
                self.status = "Working".into();
                self.tool_since = None;
                // A tool may have changed the file in the side panel: show it now, not on the next poll.
                if self.panel.as_mut().is_some_and(Panel::reload_if_changed) {
                    self.draw();
                }
            }
            "end" if !self.busy => {} // already ended (e.g. after a resync)
            "resync" => {
                self.note(format!("missed {} updates from zenbot (the terminal fell behind)", ev["skipped"]), Sty::Warn);
                if ev["busy"] == false && self.busy {
                    self.on_event(serde_json::json!({ "type": "end", "error": null }));
                }
            }
            "end" => {
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
                } else if let Some(e) = ev["error"].as_str() {
                    out.push(line(clean(e), Sty::Err));
                }
                self.aborting = false;
                self.reset_stream();
                // The kernel's totals cover the whole turn, including calls the stream never showed.
                let r = &ev["turn"];
                if r.is_object() {
                    self.turn_tokens = record_total(r);
                }
                let secs = self.turn_started.elapsed().as_secs_f32();
                let effort = self.turn_effort.take().map(|e| format!(" · {e}")).unwrap_or_default();
                out.push(line(format!("  {}{effort} · {} tokens · {:.1}s", self.turn_model, fmt_tokens(self.turn_tokens), secs), Sty::Dim));
                out.push(Vec::new());
                self.session_tokens += self.turn_tokens;
                // The workflow may continue on its own (verify, or the work after approval).
                self.busy = ev["next"] == true;
                if self.busy {
                    self.status = "Continuing".into();
                }
                entries.push(Entry::Raw(out));
                self.push_all(entries);
            }
            "error" => {
                self.busy = false;
                self.commit(vec![line(clean(ev["error"].as_str().unwrap_or("error")), Sty::Err), Vec::new()]);
            }
            "disconnected" if self.upgrading => {
                self.busy = false;
                self.sink = None; // expected: zenbot is restarting; we reconnect when it's back
            }
            "disconnected" => {
                self.busy = false;
                self.sink = None;
                self.note("lost connection to zenbot; send a message to reconnect", Sty::Warn);
                self.draw();
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
}
