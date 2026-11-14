//! Safe, subscription-only continuation when a CLI reports a hard usage limit.

use serde_json::{json, Value};
use zen_proto::worker::{Model, TurnEvent};

use crate::engine::{self, Engine};
use crate::turn::{TurnCtx, TurnInput};

pub fn enabled() -> bool {
    !matches!(
        std::env::var("ZEN_QUOTA_FAILOVER").as_deref(),
        Ok("0" | "false" | "off")
    )
}

pub use zen_proto::worker::is_hard_usage_limit;

/// The engine a turn continues on after a hard usage limit on `primary`, and that model's entry:
/// `primary`'s `failover_target`, if it names another subscription engine that lists the model.
pub async fn available_target(primary: &dyn Engine) -> Option<(&'static dyn Engine, Model)> {
    let wanted = primary.failover_target()?;
    let (engine, _) = engine::for_model(&wanted)?;
    if engine.name() == primary.name() || !engine.subscription() {
        return None;
    }
    let model = engine.models().await.into_iter().find(|m| m.id == wanted)?;
    Some((engine, model))
}

/// Whether `engine` is signed in with the owner's subscription (`Engine::subscription_login`),
/// within 5 s: failover must never silently incur API charges.
pub async fn subscription_login(engine: &'static dyn Engine) -> bool {
    tokio::time::timeout(std::time::Duration::from_secs(5), tokio::task::spawn_blocking(move || engine.subscription_login()))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(false)
}

pub async fn continuation_input(input: &TurnInput, model_info: &Model, ctx: &TurnCtx) -> Option<TurnInput> {
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
    next.model = model_info.id.split_once('/').map(|(_, model)| model)?.to_string();
    next.effort = if model_info.efforts.iter().any(|effort| Some(effort.as_str()) == input.effort.as_deref()) {
        input.effort.clone()
    } else {
        model_info.default_effort.clone()
    };
    Some(next)
}

/// Turn a consumer plan quota failure into one bounded cross-provider attempt.
pub async fn try_continue(
    primary: &'static dyn Engine,
    input: &TurnInput,
    ctx: &TurnCtx,
    abort: tokio::sync::watch::Receiver<bool>,
    error: &str,
) -> Option<Option<String>> {
    if !enabled() || !is_hard_usage_limit(error) || *abort.borrow() {
        return None;
    }
    if !primary.subscription() || !subscription_login(primary).await {
        eprintln!("[engine] failover skipped: primary is not using a subscription login");
        return None;
    }
    let Some((fallback, info)) = available_target(primary).await else {
        eprintln!("[engine] failover skipped: target model is unavailable");
        return None;
    };
    if !subscription_login(fallback).await {
        eprintln!("[engine] failover skipped: target is not using a subscription login");
        return None;
    }
    let Some(next) = continuation_input(input, &info, ctx).await else {
        eprintln!("[engine] failover skipped: the current-turn transcript is not safe to replay");
        return None;
    };
    ctx.notify(TurnEvent::Fallback { from: primary.name().into(), to: info.id.clone(), reason: error.into() }).await;
    Some(
        fallback
            .run_turn(ctx.clone(), &next, abort)
            .await
            .unwrap_or_else(|e| Some(format!("fallback on {} failed: {e:#}", info.id))),
    )
}

/// Send the turn's one usage report (several providers' reports combined after a failover).
pub async fn publish_usage(ctx: &TurnCtx) {
    if let Some(Value::Object(report)) = ctx.combined_usage().await {
        ctx.send(&TurnEvent::Usage(report)).await;
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
        assert_eq!(crate::claude::Claude.failover_target().as_deref(), Some("codex/gpt-6.1-sol"));
        assert_eq!(crate::codex::Codex.failover_target().as_deref(), Some("claude/claude-sonnet-5-5"));
        assert_eq!(crate::faux::Faux.failover_target(), None);
        assert!(!crate::faux::Faux.subscription(), "never a failover target");
    }
}
