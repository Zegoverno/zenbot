//! The command line: `zen`'s arguments and subcommands (clap).

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "zen", version, about = "zenbot from the command line, for scripts. Piped input with no command runs `zen ask`; the chat is the web app and Matrix.")]
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
    /// Model for a new session
    #[arg(short, long)]
    pub(crate) model: Option<String>,
    /// Thinking level for a new session, e.g. high (`zen models` lists each model's levels)
    #[arg(short, long)]
    pub(crate) effort: Option<String>,
    /// The agent a new session is (`zen agents` lists them; default zenbot)
    #[arg(short, long)]
    pub(crate) agent: Option<String>,
    /// Print the chat commands of the shared table as JSON (for the web app)
    #[arg(long, hide = true)]
    pub(crate) commands: bool,
    #[command(subcommand)]
    pub(crate) cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub(crate) enum Cmd {
    /// An enabled plugin command, resolved through the shared command table.
    #[command(external_subcommand)]
    Plugin(Vec<String>),
    /// Records (people, organizations, projects, long-term memory): get, list, put, schema, lint, index.
    /// Arguments as JSON with --args (or - for stdin).
    Brain {
        action: String,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        args: Option<String>,
    },
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
        /// The agent a new session is (default zenbot)
        #[arg(short, long)]
        agent: Option<String>,
        /// Don't show tool activity on stderr
        #[arg(short, long)]
        quiet: bool,
    },
    /// List, show or restore sessions
    #[command(subcommand)]
    Sessions(SessionsCmd),
    /// Start a new session; prints its id
    New {
        /// Its title
        title: Option<String>,
        #[arg(short, long)]
        model: Option<String>,
        #[arg(short, long)]
        effort: Option<String>,
        /// The agent it is (default zenbot)
        #[arg(short, long)]
        agent: Option<String>,
    },
    /// Installed plugins; `zen plugins add <name|path.tgz|folder> [--yes]` installs or upgrades one (asking you to approve its permissions), `zen plugins rm <name>` removes one
    Plugins {
        /// `add <name|path> [--yes]` or `rm <name>`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        words: Vec<String>,
    },
    /// Your agents and what each may use; `zen agents new <name> [description]` makes one, `zen agents rm <name>` removes one
    Agents {
        /// `new` to make an agent, `rm` to remove one
        action: Option<String>,
        /// The new agent's name, then what it is for
        words: Vec<String>,
    },
    /// A session's model (no model: list them)
    Model {
        model: Option<String>,
        /// The session (id or unique prefix)
        #[arg(short, long)]
        session: Option<String>,
    },
    /// A session's thinking level (no level: the model's levels)
    Effort {
        level: Option<String>,
        #[arg(short, long)]
        session: Option<String>,
    },
    /// Rename a session
    Rename {
        title: Vec<String>,
        #[arg(short, long)]
        session: String,
    },
    /// Archive a session, optionally saying how it ended: done, partial (continues in a new session with a handoff) or not-done, then how well it went (a number, 1-10) and a note: `archive done 8 shipped it`
    Archive {
        outcome: Option<String>,
        /// A rating from 1 to 10 right after the outcome, then the note
        note: Vec<String>,
        #[arg(short, long)]
        session: String,
    },
    /// Change a running session's direction at its next tool boundary
    Steer {
        message: Vec<String>,
        #[arg(short, long)]
        session: String,
    },
    /// List available models
    Models,
    /// Sign in to the model engines: Claude Code and Codex (default), or one of: claude, codex
    Login {
        /// Which engine to sign in to (default: every engine that isn't signed in yet)
        which: Option<String>,
    },
    /// Check that the kernel, database, worker and model sign-in are healthy
    Status,
    /// zenbot's skills with how often they're used
    Skills,
    /// Open tasks from the vault's projects (Projects/*/tasks.md), grouped by project, most pressing first
    Tasks,
    /// System One: each decision's numbers; check the decision files; replay, compare
    S1 {
        #[command(subcommand)]
        cmd: Option<S1Cmd>,
    },
    /// zenbot's memory: short (default), long, archived or all; `zen memory sleep` tidies it now
    Memory {
        what: Option<String>,
    },
    /// Suggested next prompts: what became of them (accepted, edited, declined) by prompt version, and the latest
    Suggestions,
    /// Scheduled jobs: the kernel's own (sleep, engines) and agent jobs; `zen jobs` lists them
    Jobs {
        #[command(subcommand)]
        cmd: Option<JobsCmd>,
    },
    /// MCP servers; `zen mcp login <server>` signs in to one (open the link, paste the address your browser ended on)
    Mcp {
        args: Vec<String>,
        /// Explicit agent scope for mcp serve
        #[arg(long)]
        agent: Option<String>,
        /// Allow memory and record writes in mcp serve
        #[arg(long)]
        write: bool,
    },
    /// List MCP forms awaiting an explicit owner response
    McpPending,
    /// Answer a pending MCP form (owner only)
    McpRespond {
        id: String,
        #[arg(long, value_parser = ["accept", "decline", "cancel"])]
        action: String,
        /// JSON object matching the pending schema; required for accept
        #[arg(long)]
        data: Option<String>,
    },
    /// Update zenbot to the latest main and restart it; `zen upgrade check` only checks
    Upgrade {
        what: Option<String>,
        /// Only check whether an update is available (same as `zen upgrade check`)
        #[arg(long)]
        check: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum S1Cmd {
    /// Check the decision files (~/.zenbot/s1/ and the built-in ones): errors with line and column
    Check,
    /// The approved System One models (~/.zenbot/s1/models.toml)
    Models,
    /// Apply a decision's policy (or an edited file's) to its recorded answers: what would change
    Replay {
        decision: String,
        /// An edited decision file to try instead of the one in force
        #[arg(long)]
        file: Option<std::path::PathBuf>,
        /// How many recent decisions (default 500)
        #[arg(long)]
        last: Option<i64>,
    },
    /// Ask recorded decisions again on approved models: agreement, accuracy, latency, cost
    Compare {
        decision: String,
        /// Approved model names, comma-separated
        #[arg(long, value_delimiter = ',', required = true)]
        models: Vec<String>,
        /// How many recent decisions (default 20)
        #[arg(long)]
        last: Option<i64>,
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
    /// Show a session's messages
    Show { id: String },
    /// Restore an archived session
    Restore { id: String },
}

#[derive(Subcommand)]
pub(crate) enum JobsCmd {
    /// Create a job: a prompt run in a fresh session on a schedule, or (--command) a shell command run with no model
    Add {
        /// Lowercase letters, digits and hyphens, e.g. morning-brief
        name: String,
        /// When: a cron expression (`0 7 * * 1-5`), `every 2h`, `at 2026-10-12 07:00` or `in 30m`
        #[arg(short, long)]
        schedule: String,
        /// The task, self-contained
        #[arg(short, long, required_unless_present = "command", conflicts_with = "command")]
        prompt: Option<String>,
        /// A script job: this shell command, run with no model; its output is the report
        #[arg(long)]
        command: Option<String>,
        /// IANA timezone the schedule is read in (default: the kernel's ZEN_TZ, else UTC)
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
        /// A session (id or its first 8+ characters) its reports are posted into, e.g. the Matrix main room's
        #[arg(long)]
        post_to: Option<String>,
        /// The agent it runs as (default zenbot)
        #[arg(long)]
        agent: Option<String>,
        /// Name of the plugin this routine came from
        #[arg(long)]
        plugin: Option<String>,
    },
    /// Change an agent job's schedule, prompt, timezone, context, skills, model, report file or target session
    Set {
        name: String,
        #[arg(short, long)]
        schedule: Option<String>,
        #[arg(short, long)]
        prompt: Option<String>,
        /// A script job's new command
        #[arg(long)]
        command: Option<String>,
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
        /// A session its reports are posted into (`""` goes back to the jobs feed only)
        #[arg(long)]
        post_to: Option<String>,
        /// Name of the plugin this routine came from (empty clears it)
        #[arg(long)]
        plugin: Option<String>,
    },
    /// Pause a job
    Pause { name: String },
    /// Resume a paused job
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use zen_client::commands::{Scope, COMMANDS};

    /// The command line offers exactly the shared table's commands that aren't the chat's own.
    #[test]
    fn the_command_line_and_the_shared_table_agree() {
        let cli = Cli::command();
        let mut line: Vec<&str> = cli.get_subcommands().map(|c| c.get_name()).collect();
        let mut table: Vec<&str> = COMMANDS.iter().filter(|c| c.scope != Scope::Screen).map(|c| c.name).collect();
        line.sort();
        table.sort();
        assert_eq!(line, table);
    }
}
