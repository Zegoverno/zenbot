//! Newline-delimited stdio MCP bridge. Stdout is protocol only; credentials stay in Client's
//! Authorization header. The kernel owns agent scope, write policy, tool execution and auditing.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use crate::client::{enc, Client};

pub(crate) fn options(args: &[String]) -> Result<(String, bool)> {
    let mut agent = None;
    let mut write = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--agent" if agent.is_none() => {
                i += 1;
                agent = args.get(i).filter(|s| !s.starts_with('-')).cloned();
                if agent.is_none() { bail!("serve requires --agent <name>"); }
            }
            "--write" if !write => write = true,
            other => bail!("unknown serve option: {other}"),
        }
        i += 1;
    }
    Ok((agent.context("serve requires explicit --agent <name>")?, write))
}

pub(crate) async fn serve(c: &Client, args: &[String]) -> Result<()> {
    let (agent, write) = options(args)?;
    let session = c.post("/api/shared-brain/sessions", json!({"agent":agent,"write":write})).await?;
    let id = session["id"].as_str().context("kernel returned no shared-brain session")?;
    let base = format!("/api/shared-brain/sessions/{}/tools", enc(id));
    let mut input = BufReader::new(tokio::io::stdin());
    let mut output = tokio::io::stdout();
    let mut initialized = false;
    let mut ready = false;
    loop {
        // Bound each frame without letting an untrusted client allocate unbounded memory.
        let mut bytes = Vec::new();
        let count = tokio::io::AsyncReadExt::take(&mut input, 1_048_577).read_until(b'\n', &mut bytes).await?;
        if count == 0 { break; }
        if bytes.len() > 1_048_576 { bail!("MCP frame exceeds 1 MiB"); }
        let response = match serde_json::from_slice::<Value>(&bytes) {
            Err(_) => Some(error(Value::Null, -32700, "parse error")),
            Ok(v) => {
                let id = v.get("id").cloned();
                let method = v["method"].as_str().unwrap_or("");
                if v["jsonrpc"] != "2.0" || !v.is_object() || method.is_empty() {
                    Some(error(id.unwrap_or(Value::Null), -32600, "invalid request"))
                } else if id.is_none() {
                    if method == "notifications/initialized" && initialized { ready = true; }
                    None
                } else {
                    let id = id.unwrap();
                    let result = match method {
                        "initialize" if !initialized => {
                            initialized = true;
                            Ok(json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"zen-shared-brain","version":env!("CARGO_PKG_VERSION")}}))
                        }
                        "ping" => Ok(json!({})),
                        "tools/list" if ready => c.get(&base).await,
                        "tools/call" if ready => {
                            // MCP reserves _meta (e.g. progressToken). Never forward arbitrary
                            // client fields to the kernel; only the two allowlisted inputs.
                            let p = &v["params"];
                            c.post(&base, json!({"name":p["name"],"arguments":p.get("arguments").cloned().unwrap_or_else(|| json!({}))})).await
                        }
                        "tools/list" | "tools/call" => {
                            let r = error(id, -32002, "initialize and notifications/initialized required");
                            output.write_all(format!("{r}\n").as_bytes()).await?;
                            output.flush().await?;
                            continue;
                        }
                        _ => {
                            let r = error(id, -32601, "method not found");
                            output.write_all(format!("{r}\n").as_bytes()).await?;
                            output.flush().await?;
                            continue;
                        }
                    };
                    Some(match result {
                        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                        Err(_) => error(id, -32603, "kernel request failed (see kernel logs)"),
                    })
                }
            }
        };
        if let Some(response) = response {
            output.write_all(format!("{response}\n").as_bytes()).await?;
            output.flush().await?;
        }
    }
    Ok(())
}

fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_mcp_flags_and_explicit_serve_scope_parse() {
        use clap::Parser;
        let old = crate::cli::Cli::try_parse_from(["zen","mcp","--json"]).unwrap();
        assert!(old.json);
        let serve = crate::cli::Cli::try_parse_from(["zen","mcp","serve","--agent","alice","--write"]).unwrap();
        match serve.cmd.unwrap() {
            crate::cli::Cmd::Mcp { args, agent, write } => {
                assert_eq!(args, ["serve"]);
                assert_eq!(agent.as_deref(), Some("alice"));
                assert!(write);
            }
            _ => panic!("wrong command"),
        }
    }
    #[test]
    fn explicit_agent_and_write_opt_in() {
        let args = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        assert!(options(&[]).is_err());
        assert_eq!(options(&args("--agent zenbot")).unwrap(), ("zenbot".into(),false));
        assert_eq!(options(&args("--write --agent zenbot")).unwrap(), ("zenbot".into(),true));
        for s in ["--agent", "--agent --write", "--agent zenbot --shell", "--agent zenbot --write --write"] {
            assert!(options(&args(s)).is_err(), "{s}");
        }
    }
}
