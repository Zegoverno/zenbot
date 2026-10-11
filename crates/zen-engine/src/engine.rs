//! Engines: what zen-engine needs from each thing it runs turns on (the Claude Code CLI, the Codex
//! app-server, the scripted faux model), and the registry the protocol loop (`main.rs`) and
//! failover (`failover.rs`) go through. Adding an engine is one module implementing `Engine` and
//! its line in `all`; nothing else matches on engine names.
//!
//! An engine keeps the runtime checks that hold zenbot's boundary in its own module (Claude Code's
//! `system/init` check, Codex's item allowlist and `codex-features`): the trait only routes to it.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use tokio::sync::watch;
use zen_proto::worker::{Completion, Model};

use crate::turn::{TurnCtx, TurnInput};
use crate::{claude, codex, faux};

/// A boxed future, so engines can sit in one list (`dyn Engine`).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One engine. Its models' ids are `<name>/<model>`; the kernel routes them to this worker, and
/// the worker to the engine by that prefix.
pub trait Engine: Sync {
    /// The engine's name, its models' id prefix (`claude` in `claude/claude-opus-5-5`).
    fn name(&self) -> &'static str;

    /// Whether it runs on one of the owner's subscriptions: `models.list` reports it under
    /// `authenticated`, and failover may switch to or from it. False for a test engine.
    fn subscription(&self) -> bool {
        true
    }

    /// Its CLI's version; None when it isn't installed or doesn't answer (then it lists no
    /// models). A blocking process run: call it off the runtime's threads.
    fn version(&self) -> Option<String>;

    /// Housekeeping when the worker starts.
    fn prepare(&self) {}

    /// The models it serves now (full ids).
    fn models(&self) -> BoxFuture<'_, Vec<Model>>;

    /// Run one turn (`turn.start`): stream its messages through `ctx`, send its `turn.usage`, and
    /// return the error it ended with (None on success, `"interrupted"` after `abort`). `turn.end`
    /// is sent by the caller.
    fn run_turn<'a>(&'a self, ctx: TurnCtx, input: &'a TurnInput, abort: watch::Receiver<bool>) -> BoxFuture<'a, Result<Option<String>>>;

    /// One completion without tools (`complete`); `model` is without the engine prefix.
    fn complete<'a>(&'a self, model: &'a str, system: &'a str, prompt: &'a str) -> BoxFuture<'a, Result<Completion>>;

    /// The model (full id) a turn continues on after a hard usage limit here, if any.
    fn failover_target(&self) -> Option<String> {
        None
    }

    /// Whether its CLI is signed in with the owner's subscription and no API key would take
    /// precedence: failover must never switch to a billable path. Blocking.
    fn subscription_login(&self) -> bool {
        false
    }
}

/// Every engine this worker runs, in the order `models.list` lists them. The faux engine only
/// when ZEN_FAUX=1.
pub fn all() -> Vec<&'static dyn Engine> {
    let mut engines: Vec<&'static dyn Engine> = vec![&claude::Claude, &codex::Codex];
    if faux::enabled() {
        engines.push(&faux::Faux);
    }
    engines
}

/// The engine named `name`.
pub fn find(name: &str) -> Option<&'static dyn Engine> {
    all().into_iter().find(|e| e.name() == name)
}

/// The engine that serves a full model id (`claude/…`), and the model id without its prefix.
pub fn for_model(model: &str) -> Option<(&'static dyn Engine, &str)> {
    let (engine, rest) = model.split_once('/')?;
    Some((find(engine)?, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_are_found_by_model_prefix() {
        assert_eq!(for_model("claude/claude-opus-5-5").map(|(e, m)| (e.name(), m)), Some(("claude", "claude-opus-5-5")));
        assert_eq!(for_model("codex/gpt-6.1-sol").map(|(e, _)| e.name()), Some("codex"));
        assert!(for_model("pi/anything").is_none());
        assert!(for_model("claude").is_none(), "a model id has its engine prefix");
        let names: Vec<&str> = all().iter().map(|e| e.name()).collect();
        assert_eq!(&names[..2], ["claude", "codex"]);
    }
}
