//! Pickers in the live region (sessions, models, thinking levels, `/done` decisions) and the
//! thinking-level helpers.

use super::*;

pub(super) enum PickKind {
    Session,
    Model,
    Effort,
    Decision,
}

/// Your decisions on a session's work (`/done`), with what each one means.
pub(super) const DECISIONS: [(&str, &str); 4] = [
    ("accept", "done, and good as it is"),
    ("more", "same goal, keep working"),
    ("reshape", "the approach was wrong: rethink it"),
    ("drop", "stop: not worth continuing"),
];

pub(super) struct Picker {
    pub(super) title: String,
    pub(super) items: Vec<(String, String)>,
    pub(super) selected: usize,
    pub(super) kind: PickKind,
}

impl App {
    pub(super) async fn open_session_picker(&mut self) -> Result<()> {
        let list = self.c.get("/api/sessions?archived=false").await?;
        let items = list
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| {
                let title = clean(s["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("(untitled)"));
                let when = s["updated_at"].as_str().unwrap_or("").get(5..16).unwrap_or("").replace('T', " ");
                (format!("{when}  {title}"), s["id"].as_str().unwrap_or("").to_string())
            })
            .collect();
        self.picker = Some(Picker { title: "Resume a session".into(), items, selected: 0, kind: PickKind::Session });
        Ok(())
    }

    pub(super) fn open_model_picker(&mut self) {
        let ids: Vec<String> = self.models.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect();
        let items: Vec<(String, String)> =
            ids.iter().map(|m| (format!("{}{}", m, if *m == self.model { "  (current)" } else { "" }), m.clone())).collect();
        let selected = ids.iter().position(|m| *m == self.model).unwrap_or(0);
        self.picker = Some(Picker { title: "Choose a model".into(), items, selected, kind: PickKind::Model });
    }

    pub(super) fn model_info(&self, model: &str) -> Option<&Value> {
        self.models.iter().find(|m| m["id"] == model)
    }

    /// Thinking levels the current model takes (empty: none to choose).
    pub(super) fn effort_levels(&self) -> Vec<String> {
        let info = self.model_info(&self.model);
        info.and_then(|m| m["efforts"].as_array()).into_iter().flatten().filter_map(|e| e.as_str().map(String::from)).collect()
    }

    pub(super) fn default_effort(&self) -> Option<String> {
        self.model_info(&self.model).and_then(|m| m["default_effort"].as_str()).map(String::from)
    }

    /// The level turns run with: the session's choice, else the model's default.
    pub(super) fn shown_effort(&self) -> Option<String> {
        self.effort.clone().or_else(|| self.default_effort())
    }

    pub(super) fn open_decision_picker(&mut self) {
        let items = DECISIONS.iter().map(|(d, help)| (format!("{d:<8} {help}"), d.to_string())).collect();
        self.picker = Some(Picker { title: "How did it go?".into(), items, selected: 0, kind: PickKind::Decision });
    }

    /// `/done [decision] [note]`: record the decision, or open the picker when none is given.
    pub(super) async fn done(&mut self, arg: &str) -> Result<()> {
        if self.session.is_none() {
            self.note("nothing to judge yet; send a message first", Sty::Warn);
            return Ok(());
        }
        let (decision, note) = arg.split_once(' ').map(|(d, n)| (d, n.trim())).unwrap_or((arg, ""));
        if decision.is_empty() {
            self.open_decision_picker();
        } else if DECISIONS.iter().any(|(d, _)| *d == decision) {
            self.record_decision(decision, note).await?;
        } else {
            self.note("usage: /done accept|more|reshape|drop [note]", Sty::Warn);
        }
        Ok(())
    }

    pub(super) async fn record_decision(&mut self, decision: &str, note: &str) -> Result<()> {
        let Some(id) = self.session.clone() else { return Ok(()) };
        self.c.post(&format!("/api/sessions/{id}/decision"), json!({ "decision": decision, "note": note })).await?;
        self.note(format!("recorded: {decision}"), Sty::Dim);
        Ok(())
    }

    pub(super) fn open_effort_picker(&mut self) {
        let levels = self.effort_levels();
        if levels.is_empty() {
            self.note(format!("{} has no thinking levels to choose from", self.model), Sty::Dim);
            return;
        }
        let current = |v: Option<&str>| if self.effort.as_deref() == v { "  (current)" } else { "" };
        let default = self.default_effort().map(|d| format!(" ({d})")).unwrap_or_default();
        let mut items = vec![(format!("default{default}{}", current(None)), "default".to_string())];
        items.extend(levels.iter().map(|l| (format!("{l}{}", current(Some(l))), l.clone())));
        let selected = self.effort.as_ref().and_then(|e| levels.iter().position(|l| l == e)).map(|i| i + 1).unwrap_or(0);
        self.picker = Some(Picker { title: "Choose a thinking level".into(), items, selected, kind: PickKind::Effort });
    }

    pub(super) async fn pick(&mut self) -> Result<()> {
        let Some(p) = self.picker.take() else { return Ok(()) };
        let Some((_, value)) = p.items.get(p.selected).cloned() else { return Ok(()) };
        match p.kind {
            PickKind::Session if self.still_working() => {}
            PickKind::Session => self.switch_session(value).await?,
            PickKind::Model => {
                self.model = value.clone();
                if let Some(id) = &self.session {
                    self.c.patch(&format!("/api/sessions/{id}"), json!({ "model": value })).await?;
                }
                // The kernel drops a level the new model doesn't take; do the same here.
                let reset = self.effort.as_ref().is_some_and(|e| !self.effort_levels().contains(e));
                if reset {
                    self.effort = None;
                }
                let effort = self.shown_effort().map(|e| format!(" · effort: {e}{}", if reset { " (default for this model)" } else { "" }));
                self.note(format!("model: {value}{}", effort.unwrap_or_default()), Sty::Dim);
            }
            PickKind::Decision => self.record_decision(&value, "").await?,
            PickKind::Effort => {
                self.effort = (value != "default").then(|| value.clone());
                if let Some(id) = &self.session {
                    self.c.patch(&format!("/api/sessions/{id}"), json!({ "effort": value })).await?;
                }
                self.note(format!("effort: {}", self.shown_effort().unwrap_or(value)), Sty::Dim);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::*;

    #[tokio::test]
    async fn effort_picker_lists_the_models_levels_and_sets_the_choice() {
        let mut a = app_with_models(80, 20);
        typed(&mut a, "/effort").await;
        key(&mut a, KeyCode::Enter).await;
        let items: Vec<String> = a.picker.as_ref().expect("picker open").items.iter().map(|(label, _)| label.clone()).collect();
        assert_eq!(items, ["default (high)  (current)", "low", "medium", "high", "xhigh", "max"]);
        for _ in 0..5 {
            key(&mut a, KeyCode::Down).await;
        }
        key(&mut a, KeyCode::Enter).await;
        assert_eq!(a.effort.as_deref(), Some("max"));
        // Choosing "default" goes back to the model's default.
        typed(&mut a, "/effort").await;
        key(&mut a, KeyCode::Enter).await;
        assert_eq!(a.picker.as_ref().unwrap().selected, 5, "the current level is highlighted");
        for _ in 0..5 {
            key(&mut a, KeyCode::Up).await;
        }
        key(&mut a, KeyCode::Enter).await;
        assert_eq!(a.effort, None);
    }

    #[tokio::test]
    async fn switching_to_a_model_without_the_level_drops_it() {
        let mut a = app_with_models(80, 20);
        a.effort = Some("max".into());
        typed(&mut a, "/model").await;
        key(&mut a, KeyCode::Enter).await;
        key(&mut a, KeyCode::Down).await;
        key(&mut a, KeyCode::Enter).await;
        assert_eq!(a.model, "faux/smoke");
        assert_eq!(a.effort, None);
        typed(&mut a, "/effort").await;
        key(&mut a, KeyCode::Enter).await;
        assert!(a.picker.is_none(), "no levels to choose for this model");
    }

    #[tokio::test]
    async fn done_opens_the_decision_picker_or_explains_itself() {
        let mut a = app(80, 20);
        typed(&mut a, "/done").await;
        key(&mut a, KeyCode::Enter).await;
        assert!(a.picker.is_none(), "no session yet: nothing to judge");
        a.session = Some("s1".into());
        typed(&mut a, "/done").await;
        key(&mut a, KeyCode::Enter).await;
        let items: Vec<String> = a.picker.as_ref().expect("picker open").items.iter().map(|(_, v)| v.clone()).collect();
        assert_eq!(items, ["accept", "more", "reshape", "drop"]);
        key(&mut a, KeyCode::Esc).await;
        a.command("/done maybe later").await.unwrap();
        assert!(a.notice.as_ref().is_some_and(|(t, _)| t.starts_with("usage: /done")), "{:?}", a.notice);
    }
}
