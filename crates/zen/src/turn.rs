//! One turn from a script or a plain terminal: `zen ask` and the line-by-line `zen chat`, which
//! stream a turn over the session's WebSocket and report what it did.

use std::io::{IsTerminal, Read, Write};

use anyhow::{bail, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use zen_proto::text_of;

use crate::client::{assistant_text, dim, record_total, short, tool_summary, usage_total, Client, NewSession, Ws};
use crate::print::{for_stderr, for_stdout};

/// Outcome of one turn, collected from the session stream.
#[derive(Default)]
pub(crate) struct Turn {
    pub(crate) text: String,
    pub(crate) tools: Vec<Value>,
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) cost: f64,
    pub(crate) model: String,
    pub(crate) effort: Option<String>,
    /// The kernel's record of the turn (harness, engine, totals), sent with its end, plus the
    /// records of child turns it ran (a verifier's).
    pub(crate) record: Value,
    /// Each turn's record, in order (verifier turns included).
    pub(crate) records: Vec<Value>,
    pub(crate) error: Option<String>,
}

/// Add a turn's record to the totals of a chain of turns. A cache break counts if any turn had an
/// unexpected one (`history`, `miss`).
pub(crate) fn add_record(total: &mut Value, r: &Value) {
    if total.is_null() {
        *total = r.clone();
        return;
    }
    for k in ["duration_ms", "model_calls", "tool_calls", "tool_errors", "input_tokens", "output_tokens", "cache_read", "cache_write"] {
        total[k] = json!(total[k].as_i64().unwrap_or(0) + r[k].as_i64().unwrap_or(0));
    }
    total["cost_usd"] = json!(total["cost_usd"].as_f64().unwrap_or(0.0) + r["cost_usd"].as_f64().unwrap_or(0.0));
    let unexpected = |v: &Value| matches!(v.as_str(), Some("history" | "miss"));
    if !unexpected(&total["cache_break"]) && unexpected(&r["cache_break"]) {
        total["cache_break"] = r["cache_break"].clone();
    }
    total["context_tokens"] = r["context_tokens"].clone();
}

/// Send a prompt and stream the turn. `show` prints live text to stdout and tools to stderr.
pub(crate) async fn run_turn(ws: &mut Ws, prompt: &str, show: bool, show_tools: bool) -> Result<Turn> {
    ws.send(Message::text(json!({ "type": "prompt", "text": prompt }).to_string())).await?;
    let mut turn = Turn::default();
    let mut started = false;
    let mut streamed = false;
    let mut stopping = false;
    let mut stdout = std::io::stdout();
    loop {
        tokio::select! {
            msg = ws.next() => {
                let Some(msg) = msg else { bail!("connection to zenbot closed") };
                let Message::Text(text) = msg? else { continue };
                let ev: Value = serde_json::from_str(&text)?;
                match ev["type"].as_str().unwrap_or("") {
                    "message" => {
                        let m = &ev["message"];
                        match m["role"].as_str() {
                            Some("user") if text_of(&m["content"]) == prompt => started = true,
                            Some("assistant") if started => {
                                let text = assistant_text(m);
                                if !text.is_empty() {
                                    if !turn.text.is_empty() { turn.text.push_str("\n\n"); }
                                    turn.text.push_str(&text);
                                    if show && !streamed { print!("{}", for_stdout(&text)); }
                                    if show { println!(); }
                                }
                                streamed = false;
                                let u = &m["usage"];
                                let output = u["output"].as_i64().unwrap_or(0);
                                turn.input_tokens += usage_total(u) - output;
                                turn.output_tokens += output;
                                turn.cost += u["cost"]["total"].as_f64().unwrap_or(0.0);
                                turn.model = m["model"].as_str().unwrap_or("").to_string();
                                if m["stopReason"] == "error" {
                                    turn.error = m["errorMessage"].as_str().map(str::to_string);
                                }
                            }
                            _ => {}
                        }
                    }
                    "delta" if started && show => {
                        print!("{}", for_stdout(ev["delta"].as_str().unwrap_or("")));
                        stdout.flush().ok();
                        streamed = true;
                    }
                    "tool_start" if started => {
                        if show_tools { eprintln!("{}", dim(&format!("  ▸ {}", tool_summary(ev["name"].as_str().unwrap_or(""), &ev["args"])))); }
                        turn.tools.push(json!({ "id": ev["call_id"], "name": ev["name"], "args": ev["args"] }));
                    }
                    "tool_end" if started => {
                        if let Some(t) = turn.tools.iter_mut().find(|t| t["id"] == ev["call_id"]) {
                            t["is_error"] = ev["is_error"].clone();
                            t["ms"] = ev["ms"].clone();
                        }
                        if show_tools && ev["is_error"] == true { eprintln!("{}", dim("    (failed)")); }
                    }
                    // The turn starts with the first `busy` after the prompt was sent, or with the
                    // prompt's echo, whichever comes first (the echo may not match exactly).
                    "busy" => {
                        started = true;
                        turn.effort = ev["effort"].as_str().map(str::to_string);
                    }
                    "end" | "child_end" if started => {
                        if let Some(e) = ev["error"].as_str() { turn.error = Some(e.to_string()); }
                        let r = &ev["turn"];
                        if r.is_object() {
                            // The kernel's totals cover the whole turn, side calls included.
                            turn.records.push(r.clone());
                            add_record(&mut turn.record, r);
                            let t = &turn.record;
                            turn.output_tokens = t["output_tokens"].as_i64().unwrap_or(0);
                            turn.input_tokens = record_total(t) - turn.output_tokens;
                            turn.cost = t["cost_usd"].as_f64().unwrap_or(turn.cost);
                        }
                        // A turn may be followed by another the kernel starts itself (`next`): wait for `idle`.
                        if ev["type"] == "end" && ev["next"] != true {
                            return Ok(turn);
                        }
                    }
                    "idle" if started => return Ok(turn),
                    "questions" if started => {
                        let qs: Vec<String> = ev["questions"].as_array().into_iter().flatten().map(|q| {
                            let opts: Vec<&str> = q["options"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                            format!("{}\n  {}", q["question"].as_str().unwrap_or(""), opts.join(" / "))
                        }).collect();
                        let text = qs.join("\n");
                        if show { println!("\n── Questions\n{}\n", for_stdout(&text)); }
                        if !turn.text.is_empty() { turn.text.push_str("\n\n"); }
                        turn.text.push_str(&format!("Questions:\n{text}"));
                    }
                    "status" if started && show_tools => {
                        eprintln!("{}", dim(&format!("  · {}", ev["text"].as_str().unwrap_or(""))));
                    }
                    "resync" if started && ev["busy"] == false => {
                        turn.error.get_or_insert_with(|| "missed the end of the turn (client fell behind); see `zen sessions show`".into());
                        return Ok(turn);
                    }
                    "error" => bail!("{}", ev["error"].as_str().unwrap_or("error")),
                    _ => {}
                }
            }
            // The first Ctrl-C stops the turn; a second one leaves without waiting for it to end
            // (the kernel still stops it). 130 is a shell's exit code for an interrupt.
            _ = tokio::signal::ctrl_c() => {
                if stopping {
                    eprintln!("{}", dim("  (left before the turn ended)"));
                    std::process::exit(130);
                }
                stopping = true;
                ws.send(Message::text(json!({ "type": "abort" }).to_string())).await.ok();
                eprintln!("{}", dim("  (stopping… Ctrl-C again to leave now)"));
            }
        }
    }
}

pub(crate) fn turn_json(session: &str, t: &Turn) -> Value {
    json!({
        "session_id": session,
        "text": t.text,
        "model": t.model,
        "effort": t.effort,
        "turn": t.record,
        "turns": t.records,
        "tools": t.tools,
        "usage": { "input_tokens": t.input_tokens, "output_tokens": t.output_tokens, "cost_usd_api_equivalent": t.cost },
        "error": t.error,
    })
}

/// Read piped stdin. With a prompt argument, stdin is optional: if nothing arrives quickly
/// (e.g. an open but idle pipe), carry on without it instead of blocking.
pub(crate) fn read_stdin(optional: bool) -> String {
    if std::io::stdin().is_terminal() {
        return String::new();
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::stdin().read_to_string(&mut s);
        let _ = tx.send(s);
    });
    if optional {
        rx.recv_timeout(std::time::Duration::from_millis(300)).unwrap_or_default()
    } else {
        rx.recv().unwrap_or_default()
    }
}

pub(crate) fn read_prompt(prompt: Option<String>) -> Result<String> {
    let needs_stdin = matches!(prompt.as_deref(), None | Some("-"));
    let piped = read_stdin(!needs_stdin);
    let text = match prompt.as_deref() {
        None | Some("-") => piped,
        Some(p) if piped.trim().is_empty() => p.to_string(),
        Some(p) => format!("{p}\n\n{piped}"),
    };
    if text.trim().is_empty() {
        bail!("empty prompt: pass it as an argument or on stdin");
    }
    Ok(text)
}

pub(crate) async fn ask(c: &Client, json_out: bool, prompt: Option<String>, session: Option<String>, new: NewSession, quiet: bool) -> Result<()> {
    let text = read_prompt(prompt)?;
    let id = match session {
        Some(s) => c.resolve(&s).await?,
        None => c.new_session(new.body()).await?,
    };
    let mut ws = c.connect(&id).await?;
    let turn = run_turn(&mut ws, &text, !json_out, !json_out && !quiet).await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&turn_json(&id, &turn))?);
    } else if !quiet {
        let effort = turn.effort.as_deref().map(|e| format!(" · {e}")).unwrap_or_default();
        eprintln!("{}", dim(&format!("  {}{effort} · {} tokens · session {}", turn.model, turn.input_tokens + turn.output_tokens, short(&id))));
    }
    if let Some(e) = turn.error {
        if !json_out {
            eprintln!("error: {}", for_stderr(&e));
        }
        std::process::exit(1);
    }
    Ok(())
}

pub(crate) async fn chat(c: &Client, session: Option<String>, new: NewSession) -> Result<()> {
    let id = match session {
        Some(s) => c.resolve(&s).await?,
        None => c.new_session(new.body()).await?,
    };
    let info = c.get(&format!("/api/sessions/{id}")).await?;
    eprintln!("{}", dim(&format!("zenbot · {} · session {} · Ctrl-C stops a turn, Ctrl-D exits", info["model"].as_str().unwrap_or(""), short(&id))));
    let mut ws = c.connect(&id).await?;
    let stdin = std::io::stdin();
    loop {
        eprint!("\n› ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            eprintln!();
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "/exit" || line == "/quit" {
            break;
        }
        println!();
        let turn = run_turn(&mut ws, line, true, true).await?;
        if let Some(e) = turn.error {
            eprintln!("error: {}", for_stderr(&e));
        }
    }
    eprintln!("{}", dim(&format!("session {id}")));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// A fake kernel session stream: reads the prompt, then sends `events`.
    async fn fake_stream(events: Vec<Value>) -> (Client, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
            let prompt = ws.next().await.unwrap().unwrap().into_text().unwrap().to_string();
            for e in events {
                ws.send(Message::text(e.to_string())).await.unwrap();
            }
            prompt
        });
        (Client::new(&url, Some("t".into())).unwrap(), server)
    }

    #[tokio::test]
    async fn ask_starts_on_busy_even_if_the_echo_differs() {
        let (c, server) = fake_stream(vec![
            json!({ "type": "busy", "busy": true, "effort": "high" }),
            json!({ "type": "message", "message": { "role": "user", "content": "do it" } }),
            json!({ "type": "message", "message": { "role": "assistant", "content": [{ "type": "text", "text": "done" }], "model": "faux/smoke" } }),
            json!({ "type": "end", "error": null }),
        ])
        .await;
        let mut ws = c.connect("s1").await.unwrap();
        let turn = tokio::time::timeout(std::time::Duration::from_secs(5), run_turn(&mut ws, "  do it  ", false, false)).await.expect("no hang").unwrap();
        assert_eq!((turn.text.as_str(), turn.effort.as_deref()), ("done", Some("high")));
        assert!(server.await.unwrap().contains("  do it  "));
    }
}
