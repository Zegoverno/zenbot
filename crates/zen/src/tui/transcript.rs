//! The conversation: entries kept as their source, rendered at the chat width; the incremental
//! view (only changed entries re-render) and the streaming reply's cache.

use super::*;

/// One piece of the conversation, kept as its source so it can be re-rendered at any width
/// (a resize, or the side panel opening or closing).
pub(super) enum Entry {
    User(String),
    /// Assistant text, as markdown.
    Md(String),
    ToolCall(String, Value),
    /// A tool's result: its first lines, how many more there are, and whether it failed.
    ToolResult { head: Vec<String>, more: usize, error: bool },
    /// Already styled lines (notes, help, end-of-turn lines); re-wrapped when too wide.
    Raw(Vec<Line>),
    /// Full screen: a run of tool calls between pieces of text, shown as one line until expanded.
    Work(Vec<Step>),
}

/// One tool call in a `Work` run, with its result once it arrives.
pub(super) struct Step {
    pub(super) name: String,
    pub(super) args: Value,
    /// First lines, how many more, and whether it failed.
    pub(super) result: Option<(Vec<String>, usize, bool)>,
}

/// Fold a tool call or result into a trailing `Work` run (full screen); anything else is added as is.
pub(super) fn absorb(list: &mut Vec<Entry>, e: Entry) {
    match e {
        Entry::ToolCall(name, args) => {
            let step = Step { name, args, result: None };
            match list.last_mut() {
                Some(Entry::Work(steps)) => steps.push(step),
                _ => list.push(Entry::Work(vec![step])),
            }
        }
        Entry::ToolResult { head, more, error } => {
            if let Some(Entry::Work(steps)) = list.last_mut() {
                if let Some(step) = steps.iter_mut().find(|s| s.result.is_none()) {
                    step.result = Some((head, more, error));
                    return;
                }
            }
            list.push(Entry::ToolResult { head, more, error });
        }
        e => list.push(e),
    }
}

/// A styled line as is when it fits `w` columns, else word-wrapped.
pub(super) fn fit_or_wrap(l: &Line, w: usize) -> Vec<Line> {
    if l.iter().map(|(t, _)| UnicodeWidthStr::width(t.as_str())).sum::<usize>() <= w {
        vec![l.clone()]
    } else {
        md::wrap(l.clone(), w, (String::new(), Sty::Plain), (String::new(), Sty::Plain))
    }
}

impl App {
    pub(super) fn render_entry(e: &Entry, w: usize, expanded: bool) -> Vec<Line> {
        match e {
            Entry::User(text) => Self::render_user(text, w),
            Entry::Md(text) => {
                let mut out = Md::default().render(text.trim_end(), w);
                out.push(Vec::new());
                out
            }
            Entry::ToolCall(name, args) => Self::render_tool_call(name, args, w),
            Entry::ToolResult { head, more, error } => Self::render_tool_result(head, *more, *error, w),
            Entry::Work(steps) => Self::render_work(steps, w, expanded),
            Entry::Raw(lines) => lines.iter().flat_map(|l| fit_or_wrap(l, w)).collect(),
        }
    }

    /// Add to the conversation: into the transcript in full screen, into scrollback inline.
    pub(super) fn push(&mut self, e: Entry) {
        self.push_all(vec![e]);
    }

    /// Bring the rendered transcript up to date: all of it when the chat width changed (or
    /// `view_w` was reset), otherwise only the entries from the first one that changed.
    pub(super) fn sync_view(&mut self) {
        let w = self.columns().0;
        let from = if self.view_w != w { 0 } else { self.view_dirty.min(self.entries.len()) };
        self.view_dirty = usize::MAX;
        if from == self.entries.len() && self.view_start.len() == from {
            return;
        }
        let keep = if from == 0 { 0 } else { self.view_start.get(from).copied().unwrap_or(self.view.len()) };
        self.view.truncate(keep);
        self.view_start.truncate(from);
        for e in &self.entries[from..] {
            self.view_start.push(self.view.len());
            self.view.extend(Self::render_entry(e, w, self.expand_work));
        }
        self.view_w = w;
    }

    /// Add lines to the conversation (see `push`).
    pub(super) fn commit(&mut self, lines: Vec<Line>) {
        self.push(Entry::Raw(lines));
    }

    /// Add several entries with one redraw.
    pub(super) fn push_all(&mut self, entries: Vec<Entry>) {
        if self.inline {
            let w = self.width();
            let lines = entries.iter().flat_map(|e| Self::render_entry(e, w, true)).collect();
            self.print(lines);
            return;
        }
        // Tool calls fold into the run before them, which changes the last entry already rendered.
        self.view_dirty = self.view_dirty.min(self.entries.len().saturating_sub(1));
        for e in entries {
            absorb(&mut self.entries, e);
        }
        self.draw();
    }

    pub(super) fn render_user(text: &str, w: usize) -> Vec<Line> {
        let mut out = Vec::new();
        for (i, l) in text.split('\n').enumerate() {
            let prefix = if i == 0 { ("› ".to_string(), Sty::Accent) } else { ("  ".to_string(), Sty::Plain) };
            out.extend(md::wrap(vec![(l.to_string(), Sty::Bold)], w, prefix, ("  ".into(), Sty::Plain)));
        }
        out.push(Vec::new());
        out
    }

    pub(super) fn render_tool_call(name: &str, args: &Value, w: usize) -> Vec<Line> {
        let summary = tool_summary(name, args);
        let detail = summary.strip_prefix(name).unwrap_or(&summary).trim().to_string();
        md::wrap(vec![(name.to_string(), Sty::Bold), (format!(" {detail}"), Sty::Dim)], w, ("• ".into(), Sty::Accent), ("  ".into(), Sty::Plain))
    }

    /// A run of tool calls: one line (the latest step and a count) or, expanded, every step with its result.
    pub(super) fn render_work(steps: &[Step], w: usize, expanded: bool) -> Vec<Line> {
        let failed = steps.iter().filter(|s| s.result.as_ref().is_some_and(|r| r.2)).count();
        let n = steps.len();
        let count = format!("{n} step{}", if n == 1 { "" } else { "s" });
        if expanded {
            let mut out = vec![vec![("▾ ".to_string(), Sty::Accent), (format!("{count} · ctrl+o to fold"), Sty::Dim)]];
            for s in steps {
                out.extend(Self::render_tool_call(&s.name, &s.args, w));
                if let Some((head, more, error)) = &s.result {
                    out.extend(Self::render_tool_result(head, *more, *error, w));
                }
            }
            return out;
        }
        let last = &steps[n - 1];
        let summary = tool_summary(&last.name, &last.args);
        let mut tail = format!("  · {count}");
        if failed > 0 {
            tail.push_str(&format!(" · {failed} failed"));
        }
        tail.push_str(" · ctrl+o");
        let room = w.saturating_sub(UnicodeWidthStr::width(tail.as_str()) + 3).max(8);
        let mut text: String = summary.chars().take(room).collect();
        while UnicodeWidthStr::width(text.as_str()) > room {
            text.pop();
        }
        vec![vec![("▸ ".to_string(), Sty::Accent), (text, Sty::Plain), (tail, if failed > 0 { Sty::Err } else { Sty::Dim })], Vec::new()]
    }

    /// A tool result as shown: its first three non-empty lines and how many more there are.
    pub(super) fn tool_result_entry(m: &Value) -> Entry {
        let text = clean(&text_of(&m["content"]));
        let body: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let head = body.iter().take(3).map(|l| l.chars().take(400).collect()).collect();
        Entry::ToolResult { head, more: body.len().saturating_sub(3), error: m["isError"] == true }
    }

    pub(super) fn render_tool_result(head: &[String], more: usize, error: bool, w: usize) -> Vec<Line> {
        let sty = if error { Sty::Err } else { Sty::Dim };
        let mut out = Vec::new();
        if head.is_empty() {
            out.push(line("  └ (no output)", Sty::Dim));
        }
        for (i, l) in head.iter().enumerate() {
            let max = w.saturating_sub(5);
            let mut s: String = l.chars().take(max).collect();
            if UnicodeWidthStr::width(s.as_str()) > max {
                s = s.chars().take(max.saturating_sub(1)).collect();
            }
            out.push(vec![(if i == 0 { "  └ " } else { "    " }.to_string(), Sty::Dim), (s, sty)]);
        }
        if more > 0 {
            out.push(line(format!("    … {more} more lines"), Sty::Dim));
        }
        out.push(Vec::new());
        out
    }

    /// A stored message as conversation entries.
    pub(super) fn message_entries(m: &Value) -> Vec<Entry> {
        match m["role"].as_str() {
            Some("user") => vec![Entry::User(clean(&text_of(&m["content"])))],
            Some("assistant") => {
                let mut out = Vec::new();
                for c in m["content"].as_array().into_iter().flatten() {
                    match c["type"].as_str() {
                        Some("text") if !c["text"].as_str().unwrap_or("").trim().is_empty() => {
                            out.push(Entry::Md(clean(c["text"].as_str().unwrap_or(""))));
                        }
                        Some("toolCall") => out.push(Entry::ToolCall(c["name"].as_str().unwrap_or("").to_string(), c["arguments"].clone())),
                        _ => {}
                    }
                }
                if m["stopReason"] == "error" {
                    out.push(Entry::Raw(vec![line(clean(m["errorMessage"].as_str().unwrap_or("error")), Sty::Err), Vec::new()]));
                }
                out
            }
            Some("toolResult") => vec![Self::tool_result_entry(m)],
            _ => Vec::new(),
        }
    }

    /// Forget the streamed text (it was shown in full, or the turn ended).
    pub(super) fn reset_stream(&mut self) {
        self.stream.clear();
        self.committed = 0;
        self.md = Md::default();
        self.stream_lines.clear();
        self.stream_done = 0;
        self.stream_md = Md::default();
    }

    /// Full screen: render the stream's newly completed lines at `w` columns (all of them again
    /// when the width changed) and return its unfinished last line, rendered in the state the
    /// complete lines left (e.g. inside a code fence).
    pub(super) fn stream_tail(&mut self, w: usize) -> Vec<Line> {
        if self.stream_w != w {
            self.stream_lines.clear();
            self.stream_done = 0;
            self.stream_md = Md::default();
            self.stream_w = w;
        }
        if let Some(pos) = self.stream[self.stream_done..].rfind('\n') {
            let end = self.stream_done + pos;
            for l in self.stream[self.stream_done..end].split('\n') {
                self.stream_lines.extend(self.stream_md.render_line(l, w));
            }
            self.stream_done = end + 1;
        }
        let mut md = self.stream_md;
        md.render(&self.stream[self.stream_done..], w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::*;

    #[test]
    fn full_screen_streams_the_whole_reply_into_the_conversation() {
        let mut a = app(80, 30);
        a.busy = true;
        let reply: String = (1..=12).map(|i| format!("line {i}\n")).collect();
        a.on_event(json!({ "type": "delta", "delta": reply }));
        let rows = a.screen.rows().join("\n");
        assert!(rows.contains("line 1") && rows.contains("line 12"), "not only the last few lines: {rows}");
        assert!(a.compose().0.iter().all(|l| !l.iter().any(|(t, _)| t.contains("line 12"))), "and not in the live region");
        // The finished message replaces the stream, rendered once.
        a.on_event(json!({ "type": "message", "message": { "role": "assistant", "content": [{ "type": "text", "text": reply }] } }));
        let rows = a.screen.rows().join("\n");
        assert_eq!(rows.matches("line 12").count(), 1, "{rows}");
    }

    #[tokio::test]
    async fn a_run_of_tool_calls_folds_into_one_line_and_ctrl_o_unfolds_it() {
        let mut a = app(80, 30);
        a.busy = true;
        tool_turn(&mut a, 4);
        let rows = a.screen.rows().join("\n");
        assert!(rows.contains("step 3") && rows.contains("4 steps"), "{rows}");
        assert!(!rows.contains("step 1") && !rows.contains("out 3a"), "earlier steps and results are hidden: {rows}");
        assert!(rows.contains("4 tools"), "the status line counts tools: {rows}");
        a.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)).await.unwrap();
        let rows = a.screen.rows().join("\n");
        assert!(rows.contains("step 1") && rows.contains("out 3a"), "{rows}");
    }

    #[tokio::test]
    async fn escape_sequences_in_replies_tool_output_and_files_are_dropped() {
        let evil = "\x1b]0;pwned\x07\x1b[5Agot you\x1b]52;c;aGk=\x07";
        let mut a = app(100, 30);
        a.busy = true;
        a.on_event(json!({ "type": "delta", "delta": evil }));
        a.on_event(json!({ "type": "message", "message": { "role": "toolResult", "content": [{ "type": "text", "text": evil }] } }));
        a.on_event(json!({ "type": "status", "text": evil }));
        let dir = TempDir::new("escapes");
        let path = dir.file("evil.txt", evil);
        a.command(&format!("/open {}", path.display())).await.unwrap();
        a.draw();
        let out = a.capture.take().unwrap();
        assert!(out.contains("got you"), "{out:?}");
        for bad in ["\x1b]", "\x07", "\x1b[5A"] {
            assert!(!out.contains(bad), "{bad:?} reached the terminal: {out:?}");
        }
    }

    /// CPU time this thread has used, in seconds (Linux: /proc/thread-self/schedstat, in ns).
    fn thread_cpu() -> f64 {
        let s = std::fs::read_to_string("/proc/thread-self/schedstat").unwrap_or_default();
        s.split_whitespace().next().and_then(|n| n.parse::<f64>().ok()).unwrap_or(0.0) / 1e9
    }

    /// A long markdown reply: paragraphs, a list and a code block, repeated to `bytes` bytes.
    fn long_reply(bytes: usize) -> String {
        let block = "## A heading\n\nSome **bold** text and `code` in a paragraph that is long enough to wrap \
                     at the chat width, as model replies usually are.\n\n- a list item\n- another item with `code`\n\n\
                     ```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\n";
        let mut s = String::new();
        while s.len() < bytes {
            s.push_str(block);
        }
        s.truncate(bytes);
        s
    }

    /// Streaming cost in full screen: a 43 KB reply in 20-byte deltas, each drawn as it arrives.
    /// Run with `cargo test --release -p zen -- --ignored --nocapture streaming_cost`.
    #[test]
    fn the_transcript_renders_only_what_changed_and_matches_a_full_render() {
        let mut a = app(80, 20);
        let full = |a: &App| a.entries.iter().flat_map(|e| App::render_entry(e, a.columns().0, a.expand_work)).collect::<Vec<_>>();
        a.commit(vec![line("hello", Sty::Plain)]);
        a.push(Entry::User("a question".into()));
        tool_turn(&mut a, 3); // folds into one Work entry, changing the last entry each time
        a.push(Entry::Md("an **answer**".into()));
        assert_eq!(a.view, full(&a));
        assert_eq!(a.view_start.len(), a.entries.len());
        // Earlier entries are not rendered again: a marker in their lines survives a push.
        a.view[0] = line("marker", Sty::Plain);
        a.push(Entry::User("more".into()));
        assert_eq!(texts(&a.view[..1]), ["marker"]);
        // A width change renders everything again.
        a.size = (60, 20);
        a.draw();
        assert_eq!(a.view, full(&a));
    }

    #[test]
    fn the_cached_stream_renders_like_the_whole_reply() {
        let reply = long_reply(1_500);
        for step in [3, 20, 300] {
            let mut a = app(70, 30);
            a.busy = true;
            let chars: Vec<char> = reply.chars().collect();
            for chunk in chars.chunks(step) {
                a.on_event(json!({ "type": "delta", "delta": chunk.iter().collect::<String>() }));
                let tail = a.stream_tail(a.columns().0);
                let lines = [a.stream_lines.clone(), tail].concat();
                assert_eq!(lines, Md::default().render(&a.stream, a.columns().0), "{step}-char deltas, at {} bytes", a.stream.len());
            }
            // A resize renders it again at the new width.
            a.size = (50, 30);
            let tail = a.stream_tail(a.columns().0);
            let lines = [a.stream_lines.clone(), tail].concat();
            assert_eq!(lines, Md::default().render(&a.stream, a.columns().0));
        }
    }

    #[test]
    #[ignore]
    fn streaming_cost() {
        let reply = long_reply(43_000);
        let mut a = app(120, 40);
        a.busy = true;
        let t0 = thread_cpu();
        let chars: Vec<char> = reply.chars().collect();
        for chunk in chars.chunks(20) {
            a.on_event(json!({ "type": "delta", "delta": chunk.iter().collect::<String>() }));
            a.capture = Some(String::new());
        }
        let secs = thread_cpu() - t0;
        println!("streaming {} bytes in 20-byte deltas: {secs:.3} s CPU", reply.len());
    }
}
