//! The owner's open tasks, read from the vault: `<zen home>/global/Projects/<project>/tasks.md`,
//! one checkbox per line in the Obsidian Tasks plugin's emoji format (the vault's AGENTS.md,
//! "Tasks"). The kernel only reads these files, never writes them: the owner ticks, adds and drops
//! tasks in Obsidian. `GET /api/tasks` serves them, the shared `tasks` command lists them.
//! `agent_title` and `brief` make the session a task is handed to (`POST /api/tasks/{id}/agent`).
//!
//! Only open tasks (`- [ ]`) are listed; done (`[x]`), dropped (`[-]`) and any other state are
//! skipped, as are lines in code fences. Fields may come in any order, with or without the emoji
//! variation selector: `📅` due, `⏳` scheduled, `🆔` id, the priorities `🔺` urgent, `⏫`
//! important, `🔼` medium, `🔽` not urgent, `⏬` parking lot; other Tasks fields (`✅ ➕ 🛫 ❌`
//! dates, `⛔` dependencies, `🔁` recurrence, `🏁`) are dropped from the text. `[[…]]` links are
//! the task's goals: listed apart, and kept in the text as words only when the sentence goes on
//! after them.
//!
//! Order: by project, the project whose most pressing task comes first leading; inside a project
//! by priority (urgent, important, medium, not urgent, none, parking lot), then due date (none
//! last), then file order. A task is overdue when its due date is before today in the owner's zone
//! (`ZEN_TZ`).
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use serde::Serialize;

/// The vault's projects folder: `<zen home>/global/Projects`.
pub fn projects_dir() -> PathBuf {
    crate::layout::global_dir().join("Projects")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Urgent,
    Important,
    Medium,
    NotUrgent,
    ParkingLot,
}

/// Where a priority sorts: urgent first, no priority just above the parking lot.
fn rank(p: Option<Priority>) -> u8 {
    match p {
        Some(Priority::Urgent) => 0,
        Some(Priority::Important) => 1,
        Some(Priority::Medium) => 2,
        Some(Priority::NotUrgent) => 3,
        None => 4,
        Some(Priority::ParkingLot) => 5,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Task {
    pub project: String,
    pub text: String,
    pub id: Option<String>,
    pub due: Option<NaiveDate>,
    pub scheduled: Option<NaiveDate>,
    pub priority: Option<Priority>,
    /// The `[[…]]` links' targets (without `|alias` or `#heading`).
    pub goals: Vec<String>,
    pub file: PathBuf,
    /// 1-based.
    pub line: usize,
    pub overdue: bool,
}

#[derive(Debug, Serialize)]
pub struct Project {
    pub project: String,
    pub tasks: Vec<Task>,
}

/// The open task on `line`, if it is one (`project`, `file` and `line` filled by the caller).
fn parse_line(raw: &str) -> Option<Task> {
    let line = raw.replace('\u{FE0F}', "");
    let s = line.trim_start();
    // A list marker: `-`, `*`, `+` or `1.` / `1)`.
    let rest = if let Some(r) = s.strip_prefix(['-', '*', '+']) {
        r
    } else {
        let digits = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        if digits == 0 {
            return None;
        }
        s[digits..].strip_prefix(['.', ')'])?
    };
    let body = rest.strip_prefix([' ', '\t'])?.trim_start().strip_prefix("[ ]")?;
    if !body.is_empty() && !body.starts_with([' ', '\t']) {
        return None;
    }
    let (body, goals) = take_links(body);
    let mut t = Task { project: String::new(), text: String::new(), id: None, due: None, scheduled: None, priority: None, goals: Vec::new(), file: PathBuf::new(), line: 0, overdue: false };
    let mut words: Vec<String> = Vec::new();
    let mut tokens = body.split_whitespace().peekable();
    while let Some(tok) = tokens.next() {
        let Some((field, glued)) = field_of(tok) else {
            words.push(tok.to_string());
            continue;
        };
        // A field's value: glued to the emoji (`📅2026-10-30`) or the next word.
        let mut value = |ok: &dyn Fn(&str) -> bool| -> Option<String> {
            if !glued.is_empty() {
                return Some(glued.to_string());
            }
            match tokens.peek() {
                Some(next) if field_of(next).is_none() && ok(next) => tokens.next().map(str::to_string),
                _ => None,
            }
        };
        let date = |v: Option<String>| v.and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok());
        let is_date = |w: &str| NaiveDate::parse_from_str(w, "%Y-%m-%d").is_ok();
        match field {
            Field::Priority(p) => t.priority = Some(p),
            Field::Due => t.due = date(value(&is_date)).or(t.due),
            Field::Scheduled => t.scheduled = date(value(&is_date)).or(t.scheduled),
            Field::OtherDate => drop(value(&is_date)),
            Field::Id => t.id = value(&|w: &str| w.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')).or(t.id.take()),
            Field::OneWord => drop(value(&|_: &str| true)),
            // Recurrence runs to the next field (`🔁 every week on Monday 📅 …`).
            Field::Words => {
                while tokens.peek().is_some_and(|n| field_of(n).is_none()) {
                    tokens.next();
                }
            }
        }
    }
    // Links are words where the sentence goes on after them; trailing ones are only goals.
    while words.last().is_some_and(|w| link_index(w).is_some()) {
        words.pop();
    }
    let text = words
        .iter()
        .map(|w| match link_index(w) {
            Some(i) => goals.get(i).map(|(_, shown)| shown.clone()).unwrap_or_default(),
            None => w.clone(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    t.text = text.trim().to_string();
    t.goals = goals.into_iter().map(|(target, _)| target).filter(|g| !g.is_empty()).collect();
    t.goals.dedup();
    Some(t)
}

#[derive(Clone, Copy)]
enum Field {
    Priority(Priority),
    Due,
    Scheduled,
    OtherDate,
    Id,
    OneWord,
    Words,
}

/// The Tasks field `tok` starts with, and whatever is glued after its emoji.
fn field_of(tok: &str) -> Option<(Field, &str)> {
    const FIELDS: &[(&str, Field)] = &[
        ("🔺", Field::Priority(Priority::Urgent)),
        ("⏫", Field::Priority(Priority::Important)),
        ("🔼", Field::Priority(Priority::Medium)),
        ("🔽", Field::Priority(Priority::NotUrgent)),
        ("⏬", Field::Priority(Priority::ParkingLot)),
        ("📅", Field::Due),
        ("📆", Field::Due),
        ("🗓", Field::Due),
        ("⏳", Field::Scheduled),
        ("⌛", Field::Scheduled),
        ("✅", Field::OtherDate),
        ("➕", Field::OtherDate),
        ("🛫", Field::OtherDate),
        ("❌", Field::OtherDate),
        ("🆔", Field::Id),
        ("⛔", Field::OneWord),
        ("🏁", Field::OneWord),
        ("🔁", Field::Words),
    ];
    FIELDS.iter().find_map(|(emoji, f)| tok.strip_prefix(emoji).map(|rest| (*f, rest)))
}

/// A placeholder for the i-th link, which no whitespace split can break.
fn link_mark(i: usize) -> String {
    format!("\u{1}{i}\u{1}")
}

fn link_index(word: &str) -> Option<usize> {
    word.strip_prefix('\u{1}')?.strip_suffix('\u{1}')?.parse().ok()
}

/// `body` with each `[[target|shown]]` replaced by a placeholder word, and the links as
/// (target, shown text).
fn take_links(body: &str) -> (String, Vec<(String, String)>) {
    let mut out = String::new();
    let mut links = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("[[") {
        let Some(len) = rest[start + 2..].find("]]") else { break };
        let inner = &rest[start + 2..start + 2 + len];
        let (target, shown) = inner.split_once('|').unwrap_or((inner, inner));
        let target = target.split('#').next().unwrap_or("").trim().to_string();
        out.push_str(&rest[..start]);
        out.push(' ');
        out.push_str(&link_mark(links.len()));
        out.push(' ');
        links.push((target, shown.trim().to_string()));
        rest = &rest[start + 2 + len + 2..];
    }
    out.push_str(rest);
    (out, links)
}

/// The open tasks in one file's text.
pub fn parse_file(project: &str, file: &Path, text: &str, today: NaiveDate) -> Vec<Task> {
    let mut fence = false;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if fence {
            continue;
        }
        if let Some(mut t) = parse_line(line) {
            t.project = project.to_string();
            t.file = file.to_path_buf();
            t.line = n + 1;
            t.overdue = t.due.is_some_and(|d| d < today);
            out.push(t);
        }
    }
    out
}

/// Group by project and sort (see the header).
pub fn sort(tasks: Vec<Task>) -> Vec<Project> {
    let key = |t: &Task| (rank(t.priority), t.due.is_none(), t.due, t.line);
    let mut projects: Vec<Project> = Vec::new();
    for t in tasks {
        match projects.iter_mut().find(|p| p.project == t.project) {
            Some(p) => p.tasks.push(t),
            None => projects.push(Project { project: t.project.clone(), tasks: vec![t] }),
        }
    }
    for p in &mut projects {
        p.tasks.sort_by_key(key);
    }
    projects.sort_by(|a, b| key(&a.tasks[0]).0.cmp(&key(&b.tasks[0]).0)
        .then_with(|| (a.tasks[0].due.is_none(), a.tasks[0].due).cmp(&(b.tasks[0].due.is_none(), b.tasks[0].due)))
        .then_with(|| a.project.cmp(&b.project)));
    projects
}

/// Every open task under `dir` (each subfolder's `tasks.md`), grouped and sorted, and the files
/// that couldn't be read. Symbolic links are not followed: only the vault's own files are read.
pub fn read(dir: &Path, today: NaiveDate) -> (Vec<Project>, Vec<String>) {
    let mut tasks = Vec::new();
    let mut problems = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return (Vec::new(), problems) };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let file = e.path().join("tasks.md");
        match std::fs::symlink_metadata(&file) {
            Ok(m) if m.is_file() => match std::fs::read_to_string(&file) {
                Ok(text) => tasks.extend(parse_file(&name, &file, &text, today)),
                Err(err) => problems.push(format!("{}: {err}", file.display())),
            },
            _ => {}
        }
    }
    (sort(tasks), problems)
}

/// Today in the owner's zone (`ZEN_TZ`, UTC when it doesn't parse).
pub fn today() -> (NaiveDate, String) {
    let name = crate::config::get().tz.clone();
    let tz = crate::jobs::parse_tz(&name).unwrap_or(chrono_tz::UTC);
    (chrono::Utc::now().with_timezone(&tz).date_naive(), tz.name().to_string())
}

/// The title of the session a task is handed to.
pub fn agent_title(t: &Task) -> String {
    let text: String = t.text.chars().take(60).collect();
    format!("task: {}", if text.is_empty() { t.id.clone().unwrap_or_default() } else { text })
}

/// The first prompt of the agent the owner hands task `t` to: the task, where it comes from, and
/// how to work it. The agent reports; the owner ticks the box.
pub fn brief(t: &Task) -> String {
    let vault = crate::layout::global_dir();
    let mut s = format!("The owner handed you this task from the vault's Tasks view:\n\n> {}\n\n", if t.text.is_empty() { "(no text)" } else { &t.text });
    s.push_str(&format!("- Project: {} (its page: {})\n", t.project, vault.join("Projects").join(&t.project).join(format!("{}.md", t.project)).display()));
    for g in &t.goals {
        s.push_str(&format!("- Goal: [[{g}]] (in {})\n", vault.join("Goals").display()));
    }
    if let Some(p) = t.priority {
        let name = match p {
            Priority::Urgent => "urgent",
            Priority::Important => "important (moves a goal forward)",
            Priority::Medium => "medium",
            Priority::NotUrgent => "not urgent",
            Priority::ParkingLot => "parking lot",
        };
        s.push_str(&format!("- Priority: {name}\n"));
    }
    if let Some(d) = t.due {
        s.push_str(&format!("- Due: {d}{}\n", if t.overdue { " (overdue)" } else { "" }));
    }
    if let Some(d) = t.scheduled {
        s.push_str(&format!("- Scheduled: {d}\n"));
    }
    s.push_str(&format!("- Task line: {}:{} (🆔 {})\n\n", t.file.display(), t.line, t.id.as_deref().unwrap_or("none")));
    s.push_str(
        "Read the project page and the goal first, then do the task end to end as you always work. \
         Ask the owner before anything outward-facing, costly or irreversible, and when a choice is really his. \
         Don't edit the task line: the owner ticks it in Obsidian. When you're done, report what you did and the \
         evidence, and say it's ready to tick; if it can't be finished, say what's left and what you need.",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn one(line: &str) -> Task {
        parse_line(line).unwrap_or_else(|| panic!("not a task: {line}"))
    }

    #[test]
    fn reads_the_fields_in_any_order() {
        let t = one("- [ ] Draft the seed order [[Goal - Garden first harvest]] 📅 2026-10-30 🆔 t7k2");
        assert_eq!(t.text, "Draft the seed order");
        assert_eq!(t.goals, ["Goal - Garden first harvest"]);
        assert_eq!((t.due, t.id.as_deref(), t.priority), (Some(d("2026-10-30")), Some("t7k2"), None));
        let t = one("  * [ ] 🆔 a1 ⏳ 2026-11-02 Pay the bill 🔺 📅2026-11-01");
        assert_eq!((t.text.as_str(), t.id.as_deref(), t.scheduled, t.due, t.priority), ("Pay the bill", Some("a1"), Some(d("2026-11-02")), Some(d("2026-11-01")), Some(Priority::Urgent)));
    }

    #[test]
    fn tolerates_messy_lines() {
        // Variation selectors, other fields, recurrence, a link mid-sentence, an alias, a heading.
        let t = one("1. [ ] Read [[Bayes' theorem|Bayes]] notes ⏫\u{FE0F} ➕ 2026-10-01 🔁 every week on Monday 📅\u{FE0F} 2026-10-27 ⛔ x1,x2 #reading [[Goals/Launch#Why]]");
        assert_eq!(t.text, "Read Bayes notes #reading");
        assert_eq!(t.goals, ["Bayes' theorem", "Goals/Launch"]);
        assert_eq!((t.priority, t.due), (Some(Priority::Important), Some(d("2026-10-27"))));
        // A field without a value, or a bad date, doesn't eat the next word or fail the line.
        let t = one("- [ ] Ring 📅 soon 🆔 Mom");
        assert_eq!((t.text.as_str(), t.due, t.id.as_deref()), ("Ring soon", None, Some("Mom")));
        assert_eq!(one("- [ ]").text, "");
        assert_eq!(one("- [ ] Drop it (dropped later) ⏬").priority, Some(Priority::ParkingLot));
        assert_eq!(one("+ [ ] Low 🔽").priority, Some(Priority::NotUrgent));
    }

    #[test]
    fn only_open_checkboxes_are_tasks() {
        for not in ["- [x] Done 🆔 a", "- [X] Done", "- [-] Dropped", "- [/] Half", "- [>] Later", "- Plain item", "[ ] no marker",
                    "-[ ] no space", "- [ ]x glued", "# Heading", "", "1 [ ] no dot", "Text - [ ] later"] {
            assert!(parse_line(not).is_none(), "{not}");
        }
        let text = "# x\n- [ ] one\n```\n- [ ] in a fence\n```\n- [x] two\n\t- [ ] nested 📅 2026-10-27\n";
        let tasks = parse_file("p", Path::new("p/tasks.md"), text, d("2026-10-28"));
        assert_eq!(tasks.iter().map(|t| (t.text.as_str(), t.line, t.overdue)).collect::<Vec<_>>(), [("one", 2, false), ("nested", 7, true)]);
    }

    #[test]
    fn sorts_by_project_priority_then_due() {
        let today = d("2026-10-28");
        let a = "- [ ] a-none-due 📅 2026-10-29\n- [ ] a-park ⏬ 📅 2026-10-01\n- [ ] a-imp-nodue ⏫\n- [ ] a-imp-due ⏫ 📅 2026-11-30\n- [ ] a-low 🔽\n";
        let b = "- [ ] b-urgent 🔺 📅 2026-12-01\n- [ ] b-none\n";
        let c = "- [ ] c-park ⏬\n";
        let mut all = parse_file("a", Path::new("a"), a, today);
        all.extend(parse_file("c", Path::new("c"), c, today));
        all.extend(parse_file("b", Path::new("b"), b, today));
        let got: Vec<(String, Vec<String>)> = sort(all).into_iter().map(|p| (p.project, p.tasks.into_iter().map(|t| t.text).collect())).collect();
        assert_eq!(got, [
            ("b".to_string(), vec!["b-urgent".to_string(), "b-none".into()]),
            ("a".into(), vec!["a-imp-due".into(), "a-imp-nodue".into(), "a-low".into(), "a-none-due".into(), "a-park".into()]),
            ("c".into(), vec!["c-park".into()]),
        ]);
        // Same rank: the earlier due date leads.
        let x = parse_file("x", Path::new("x"), "- [ ] x ⏫ 📅 2026-11-02", today);
        let y = parse_file("y", Path::new("y"), "- [ ] y ⏫ 📅 2026-11-01", today);
        assert_eq!(sort([x, y].concat()).iter().map(|p| p.project.as_str()).collect::<Vec<_>>(), ["y", "x"]);
    }

    #[test]
    fn reads_each_projects_tasks_file_and_nothing_else() {
        let dir = crate::test_util::TestDir::new("tasks");
        for (p, body) in [("self", "- [ ] mine 📅 2026-10-27 🆔 s1\n- [x] old"), ("empty", "# empty tasks\n"), (".hidden", "- [ ] no")] {
            std::fs::create_dir_all(dir.join(p)).unwrap();
            std::fs::write(dir.join(p).join("tasks.md"), body).unwrap();
        }
        std::fs::write(dir.join("loose.md"), "- [ ] not in a project").unwrap();
        std::fs::create_dir_all(dir.join("linked")).unwrap();
        std::os::unix::fs::symlink(dir.join("self/tasks.md"), dir.join("linked/tasks.md")).unwrap();
        let before = std::fs::read_to_string(dir.join("self/tasks.md")).unwrap();
        let (projects, problems) = read(&dir, d("2026-10-28"));
        assert!(problems.is_empty());
        assert_eq!(projects.len(), 1);
        let t = &projects[0].tasks[0];
        assert_eq!((t.project.as_str(), t.text.as_str(), t.line, t.overdue), ("self", "mine", 1, true));
        assert_eq!(t.file, dir.join("self/tasks.md"));
        assert_eq!(std::fs::read_to_string(dir.join("self/tasks.md")).unwrap(), before, "never written");
        assert!(read(&dir.join("missing"), d("2026-10-28")).0.is_empty());
    }

    #[test]
    fn the_brief_carries_the_task_and_its_context() {
        let t = parse_file("garden", Path::new("/v/Projects/garden/tasks.md"), "- [ ] Draft the seed order [[Goal - First harvest]] ⏫ 📅 2026-10-27 🆔 t7k2\n", d("2026-10-28")).remove(0);
        let b = brief(&t);
        for want in ["> Draft the seed order", "- Project: garden", "garden/garden.md", "- Goal: [[Goal - First harvest]]", "- Priority: important", "- Due: 2026-10-27 (overdue)", "tasks.md:1 (🆔 t7k2)", "Don't edit the task line"] {
            assert!(b.contains(want), "{want} missing from:\n{b}");
        }
        assert_eq!(agent_title(&t), "task: Draft the seed order");
    }
}
