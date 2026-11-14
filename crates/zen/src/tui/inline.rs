//! Inline mode: the conversation is printed into the terminal's scrollback and the live region
//! is painted in place below it.

use super::*;

/// The live region at the bottom of the terminal, as last painted.
#[derive(Default)]
pub(super) struct Region {
    pub(super) height: usize,
    pub(super) caret_row: usize,
    pub(super) caret_col: usize,
    /// Display width of each painted line, to work out where the caret ended up after the
    /// terminal reflows them on resize.
    pub(super) widths: Vec<usize>,
}

impl Region {
    /// The terminal was resized to `cols` columns and has re-wrapped the painted lines:
    /// recompute how many rows sit above the caret, so `erase` clears exactly the region.
    pub(super) fn reflow(&mut self, cols: usize) {
        let cols = cols.max(1);
        let rows = |w: usize| w.div_ceil(cols).max(1);
        let above: usize = self.widths.iter().take(self.caret_row).map(|&w| rows(w)).sum();
        self.caret_row = above + self.caret_col / cols;
        self.height = self.widths.iter().map(|&w| rows(w)).sum();
    }
}

impl App {
    pub(super) fn erase(&mut self, out: &mut String) {
        if self.region.height > 0 {
            if self.region.caret_row > 0 {
                out.push_str(&format!("\x1b[{}A", self.region.caret_row));
            }
            out.push_str("\r\x1b[J");
        } else {
            out.push('\r');
        }
        self.region = Region::default();
    }

    pub(super) fn paint_region(&mut self, out: &mut String) {
        let (lines, mut caret_row, caret_col, show_caret) = if self.too_small() { (vec![self.too_small_line()], 0, 0, false) } else { self.compose() };
        // Every line exactly fits a row: a wrapped one would make `region.height` wrong, and
        // `erase` would leave its extra rows behind.
        let w = self.width();
        let mut lines: Vec<Line> = lines.iter().map(|l| screen::fit(l, w, false)).collect();
        let h = self.height();
        let max = h.saturating_sub(1);
        if lines.len() > max {
            let cut = lines.len() - max;
            lines.drain(..cut);
            caret_row = caret_row.saturating_sub(cut);
        }
        // Full screen: pad above so the region sits at the bottom of the screen.
        let pad = if self.inline { 0 } else { h.saturating_sub(self.filled.min(h) + lines.len()) };
        if pad > 0 {
            lines.splice(0..0, std::iter::repeat_with(Vec::new).take(pad));
            caret_row += pad;
        }
        let n = lines.len();
        self.filled = self.filled.min(h - n);
        out.push_str(&lines.iter().map(md::to_ansi).collect::<Vec<_>>().join("\r\n"));
        let up = n.saturating_sub(1).saturating_sub(caret_row);
        if up > 0 {
            out.push_str(&format!("\x1b[{up}A"));
        }
        out.push('\r');
        if caret_col > 0 {
            out.push_str(&format!("\x1b[{caret_col}C"));
        }
        out.push_str(if show_caret { "\x1b[?25h" } else { "\x1b[?25l" });
        let widths = lines.iter().map(|l| l.iter().map(|(t, _)| UnicodeWidthStr::width(t.as_str())).sum()).collect();
        self.region = Region { height: n, caret_row, caret_col, widths };
    }

    /// Inline: print lines permanently into scrollback above the live region. Lines wider than the
    /// screen are wrapped here, so every printed line is exactly one row and `filled` stays true.
    pub(super) fn print(&mut self, lines: Vec<Line>) {
        if lines.is_empty() {
            return;
        }
        let w = self.width();
        let lines: Vec<Line> = lines.iter().flat_map(|l| fit_or_wrap(l, w)).collect();
        let mut out = String::from("\x1b[?2026h");
        self.erase(&mut out);
        for l in &lines {
            out.push_str(&md::to_ansi(l));
            out.push_str("\x1b[K\r\n");
        }
        self.filled = (self.filled + lines.len()).min(self.height());
        self.paint_region(&mut out);
        out.push_str("\x1b[?2026l");
        self.flush(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::*;

    #[test]
    fn committed_lines_wider_than_the_screen_are_wrapped_and_counted() {
        let mut a = app(40, 30);
        a.inline = true;
        a.commit(vec![line("x".repeat(100), Sty::Err)]);
        // 39 usable columns: 100 characters take 3 rows, and the pinning must count all 3.
        assert_eq!(a.filled, 3);
        let out = a.capture.take().unwrap();
        assert_eq!(out.matches("\x1b[K\r\n").count(), 3);
    }

    #[test]
    fn inline_region_lines_fit_the_width_even_with_wide_text() {
        let mut a = app_with_models(50, 20);
        a.inline = true;
        a.session = Some("0123456789".into());
        a.title = "会議の議事録をまとめてください、それから".into();
        a.busy = true;
        a.turn_tools = 12;
        a.turn_files.extend(["a".to_string(), "b".to_string()]);
        a.status = "Running bash 「長いコマンド」 with a very long status that would not fit".into();
        a.draw();
        assert!(a.region.widths.iter().all(|&w| w <= a.width()), "{:?}", a.region.widths);
        let status = texts(&a.compose().0).into_iter().find(|l| l.contains("esc to interrupt")).expect("status line");
        assert!(status.contains("Running"), "the status keeps its start: {status}");
        a.busy = false;
        a.picker = Some(Picker { title: "pick".into(), items: vec![("😀".repeat(40), "x".into())], selected: 0, kind: PickKind::Session });
        a.draw();
        assert!(a.region.widths.iter().all(|&w| w <= a.width()), "{:?}", a.region.widths);
    }

    #[test]
    fn resize_recomputes_where_the_caret_is() {
        let mut r = Region { height: 3, caret_row: 2, caret_col: 5, widths: vec![39, 39, 10] };
        r.reflow(20);
        // Narrowed to 20 columns, each 39-wide line now takes 2 rows.
        assert_eq!(r.caret_row, 4);
        assert_eq!(r.height, 5);
    }

    #[test]
    fn a_streaming_partial_line_inside_a_code_fence_renders_as_code() {
        let mut a = app(80, 20);
        a.inline = true;
        a.busy = true;
        a.on_event(json!({ "type": "delta", "delta": "```\nlet x = 1;" }));
        let (lines, _, _, _) = a.compose();
        let code = lines.iter().find(|l| l.iter().any(|(t, _)| t.contains("let x = 1;"))).expect("partial line shown");
        assert!(code.iter().any(|(t, s)| t.contains("let x") && *s == Sty::Code), "{code:?}");
        // Rendering the partial line leaves the committed state alone.
        a.on_event(json!({ "type": "delta", "delta": "\n```\nafter\n" }));
        let out = a.capture.take().unwrap();
        assert!(out.contains("\x1b[36mlet x = 1;") && out.contains("\r\nafter"), "{out:?}");
    }
}
