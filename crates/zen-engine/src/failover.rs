//! Safe, subscription-only continuation when a CLI reports a hard usage limit.

use serde_json::{json, Value};

use crate::{claude, codex};
use crate::turn::{TurnCtx, TurnInput};

pub fn enabled() -> bool {
    !matches!(
        std::env::var("ZEN_QUOTA_FAILOVER").as_deref(),
        Ok("0" | "false" | "off")
    )
}

/// Only explicit exhausted-usage messages qualify. Throttles, overloads, and generic 429s do not.
pub fn is_hard_usage_limit(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    if error.contains("temporarily") || error.contains("rate limit") {
        return false;
    }
    [
        "you've hit your session limit",
        "you have hit your session limit",
        "you've hit your weekly limit",
        "you have hit your weekly limit",
        "you've hit your opus limit",
        "you've hit your sonnet limit",
        "you've reached your usage limit",
        "you have reached your usage limit",
        "you've hit your usage limit",
        "you have hit your usage limit",
        "you've hit your monthly spend limit",
        "you've hit your org's monthly spend limit",
        "you've hit your individual spend limit",
        "you've hit your team's shared budget",
        "you've hit your individual usage limit",
    ]
    .iter()
    .any(|phrase| error.contains(phrase))
}

pub fn target(primary: &str) -> Option<String> {
    let (key, default) = match primary {
        "claude" => ("ZEN_FAILOVER_CLAUDE_TO_CODEX", "codex/gpt-6.1-sol"),
        "codex" => ("ZEN_FAILOVER_CODEX_TO_CLAUDE", "claude/claude-sonnet-5-5"),
        _ => return None,
    };
    Some(std::env::var(key).unwrap_or_else(|_| default.to_string()))
}

/// Require the signed-in consumer subscription: failover must never silently incur API charges.
pub async fn subscription_login(engine: &str) -> bool {
    // The CLIs may prefer an API key over their saved subscription login. Do not switch to a
    // billable API path, even when the subscription itself is signed in.
    if match engine {
        "claude" => std::env::var_os("ANTHROPIC_API_KEY").is_some() || std::env::var_os("CLAUDE_CODE_OAUTH_TOKEN").is_some(),
        "codex" => std::env::var_os("OPENAI_API_KEY").is_some() || std::env::var_os("CODEX_ACCESS_TOKEN").is_some(),
        _ => true,
    } {
        return false;
    }
    let engine = engine.to_string();
    tokio::time::timeout(std::time::Duration::from_secs(5), tokio::task::spawn_blocking(move || match engine.as_str() {
        "claude" => {
            let Ok(out) = std::process::Command::new("claude")
                .args(["auth", "status", "--json"])
                .output()
            else {
                return false;
            };
            if !out.status.success() {
                return false;
            }
            serde_json::from_slice::<Value>(&out.stdout).is_ok_and(|v| {
                v["loggedIn"] == true && v["authMethod"] == "claude.ai"
            })
        }
        "codex" => {
            let Ok(out) = std::process::Command::new("codex")
                .args(["login", "status"])
                .output()
            else {
                return false;
            };
            out.status.success()
                && [out.stdout.as_slice(), out.stderr.as_slice()].iter().any(|bytes| {
                    String::from_utf8_lossy(bytes).to_ascii_lowercase().contains("logged in using chatgpt")
                })
        }
        _ => false,
    }))
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or(false)
}

pub async fn available_target(primary: &str) -> Option<(String, Value)> {
    let wanted = target(primary)?;
    let (engine, _) = wanted.split_once('/')?;
    if engine == primary || !matches!(engine, "claude" | "codex") {
        return None;
    }
    let models = match engine {
        "claude" => claude::models(),
        "codex" => codex::models().await,
        _ => return None,
    };
    models
        .into_iter()
        .find(|m| m["id"] == wanted)
        .map(|m| (wanted, m))
}

pub async fn continuation_input(
    input: &TurnInput,
    target: &str,
    model_info: &Value,
    ctx: &TurnCtx,
) -> Option<TurnInput> {
    let (history, partial) = ctx.captured_transcript().await?;
    let mut next = input.clone();
    let mut original = json!({ "role": "user", "content": input.prompt });
    if let Some(context) = &input.context {
        original["context"] = json!(context);
    }
    next.history.push(original);
    next.history.extend(history);
    if !partial.trim().is_empty() {
        next.history.push(json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": format!(
                "[Partial response from the interrupted provider; this was not a completed answer.]\n{partial}"
            ) }]
        }));
    }
    next.prompt = "Continue the original request from the conversation above. The previous provider stopped because its subscription usage limit was reached. Treat recorded tool calls and their results as completed work: do not repeat completed actions. If a tool call has no recorded result, inspect the current state before retrying it. Finish and verify the original request.".into();
    next.context = None;
    next.resume = None;
    next.model = target.split_once('/').map(|(_, model)| model)?.to_string();
    next.effort = if model_info["efforts"]
        .as_array()
        .is_some_and(|efforts| efforts.iter().any(|effort| effort.as_str() == input.effort.as_deref()))
    {
        input.effort.clone()
    } else {
        model_info["default_effort"].as_str().map(String::from)
    };
    Some(next)
}

pub fn engine_for(model: &str) -> Option<&str> {
    let (engine, _) = model.split_once('/')?;
    matches!(engine, "claude" | "codex").then_some(engine)
}

pub async fn notify_switch(ctx: &TurnCtx, from: &str, to: &str, reason: &str) {
    ctx.notify(
        "turn.fallback",
        json!({ "from": from, "to": to, "reason": reason }),
    )
    .await;
}

pub async fn run(engine: &str, ctx: TurnCtx, input: &TurnInput, abort: tokio::sync::watch::Receiver<bool>) -> anyhow::Result<Option<String>> {
    match engine {
        "claude" => claude::run_turn(ctx, input, abort).await,
        "codex" => codex::run_turn(ctx, input, abort).await,
        _ => Ok(Some(format!("zen-engine has no `{engine}` engine"))),
    }
}

/// Turn a consumer plan quota failure into one bounded cross-provider attempt.
pub async fn try_continue(
    primary: &str,
    input: &TurnInput,
    ctx: &TurnCtx,
    abort: tokio::sync::watch::Receiver<bool>,
    error: &str,
) -> Option<Option<String>> {
    if !enabled() || !is_hard_usage_limit(error) || *abort.borrow() {
        return None;
    }
    if !subscription_login(primary).await {
        eprintln!("[engine] failover skipped: primary is not using a subscription login");
        return None;
    }
    let Some((model_id, info)) = available_target(primary).await else {
        eprintln!("[engine] failover skipped: target model is unavailable");
        return None;
    };
    let fallback_engine = engine_for(&model_id)?;
    if !subscription_login(fallback_engine).await {
        eprintln!("[engine] failover skipped: target is not using a subscription login");
        return None;
    }
    let Some(next) = continuation_input(input, &model_id, &info, ctx).await else {
        eprintln!("[engine] failover skipped: the current-turn transcript is not safe to replay");
        return None;
    };
    notify_switch(ctx, primary, &model_id, error).await;
    Some(
        run(fallback_engine, ctx.clone(), &next, abort)
            .await
            .unwrap_or_else(|e| Some(format!("fallback on {model_id} failed: {e:#}"))),
    )
}

pub async fn publish_usage(ctx: &TurnCtx) {
    let report = ctx.combined_usage().await;
    if let Some(mut report) = report {
        report["session_id"] = json!(ctx.session_id);
        report["turn_id"] = json!(ctx.turn_id);
        ctx.rpc.notify("turn.usage", report).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_hard_quota_errors_trigger_failover() {
        for text in ["You've hit your session limit", "You've hit your weekly limit", "You've reached your usage limit", "You've hit your org's monthly spend limit · run /usage-credits to raise it"] {
            assert!(is_hard_usage_limit(text), "{text}");
        }
        for text in ["Request rejected (429)", "rate limit exceeded", "Server is temporarily limiting requests", "context window exceeded", "authentication failed", "overloaded", "quota exceeded", "usage limit reached"] {
            assert!(!is_hard_usage_limit(text), "{text}");
        }
    }

    #[test]
    fn default_failover_targets_are_on_the_other_provider() {
        assert_eq!(target("claude").as_deref(), Some("codex/gpt-6.1-sol"));
        assert_eq!(target("codex").as_deref(), Some("claude/claude-sonnet-5-5"));
        assert_eq!(target("faux"), None);
    }
}
