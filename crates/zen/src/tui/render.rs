//! Drawing: the full-screen frame (conversation, side panel, live region), the live region's
//! lines, sizes, columns and scrolling.

use super::*;

pub(super) const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub(super) const PLACEHOLDER: &str = "Ask zenbot to do something…";

/// Smallest terminal (columns, rows) zen lays out; below it, a frame asks for more room.
pub(super) const MIN_SIZE: (usize, usize) = (21, 6);

pub(super) fn fmt_tokens(n: i64) -> String {
    if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

impl App {
    /// Columns to draw in (one less than the terminal, so lines never trigger an auto-wrap).
    /// Never below `MIN_SIZE`: a smaller terminal gets the `too_small` frame instead.
    pub(super) fn width(&self) -> usize {
        self.size.0.saturating_sub(1).max(MIN_SIZE.0 - 1)
    }

    pub(super) fn height(&self) -> usize {
        self.size.1.max(MIN_SIZE.1)
    }

    /// The terminal is smaller than zen can lay out.
    pub(super) fn too_small(&self) -> bool {
        self.size.0 < MIN_SIZE.0 || self.size.1 < MIN_SIZE.1
    }

    /// What a too-small terminal shows: one line, cut to its real width.
    pub(super) fn too_small_line(&self) -> Line {
        screen::fit(&line("zen: enlarge the terminal", Sty::Warn), self.size.0.saturating_sub(1).max(1), false)
    }

    pub(super) fn flush(&mut self, out: String) {
        if let Some(c) = &mut self.capture {
            c.push_str(&out);
            return;
        }
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }

    /// Redraw: the whole frame in full screen, the live region in place inline.
    pub(super) fn draw(&mut self) {
        if !self.inline {
            self.frame();
            return;
        }
        let mut out = String::from("\x1b[?2026h");
        self.erase(&mut out);
        self.paint_region(&mut out);
        out.push_str("\x1b[?2026l");
        self.flush(out);
    }

    /// Columns for the chat and for the side panel (0 when it's closed or the screen is too narrow).
    pub(super) fn columns(&self) -> (usize, usize) {
        let total = self.width();
        if self.side_open() && !self.inline && total >= SPLIT_MIN {
            let panel = (total - 3) / 2;
            (total - 3 - panel, panel)
        } else {
            (total, 0)
        }
    }

    /// Rows the conversation gets above the live region: as in the last frame (every event
    /// redraws), so keys and clicks don't build the live region again just to measure it.
    pub(super) fn viewport_rows(&self) -> usize {
        if self.last_vh > 0 {
            return self.last_vh;
        }
        let region = self.compose().0.len().min(self.height().saturating_sub(1));
        self.height().saturating_sub(region).max(1)
    }

    pub(super) fn scroll_chat(&mut self, up: bool, n: usize) {
        self.scroll = if up { self.scroll + n } else { self.scroll.saturating_sub(n) };
        self.scroll_input = true;
    }

    /// Full screen: compose every row (conversation and side panel above, live region below)
    /// and write the rows that changed.
    pub(super) fn frame(&mut self) {
        if self.too_small() {
            let mut rows = vec![md::to_ansi(&self.too_small_line())];
            rows.resize(self.size.1.max(1), String::new());
            self.last_vh = 0;
            let out = self.screen.frame(rows, None);
            self.flush(out);
            return;
        }
        // Scrolled up: remember which entry the top line belongs to before the view re-renders.
        let anchor = (!self.scroll_input && self.scroll > 0 && self.top_line < self.view.len()).then(|| {
            let e = self.view_start.partition_point(|&s| s <= self.top_line).saturating_sub(1);
            (e, self.top_line - self.view_start.get(e).copied().unwrap_or(0))
        });
        self.scroll_input = false;
        self.sync_view();
        let (cw, pw) = self.columns();
        let h = self.height();
        let (mut region, mut caret_row, caret_col, show_caret) = self.compose();
        let max = h.saturating_sub(1);
        if region.len() > max {
            let cut = region.len() - max;
            region.drain(..cut);
            caret_row = caret_row.saturating_sub(cut);
        }
        let vh = h - region.len();
        self.last_vh = vh;

        // The conversation, with the streaming reply at the end: all of it, as it arrives.
        let streaming = self.busy && !self.stream.is_empty();
        let tail = if streaming { self.stream_tail(cw) } else { Vec::new() };
        let done: &[Line] = if streaming { &self.stream_lines } else { &[] };
        let total = self.view.len() + done.len() + tail.len();
        // Scrolled up, a row at the bottom says how much is below, so content gets one row less.
        let rows_up = vh.saturating_sub(1).max(1);
        if self.scroll > 0 {
            match anchor.filter(|(e, _)| *e < self.view_start.len()) {
                // Keep the same line of the same entry at the top (it may have re-wrapped).
                Some((e, off)) => {
                    let first = self.view_start[e];
                    let len = self.view_start.get(e + 1).copied().unwrap_or(self.view.len()) - first;
                    let top = first + off.min(len.saturating_sub(1));
                    self.scroll = total.saturating_sub(top + rows_up).max(1);
                }
                // The top was in the streaming reply: only lines added below move it.
                None if total > self.last_total => self.scroll += total - self.last_total,
                None => {}
            }
        }
        self.last_total = total;
        self.scroll = self.scroll.min(total.saturating_sub(rows_up));
        let end = total - self.scroll;
        let start = end.saturating_sub(if self.scroll > 0 { rows_up } else { vh });
        self.top_line = start;
        let (v, d) = (self.view.len(), done.len());
        let at = |i: usize| if i < v { &self.view[i] } else if i < v + d { &done[i - v] } else { &tail[i - v - d] };
        let mut chat: Vec<Line> = (start..end).map(|i| at(i).clone()).collect();
        if self.scroll > 0 {
            chat.push(line(format!("↓ {} more lines · PgDn", self.scroll), Sty::Accent));
        }

        // The side panel: tabs on top, the folder tree or the open file below.
        let side: Vec<Line> = if pw > 0 { self.side_lines(pw, vh) } else { Vec::new() };

        let mut rows: Vec<String> = (0..vh)
            .map(|r| screen::row(chat.get(r), (pw > 0).then(|| (side.get(r), pw)), cw))
            .collect();
        rows.extend(region.iter().map(|l| md::to_ansi(&screen::fit(l, self.width(), false))));
        let caret = show_caret.then_some((vh + caret_row, caret_col));
        let out = self.screen.frame(rows, caret);
        self.flush(out);
    }

    pub(super) fn note(&mut self, text: impl Into<String>, sty: Sty) {
        self.notice = Some((clean(&text.into()), sty));
    }

    /// Build the live region: (lines, caret row, caret col, show caret). Lines may be wider than
    /// the screen; whoever paints them cuts them to fit.
    pub(super) fn compose(&self) -> (Vec<Line>, usize, usize, bool) {
        let w = self.width();
        let mut lines: Vec<Line> = Vec::new();

        if let Some(p) = &self.picker {
            lines.push(line(p.title.clone(), Sty::Bold));
            let window = 10.min(p.items.len().max(1));
            let start = p.selected.saturating_sub(window - 1).min(p.items.len().saturating_sub(window));
            for (i, (label, _)) in p.items.iter().enumerate().skip(start).take(window) {
                let (mark, sty) = if i == p.selected { ("› ", Sty::Accent) } else { ("  ", Sty::Plain) };
                lines.push(vec![(mark.into(), sty), (label.clone(), sty)]);
            }
            if p.items.is_empty() {
                lines.push(line("  (nothing here)", Sty::Dim));
            }
            lines.push(line("↑↓ choose · enter select · esc cancel", Sty::Dim));
            let row = 1 + p.selected - start;
            return (lines, row, 0, false);
        }

        // Streaming text that hasn't completed a line yet, rendered in the state the committed
        // lines left (e.g. inside a code fence), without changing it.
        // (Full screen shows it in the conversation instead.)
        if self.inline && self.busy && self.committed < self.stream.len() {
            let mut m = self.md;
            let partial = m.render(&self.stream[self.committed..], w);
            let skip = partial.len().saturating_sub(6);
            lines.extend(partial.into_iter().skip(skip));
        }
        if self.busy {
            let secs = self.turn_started.elapsed().as_secs();
            // How long the running tool has taken, so a long one (a subagent) visibly progresses.
            let tool = self.tool_since.map(|t| t.elapsed().as_secs()).filter(|s| *s > 0).map(|s| format!(" ({s}s)")).unwrap_or_default();
            let mut work = String::new();
            if self.turn_tools > 0 {
                work.push_str(&format!(" · {} tool{}", self.turn_tools, if self.turn_tools == 1 { "" } else { "s" }));
            }
            if !self.turn_files.is_empty() {
                work.push_str(&format!(" · {} file{} edited", self.turn_files.len(), if self.turn_files.len() == 1 { "" } else { "s" }));
            }
            // The status gives way first, so the timing and the interrupt hint stay in view.
            let tail = format!("{tool}  ·  turn {secs}s{work} · esc to interrupt");
            let room = w.saturating_sub(2 + UnicodeWidthStr::width(tail.as_str())).max(10);
            let mut status = vec![(format!("{} ", SPINNER[self.spin % SPINNER.len()]), Sty::Accent)];
            status.extend(screen::fit(&line(self.status.clone(), Sty::Plain), room, false));
            status.push((tail, Sty::Dim));
            lines.push(status);
        } else if let Some((n, s)) = &self.notice {
            lines.push(line(n.clone(), *s));
        }

        // A suggested next prompt shows as the input's grey placeholder.
        let suggested = self.suggestion.as_ref().filter(|_| self.editor.is_empty() && !self.busy).map(|s| s.text.as_str());
        let hint = if suggested.is_some() {
            "tab: use suggestion"
        } else if self.editor.is_empty() {
            ""
        } else if self.enhanced {
            "enter send · shift+enter new line"
        } else {
            "enter send · alt+enter new line"
        };
        let (input, crow, ccol) = self.editor.render(w, suggested.unwrap_or(PLACEHOLDER), hint, (self.height() / 2).max(3));
        let caret_row = lines.len() + crow;
        lines.extend(input);

        let menu = self.menu();
        if !menu.is_empty() {
            let sel = self.menu_sel.min(menu.len() - 1);
            for (i, c) in menu.iter().enumerate() {
                let mark = if i == sel { "› " } else { "  " };
                let help_sty = if i == sel { Sty::Plain } else { Sty::Dim };
                lines.push(vec![(format!("{mark}{:<10}", c.name), Sty::Accent), (c.help.to_string(), help_sty)]);
            }
        } else {
            let model = self.model.split('/').next_back().unwrap_or(&self.model);
            let session = match &self.session {
                Some(id) if !self.title.is_empty() => {
                    let t: String = self.title.chars().take(32).collect();
                    let ell = if self.title.chars().count() > 32 { "…" } else { "" };
                    format!("{t}{ell} ({})", short(id))
                }
                Some(id) => short(id).to_string(),
                None => "new session".into(),
            };
            let effort = self.shown_effort().map(|e| format!(" · {e}")).unwrap_or_default();
            let footer = format!("  {model}{effort} · {session} · {} tokens", fmt_tokens(self.session_tokens));
            lines.push(line(footer, Sty::Dim));
        }
        (lines, caret_row, ccol, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::*;

    #[test]
    fn region_fits_the_screen_and_shows_the_input_box() {
        let mut a = app(40, 12);
        let (lines, caret_row, _, _) = a.compose();
        let t = texts(&lines);
        a.inline = true;
        a.draw();
        assert!(a.region.widths.iter().all(|&w| w <= a.width()), "{t:#?}");
        assert!(t.iter().any(|l| l.starts_with('╭')) && t.iter().any(|l| l.starts_with('╰')));
        assert!(t[caret_row].contains(PLACEHOLDER.chars().take(10).collect::<String>().as_str()));
        // The viewport size measured by the last frame matches the region it drew.
        let mut b = app(40, 12);
        b.draw();
        assert_eq!(b.viewport_rows(), 12 - lines.len());
    }

    #[test]
    fn full_screen_pins_the_region_to_the_bottom_and_inline_does_not() {
        let mut a = app(40, 20);
        a.draw();
        let rows = a.screen.rows();
        assert_eq!(rows.len(), 20, "every row of the screen is drawn");
        assert!(rows[rows.len() - 2].contains('╰'), "the input box sits at the bottom, above the footer: {rows:#?}");
        let mut b = app(40, 20);
        b.inline = true;
        b.draw();
        assert!(b.region.height < 20);
    }

    #[test]
    fn a_terminal_too_small_to_lay_out_shows_one_line_that_fits() {
        for (cols, rows) in [(10, 30), (80, 4), (3, 2)] {
            let mut a = app(cols, rows);
            a.commit(vec![line("some conversation", Sty::Plain)]);
            let shown = a.screen.rows().to_vec();
            assert_eq!(shown.len(), rows, "exactly the real rows");
            assert_eq!(a.last_vh, 0, "no conversation viewport while too small");
            assert!(shown[0].contains("zen: enlarge"[..(cols - 1).min(12)].trim()), "{shown:?}");
            assert!(shown.iter().skip(1).all(|r| r.is_empty()));
            let mut b = app(cols, rows);
            b.inline = true;
            b.draw();
            assert!(b.region.height == 1 && b.region.widths[0] < cols, "{:?}", b.region.widths);
        }
        let mut a = app(10, 30);
        a.size = (80, 30); // enlarged again: the normal layout comes back
        a.screen.invalidate();
        a.draw();
        assert!(a.screen.rows().iter().any(|r| r.contains('╰')));
    }

    #[test]
    fn footer_shows_the_thinking_level_turns_run_with() {
        let mut a = app_with_models(80, 20);
        assert!(footer(&a).contains("claude-opus-5-5 · high · new session"), "{}", footer(&a));
        a.effort = Some("max".into());
        assert!(footer(&a).contains("claude-opus-5-5 · max · "), "{}", footer(&a));
        a.model = "faux/smoke".into();
        a.effort = None;
        assert!(footer(&a).contains("smoke · new session"), "a model without levels shows none: {}", footer(&a));
    }

    #[test]
    fn edits_count_distinct_files_in_the_status_line() {
        let mut a = app(100, 30);
        a.busy = true;
        for p in ["a.rs", "b.rs", "a.rs"] {
            a.on_event(json!({ "type": "tool_start", "name": "edit", "args": { "path": p } }));
        }
        let rows = a.screen.rows().join("\n");
        assert!(rows.contains("3 tools") && rows.contains("2 files edited"), "{rows}");
    }

    #[test]
    fn a_delta_rewrites_only_the_rows_that_changed() {
        let mut a = app(80, 30);
        a.busy = true;
        a.commit(vec![line("earlier conversation", Sty::Plain)]);
        a.on_event(json!({ "type": "delta", "delta": "Hello" }));
        a.capture = Some(String::new());
        a.on_event(json!({ "type": "delta", "delta": " world" }));
        let out = a.capture.take().unwrap();
        assert!(out.contains("Hello world"), "{out:?}");
        assert!(!out.contains("earlier conversation"), "unchanged rows are not rewritten: {out:?}");
        assert!(!out.contains("\x1b[2J") && !out.contains("\x1b[J"), "no clearing, so no flicker: {out:?}");
    }

    #[tokio::test]
    async fn page_up_scrolls_and_new_lines_keep_the_view_still() {
        let mut a = app(80, 20);
        for i in 0..100 {
            a.commit(vec![line(format!("row {i}"), Sty::Plain)]);
        }
        let bottom = a.screen.rows().join("\n");
        assert!(bottom.contains("row 99"));
        key(&mut a, KeyCode::PageUp).await;
        a.draw();
        assert!(a.scroll > 1, "PageUp moves by a viewport, not just one line");
        let up = a.screen.rows().join("\n");
        assert!(!up.contains("row 99") && up.contains("more lines · PgDn"), "{up}");
        a.commit(vec![line("row 100", Sty::Plain)]);
        assert_eq!(a.screen.rows().join("\n").replace("more lines", ""), up.replace("more lines", "").replace(&format!("↓ {} ", a.scroll - 1), &format!("↓ {} ", a.scroll)), "new output doesn't move a scrolled-up view");
        a.command("/new").await.unwrap(); // any send returns to the bottom; here PgDn does
        for _ in 0..20 {
            key(&mut a, KeyCode::PageDown).await;
        }
        a.draw();
        assert_eq!(a.scroll, 0);
        assert!(a.screen.rows().join("\n").contains("new session"));
    }

    #[tokio::test]
    async fn a_scrolled_up_view_stays_on_its_line_when_the_conversation_shrinks() {
        let mut a = app(80, 20);
        for i in 0..50 {
            a.commit(vec![line(format!("row {i}"), Sty::Plain)]);
        }
        tool_turn(&mut a, 10);
        for i in 50..100 {
            a.commit(vec![line(format!("row {i}"), Sty::Plain)]);
        }
        let ctrl_o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
        a.on_key(ctrl_o).await.unwrap(); // expanded: 10 steps with their results
        a.draw();
        for _ in 0..3 {
            key(&mut a, KeyCode::PageUp).await;
            a.draw();
        }
        let top = a.screen.rows()[0].clone();
        assert!(top.contains("out"), "the expanded run is at the top: {top}");
        a.on_key(ctrl_o).await.unwrap(); // folded: far fewer lines above the top
        assert!(a.screen.rows()[1].contains("row 50"), "folding keeps the viewport at the adjacent conversation");
        let rows = a.screen.rows().to_vec();
        let marker = rows.iter().position(|r| r.contains("more lines · PgDn")).expect("marker shown");
        assert!(rows[marker - 1].contains("row "), "the marker has its own row, after real content: {rows:#?}");
    }
}
