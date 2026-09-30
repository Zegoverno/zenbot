//! Built-in tools. The kernel is the only place side effects happen.
//! v0: commands run directly on the host inside the workspace; sandboxes come in M2.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;

const MAX_OUTPUT: usize = 50 * 1024;
const DEFAULT_READ_LINES: usize = 2000;

pub fn specs() -> Value {
    json!([
        {
            "name": "bash",
            "description": "Run a shell command with bash in the workspace directory. Returns combined stdout/stderr and the exit code. Use for listing files, searching (rg, grep), git, builds, tests and running programs.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command to run" },
                    "timeout_secs": { "type": "integer", "description": "Timeout in seconds (default 120, max 600)" }
                },
                "required": ["command"]
            }
        },
        {
            "name": "read",
            "description": "Read a text file. Returns numbered lines. Paths are relative to the workspace unless absolute.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path" },
                    "offset": { "type": "integer", "description": "1-based line to start from" },
                    "limit": { "type": "integer", "description": "Maximum number of lines (default 2000)" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "write",
            "description": "Create or overwrite a file with the given content. Creates parent directories.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path" },
                    "content": { "type": "string", "description": "Full file content" }
                },
                "required": ["path", "content"]
            }
        },
        {
            "name": "edit",
            "description": "Replace an exact string in a file. old_text must match exactly and be unique unless replace_all is true. Read the file first.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path" },
                    "old_text": { "type": "string", "description": "Exact text to replace" },
                    "new_text": { "type": "string", "description": "Replacement text" },
                    "replace_all": { "type": "boolean", "description": "Replace every occurrence" }
                },
                "required": ["path", "old_text", "new_text"]
            }
        },
        {
            "name": "move",
            "description": "Move or rename a file or directory. Creates parent directories of the destination.",
            "parameters": {
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "Source path" },
                    "to": { "type": "string", "description": "Destination path" }
                },
                "required": ["from", "to"]
            }
        }
    ])
}

pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

fn ok(content: impl Into<String>) -> ToolOutput {
    ToolOutput { content: content.into(), is_error: false }
}

fn err(content: impl Into<String>) -> ToolOutput {
    ToolOutput { content: content.into(), is_error: true }
}

fn resolve(workspace: &Path, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolOutput> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| err(format!("missing string argument `{key}`")))
}

/// Keep the head and tail of oversized output so errors at the end stay visible.
fn truncate(s: String) -> String {
    if s.len() <= MAX_OUTPUT {
        return s;
    }
    let half = MAX_OUTPUT / 2;
    let mut head_end = half;
    while !s.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = s.len() - half;
    while !s.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!(
        "{}\n\n[... {} bytes truncated ...]\n\n{}",
        &s[..head_end],
        tail_start - head_end,
        &s[tail_start..]
    )
}

pub async fn execute(workspace: &Path, name: &str, args: &Value) -> ToolOutput {
    let result = match name {
        "bash" => bash(workspace, args).await,
        "read" => read(workspace, args).await,
        "write" => write(workspace, args).await,
        "edit" => edit(workspace, args).await,
        "move" => move_path(workspace, args).await,
        _ => Err(err(format!("unknown tool `{name}`"))),
    };
    result.unwrap_or_else(|e| e)
}

async fn bash(workspace: &Path, args: &Value) -> Result<ToolOutput, ToolOutput> {
    let command = str_arg(args, "command")?;
    let timeout = args.get("timeout_secs").and_then(Value::as_u64).unwrap_or(120).clamp(1, 600);
    let child = Command::new("bash")
        .arg("-lc")
        .arg(command)
        .current_dir(workspace)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = match tokio::time::timeout(Duration::from_secs(timeout), child).await {
        Err(_) => return Err(err(format!("command timed out after {timeout}s"))),
        Ok(Err(e)) => return Err(err(format!("failed to start bash: {e}"))),
        Ok(Ok(out)) => out,
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&stderr);
    }
    let code = out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
    let body = format!("{}\n[exit code: {code}]", truncate(text));
    Ok(if out.status.success() { ok(body) } else { err(body) })
}

async fn read(workspace: &Path, args: &Value) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(workspace, str_arg(args, "path")?);
    let text = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| err(format!("cannot read {}: {e}", path.display())))?;
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    let limit = args.get("limit").and_then(Value::as_u64).map(|l| l as usize).unwrap_or(DEFAULT_READ_LINES);
    let total = text.lines().count();
    let mut out = String::new();
    for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        out.push_str(&format!("{:>6}\t{}\n", i + 1, line));
    }
    let shown_end = (offset - 1 + limit).min(total);
    if shown_end < total {
        out.push_str(&format!("[showing lines {offset}-{shown_end} of {total}; use offset to read more]\n"));
    }
    if total == 0 {
        out.push_str("[empty file]\n");
    }
    Ok(ok(truncate(out)))
}

async fn write(workspace: &Path, args: &Value) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(workspace, str_arg(args, "path")?);
    let content = str_arg(args, "content")?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| err(format!("cannot create {}: {e}", parent.display())))?;
    }
    tokio::fs::write(&path, content).await.map_err(|e| err(format!("cannot write {}: {e}", path.display())))?;
    Ok(ok(format!("wrote {} bytes to {}", content.len(), path.display())))
}

async fn edit(workspace: &Path, args: &Value) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(workspace, str_arg(args, "path")?);
    let old = str_arg(args, "old_text")?;
    let new = str_arg(args, "new_text")?;
    let all = args.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
    if old.is_empty() {
        return Err(err("old_text must not be empty"));
    }
    let text = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| err(format!("cannot read {}: {e}", path.display())))?;
    let count = text.matches(old).count();
    if count == 0 {
        return Err(err("old_text not found in file; read the file and copy the exact text"));
    }
    if count > 1 && !all {
        return Err(err(format!("old_text matches {count} times; add more context or set replace_all")));
    }
    let updated = if all { text.replace(old, new) } else { text.replacen(old, new, 1) };
    tokio::fs::write(&path, &updated).await.map_err(|e| err(format!("cannot write {}: {e}", path.display())))?;
    Ok(ok(format!("replaced {} occurrence(s) in {}", if all { count } else { 1 }, path.display())))
}

async fn move_path(workspace: &Path, args: &Value) -> Result<ToolOutput, ToolOutput> {
    let from = resolve(workspace, str_arg(args, "from")?);
    let to = resolve(workspace, str_arg(args, "to")?);
    if let Some(parent) = to.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| err(format!("cannot create {}: {e}", parent.display())))?;
    }
    tokio::fs::rename(&from, &to)
        .await
        .map_err(|e| err(format!("cannot move {} to {}: {e}", from.display(), to.display())))?;
    Ok(ok(format!("moved {} to {}", from.display(), to.display())))
}
