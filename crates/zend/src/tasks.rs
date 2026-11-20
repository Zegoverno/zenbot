//! The owner's tasks, in the vault: `<zen home>/global/Projects/<project>/tasks.md`, one checkbox
//! per line in the Obsidian Tasks plugin's emoji format (the vault's AGENTS.md, "Tasks"). The web
//! app reads and writes them through the API; the owner still edits the same files in Obsidian.
//! `GET /api/tasks` lists them, the shared `tasks` command shows them; `POST /api/tasks`,
//! `PATCH` and `DELETE /api/tasks/{id}` write lines in place; `GET /api/tasks/meta` lists the
//! projects and goals a task can name. `agent_title` and `brief` make the session a task is handed
//! to (`POST /api/tasks/{id}/agent`).
//!
//! Reading: open tasks (`- [ ]`) are listed; with `?done=Nd` the tasks ticked in the last N days
//! too (`done: true`, `done_on`). Dropped (`[-]`) and any other state are skipped, as are lines in
//! code fences. Fields may come in any order, with or without the emoji variation selector: `📅`
//! due, `⏳` scheduled, `🆔` id, `✅` done on, the priorities `🔺` urgent, `⏫` important, `🔼`
//! medium (read, never written), `🔽` not urgent, `⏬` parking lot; other Tasks fields (`➕ 🛫 ❌`
//! dates, `⛔` dependencies, `🔁` recurrence, `🏁`) are kept on the line and dropped from the
//! text. `[[…]]` links are the task's goals: listed apart, and kept in the text as words only
//! when the sentence goes on after them.
//!
//! Ids: every task has a stable `id`: its `🆔`, else the synthetic `L:<path from the vault
//! root>:<1-based line>` (the web app and the zenread plugin build the same). A task without a
//! `🆔` that is handed to an agent gets one written first. `rev` is a short hash of the line as
//! read; a write that names it is refused (409) when the line has changed since.
//!
//! Writing: a line is rewritten in place and the rest of the file is kept as it is. Each write
//! re-reads the file, checks that the line is still the task the id resolves to (and has the
//! `rev`, when given), and replaces the file atomically (temporary file, then rename); a line
//! that is gone or different is a conflict (`WriteError::Conflict`, HTTP 409), never a guess.
//! Fields the app doesn't manage (recurrence, dependencies, tags, notes under the line) are kept.
//! Writers in the kernel run one at a time. Done: `[x]` and `✅ <today>` (the owner's zone, `ZEN_TZ`);
//! dropped: `[-]` and `❌ <today>`; a project change moves the line (and the notes under it) to
//! the other project's `tasks.md`.
//!
//! Order of the list: by project, the project whose most pressing task comes first leading;
//! inside a project by priority (urgent, important, medium, not urgent, none, parking lot), then
//! due date (none last), then file order; done tasks last, the latest first. A task is overdue
//! when its due date is before today in the owner's zone.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::NaiveDate;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// The vault's projects folder: `<zen home>/global/Projects`.
pub fn projects_dir() -> PathBuf {
    crate::layout::global_dir().join("Projects")
}

/// The vault: `<zen home>/global`.
pub fn vault_dir() -> PathBuf {
    crate::layout::global_dir()
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

impl Priority {
    fn emoji(self) -> &'static str {
        match self {
            Priority::Urgent => "🔺",
            Priority::Important => "⏫",
            Priority::Medium => "🔼",
            Priority::NotUrgent => "🔽",
            Priority::ParkingLot => "⏬",
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Open,
    Done,
    Dropped,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Task {
    pub project: String,
    pub text: String,
    /// The `🆔`, else `L:<path from the vault root>:<line>`.
    pub id: String,
    #[serde(skip)]
    pub has_id: bool,
    pub due: Option<NaiveDate>,
    pub scheduled: Option<NaiveDate>,
    pub priority: Option<Priority>,
    /// The `[[…]]` links' targets (without `|alias` or `#heading`).
    pub goals: Vec<String>,
    /// The first goal as the goals list (`GET /api/tasks/meta`) names it: its note's name.
    pub goal: Option<String>,
    pub file: PathBuf,
    /// 1-based.
    pub line: usize,
    pub overdue: bool,
    pub done: bool,
    pub done_on: Option<NaiveDate>,
    /// A short hash of the line as read, for a write that must not overwrite a newer one.
    pub rev: String,
    #[serde(skip)]
    pub state: State,
}

#[derive(Debug, Serialize)]
pub struct Project {
    pub project: String,
    pub tasks: Vec<Task>,
}

// ---------- reading a line ----------

/// What a line holds between its checkbox and its end, in order. Writing a line back is these
/// items joined with single spaces, so whatever isn't changed stays.
#[derive(Debug, Clone, PartialEq)]
enum Item {
    Word(String),
    /// `[[inner]]`, `inner` without the brackets.
    Link(String),
    Field { kind: Kind, value: Option<String>, raw: String },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Priority(Priority),
    Due,
    Scheduled,
    Done,
    Dropped,
    Id,
    /// Created, start, dependencies, recurrence, on-completion: kept as they are.
    Other,
}

#[derive(Clone, Copy)]
enum Shape {
    Priority(Priority),
    Due,
    Scheduled,
    Done,
    OtherDate,
    Dropped,
    Id,
    OneWord,
    Words,
}

const FIELDS: &[(&str, Shape)] = &[
    ("🔺", Shape::Priority(Priority::Urgent)),
    ("⏫", Shape::Priority(Priority::Important)),
    ("🔼", Shape::Priority(Priority::Medium)),
    ("🔽", Shape::Priority(Priority::NotUrgent)),
    ("⏬", Shape::Priority(Priority::ParkingLot)),
    ("📅", Shape::Due),
    ("📆", Shape::Due),
    ("🗓", Shape::Due),
    ("⏳", Shape::Scheduled),
    ("⌛", Shape::Scheduled),
    ("✅", Shape::Done),
    ("➕", Shape::OtherDate),
    ("🛫", Shape::OtherDate),
    ("❌", Shape::Dropped),
    ("🆔", Shape::Id),
    ("⛔", Shape::OneWord),
    ("🏁", Shape::OneWord),
    ("🔁", Shape::Words),
];

/// The Tasks field `tok` starts with, and whatever is glued after its emoji.
fn field_of(tok: &str) -> Option<(Shape, &str)> {
    FIELDS.iter().find_map(|(emoji, f)| tok.strip_prefix(emoji).map(|rest| (*f, rest)))
}

fn is_date(w: &str) -> bool {
    NaiveDate::parse_from_str(w, "%Y-%m-%d").is_ok()
}

fn is_id(w: &str) -> bool {
    !w.is_empty() && w.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

/// A piece of a line's body: a `[[link]]` (which may hold spaces) or a run without whitespace.
enum Tok {
    Link(String),
    Text(String),
}

fn tokens(body: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut rest = body;
    let flush = |word: &mut String, out: &mut Vec<Tok>| {
        if !word.is_empty() {
            out.push(Tok::Text(std::mem::take(word)));
        }
    };
    while let Some(c) = rest.chars().next() {
        if let Some(after) = rest.strip_prefix("[[") {
            if let Some(len) = after.find("]]") {
                flush(&mut word, &mut out);
                out.push(Tok::Link(after[..len].to_string()));
                rest = &after[len + 2..];
                continue;
            }
        }
        if c.is_whitespace() {
            flush(&mut word, &mut out);
        } else {
            word.push(c);
        }
        rest = &rest[c.len_utf8()..];
    }
    flush(&mut word, &mut out);
    out
}

/// The items of a line's body (what follows its checkbox).
fn lex(body: &str) -> Vec<Item> {
    let toks = tokens(body);
    let mut items = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let tok = match &toks[i] {
            Tok::Link(inner) => {
                items.push(Item::Link(inner.clone()));
                i += 1;
                continue;
            }
            Tok::Text(t) => t,
        };
        i += 1;
        let Some((shape, glued)) = field_of(tok) else {
            items.push(Item::Word(tok.clone()));
            continue;
        };
        let mut raw = tok.clone();
        // A field's value: glued to the emoji (`📅2026-10-30`) or the next word.
        let mut value = |ok: &dyn Fn(&str) -> bool| -> Option<String> {
            if !glued.is_empty() {
                return Some(glued.to_string());
            }
            match toks.get(i) {
                Some(Tok::Text(next)) if field_of(next).is_none() && ok(next) => {
                    i += 1;
                    raw.push(' ');
                    raw.push_str(next);
                    Some(next.clone())
                }
                _ => None,
            }
        };
        let (kind, value) = match shape {
            Shape::Priority(p) => (Kind::Priority(p), None),
            Shape::Due => (Kind::Due, value(&is_date)),
            Shape::Scheduled => (Kind::Scheduled, value(&is_date)),
            Shape::Done => (Kind::Done, value(&is_date)),
            Shape::OtherDate => (Kind::Other, value(&is_date)),
            Shape::Dropped => (Kind::Dropped, value(&is_date)),
            Shape::Id => (Kind::Id, value(&is_id)),
            Shape::OneWord => (Kind::Other, value(&|_: &str| true)),
            // Recurrence runs to the next field (`🔁 every week on Monday 📅 …`).
            Shape::Words => {
                while let Some(Tok::Text(next)) = toks.get(i).filter(|t| matches!(t, Tok::Text(n) if field_of(n).is_none())) {
                    raw.push(' ');
                    raw.push_str(next);
                    i += 1;
                }
                (Kind::Other, None)
            }
        };
        items.push(Item::Field { kind, value, raw });
    }
    items
}

fn render(items: &[Item]) -> String {
    items
        .iter()
        .map(|i| match i {
            Item::Word(w) => w.clone(),
            Item::Link(inner) => format!("[[{inner}]]"),
            Item::Field { raw, .. } => raw.clone(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The link's target without `|alias` or `#heading`, and the text it shows.
fn link_parts(inner: &str) -> (String, String) {
    let (target, shown) = inner.split_once('|').unwrap_or((inner, inner));
    (target.split('#').next().unwrap_or("").trim().to_string(), shown.trim().to_string())
}

/// A goal link's target as the goals list names it: the note's name, without `Goals/`.
fn goal_id(target: &str) -> String {
    target.strip_prefix("Goals/").unwrap_or(target).to_string()
}

/// A line that is a task in any state, split into what comes before its box, its state and items.
struct Parsed {
    /// Indent and list marker, without the checkbox (`"  - "`).
    head: String,
    state: State,
    items: Vec<Item>,
    task: Task,
}

fn parse_full(raw: &str) -> Option<Parsed> {
    let line = raw.replace('\u{FE0F}', "");
    let line = line.trim_end_matches('\r');
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
    let after = rest.strip_prefix([' ', '\t'])?.trim_start();
    let (state, body) = [("[ ]", State::Open), ("[x]", State::Done), ("[X]", State::Done), ("[-]", State::Dropped)]
        .into_iter()
        .find_map(|(m, st)| after.strip_prefix(m).map(|b| (st, b)))?;
    if !body.is_empty() && !body.starts_with([' ', '\t']) {
        return None;
    }
    let head = line[..line.len() - body.len() - 3].to_string();
    let items = lex(body);
    let mut task = Task {
        project: String::new(),
        text: String::new(),
        id: String::new(),
        has_id: false,
        due: None,
        scheduled: None,
        priority: None,
        goals: Vec::new(),
        goal: None,
        file: PathBuf::new(),
        line: 0,
        overdue: false,
        done: state == State::Done,
        done_on: None,
        rev: rev_of(raw),
        state,
    };
    let date = |v: &Option<String>| v.as_deref().and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    let mut id = None;
    for item in &items {
        if let Item::Field { kind, value, .. } = item {
            match kind {
                Kind::Priority(p) => task.priority = Some(*p),
                Kind::Due => task.due = date(value).or(task.due),
                Kind::Scheduled => task.scheduled = date(value).or(task.scheduled),
                Kind::Done => task.done_on = date(value).or(task.done_on),
                Kind::Id => id = value.clone().or(id),
                Kind::Dropped | Kind::Other => {}
            }
        }
    }
    if let Some(id) = id {
        task.id = id;
        task.has_id = true;
    }
    // Links are words where the sentence goes on after them; trailing ones are only goals.
    let words: Vec<&Item> = items.iter().filter(|i| !matches!(i, Item::Field { .. })).collect();
    let keep = words.iter().rposition(|i| matches!(i, Item::Word(_))).map_or(0, |p| p + 1);
    let text = words[..keep]
        .iter()
        .map(|i| match i {
            Item::Word(w) => w.clone(),
            Item::Link(inner) => link_parts(inner).1,
            Item::Field { .. } => String::new(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    task.text = text.trim().to_string();
    for item in &items {
        if let Item::Link(inner) = item {
            let target = link_parts(inner).0;
            if !target.is_empty() && task.goals.last() != Some(&target) {
                task.goals.push(target);
            }
        }
    }
    task.goal = task.goals.first().map(|g| goal_id(g));
    Some(Parsed { head, state, items, task })
}

/// A short hash of a line, so a write can tell the line is still the one that was read.
fn rev_of(raw: &str) -> String {
    let h = Sha256::digest(raw.trim_end_matches('\r').as_bytes());
    h[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// The open task on `line`, if it is one (`project`, `file` and `line` filled by the caller).
#[cfg(test)]
fn parse_line(raw: &str) -> Option<Task> {
    parse_full(raw).filter(|p| p.state == State::Open).map(|p| p.task)
}

/// Every task line of a file's text (any state), with its 1-based line number; lines in code
/// fences are not tasks.
fn scan_text(text: &str) -> Vec<(usize, Parsed)> {
    let mut fence = false;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if !fence {
            if let Some(p) = parse_full(line) {
                out.push((n + 1, p));
            }
        }
    }
    out
}

/// The task parsed on a line of `file` (`rel`: its path from the vault root), ready to list.
fn placed(mut p: Parsed, rel: &str, file: &Path, line: usize, today: NaiveDate) -> Task {
    let t = &mut p.task;
    t.project = project_of(rel);
    t.file = file.to_path_buf();
    t.line = line;
    if !t.has_id {
        t.id = format!("L:{rel}:{line}");
    }
    t.overdue = t.state == State::Open && t.due.is_some_and(|d| d < today);
    p.task
}

/// The project a note belongs to: the folder under `Projects/` (empty for any other note).
fn project_of(rel: &str) -> String {
    let mut parts = rel.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("Projects"), Some(p), Some(_)) => p.to_string(),
        _ => String::new(),
    }
}

/// The open tasks in one file's text. `rel`: the file's path from the vault root.
#[cfg(test)]
pub fn parse_file(rel: &str, file: &Path, text: &str, today: NaiveDate) -> Vec<Task> {
    scan_text(text).into_iter().filter(|(_, p)| p.state == State::Open).map(|(n, p)| placed(p, rel, file, n, today)).collect()
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

/// Every open task under `dir` (each subfolder's `tasks.md`; the vault is `dir`'s parent), grouped
/// and sorted, with the tasks ticked on or after `done_since` last in their project, and the
/// files that couldn't be read. Symbolic links are not followed: only the vault's own files are read.
pub fn read(dir: &Path, today: NaiveDate, done_since: Option<NaiveDate>) -> (Vec<Project>, Vec<String>) {
    let vault = dir.parent().unwrap_or(dir);
    let mut open = Vec::new();
    let mut done = Vec::new();
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
                Ok(text) => {
                    let rel = file.strip_prefix(vault).unwrap_or(&file).to_string_lossy().to_string();
                    for (n, p) in scan_text(&text) {
                        let state = p.state;
                        let t = placed(p, &rel, &file, n, today);
                        match state {
                            State::Open => open.push(t),
                            State::Done if done_since.is_some_and(|s| t.done_on.is_some_and(|d| d >= s)) => done.push(t),
                            _ => {}
                        }
                    }
                }
                Err(err) => problems.push(format!("{}: {err}", file.display())),
            },
            _ => {}
        }
    }
    let mut projects = sort(open);
    done.sort_by(|a, b| b.done_on.cmp(&a.done_on).then_with(|| a.line.cmp(&b.line)));
    for t in done {
        match projects.iter_mut().find(|p| p.project == t.project) {
            Some(p) => p.tasks.push(t),
            None => projects.push(Project { project: t.project.clone(), tasks: vec![t] }),
        }
    }
    (projects, problems)
}

/// Today in the owner's zone (`ZEN_TZ`, UTC when it doesn't parse).
pub fn today() -> (NaiveDate, String) {
    let name = crate::config::get().tz.clone();
    let tz = crate::jobs::parse_tz(&name).unwrap_or(chrono_tz::UTC);
    (chrono::Utc::now().with_timezone(&tz).date_naive(), tz.name().to_string())
}

/// `7d` → 7 (a number of days, 1 to 366), for `?done=`.
pub fn parse_days(s: &str) -> Option<i64> {
    s.strip_suffix('d')?.parse::<i64>().ok().filter(|n| (1..=366).contains(n))
}

// ---------- writing ----------

/// Why a write didn't happen; the API turns each into its status.
#[derive(Debug)]
pub enum WriteError {
    /// No such task, project or goal (404).
    NotFound(String),
    /// The line changed or went away since it was read (409).
    Conflict(String),
    /// What was asked can't be written (400).
    Invalid(String),
    /// The disk said no (500).
    Io(String),
}

impl WriteError {
    pub fn message(&self) -> &str {
        match self {
            WriteError::NotFound(m) | WriteError::Conflict(m) | WriteError::Invalid(m) | WriteError::Io(m) => m,
        }
    }
}

impl From<std::io::Error> for WriteError {
    fn from(e: std::io::Error) -> Self {
        WriteError::Io(e.to_string())
    }
}

pub(crate) type Wr<T> = Result<T, WriteError>;

fn invalid<T>(m: impl Into<String>) -> Wr<T> {
    Err(WriteError::Invalid(m.into()))
}

/// Writers run one at a time, so two requests can't read the same file and each write over the
/// other's change.
static WRITES: Mutex<()> = Mutex::new(());

/// What a `PATCH` changes. `Some(None)` removes the field.
#[derive(Debug, Default)]
pub struct Patch {
    pub done: Option<bool>,
    pub dropped: Option<bool>,
    pub text: Option<String>,
    pub due: Option<Option<NaiveDate>>,
    pub priority: Option<Option<Priority>>,
    /// The goal's note name.
    pub goal: Option<Option<String>>,
    pub project: Option<String>,
}

/// A priority the app may write: `none` removes it; `medium` is read, never written.
pub fn priority_from_name(name: &str) -> Wr<Option<Priority>> {
    match name {
        "urgent" => Ok(Some(Priority::Urgent)),
        "important" => Ok(Some(Priority::Important)),
        "not_urgent" => Ok(Some(Priority::NotUrgent)),
        "parking_lot" => Ok(Some(Priority::ParkingLot)),
        "none" => Ok(None),
        other => invalid(format!("priority must be urgent, important, not_urgent, parking_lot or none; got `{other}`")),
    }
}

/// A task's text as the app sends it: one line, no Tasks field emoji (they'd turn into fields).
fn clean_text(text: &str) -> Wr<String> {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() {
        return invalid("a task needs text");
    }
    if t.chars().count() > 2000 {
        return invalid("a task's text can be 2000 characters at most");
    }
    if let Some((emoji, _)) = FIELDS.iter().find(|(e, _)| t.contains(e)) {
        return invalid(format!("the text can't contain {emoji}: it is a task field (due date, priority, id…); use the field instead"));
    }
    if t.contains('\u{FE0F}') {
        return invalid("the text can't contain emoji variation selectors");
    }
    Ok(t)
}

/// A name that is one folder or one note's name: no path, no hidden file, no wiki-link syntax.
fn plain_name(name: &str, what: &str) -> Wr<String> {
    let n = name.trim();
    if n.is_empty() || n.len() > 200 || n.starts_with('.') || n.contains(['/', '\\', '\0', '\n', '\r', '|', '#', '[', ']']) || n == ".." {
        return invalid(format!("not a valid {what} name: `{name}`"));
    }
    Ok(n.to_string())
}

/// A file inside the vault by its path from the root: a Markdown note, no hidden parts, nothing
/// reached through a symbolic link.
fn safe_note(vault: &Path, rel: &str) -> Wr<PathBuf> {
    let p = Path::new(rel);
    let bad = rel.is_empty()
        || !rel.to_ascii_lowercase().ends_with(".md")
        || p.components().any(|c| !matches!(c, std::path::Component::Normal(n) if !n.to_string_lossy().starts_with('.')));
    if bad {
        return invalid(format!("not a note inside the vault: `{rel}`"));
    }
    let mut cur = vault.to_path_buf();
    for c in p.components() {
        cur.push(c);
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => return invalid(format!("`{rel}` goes through a symbolic link")),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(WriteError::NotFound(format!("no note `{rel}`"))),
            Err(e) => return Err(e.into()),
        }
    }
    if !std::fs::symlink_metadata(&cur)?.is_file() {
        return invalid(format!("`{rel}` is not a file"));
    }
    Ok(cur)
}

fn rel_of(vault: &Path, file: &Path) -> String {
    file.strip_prefix(vault).unwrap_or(file).to_string_lossy().to_string()
}

/// Write `text` over `path` atomically: a temporary file next to it, then a rename. An existing
/// file keeps its permissions.
pub(crate) fn write_atomic(path: &Path, text: &str) -> Wr<()> {
    use std::io::Write;
    let dir = path.parent().ok_or_else(|| WriteError::Io("no folder".into()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        if let Ok(m) = std::fs::metadata(path) {
            std::fs::set_permissions(&tmp, m.permissions())?;
        }
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(result?)
}

/// A file's lines and the line ending it uses.
fn split_lines(text: &str) -> (Vec<String>, &'static str) {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    (text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect(), eol)
}

/// The notes under a task line: the lines after it that are indented deeper, aren't blank and
/// aren't tasks themselves.
fn continuation(lines: &[String], at: usize) -> usize {
    let indent = |l: &str| l.len() - l.trim_start().len();
    let base = indent(&lines[at]);
    lines[at + 1..].iter().take_while(|l| !l.trim().is_empty() && indent(l) > base && parse_full(l).is_none()).count()
}

/// Where a task id points: its file, that file's path from the vault root, and its line (0-based).
struct Place {
    file: PathBuf,
    rel: String,
    index: usize,
}

/// Every note of the vault, `Projects/*/tasks.md` first (where tasks live), hidden folders and
/// symbolic links left out.
fn vault_notes(vault: &Path) -> Vec<PathBuf> {
    let mut first = Vec::new();
    for e in std::fs::read_dir(vault.join("Projects")).into_iter().flatten().flatten() {
        let f = e.path().join("tasks.md");
        if std::fs::symlink_metadata(&f).is_ok_and(|m| m.is_file()) && !e.file_name().to_string_lossy().starts_with('.') {
            first.push(f);
        }
    }
    first.sort();
    let mut rest = Vec::new();
    let mut stack = vec![(vault.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Ok(t) = e.file_type() else { continue };
            if name.starts_with('.') || t.is_symlink() {
                continue;
            }
            if t.is_dir() && depth < 12 {
                stack.push((e.path(), depth + 1));
            } else if t.is_file() && name.to_ascii_lowercase().ends_with(".md") && !first.contains(&e.path()) {
                rest.push(e.path());
            }
        }
    }
    rest.sort();
    first.extend(rest);
    first
}

/// The `🆔`s used in the vault.
fn used_ids(vault: &Path) -> HashSet<String> {
    let mut ids = HashSet::new();
    for f in vault_notes(vault) {
        if std::fs::metadata(&f).is_ok_and(|m| m.len() > 4_000_000) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        for line in text.lines().filter(|l| l.contains("🆔")) {
            for item in lex(&line.replace('\u{FE0F}', "")) {
                if let Item::Field { kind: Kind::Id, value: Some(v), .. } = item {
                    ids.insert(v);
                }
            }
        }
    }
    ids
}

/// A new `🆔`: six lowercase letters and digits, unused in the vault.
fn new_id(vault: &Path) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let used = used_ids(vault);
    loop {
        let bytes = *uuid::Uuid::new_v4().as_bytes();
        let id: String = bytes[..6].iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect();
        if !used.contains(&id) {
            return id;
        }
    }
}

/// The line `id` points to: a `🆔`'s line anywhere in the vault, or a synthetic id's line, which
/// must still be a task. The file is read once here and again before it is written.
fn locate(vault: &Path, id: &str) -> Wr<Place> {
    if let Some(rest) = id.strip_prefix("L:") {
        let (rel, line) = rest.rsplit_once(':').ok_or_else(|| WriteError::Invalid(format!("not a task id: `{id}`")))?;
        let line: usize = line.parse().ok().filter(|n| *n >= 1).ok_or_else(|| WriteError::Invalid(format!("not a task id: `{id}`")))?;
        let file = safe_note(vault, rel)?;
        let text = std::fs::read_to_string(&file)?;
        return match scan_text(&text).iter().any(|(n, _)| *n == line) {
            true => Ok(Place { file, rel: rel.to_string(), index: line - 1 }),
            false => Err(WriteError::Conflict(format!("line {line} of {rel} is no longer a task; reload the tasks"))),
        };
    }
    if !is_id(id) {
        return invalid(format!("not a task id: `{id}`"));
    }
    for file in vault_notes(vault) {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        if !text.contains("🆔") {
            continue;
        }
        if let Some((n, _)) = scan_text(&text).into_iter().find(|(_, p)| p.task.has_id && p.task.id == id) {
            return Ok(Place { rel: rel_of(vault, &file), file, index: n - 1 });
        }
    }
    Err(WriteError::NotFound(format!("no task with id {id}")))
}

/// The task parsed on a line of the vault, listed as `read` would.
fn task_at(vault: &Path, file: &Path, lines: &[String], index: usize, today: NaiveDate) -> Wr<Task> {
    let p = lines.get(index).and_then(|l| parse_full(l)).ok_or_else(|| WriteError::Conflict("the line is no longer a task".into()))?;
    Ok(placed(p, &rel_of(vault, file), file, index + 1, today))
}

/// Read the file of `place`, check its line is a task (with the `rev`, when given), let `f`
/// change the lines, check the file is still as it was read, and write it.
fn edit<T>(place: &Place, rev: Option<&str>, f: impl FnOnce(&mut Vec<String>, &Parsed) -> Wr<T>) -> Wr<T> {
    let text = std::fs::read_to_string(&place.file)?;
    let (mut lines, eol) = split_lines(&text);
    let parsed = lines.get(place.index).and_then(|l| parse_full(l)).ok_or_else(|| WriteError::Conflict(format!("line {} of {} is no longer a task; reload the tasks", place.index + 1, place.rel)))?;
    if let Some(r) = rev.filter(|r| !r.is_empty()) {
        if r != parsed.task.rev {
            return Err(WriteError::Conflict(format!("line {} of {} changed since it was read; reload the tasks", place.index + 1, place.rel)));
        }
    }
    let out = f(&mut lines, &parsed)?;
    if std::fs::read_to_string(&place.file)? != text {
        return Err(WriteError::Conflict(format!("{} changed while the task was being written; try again", place.rel)));
    }
    write_atomic(&place.file, &lines.join(eol))?;
    Ok(out)
}

fn is_tail(i: &Item) -> bool {
    matches!(i, Item::Field { kind: Kind::Id | Kind::Done | Kind::Dropped, .. })
}

/// Put a field after the text and before the id and done marks (`… ⏫ 📅 2026-11-20 🆔 abc ✅ …`).
fn put_field(items: &mut Vec<Item>, field: Item) {
    let tail = items.iter().rev().take_while(|i| is_tail(i)).count();
    items.insert(items.len() - tail, field);
}

fn field(kind: Kind, emoji: &str, value: Option<String>) -> Item {
    let raw = match &value {
        Some(v) => format!("{emoji} {v}"),
        None => emoji.to_string(),
    };
    Item::Field { kind, value, raw }
}

fn without_fields(items: &mut Vec<Item>, drop: impl Fn(Kind) -> bool) {
    items.retain(|i| !matches!(i, Item::Field { kind, .. } if drop(*kind)));
}

/// Links out of the line: a trailing one goes, one inside the sentence stays as its shown text.
fn without_links(items: &mut Vec<Item>) {
    let last_word = items.iter().rposition(|i| matches!(i, Item::Word(_)));
    let mut out = Vec::new();
    for (n, i) in items.drain(..).enumerate() {
        match i {
            Item::Link(inner) if last_word.is_some_and(|w| n < w) => out.push(Item::Word(link_parts(&inner).1)),
            Item::Link(_) => {}
            other => out.push(other),
        }
    }
    *items = out;
}

/// A goal's link target from what the app sends: a goal's note name, or its title.
fn goal_link(vault: &Path, goal: &str) -> Wr<String> {
    let want = goal.trim();
    let want = want.strip_prefix("Goals/").unwrap_or(want);
    let stem = plain_name(want, "goal").or_else(|_| invalid(format!("not a goal: `{goal}`")))?;
    let goals = meta(vault)["goals"].as_array().cloned().unwrap_or_default();
    let found = goals.iter().find(|g| g["id"] == stem.as_str()).or_else(|| goals.iter().find(|g| g["title"] == stem.as_str()));
    match found.and_then(|g| g["id"].as_str()) {
        Some(id) => Ok(format!("Goals/{id}")),
        None => Err(WriteError::NotFound(format!("no goal `{goal}` in Goals/"))),
    }
}

/// The line a patch makes of `p`'s, and its checkbox state.
fn patched(vault: &Path, p: &Parsed, patch: &Patch, today: NaiveDate) -> Wr<(State, Vec<Item>)> {
    if patch.done == Some(true) && patch.dropped == Some(true) {
        return invalid("a task can't be done and dropped at once");
    }
    let mut items = p.items.clone();
    let mut state = p.state;
    if let Some(text) = &patch.text {
        let text = clean_text(text)?;
        let links: Vec<Item> = items.iter().filter(|i| matches!(i, Item::Link(_))).cloned().collect();
        items.retain(|i| matches!(i, Item::Field { .. }));
        let mut front = vec![Item::Word(text)];
        front.extend(links);
        front.append(&mut items);
        items = front;
    }
    if let Some(goal) = &patch.goal {
        without_links(&mut items);
        if let Some(g) = goal {
            let at = items.iter().rposition(|i| matches!(i, Item::Word(_) | Item::Link(_))).map_or(0, |p| p + 1);
            items.insert(at, Item::Link(goal_link(vault, g)?));
        }
    }
    if let Some(pr) = &patch.priority {
        without_fields(&mut items, |k| matches!(k, Kind::Priority(_)));
        if let Some(pr) = pr {
            put_field(&mut items, field(Kind::Priority(*pr), pr.emoji(), None));
        }
    }
    if let Some(due) = &patch.due {
        without_fields(&mut items, |k| k == Kind::Due);
        if let Some(d) = due {
            put_field(&mut items, field(Kind::Due, "📅", Some(d.to_string())));
        }
    }
    match (patch.done, patch.dropped) {
        (Some(true), _) if state != State::Done => {
            without_fields(&mut items, |k| matches!(k, Kind::Done | Kind::Dropped));
            items.push(field(Kind::Done, "✅", Some(today.to_string())));
            state = State::Done;
        }
        (_, Some(true)) if state != State::Dropped => {
            without_fields(&mut items, |k| matches!(k, Kind::Done | Kind::Dropped));
            items.push(field(Kind::Dropped, "❌", Some(today.to_string())));
            state = State::Dropped;
        }
        (Some(false), _) if state == State::Done => {
            without_fields(&mut items, |k| k == Kind::Done);
            state = State::Open;
        }
        (_, Some(false)) if state == State::Dropped => {
            without_fields(&mut items, |k| k == Kind::Dropped);
            state = State::Open;
        }
        _ => {}
    }
    Ok((state, items))
}

fn line_of(head: &str, state: State, items: &[Item]) -> String {
    let mark = match state {
        State::Open => "[ ]",
        State::Done => "[x]",
        State::Dropped => "[-]",
    };
    let body = render(items);
    if body.is_empty() {
        format!("{head}{mark}")
    } else {
        format!("{head}{mark} {body}")
    }
}

/// A project's `tasks.md` (created when missing) as a safe path: the project is a folder of
/// `Projects/`.
fn project_file(vault: &Path, project: &str) -> Wr<(PathBuf, String)> {
    let name = plain_name(project, "project")?;
    let dir = vault.join("Projects").join(&name);
    match std::fs::symlink_metadata(&dir) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return invalid(format!("Projects/{name} is not a folder")),
        Err(_) => return Err(WriteError::NotFound(format!("no project `{name}` in Projects/"))),
    }
    let file = dir.join("tasks.md");
    if let Ok(m) = std::fs::symlink_metadata(&file) {
        if !m.is_file() {
            return invalid(format!("Projects/{name}/tasks.md is not a plain file"));
        }
    }
    Ok((file, format!("Projects/{name}/tasks.md")))
}

/// The lines of `path` (empty when it doesn't exist), the line ending, and the text read.
fn read_for_append(path: &Path, project: &str) -> Wr<(String, Vec<String>, &'static str)> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let (lines, eol) = split_lines(&text);
            Ok((text, lines, eol))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((String::new(), vec![format!("# {project} tasks")], "\n")),
        Err(e) => Err(e.into()),
    }
}

/// Add `new` lines at the end of `path`, after the last line with text; returns the (0-based)
/// line the first of them is on.
fn append_lines(path: &Path, project: &str, new: &[String]) -> Wr<usize> {
    let (before, mut lines, eol) = read_for_append(path, project)?;
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.len() == 1 && lines[0].starts_with("# ") {
        lines.push(String::new());
    }
    let at = lines.len();
    lines.extend(new.iter().cloned());
    lines.push(String::new());
    if std::fs::read_to_string(path).unwrap_or_default() != before {
        return Err(WriteError::Conflict(format!("{} changed while the task was being written; try again", path.display())));
    }
    write_atomic(path, &lines.join(eol))?;
    Ok(at)
}

/// A new task, written with a new `🆔` at the end of `Projects/<project>/tasks.md`.
pub fn create(vault: &Path, project: &str, text: &str, due: Option<NaiveDate>, priority: Option<Priority>, goal: Option<&str>, today: NaiveDate) -> Wr<Task> {
    let _guard = WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let text = clean_text(text)?;
    let (file, rel) = project_file(vault, project)?;
    let mut items = vec![Item::Word(text)];
    if let Some(g) = goal {
        items.push(Item::Link(goal_link(vault, g)?));
    }
    if let Some(p) = priority {
        items.push(field(Kind::Priority(p), p.emoji(), None));
    }
    if let Some(d) = due {
        items.push(field(Kind::Due, "📅", Some(d.to_string())));
    }
    let id = new_id(vault);
    items.push(field(Kind::Id, "🆔", Some(id.clone())));
    let name = plain_name(project, "project")?;
    append_lines(&file, &name, &[line_of("- ", State::Open, &items)])?;
    let text = std::fs::read_to_string(&file)?;
    let (n, p) = scan_text(&text).into_iter().rev().find(|(_, p)| p.task.has_id && p.task.id == id).ok_or_else(|| WriteError::Io("the new task was not found after writing".into()))?;
    Ok(placed(p, &rel, &file, n, today))
}

/// The task `id`, as it is now.
pub fn find(vault: &Path, id: &str, today: NaiveDate) -> Wr<Task> {
    let place = locate(vault, id)?;
    let text = std::fs::read_to_string(&place.file)?;
    let (lines, _) = split_lines(&text);
    task_at(vault, &place.file, &lines, place.index, today)
}

/// Change a task's line in place (see the header). Returns the task as written, which has a new
/// id when it moved to another project (and a synthetic one when it has no `🆔`).
pub fn patch(vault: &Path, id: &str, rev: Option<&str>, patch: &Patch, today: NaiveDate) -> Wr<Task> {
    let _guard = WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let place = locate(vault, id)?;
    let moving = match &patch.project {
        Some(p) => {
            let (dest, dest_rel) = project_file(vault, p)?;
            (dest_rel != place.rel).then_some((dest, dest_rel, plain_name(p, "project")?))
        }
        None => None,
    };
    let mut destination_written = false;
    let result = edit(&place, rev, |lines, parsed| {
        let (state, items) = patched(vault, parsed, patch, today)?;
        let new = line_of(&parsed.head, state, &items);
        match &moving {
            None => {
                lines[place.index] = new;
                Ok(None)
            }
            Some((dest, _, name)) => {
                // The line and the notes under it, written to the other file first: a failure
                // in between leaves the task twice, never lost.
                let notes = continuation(lines, place.index);
                let mut moved = vec![new];
                moved.extend(lines[place.index + 1..=place.index + notes].iter().cloned());
                let at = append_lines(dest, name, &moved)?;
                destination_written = true;
                lines.drain(place.index..=place.index + notes);
                Ok(Some(at))
            }
        }
    });
    let moved_to = result.map_err(|err| {
        if destination_written {
            WriteError::Conflict(format!("move may have left a duplicate in the destination; inspect both files before retrying: {err:?}"))
        } else {
            err
        }
    })?;
    let (file, index) = match (&moving, moved_to) {
        (Some((dest, _, _)), Some(at)) => (dest.clone(), at),
        _ => (place.file.clone(), place.index),
    };
    let text = std::fs::read_to_string(&file)?;
    let (lines, _) = split_lines(&text);
    task_at(vault, &file, &lines, index, today)
}

/// Remove a task's line (and the notes under it).
pub fn delete(vault: &Path, id: &str, rev: Option<&str>) -> Wr<()> {
    let _guard = WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let place = locate(vault, id)?;
    edit(&place, rev, |lines, _| {
        let notes = continuation(lines, place.index);
        lines.drain(place.index..=place.index + notes);
        Ok(())
    })
}

/// The task `id` with a `🆔` (written to its line when it had none: the id changes from the
/// synthetic one to the new `🆔`).
pub fn give_id(vault: &Path, id: &str, rev: &str, today: NaiveDate) -> Wr<Task> {
    let _guard = WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let place = locate(vault, id)?;
    let new = new_id(vault);
    edit(&place, Some(rev), |lines, parsed| {
        if parsed.task.has_id {
            return Ok(());
        }
        let mut items = parsed.items.clone();
        items.push(field(Kind::Id, "🆔", Some(new.clone())));
        lines[place.index] = line_of(&parsed.head, parsed.state, &items);
        Ok(())
    })?;
    let text = std::fs::read_to_string(&place.file)?;
    let (lines, _) = split_lines(&text);
    task_at(vault, &place.file, &lines, place.index, today)
}

// ---------- what the editor lists ----------

/// A note's frontmatter values (`key: value` lines, quotes removed) and its first `# heading`.
fn note_facts(text: &str) -> (std::collections::HashMap<String, String>, Option<String>) {
    let mut front = std::collections::HashMap::new();
    let mut lines = text.lines();
    let mut heading = None;
    if lines.clone().next().is_some_and(|l| l.trim_end() == "---") {
        lines.next();
        for l in lines.by_ref() {
            if l.trim_end() == "---" {
                break;
            }
            if let Some((k, v)) = l.split_once(':').filter(|(k, _)| !k.starts_with([' ', '\t'])) {
                front.insert(k.trim().to_string(), v.trim().trim_matches(['"', '\'']).to_string());
            }
        }
    }
    for l in lines {
        if let Some(h) = l.strip_prefix("# ") {
            heading = Some(h.trim().to_string());
            break;
        }
    }
    (front, heading)
}

/// The projects (`Projects/<folder>/`, titled by the project note `<folder>.md`) and goals
/// (`Goals/*.md`, with their status) a task can name.
pub fn meta(vault: &Path) -> Value {
    let read = |p: PathBuf| std::fs::symlink_metadata(&p).ok().filter(|m| m.is_file()).and_then(|_| std::fs::read_to_string(p).ok());
    let mut projects = Vec::new();
    for e in std::fs::read_dir(vault.join("Projects")).into_iter().flatten().flatten() {
        let id = e.file_name().to_string_lossy().to_string();
        if id.starts_with('.') || !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let title = read(e.path().join(format!("{id}.md")))
            .and_then(|t| {
                let (front, heading) = note_facts(&t);
                front.get("title").cloned().filter(|t| !t.is_empty()).or(heading)
            })
            .unwrap_or_else(|| id.clone());
        projects.push(json!({ "id": id, "title": title }));
    }
    projects.sort_by_key(|p| p["title"].as_str().unwrap_or("").to_lowercase());
    let mut goals = Vec::new();
    for e in std::fs::read_dir(vault.join("Goals")).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(id) = name.strip_suffix(".md").filter(|i| !i.starts_with('.')) else { continue };
        let Some(text) = read(e.path()) else { continue };
        let (front, heading) = note_facts(&text);
        let title = front.get("title").cloned().filter(|t| !t.is_empty()).or(heading).unwrap_or_else(|| id.to_string());
        let status = front.get("status").cloned().filter(|s| !s.is_empty()).unwrap_or_else(|| "active".into());
        goals.push(json!({ "id": id, "title": title, "status": status }));
    }
    goals.sort_by_key(|g| g["title"].as_str().unwrap_or("").to_lowercase());
    json!({ "projects": projects, "goals": goals })
}

// ---------- handing a task to an agent ----------

/// The title of the session a task is handed to.
pub fn agent_title(t: &Task) -> String {
    let text: String = t.text.chars().take(60).collect();
    format!("task: {}", if text.is_empty() { t.id.clone() } else { text })
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
    s.push_str(&format!("- Task line: {}:{} (🆔 {})\n\n", t.file.display(), t.line, if t.has_id { t.id.as_str() } else { "none" }));
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
        assert_eq!((t.due, t.id.as_str(), t.priority), (Some(d("2026-10-30")), "t7k2", None));
        let t = one("  * [ ] 🆔 a1 ⏳ 2026-11-02 Pay the bill 🔺 📅2026-11-01");
        assert_eq!((t.text.as_str(), t.id.as_str(), t.scheduled, t.due, t.priority), ("Pay the bill", "a1", Some(d("2026-11-02")), Some(d("2026-11-01")), Some(Priority::Urgent)));
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
        assert_eq!((t.text.as_str(), t.due, t.id.as_str()), ("Ring soon", None, "Mom"));
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
        let tasks = parse_file("Projects/p/tasks.md", Path::new("p/tasks.md"), text, d("2026-10-28"));
        assert_eq!(tasks.iter().map(|t| (t.text.as_str(), t.line, t.overdue)).collect::<Vec<_>>(), [("one", 2, false), ("nested", 7, true)]);
    }

    #[test]
    fn sorts_by_project_priority_then_due() {
        let today = d("2026-10-28");
        let a = "- [ ] a-none-due 📅 2026-10-29\n- [ ] a-park ⏬ 📅 2026-10-01\n- [ ] a-imp-nodue ⏫\n- [ ] a-imp-due ⏫ 📅 2026-11-30\n- [ ] a-low 🔽\n";
        let b = "- [ ] b-urgent 🔺 📅 2026-12-01\n- [ ] b-none\n";
        let c = "- [ ] c-park ⏬\n";
        let mut all = parse_file("Projects/a/tasks.md", Path::new("a"), a, today);
        all.extend(parse_file("Projects/c/tasks.md", Path::new("c"), c, today));
        all.extend(parse_file("Projects/b/tasks.md", Path::new("b"), b, today));
        let got: Vec<(String, Vec<String>)> = sort(all).into_iter().map(|p| (p.project, p.tasks.into_iter().map(|t| t.text).collect())).collect();
        assert_eq!(got, [
            ("b".to_string(), vec!["b-urgent".to_string(), "b-none".into()]),
            ("a".into(), vec!["a-imp-due".into(), "a-imp-nodue".into(), "a-low".into(), "a-none-due".into(), "a-park".into()]),
            ("c".into(), vec!["c-park".into()]),
        ]);
        // Same rank: the earlier due date leads.
        let x = parse_file("Projects/x/tasks.md", Path::new("x"), "- [ ] x ⏫ 📅 2026-11-02", today);
        let y = parse_file("Projects/y/tasks.md", Path::new("y"), "- [ ] y ⏫ 📅 2026-11-01", today);
        assert_eq!(sort([x, y].concat()).iter().map(|p| p.project.as_str()).collect::<Vec<_>>(), ["y", "x"]);
    }

    #[test]
    fn reads_each_projects_tasks_file_and_nothing_else() {
        let vault = crate::test_util::TestDir::new("tasks");
        let dir = vault.join("Projects");
        for (p, body) in [("self", "- [ ] mine 📅 2026-10-27 🆔 s1\n- [x] old"), ("empty", "# empty tasks\n"), (".hidden", "- [ ] no")] {
            std::fs::create_dir_all(dir.join(p)).unwrap();
            std::fs::write(dir.join(p).join("tasks.md"), body).unwrap();
        }
        std::fs::write(dir.join("loose.md"), "- [ ] not in a project").unwrap();
        std::fs::create_dir_all(dir.join("linked")).unwrap();
        std::os::unix::fs::symlink(dir.join("self/tasks.md"), dir.join("linked/tasks.md")).unwrap();
        let before = std::fs::read_to_string(dir.join("self/tasks.md")).unwrap();
        let (projects, problems) = read(&dir, d("2026-10-28"), None);
        assert!(problems.is_empty());
        assert_eq!(projects.len(), 1);
        let t = &projects[0].tasks[0];
        assert_eq!((t.project.as_str(), t.text.as_str(), t.line, t.overdue), ("self", "mine", 1, true));
        assert_eq!((t.file.clone(), t.id.as_str(), t.rev.len()), (dir.join("self/tasks.md"), "s1", 8));
        assert_eq!(std::fs::read_to_string(dir.join("self/tasks.md")).unwrap(), before, "never written");
        assert!(read(&dir.join("missing"), d("2026-10-28"), None).0.is_empty());
    }

    #[test]
    fn the_brief_carries_the_task_and_its_context() {
        let t = parse_file("Projects/garden/tasks.md", Path::new("/v/Projects/garden/tasks.md"), "- [ ] Draft the seed order [[Goal - First harvest]] ⏫ 📅 2026-10-27 🆔 t7k2\n", d("2026-10-28")).remove(0);
        let b = brief(&t);
        for want in ["> Draft the seed order", "- Project: garden", "garden/garden.md", "- Goal: [[Goal - First harvest]]", "- Priority: important", "- Due: 2026-10-27 (overdue)", "tasks.md:1 (🆔 t7k2)", "Don't edit the task line"] {
            assert!(b.contains(want), "{want} missing from:\n{b}");
        }
        assert_eq!(agent_title(&t), "task: Draft the seed order");
    }

    // ---------- states, ids, rewriting ----------

    #[test]
    fn done_and_dropped_lines_are_read_too() {
        let p = parse_full("- [x] Install it 🆔 a91c ✅ 2026-10-28").unwrap();
        assert_eq!((p.state, p.task.done, p.task.done_on, p.task.id.as_str(), p.task.text.as_str()), (State::Done, true, Some(d("2026-10-28")), "a91c", "Install it"));
        let p = parse_full("\t- [X] Upper 🆔 b2 ✅2026-10-29").unwrap();
        assert_eq!((p.state, p.task.done_on), (State::Done, Some(d("2026-10-29"))));
        let p = parse_full("- [-] Dropped 🆔 s3 ❌ 2026-10-27 (why)").unwrap();
        assert_eq!((p.state, p.task.done, p.task.text.as_str()), (State::Dropped, false, "Dropped (why)"));
        assert!(parse_full("- [/] half").is_none() && parse_full("- [x]glued").is_none());
    }

    #[test]
    fn a_task_without_an_id_gets_the_synthetic_one() {
        let text = "# t\n\n- [ ] first\n- [ ] second 🆔 k9\n```\n- [ ] fenced\n```\n- [ ] third\n";
        let tasks = parse_file("Projects/p/tasks.md", Path::new("/v/Projects/p/tasks.md"), text, d("2026-11-19"));
        assert_eq!(tasks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["L:Projects/p/tasks.md:3", "k9", "L:Projects/p/tasks.md:8"]);
        assert!(!tasks[0].has_id && tasks[1].has_id);
        assert_eq!(tasks[0].project, "p");
    }

    #[test]
    fn a_goal_is_named_like_its_note() {
        let t = one("- [ ] Do it [[Goals/Rebuild a sustainable athletic life|Rebuild]] [[Other]]");
        assert_eq!((t.goal.as_deref(), t.goals.len()), (Some("Rebuild a sustainable athletic life"), 2));
        assert_eq!(one("- [ ] Plain").goal, None);
    }

    #[test]
    fn rewriting_keeps_what_the_line_says() {
        // The items join back to the line (single spaces, selectors gone), whatever they hold.
        for body in ["Pay ⏫ 📅 2026-10-30 🆔 a1", "Read [[A|b]] notes #x 🔁 every week on Monday ⛔ x1,x2 ➕ 2026-10-01", "🆔 z ⏳ 2026-11-02 Words [[G]] 🔺"] {
            assert_eq!(render(&lex(body)), body, "{body}");
        }
        assert_eq!(render(&lex("Glued 📅2026-10-30")), "Glued 📅2026-10-30");
        assert_eq!(render(&lex("Spaced   out\t[[a b]]")), "Spaced out [[a b]]");
    }

    /// A vault with `Projects/<p>/tasks.md` files, notes and goals, for the writing tests.
    fn vault(files: &[(&str, &str)]) -> crate::test_util::TestDir {
        let v = crate::test_util::TestDir::new("vault");
        for (rel, body) in files {
            let p = v.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        v
    }

    fn text(v: &Path, rel: &str) -> String {
        std::fs::read_to_string(v.join(rel)).unwrap()
    }

    const TODAY: &str = "2026-11-19";

    fn patch_with(v: &Path, id: &str, p: Patch) -> Wr<Task> {
        patch(v, id, None, &p, d(TODAY))
    }

    #[test]
    fn ticking_writes_the_done_date_and_unticking_removes_it() {
        let v = vault(&[("Projects/p/tasks.md", "# p tasks\n\n- [ ] Pay the bill ⏫ 📅 2026-11-20 🆔 a1\n- [ ] Other 🆔 b2\n")]);
        let t = patch_with(&v, "a1", Patch { done: Some(true), ..Default::default() }).unwrap();
        assert_eq!((t.done, t.done_on, t.state), (true, Some(d(TODAY)), State::Done));
        assert_eq!(text(&v, "Projects/p/tasks.md"), "# p tasks\n\n- [x] Pay the bill ⏫ 📅 2026-11-20 🆔 a1 ✅ 2026-11-19\n- [ ] Other 🆔 b2\n");
        // Ticking again changes nothing (the first date stays).
        let again = patch(&v, "a1", None, &Patch { done: Some(true), ..Default::default() }, d("2026-12-01")).unwrap();
        assert_eq!(again.done_on, Some(d(TODAY)));
        let t = patch_with(&v, "a1", Patch { done: Some(false), ..Default::default() }).unwrap();
        assert_eq!((t.done, t.done_on), (false, None));
        assert_eq!(text(&v, "Projects/p/tasks.md"), "# p tasks\n\n- [ ] Pay the bill ⏫ 📅 2026-11-20 🆔 a1\n- [ ] Other 🆔 b2\n");
    }

    #[test]
    fn dropping_writes_the_dash_and_the_date() {
        let v = vault(&[("Projects/p/tasks.md", "- [ ] Nope 🆔 a1\n")]);
        patch_with(&v, "a1", Patch { dropped: Some(true), ..Default::default() }).unwrap();
        assert_eq!(text(&v, "Projects/p/tasks.md"), "- [-] Nope 🆔 a1 ❌ 2026-11-19\n");
        patch_with(&v, "a1", Patch { dropped: Some(false), ..Default::default() }).unwrap();
        assert_eq!(text(&v, "Projects/p/tasks.md"), "- [ ] Nope 🆔 a1\n");
        assert!(matches!(patch_with(&v, "a1", Patch { done: Some(true), dropped: Some(true), ..Default::default() }), Err(WriteError::Invalid(_))));
        // A done task can be dropped: the done mark goes.
        patch_with(&v, "a1", Patch { done: Some(true), ..Default::default() }).unwrap();
        patch_with(&v, "a1", Patch { dropped: Some(true), ..Default::default() }).unwrap();
        assert_eq!(text(&v, "Projects/p/tasks.md"), "- [-] Nope 🆔 a1 ❌ 2026-11-19\n");
    }

    #[test]
    fn text_due_and_priority_change_in_place_and_other_fields_stay() {
        let v = vault(&[("Projects/p/tasks.md", "- [ ] Read notes #reading 🔼 🔁 every week ⛔ x1 📅 2026-11-20 🆔 a1\n  - a note under it\n- [ ] Next 🆔 b2\n")]);
        let t = patch_with(&v, "a1", Patch { text: Some(" Read  the notes ".into()), priority: Some(Some(Priority::Urgent)), due: Some(Some(d("2026-12-01"))), ..Default::default() }).unwrap();
        assert_eq!((t.text.as_str(), t.priority, t.due), ("Read the notes", Some(Priority::Urgent), Some(d("2026-12-01"))));
        assert_eq!(text(&v, "Projects/p/tasks.md"), "- [ ] Read the notes 🔁 every week ⛔ x1 🔺 📅 2026-12-01 🆔 a1\n  - a note under it\n- [ ] Next 🆔 b2\n");
        let t = patch_with(&v, "a1", Patch { priority: Some(None), due: Some(None), ..Default::default() }).unwrap();
        assert_eq!((t.priority, t.due), (None, None));
        assert_eq!(text(&v, "Projects/p/tasks.md"), "- [ ] Read the notes 🔁 every week ⛔ x1 🆔 a1\n  - a note under it\n- [ ] Next 🆔 b2\n");
        for bad in ["", "   ", "Buy 📅 tomorrow", "done ✅"] {
            assert!(matches!(patch_with(&v, "b2", Patch { text: Some(bad.into()), ..Default::default() }), Err(WriteError::Invalid(_))), "{bad:?}");
        }
        // A line break can't start another line: it becomes a space.
        patch_with(&v, "b2", Patch { text: Some("Next\n- [ ] injected".into()), ..Default::default() }).unwrap();
        assert_eq!(text(&v, "Projects/p/tasks.md").lines().count(), 3);
        assert_eq!(priority_from_name("none").unwrap(), None);
        assert!(priority_from_name("medium").is_err() && priority_from_name("high").is_err());
    }

    #[test]
    fn a_goal_link_is_added_replaced_and_removed() {
        let v = vault(&[
            ("Projects/p/tasks.md", "- [ ] Read [[Old goal|the]] book [[Goals/A]] 🆔 a1\n- [ ] Plain ⏫ 🆔 b2\n"),
            ("Goals/Grow.md", "---\nstatus: active\n---\n# Grow\n"),
            ("Goals/Retired one.md", "---\ntitle: \"Retired\"\nstatus: dropped\n---\n"),
        ]);
        let t = patch_with(&v, "b2", Patch { goal: Some(Some("Grow".into())), ..Default::default() }).unwrap();
        assert_eq!((t.goal.as_deref(), t.goals.as_slice()), (Some("Grow"), ["Goals/Grow".to_string()].as_slice()));
        assert_eq!(text(&v, "Projects/p/tasks.md").lines().nth(1).unwrap(), "- [ ] Plain [[Goals/Grow]] ⏫ 🆔 b2");
        // By title too; an unknown goal is refused and nothing is written.
        patch_with(&v, "b2", Patch { goal: Some(Some("Retired".into())), ..Default::default() }).unwrap();
        assert!(text(&v, "Projects/p/tasks.md").contains("Plain [[Goals/Retired one]] ⏫"));
        let before = text(&v, "Projects/p/tasks.md");
        assert!(matches!(patch_with(&v, "b2", Patch { goal: Some(Some("Nope".into())), ..Default::default() }), Err(WriteError::NotFound(_))));
        assert!(matches!(patch_with(&v, "b2", Patch { goal: Some(Some("../x".into())), ..Default::default() }), Err(WriteError::Invalid(_))));
        assert_eq!(text(&v, "Projects/p/tasks.md"), before);
        // Removing keeps the sentence's words; trailing links go.
        let t = patch_with(&v, "a1", Patch { goal: Some(None), ..Default::default() }).unwrap();
        assert!(t.goals.is_empty());
        assert_eq!(text(&v, "Projects/p/tasks.md").lines().next().unwrap(), "- [ ] Read the book 🆔 a1");
        // Changing the text keeps the goal.
        patch_with(&v, "b2", Patch { text: Some("Plainer".into()), ..Default::default() }).unwrap();
        assert!(text(&v, "Projects/p/tasks.md").contains("- [ ] Plainer [[Goals/Retired one]] ⏫ 🆔 b2"));
    }

    #[test]
    fn a_project_change_moves_the_line_and_its_notes() {
        let v = vault(&[
            ("Projects/p/tasks.md", "# p tasks\n\n- [ ] Move me 🆔 a1\n  - note one\n  continued\n- [ ] Stay 🆔 b2\n"),
            ("Projects/q/tasks.md", "# q tasks\n\n- [ ] Existing 🆔 c3\n\n"),
            ("Projects/r/r.md", "# r\n"),
        ]);
        let t = patch_with(&v, "a1", Patch { project: Some("q".into()), due: Some(Some(d("2026-12-24"))), ..Default::default() }).unwrap();
        assert_eq!((t.project.as_str(), t.id.as_str(), t.line), ("q", "a1", 4));
        assert_eq!(text(&v, "Projects/p/tasks.md"), "# p tasks\n\n- [ ] Stay 🆔 b2\n");
        assert_eq!(text(&v, "Projects/q/tasks.md"), "# q tasks\n\n- [ ] Existing 🆔 c3\n- [ ] Move me 📅 2026-12-24 🆔 a1\n  - note one\n  continued\n");
        // To a project with no tasks file yet: created with its heading.
        let t = patch_with(&v, "b2", Patch { project: Some("r".into()), ..Default::default() }).unwrap();
        assert_eq!((t.project.as_str(), t.line), ("r", 3));
        assert_eq!(text(&v, "Projects/r/tasks.md"), "# r tasks\n\n- [ ] Stay 🆔 b2\n");
        // Unknown or unsafe projects are refused; the same project is no move.
        assert!(matches!(patch_with(&v, "c3", Patch { project: Some("nope".into()), ..Default::default() }), Err(WriteError::NotFound(_))));
        assert!(matches!(patch_with(&v, "c3", Patch { project: Some("../Goals".into()), ..Default::default() }), Err(WriteError::Invalid(_))));
        let t = patch_with(&v, "c3", Patch { project: Some("q".into()), ..Default::default() }).unwrap();
        assert_eq!(t.line, 3);
    }

    #[test]
    fn a_line_that_changed_is_a_conflict() {
        let v = vault(&[("Projects/p/tasks.md", "- [ ] One 🆔 a1\n- [ ] Two\n- [ ] Three\n")]);
        let before = text(&v, "Projects/p/tasks.md");
        // The synthetic id points at a line: shift the file and it is no longer a task there.
        let id = "L:Projects/p/tasks.md:2";
        let t = find(&v, id, d(TODAY)).unwrap();
        assert_eq!((t.text.as_str(), t.id.as_str()), ("Two", id));
        std::fs::write(v.join("Projects/p/tasks.md"), "# new heading\n- [ ] One 🆔 a1\n- [ ] Two\n").unwrap();
        // Line 2 is a task again (another one): the rev says so.
        assert!(matches!(patch(&v, id, Some(&t.rev), &Patch { done: Some(true), ..Default::default() }, d(TODAY)), Err(WriteError::Conflict(_))));
        std::fs::write(v.join("Projects/p/tasks.md"), "- [ ] One 🆔 a1\n\nnot a task\n").unwrap();
        assert!(matches!(patch_with(&v, "L:Projects/p/tasks.md:3", Patch { done: Some(true), ..Default::default() }), Err(WriteError::Conflict(_))));
        assert!(matches!(patch_with(&v, "L:Projects/p/tasks.md:9", Patch { done: Some(true), ..Default::default() }), Err(WriteError::Conflict(_))));
        assert!(matches!(delete(&v, "L:Projects/p/tasks.md:3", None), Err(WriteError::Conflict(_))));
        assert!(matches!(patch_with(&v, "zz9", Patch::default()), Err(WriteError::NotFound(_))));
        assert_ne!(text(&v, "Projects/p/tasks.md"), before);
        assert_eq!(text(&v, "Projects/p/tasks.md"), "- [ ] One 🆔 a1\n\nnot a task\n", "never written");
    }

    #[test]
    fn a_synthetic_id_ticks_the_line_and_the_note_can_be_any_vault_note() {
        let v = vault(&[("Projects/p/tasks.md", "# p\n- [ ] No id here\n"), ("Daily/2026-11-19.md", "## Plan\r\n- [ ] From a daily note\r\n- [ ] Another\r\n")]);
        let t = patch_with(&v, "L:Projects/p/tasks.md:2", Patch { done: Some(true), ..Default::default() }).unwrap();
        assert_eq!((t.id.as_str(), t.done), ("L:Projects/p/tasks.md:2", true));
        assert_eq!(text(&v, "Projects/p/tasks.md"), "# p\n- [x] No id here ✅ 2026-11-19\n");
        // A note that isn't a project's: edited in place, its CRLF endings kept.
        patch_with(&v, "L:Daily/2026-11-19.md:2", Patch { done: Some(true), ..Default::default() }).unwrap();
        assert_eq!(text(&v, "Daily/2026-11-19.md"), "## Plan\r\n- [x] From a daily note ✅ 2026-11-19\r\n- [ ] Another\r\n");
    }

    #[test]
    fn only_notes_inside_the_vault_can_be_written() {
        let v = vault(&[("Projects/p/tasks.md", "- [ ] ok\n"), (".obsidian/x.md", "- [ ] hidden\n"), ("notes.txt", "- [ ] text\n")]);
        let outside = crate::test_util::TestDir::new("outside");
        std::fs::write(outside.join("o.md"), "- [ ] outside\n").unwrap();
        std::os::unix::fs::symlink(outside.join("o.md"), v.join("link.md")).unwrap();
        std::os::unix::fs::symlink(&*outside, v.join("dir")).unwrap();
        for id in ["L:../x.md:1", "L:/etc/passwd.md:1", "L:.obsidian/x.md:1", "L:notes.txt:1", "L:link.md:1", "L:dir/o.md:1", "L:Projects/p/../../x.md:1", "L:Projects/p/tasks.md:0", "L:nothing", "L::1"] {
            let e = patch_with(&v, id, Patch { done: Some(true), ..Default::default() }).unwrap_err();
            assert!(matches!(e, WriteError::Invalid(_) | WriteError::NotFound(_)), "{id}: {e:?}");
        }
        assert_eq!(text(&outside, "o.md"), "- [ ] outside\n");
        assert_eq!(text(&v, "Projects/p/tasks.md"), "- [ ] ok\n");
    }

    #[test]
    fn a_new_task_gets_a_fresh_id_and_the_project_file_is_made() {
        let v = vault(&[("Projects/p/p.md", "---\nstatus: active\n---\n# p\n"), ("Projects/q/tasks.md", "# q tasks\n\n- [ ] Old 🆔 abcdef\n  - sub\n"), ("Goals/Grow.md", "# Grow\n")]);
        let t = create(&v, "p", "  Call the  bank ", Some(d("2026-11-30")), Some(Priority::Important), Some("Grow"), d(TODAY)).unwrap();
        assert!(t.has_id && t.id.len() == 6 && t.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()), "{}", t.id);
        assert_eq!((t.project.as_str(), t.text.as_str(), t.due, t.priority, t.goal.as_deref(), t.line), ("p", "Call the bank", Some(d("2026-11-30")), Some(Priority::Important), Some("Grow"), 3));
        assert_eq!(text(&v, "Projects/p/tasks.md"), format!("# p tasks\n\n- [ ] Call the bank [[Goals/Grow]] ⏫ 📅 2026-11-30 🆔 {}\n", t.id));
        // Appended after the last line of an existing file, notes and all; never an id in use.
        let u = create(&v, "q", "Second", None, None, None, d(TODAY)).unwrap();
        assert_ne!(u.id, "abcdef");
        assert_eq!(text(&v, "Projects/q/tasks.md"), format!("# q tasks\n\n- [ ] Old 🆔 abcdef\n  - sub\n- [ ] Second 🆔 {}\n", u.id));
        let ids: HashSet<String> = (0..40).map(|i| create(&v, "p", &format!("n{i}"), None, None, None, d(TODAY)).unwrap().id).collect();
        assert_eq!(ids.len(), 40);
        assert!(matches!(create(&v, "nope", "x", None, None, None, d(TODAY)), Err(WriteError::NotFound(_))));
        assert!(matches!(create(&v, "../x", "x", None, None, None, d(TODAY)), Err(WriteError::Invalid(_))));
        assert!(matches!(create(&v, "p", "  ", None, None, None, d(TODAY)), Err(WriteError::Invalid(_))));
        assert!(matches!(create(&v, "p", "x", None, None, Some("Missing"), d(TODAY)), Err(WriteError::NotFound(_))));
    }

    #[test]
    fn deleting_takes_the_line_and_the_notes_under_it() {
        let v = vault(&[("Projects/p/tasks.md", "# p\n- [ ] Gone 🆔 a1\n  - note\n- [ ] Kept 🆔 b2\n    - [ ] Nested task 🆔 c3\n")]);
        delete(&v, "a1", None).unwrap();
        assert_eq!(text(&v, "Projects/p/tasks.md"), "# p\n- [ ] Kept 🆔 b2\n    - [ ] Nested task 🆔 c3\n");
        // Another task nested under it stays a task of its own.
        delete(&v, "b2", None).unwrap();
        assert_eq!(text(&v, "Projects/p/tasks.md"), "# p\n    - [ ] Nested task 🆔 c3\n");
        let rev = find(&v, "c3", d(TODAY)).unwrap().rev;
        assert!(matches!(delete(&v, "c3", Some("00000000")), Err(WriteError::Conflict(_))));
        delete(&v, "c3", Some(&rev)).unwrap();
        assert!(matches!(delete(&v, "c3", None), Err(WriteError::NotFound(_))));
    }

    #[test]
    fn handing_a_task_without_an_id_writes_one() {
        let v = vault(&[("Projects/p/tasks.md", "- [ ] Bare ⏫\n- [ ] Has 🆔 k1\n")]);
        let t = give_id(&v, "L:Projects/p/tasks.md:1", &find(&v, "L:Projects/p/tasks.md:1", d(TODAY)).unwrap().rev, d(TODAY)).unwrap();
        assert!(t.has_id && t.id.len() == 6 && t.priority == Some(Priority::Important));
        assert_eq!(text(&v, "Projects/p/tasks.md"), format!("- [ ] Bare ⏫ 🆔 {}\n- [ ] Has 🆔 k1\n", t.id));
        assert_eq!(give_id(&v, "k1", &find(&v, "k1", d(TODAY)).unwrap().rev, d(TODAY)).unwrap().id, "k1");
        assert_eq!(text(&v, "Projects/p/tasks.md").lines().nth(1), Some("- [ ] Has 🆔 k1"));
    }

    #[test]
    fn stale_synthetic_handoff_never_assigns_id_to_new_occupant() {
        let v = vault(&[("Projects/p/tasks.md", "- [ ] First\n- [ ] Second\n")]);
        let id = "L:Projects/p/tasks.md:1";
        let rev = find(&v, id, d(TODAY)).unwrap().rev;
        std::fs::write(v.join("Projects/p/tasks.md"), "- [ ] Second\n").unwrap();
        assert!(matches!(give_id(&v, id, &rev, d(TODAY)), Err(WriteError::Conflict(_))));
        assert_eq!(text(&v, "Projects/p/tasks.md"), "- [ ] Second\n");
    }

    #[test]
    fn done_tasks_are_listed_for_a_window_and_after_the_open_ones() {
        let v = vault(&[("Projects/p/tasks.md", "- [x] Old ✅ 2026-11-01 🆔 o1\n- [x] Recent ✅ 2026-11-17 🆔 r1\n- [x] Today ✅ 2026-11-19 🆔 t1\n- [x] No date 🆔 n1\n- [-] Dropped ❌ 2026-11-18 🆔 d1\n- [ ] Open 🔽 🆔 p1\n"), ("Projects/q/tasks.md", "- [x] Only done ✅ 2026-11-18 🆔 q1\n")]);
        let ids = |ps: &[Project]| ps.iter().map(|p| (p.project.clone(), p.tasks.iter().map(|t| t.id.clone()).collect::<Vec<_>>())).collect::<Vec<_>>();
        let (open, _) = read(&v.join("Projects"), d(TODAY), None);
        assert_eq!(ids(&open), [("p".to_string(), vec!["p1".to_string()])]);
        let (with, _) = read(&v.join("Projects"), d(TODAY), Some(d("2026-11-12")));
        assert_eq!(ids(&with), [("p".to_string(), vec!["p1".to_string(), "t1".into(), "r1".into()]), ("q".into(), vec!["q1".into()])]);
        assert!(with[0].tasks[1].done && with[0].tasks[1].done_on == Some(d(TODAY)) && !with[0].tasks[0].done);
        assert_eq!((parse_days("7d"), parse_days("0d"), parse_days("7"), parse_days("x")), (Some(7), None, None, None));
    }

    #[test]
    fn the_editor_lists_projects_and_goals() {
        let v = vault(&[
            ("Projects/zenbot/zenbot.md", "---\nstatus: active\n---\n# zenbot\n"),
            ("Projects/garden/garden.md", "---\ntitle: \"The garden\"\n---\n"),
            ("Projects/bare/tasks.md", ""),
            ("Projects/.hidden/x.md", ""),
            ("Goals/Zenfinance to R$2M ARR.md", "---\nstatus: active\n---\n# Zenfinance to R$2M ARR\n"),
            ("Goals/Old one.md", "---\nstatus: done\n---\n"),
            ("Goals/notes.txt", "x"),
        ]);
        let m = meta(&v);
        assert_eq!(m["projects"], json!([{ "id": "bare", "title": "bare" }, { "id": "garden", "title": "The garden" }, { "id": "zenbot", "title": "zenbot" }]));
        assert_eq!(m["goals"], json!([{ "id": "Old one", "title": "Old one", "status": "done" }, { "id": "Zenfinance to R$2M ARR", "title": "Zenfinance to R$2M ARR", "status": "active" }]));
        assert_eq!(meta(&v.join("nothing")), json!({ "projects": [], "goals": [] }));
    }
}
