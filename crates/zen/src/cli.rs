//! The command line: `zen`'s arguments and subcommands (clap).

use clap::{Parser, Subcommand};
use serde_json::Value;

#[derive(Parser)]
#[command(name = "zen", version, about = "zenbot in your terminal. Run `zen` for an interactive session, or use the commands below for scripts.")]
pub(crate) struct Cli {
    /// Kernel URL
    #[arg(long, env = "ZEN_URL", default_value = "http://127.0.0.1:8100", global = true)]
    pub(crate) url: String,
    /// Access token (default: contents of $ZEN_HOME/token, ~/.zenbot/token)
    #[arg(long, env = "ZEN_TOKEN", hide_env_values = true, global = true)]
    pub(crate) token: Option<String>,
    /// Print machine-readable JSON
    #[arg(long, global = true)]
    pub(crate) json: bool,
    /// Continue the most recent session
    #[arg(short = 'c', long = "continue")]
    pub(crate) cont: bool,
    /// Resume a session (opens a picker when no id is given)
    #[arg(short = 'r', long, num_args = 0..=1, default_missing_value = "")]
    pub(crate) resume: Option<String>,
    /// Model for a new session
    #[arg(short, long)]
    pub(crate) model: Option<String>,
    /// Thinking level for a new session, e.g. high (`zen models` lists each model's levels)
    #[arg(short, long)]
    pub(crate) effort: Option<String>,
    /// Inline terminal app (no full-screen clearing; the input follows the conversation)
    #[arg(long, env = "ZEN_INLINE", global = true)]
    pub(crate) inline: bool,
    #[command(subcommand)]
    pub(crate) cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub(crate) enum Cmd {
    /// Run one task and print the answer. Reads the prompt from stdin when omitted or "-".
    Ask {
        /// The prompt
        prompt: Option<String>,
        /// Continue an existing session (id or unique prefix); default creates a new one
        #[arg(short, long)]
        session: Option<String>,
        /// Model for a new session, e.g. openai/gpt-6.1-sol
        #[arg(short, long)]
        model: Option<String>,
        /// Thinking level for a new session, e.g. high
        #[arg(short, long)]
        effort: Option<String>,
        /// Don't show tool activity on stderr
        #[arg(short, long)]
        quiet: bool,
    },
    /// Interactive session in the terminal (same as running `zen` with no command)
    Chat {
        /// Continue an existing session (id or unique prefix)
        #[arg(short, long)]
        session: Option<String>,
        /// Model for a new session
        #[arg(short, long)]
        model: Option<String>,
        /// Thinking level for a new session
        #[arg(short, long)]
        effort: Option<String>,
    },
    /// Manage sessions
    #[command(subcommand)]
    Sessions(SessionsCmd),
    /// List available models
    Models,
    /// Sign in to the model engines: Claude Code and Codex (default), or one of: claude, codex
    Login {
        /// Which engine to sign in to (default: every engine that isn't signed in yet)
        which: Option<String>,
    },
    /// Check that the kernel, database, worker and model sign-in are healthy
    Status,
    /// zenbot's skills (active and drafts) with how often they're used, and the tools it made
    Skills {
        #[command(subcommand)]
        cmd: Option<ReviewCmd>,
    },
    /// The routing policy for subagents (which model per kind of work) and the evidence for it
    Policy {
        #[command(subcommand)]
        cmd: Option<PolicyCmd>,
    },
    /// Approve or reject a tool zenbot made (`zen skills` lists them)
    Tools {
        #[command(subcommand)]
        cmd: ReviewCmd,
    },
    /// Show zenbot's memory (short-term by default) and the last sleep; `zen memory sleep` tidies it now
    Memory {
        #[command(subcommand)]
        cmd: Option<MemoryCmd>,
        /// Which memories: short (default), archived or all
        #[arg(long, default_value = "short")]
        tier: String,
    },
    /// Suggested next prompts: what became of them (accepted, edited, declined) by prompt version, and the latest
    Suggestions,
    /// Scheduled jobs: the kernel's own (sleep, engines) and agent jobs; `zen jobs` lists them
    Jobs {
        #[command(subcommand)]
        cmd: Option<JobsCmd>,
    },
    /// Update zenbot to the latest version on GitHub (main) and restart it
    Upgrade {
        /// Only check whether an update is available
        #[arg(long)]
        check: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum SessionsCmd {
    /// List sessions
    Ls {
        /// Show archived sessions instead
        #[arg(long)]
        archived: bool,
    },
    /// Create a session
    New {
        #[arg(short, long)]
        model: Option<String>,
        #[arg(short, long)]
        effort: Option<String>,
        #[arg(short, long)]
        title: Option<String>,
    },
    /// Show a session's messages
    Show { id: String },
    /// Archive a session
    Archive { id: String },
    /// Restore an archived session
    Restore { id: String },
    /// Rename a session
    Rename { id: String, title: String },
    /// Record your decision on a session's work so far: accept, more, reshape or drop
    Decide {
        id: String,
        decision: String,
        /// Why, in a few words
        #[arg(short, long)]
        note: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum PolicyCmd {
    /// Route a kind of work (understand, shape, build, verify, maintain, reflect, reach, bet, default) to a model
    Set {
        kind: String,
        model: String,
        /// Other models to try now and then, comma-separated
        #[arg(long)]
        candidates: Option<String>,
        /// Share of subtasks that try a candidate (default 0.1)
        #[arg(long)]
        explore: Option<f64>,
    },
    /// Go back to the policy before the latest change
    Undo,
}

#[derive(Subcommand)]
pub(crate) enum ReviewCmd {
    /// Accept: activate a draft skill (domain/name), or let a tool use the network
    Accept {
        name: String,
        /// The fingerprint `zen skills` showed for what you reviewed (refused if it changed since)
        sha: String,
    },
    /// Reject: archive a draft skill, or stop a tool from running
    Reject {
        name: String,
        /// The fingerprint `zen skills` showed for what you reviewed (refused if it changed since)
        sha: String,
    },
}

impl ReviewCmd {
    /// The name, the decision and the reviewed fingerprint to send.
    pub(crate) fn parts(self) -> (String, &'static str, String) {
        match self {
            ReviewCmd::Accept { name, sha } => (name, "accept", sha),
            ReviewCmd::Reject { name, sha } => (name, "reject", sha),
        }
    }
}

/// How a draft's or a tool's fingerprint is shown in `zen skills` (and typed back to review it).
pub(crate) fn short_sha(v: &Value) -> String {
    v["sha"].as_str().map(|s| format!(" {}", &s[..12.min(s.len())])).unwrap_or_default()
}

#[derive(Subcommand)]
pub(crate) enum JobsCmd {
    /// Create an agent job: a prompt run in a fresh session on a schedule
    Add {
        /// Lowercase letters, digits and hyphens, e.g. morning-brief
        name: String,
        /// When: a cron expression (`0 7 * * 1-5`), `every 2h`, `at 2026-10-12 07:00` or `in 30m`
        #[arg(short, long)]
        schedule: String,
        /// The task, self-contained
        #[arg(short, long)]
        prompt: String,
        /// IANA timezone the schedule is read in (default America/Sao_Paulo)
        #[arg(long)]
        tz: Option<String>,
        /// Instruction parts besides the soul, comma-separated: identity, agents, user, memory, skills, project (default user,memory)
        #[arg(long)]
        context: Option<String>,
        /// Skills loaded into its instructions, comma-separated (domain/name)
        #[arg(long)]
        skills: Option<String>,
        /// Model (default: the default model when it runs)
        #[arg(short, long)]
        model: Option<String>,
        /// Directory it works in (default: the workspace)
        #[arg(long)]
        dir: Option<String>,
        /// A file in its directory the run writes the report to; the kernel posts the file, not the run's answer
        #[arg(long)]
        report_file: Option<String>,
    },
    /// Change an agent job's schedule, prompt, timezone, context, skills, model or report file
    Set {
        name: String,
        #[arg(short, long)]
        schedule: Option<String>,
        #[arg(short, long)]
        prompt: Option<String>,
        #[arg(long)]
        tz: Option<String>,
        #[arg(long)]
        context: Option<String>,
        #[arg(long)]
        skills: Option<String>,
        #[arg(short, long)]
        model: Option<String>,
        /// A file in its directory the run writes the report to (`""` goes back to the run's answer)
        #[arg(long)]
        report_file: Option<String>,
    },
    /// Pause a job
    Pause { name: String },
    /// Resume a paused job (also approves one the agent created)
    Resume { name: String },
    /// Remove an agent job (its runs stay on record)
    Rm { name: String },
    /// Run a job now
    Run { name: String },
    /// Recent runs and their reports (of one job, or all)
    Runs {
        name: Option<String>,
        /// How many
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: i64,
    },
}

#[derive(Subcommand)]
pub(crate) enum MemoryCmd {
    /// Tidy short-term memory now (what the nightly sleep does)
    Sleep,
}
