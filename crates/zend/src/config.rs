//! The kernel's settings.
//!
//! Every tunable `ZEN_*` variable is read here once, at start, into a typed [`Config`] with its
//! default next to it; modules read `config::get()`, never the environment, so a setting's name,
//! type and default live in one place. A value that doesn't parse falls back to the default (and is logged at start); a flag is
//! off with `0`, `off`, `false` or `no`, on otherwise.
//!
//! Not read here: the boot settings `main.rs` reads once anyway
//! (`ZEN_TOKEN`, `ZEN_PORT`, `ZEN_BIND`, `ZEN_WORKSPACE`, `ZEN_REPO`, `ZEN_DEFAULT_MODEL`,
//! `ZEN_HARNESS`), the paths (`ZEN_HOME`, `ZEN_SKILLS_DIR`, `ZEN_WIKI_DIR`, `ZEN_TOOLS_DIR`,
//! `ZEN_MCP_CONFIG`), the workers' (`ZEN_WORKERS`, `ZEN_ENGINE_CMD`, `ZEN_WORKER_<NAME>_CMD`,
//! `ZEN_MODELS`) and `ZEN_FAUX`. API keys stay in the environment: a secret has no place in a
//! struct that could be logged.

use std::sync::LazyLock;

/// The settings, read from the environment the first time they're needed (`main` does it at start).
static CONFIG: LazyLock<Config> = LazyLock::new(|| Config::read(|k| std::env::var(k).ok()));

/// The kernel's settings.
pub fn get() -> &'static Config {
    &CONFIG
}

/// Every tunable setting, typed, with its variable and default.
#[derive(Debug, Clone)]
pub struct Config {
    // ---- models ----
    /// `ZEN_SUMMARY_MODEL` (`claude/claude-sonnet-5-5`): writes session summaries (compact.rs).
    pub summary_model: String,
    /// `ZEN_ASSIST_MODEL` (`claude/claude-haiku-5-5`): names sessions and suggests the next prompt
    /// (assist.rs; measured 2026-10-09: about 2 s and $0.0006 a call, the cheapest and fastest
    /// model on the owner's plans).
    pub assist_model: String,
    /// `ZEN_EMBED_MODEL` (`openai/text-embedding-3-small`, must give 1536 dimensions): search embeddings.
    pub embed_model: String,
    /// `ZEN_EMBED_URL` (`https://openrouter.ai/api/v1`): OpenAI-compatible base for `/embeddings`.
    pub embed_url: String,
    /// `ZEN_EMBED` (on): off means no embeddings (search by names and full text only).
    pub embed: bool,
    /// `ZEN_S1_MODEL` (unset: off): the System One classifier.
    pub s1_model: Option<String>,
    /// `ZEN_S1_URL` (`https://openrouter.ai/api/v1/systemone`).
    pub s1_url: String,
    /// `ZEN_S1_PRIVATE` (on): whether System One and embeddings may see private content.
    pub s1_private: bool,
    /// `ZEN_S1_CONCURRENCY` (8, at least 1): System One calls in flight at once (s1/run.rs).
    pub s1_concurrency: usize,
    /// `ZEN_CACHE_TTL` (`claude=3600`): how long each engine's prompt cache lives, in seconds, as
    /// `engine=secs` pairs (`*=secs` for the rest; 300 when unnamed). Claude Code asks for one hour;
    /// Codex and others get the providers' five minutes (measure.rs).
    pub cache_ttl: Vec<(String, i64)>,

    // ---- instructions and context ----
    /// `ZEN_SOUL_CHARS` / `ZEN_IDENTITY_CHARS` / `ZEN_AGENTS_CHARS` / `ZEN_USER_CHARS`
    /// (4000 / 4000 / 12000 / 3000): each prompt file's size in the instructions (sizes from Hermes
    /// and OpenClaw).
    pub soul_chars: usize,
    pub identity_chars: usize,
    pub agents_chars: usize,
    pub user_chars: usize,
    /// `ZEN_SKILL_INDEX_CHARS` (2500): the skills index's budget; past it each skill keeps only its
    /// trigger (first clause), then only its name (skills.rs `index_within`).
    pub skill_index_chars: usize,
    /// `ZEN_JUDGE_TOOL` (on): off hides the `judge` tool (s1/run.rs).
    pub judge_tool: bool,

    // ---- compaction ----
    /// `ZEN_CONTEXT_TOKENS` (unset): a cap on the context budget, which is otherwise the model's
    /// window (200000 when the window isn't known).
    pub context_tokens: Option<i64>,
    /// `ZEN_COMPACT_SOFT` / `ZEN_COMPACT_HARD` / `ZEN_COMPACT_KEEP` (0.7 / 0.9 / 0.3): fractions of
    /// the budget: prepare a summary / apply it now / keep word for word.
    pub compact_soft: f64,
    pub compact_hard: f64,
    pub compact_keep: f64,
    /// `ZEN_COMPACT_IDLE_SECS` (300): the pause after which a prepared summary is applied.
    pub compact_idle_secs: i64,
    /// `ZEN_SUGGEST` (on): off means no suggested next prompts (names stay on).
    pub suggest: bool,

    // ---- memory, search, web ----
    /// `ZEN_MEMORY_CHARS` (4000, at least 200): short-term memory's size; twice it is the ceiling.
    pub memory_chars: usize,
    /// `ZEN_INDEX_SECS` (20, at least 1): the pause between search-index passes without a backlog.
    pub index_secs: u64,
    /// `ZEN_SEARCH_PROVIDER` (unset: Brave or Tavily when their key is set, else SearXNG).
    pub search_provider: Option<String>,
    /// `ZEN_SEARXNG_URL` (`http://127.0.0.1:8888`): the SearXNG web search uses; the installer runs
    /// one natively there (scripts/searxng.sh); empty turns it off.
    pub searxng_url: String,
    /// `ZEN_SEARCH_RERANK` (on): off means System One doesn't rerank web results.
    pub search_rerank: bool,

    // ---- jobs, delegation, scoring, turns, updates ----
    /// `ZEN_TZ` (the owner's zone, default `UTC`): a new agent job's default timezone.
    pub tz: String,
    /// `ZEN_JOBS` (on): off means the scheduler doesn't run (system jobs are still seeded).
    pub jobs: bool,
    /// `ZEN_JOBS_TICK` (60, between 0.2 and 60): the scheduler's longest sleep, in seconds.
    pub jobs_tick: f64,
    /// `ZEN_ENGINE_CONCURRENCY` (3, at least 1): engine processes background work (job runs,
    /// subagents, verifiers, kernel sessions) may run at once; the owner's never wait.
    pub engine_concurrency: usize,
    /// `ZEN_TURN_IDLE_SECS` (600): the watchdog aborts a turn quiet this long.
    pub turn_idle_secs: u64,
    /// `ZEN_TURN_ABORT_GRACE_SECS` (30): then ends it in the kernel this long after the abort.
    pub turn_abort_grace_secs: u64,
    /// `ZEN_UPDATE_CHECK_SECS` (3600; 0 off): how often to compare with `origin/main`.
    pub update_check_secs: u64,

    /// Settings that were set but didn't parse (their default is used), for the start-up log.
    pub unparsed: Vec<String>,
}

impl Config {
    /// The settings from `var` (the environment, or a map in tests).
    pub fn read(var: impl Fn(&str) -> Option<String>) -> Config {
        let unparsed = std::cell::RefCell::new(Vec::new());
        let set = |k: &str| var(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let text = |k: &str, default: &str| set(k).unwrap_or_else(|| default.to_string());
        let num = |k: &str, default: f64| match set(k) {
            None => default,
            Some(v) => v.parse::<f64>().ok().filter(|n| n.is_finite()).unwrap_or_else(|| {
                unparsed.borrow_mut().push(format!("{k}={v}"));
                default
            }),
        };
        let on = |k: &str| set(k).is_none_or(|v| !matches!(v.to_ascii_lowercase().as_str(), "0" | "off" | "false" | "no"));
        let cache_ttl = text("ZEN_CACHE_TTL", "claude=3600")
            .split(',')
            .filter_map(|pair| {
                let (engine, secs) = pair.split_once('=')?;
                Some((engine.trim().to_string(), secs.trim().parse().ok()?))
            })
            .collect();
        let c = Config {
            summary_model: text("ZEN_SUMMARY_MODEL", "claude/claude-sonnet-5-5"),
            assist_model: text("ZEN_ASSIST_MODEL", "claude/claude-haiku-5-5"),
            embed_model: text("ZEN_EMBED_MODEL", "openai/text-embedding-3-small"),
            embed_url: text("ZEN_EMBED_URL", "https://openrouter.ai/api/v1"),
            embed: on("ZEN_EMBED"),
            s1_model: set("ZEN_S1_MODEL"),
            s1_url: text("ZEN_S1_URL", "https://openrouter.ai/api/v1/systemone"),
            s1_private: on("ZEN_S1_PRIVATE"),
            s1_concurrency: num("ZEN_S1_CONCURRENCY", 8.0).max(1.0) as usize,
            cache_ttl,
            soul_chars: num("ZEN_SOUL_CHARS", 4000.0) as usize,
            identity_chars: num("ZEN_IDENTITY_CHARS", 4000.0) as usize,
            agents_chars: num("ZEN_AGENTS_CHARS", 12000.0) as usize,
            user_chars: num("ZEN_USER_CHARS", 3000.0) as usize,
            skill_index_chars: num("ZEN_SKILL_INDEX_CHARS", 2500.0) as usize,
            judge_tool: on("ZEN_JUDGE_TOOL"),
            context_tokens: set("ZEN_CONTEXT_TOKENS").map(|_| num("ZEN_CONTEXT_TOKENS", 0.0) as i64).filter(|n| *n > 0),
            compact_soft: num("ZEN_COMPACT_SOFT", 0.7),
            compact_hard: num("ZEN_COMPACT_HARD", 0.9),
            compact_keep: num("ZEN_COMPACT_KEEP", 0.3),
            compact_idle_secs: num("ZEN_COMPACT_IDLE_SECS", 300.0) as i64,
            suggest: on("ZEN_SUGGEST"),
            memory_chars: num("ZEN_MEMORY_CHARS", 4000.0).max(200.0) as usize,
            index_secs: num("ZEN_INDEX_SECS", 20.0).max(1.0) as u64,
            search_provider: set("ZEN_SEARCH_PROVIDER"),
            searxng_url: text("ZEN_SEARXNG_URL", "http://127.0.0.1:8888"),
            search_rerank: on("ZEN_SEARCH_RERANK"),
            tz: text("ZEN_TZ", "UTC"),
            jobs: on("ZEN_JOBS"),
            jobs_tick: num("ZEN_JOBS_TICK", 60.0).clamp(0.2, 60.0),
            engine_concurrency: (num("ZEN_ENGINE_CONCURRENCY", 3.0) as usize).max(1),
            turn_idle_secs: num("ZEN_TURN_IDLE_SECS", 600.0) as u64,
            turn_abort_grace_secs: num("ZEN_TURN_ABORT_GRACE_SECS", 30.0) as u64,
            update_check_secs: num("ZEN_UPDATE_CHECK_SECS", 3600.0) as u64,
            unparsed: Vec::new(),
        };
        Config { unparsed: unparsed.into_inner(), ..c }
    }

    /// How long the prompt cache of `model`'s engine (the part before `/`) lives, in seconds.
    pub fn cache_ttl_secs(&self, model: &str) -> i64 {
        let engine = model.split('/').next().unwrap_or("");
        let of = |e: &str| self.cache_ttl.iter().find(|(name, _)| name == e).map(|(_, s)| *s);
        of(engine).or_else(|| of("*")).unwrap_or(300)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from(pairs: &[(&str, &str)]) -> Config {
        let map: std::collections::HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Config::read(|k| map.get(k).cloned())
    }

    #[test]
    fn defaults_hold_when_nothing_is_set() {
        let c = from(&[]);
        assert_eq!((c.tz.as_str(), c.summary_model.as_str(), c.assist_model.as_str()), ("UTC", "claude/claude-sonnet-5-5", "claude/claude-haiku-5-5"));
        assert_eq!((c.memory_chars, c.jobs, c.s1_model.clone(), c.context_tokens), (4000, true, None, None));
        assert!(c.unparsed.is_empty());
    }

    #[test]
    fn values_parse_and_bad_ones_fall_back() {
        let c = from(&[("ZEN_TZ", " UTC "), ("ZEN_JOBS", "off"), ("ZEN_MEMORY_CHARS", "50"), ("ZEN_TURN_IDLE_SECS", "long"), ("ZEN_S1_MODEL", " ")]);
        assert_eq!((c.tz.as_str(), c.jobs, c.memory_chars, c.turn_idle_secs), ("UTC", false, 200, 600));
        assert_eq!(c.s1_model, None, "blank is unset");
        assert_eq!(c.unparsed, vec!["ZEN_TURN_IDLE_SECS=long"]);
        assert_eq!(from(&[("ZEN_CONTEXT_TOKENS", "400000")]).context_tokens, Some(400_000));
        assert_eq!(from(&[("ZEN_CONTEXT_TOKENS", "0")]).context_tokens, None, "0 is no cap");
    }

    #[test]
    fn the_cache_lives_per_engine() {
        let c = from(&[]);
        assert_eq!((c.cache_ttl_secs("claude/claude-opus-5-5"), c.cache_ttl_secs("codex/gpt-6.1-sol")), (3600, 300));
        let c = from(&[("ZEN_CACHE_TTL", "claude=600, codex=1800,*=120")]);
        assert_eq!((c.cache_ttl_secs("claude/x"), c.cache_ttl_secs("codex/y"), c.cache_ttl_secs("faux/smoke")), (600, 1800, 120));
    }
}
