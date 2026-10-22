//! API-equivalent prices for engines whose CLI reports no cost (Codex on a subscription, D-075),
//! so cost reports, evals and the router's grading (D-074) compare engines on one basis. Claude
//! Code reports its own API-equivalent cost; this is the same idea for the rest, an estimate.
//!
//! USD per million tokens: uncached input, cached input, output. The defaults are OpenAI's
//! standard-tier list prices for prompts up to 272K tokens, as published by aipricing.guru
//! (verified 2026-10-22) and api.foo (copied from OpenAI's page 2026-10-15); openai.com itself
//! refuses automated reads. `~/.zenbot/prices.toml` overrides them, a table per model id without
//! its engine prefix (`[gpt-6.1-sol]` with `input`, `cached`, `output`).

use std::collections::HashMap;

/// (model, input, cached input, output), USD per million tokens.
const DEFAULTS: &[(&str, f64, f64, f64)] = &[
    ("gpt-6.1-sol", 2.00, 0.10, 10.00),
    ("gpt-6-astra", 10.00, 1.00, 50.00),
    ("gpt-6-sol", 2.00, 0.20, 10.00),
    ("gpt-6-luna", 0.10, 0.01, 0.50),
    ("gpt-5.6-sol", 4.00, 0.40, 20.00),
    ("gpt-5.6-terra", 2.00, 0.20, 12.00),
    ("gpt-5.6-luna", 0.20, 0.02, 1.20),
];

#[derive(Clone, Copy, Debug, PartialEq)]
struct Price {
    input: f64,
    cached: f64,
    output: f64,
}

/// The owner's overrides, read when they change.
fn overrides() -> HashMap<String, Price> {
    let Ok(text) = std::fs::read_to_string(crate::zen_home().join("prices.toml")) else { return HashMap::new() };
    let Ok(table) = text.parse::<toml::Table>() else {
        tracing::warn!("prices.toml can't be read as TOML; using the built-in prices");
        return HashMap::new();
    };
    table
        .iter()
        .filter_map(|(model, v)| {
            let f = |k: &str| v.get(k).and_then(|x| x.as_float().or_else(|| x.as_integer().map(|i| i as f64)));
            Some((model.clone(), Price { input: f("input")?, cached: f("cached")?, output: f("output")? }))
        })
        .collect()
}

fn price(model: &str) -> Option<Price> {
    let id = model.rsplit('/').next().unwrap_or(model);
    if let Some(p) = overrides().get(id) {
        return Some(*p);
    }
    DEFAULTS.iter().find(|(m, ..)| *m == id).map(|&(_, input, cached, output)| Price { input, cached, output })
}

/// The API-equivalent cost of these tokens on `model` (uncached input, cached input, output);
/// None for a model without a price.
pub fn estimate(model: &str, input: i64, cache_read: i64, output: i64) -> Option<f64> {
    let p = price(model)?;
    Some((input.max(0) as f64 * p.input + cache_read.max(0) as f64 * p.cached + output.max(0) as f64 * p.output) / 1_000_000.0)
}

/// Turns recorded before D-075: Codex's input counted its cached tokens twice, a resumed turn
/// had the previous turn's usage subtracted, and the cost was $0. Rebuilt from each turn's own
/// stored report (`usage`), once per turn (marked `usage.repriced`).
pub async fn backfill(db: &sqlx::SqlitePool) -> anyhow::Result<u64> {
    let rows: Vec<(uuid::Uuid, Option<String>, Option<String>, serde_json::Value)> = sqlx::query_as(
        "SELECT id, model_resolved, model, usage FROM turns WHERE engine = 'codex' AND usage IS NOT NULL AND usage->>'repriced' IS NULL AND usage->>'totals' IS NULL",
    )
    .fetch_all(db)
    .await?;
    let mut n = 0;
    for (id, resolved, model, usage) in rows {
        let int = |k: &str| usage[k].as_i64().unwrap_or(0);
        let (cache_read, output) = (int("cache_read"), int("output"));
        // Before D-075 the worker reported input with the cached tokens in it.
        let input = (int("input") - cache_read).max(0);
        let model = resolved.or(model).unwrap_or_default();
        let cost = estimate(&model, input, cache_read, output);
        sqlx::query("UPDATE turns SET input_tokens = $2, cache_read = $3, output_tokens = $4, cost_usd = COALESCE($5, cost_usd), usage = json_patch(usage, '{\"repriced\": true}') WHERE id = $1")
            .bind(id)
            .bind(input)
            .bind(cache_read)
            .bind(output)
            .bind(cost)
            .execute(db)
            .await?;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sqlite_backfill_preserves_usage_and_runs_once() {
        let (_dir,db)=crate::test_util::database().await;
        let session=uuid::Uuid::new_v4();
        let turn=uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO sessions(id,model) VALUES($1,'codex/gpt-6.1-sol')").bind(session).execute(&db).await.unwrap();
        let usage=serde_json::json!({"input":120,"cache_read":20,"output":10,"extra":"preserved"});
        sqlx::query("INSERT INTO turns(id,session_id,harness,worker,engine,model,usage) VALUES($1,$2,'test','test','codex','codex/gpt-6.1-sol',$3)")
            .bind(turn).bind(session).bind(usage).execute(&db).await.unwrap();
        assert_eq!(backfill(&db).await.unwrap(),1);
        assert_eq!(backfill(&db).await.unwrap(),0);
        let (input,cached,output,cost,usage):(i64,i64,i64,f64,serde_json::Value)=sqlx::query_as("SELECT input_tokens,cache_read,output_tokens,cost_usd,usage FROM turns WHERE id=$1")
            .bind(turn).fetch_one(&db).await.unwrap();
        assert_eq!((input,cached,output),(100,20,10));
        assert!(cost>0.0);
        assert_eq!(usage["repriced"],true);
        assert_eq!(usage["extra"],"preserved");
    }

    #[test]
    fn codex_tokens_are_priced_per_million_with_cached_input_cheap() {
        let c = estimate("codex/gpt-6.1-sol", 40_000, 1_000_000, 5_000).unwrap();
        assert!((c - (0.08 + 0.10 + 0.05)).abs() < 1e-9, "{c}");
        assert_eq!(estimate("claude/claude-opus-5-5", 1, 1, 1), None, "a model without a price has no estimate");
    }
}
