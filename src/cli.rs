//! The command-line argument surface.
//!
//! Exactly the commands, positionals, and flags `docs/contract.md` lists —
//! parsing only. Nothing here runs, validates, cancels, or reports anything; the
//! binary parses one of these and refuses.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Compose agents into a graph and merge their outputs into one event stream.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
#[command(name = "oneagentgraph", version, about, long_about = None)]
pub struct Cli {
    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// The top-level commands.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
#[command(rename_all = "kebab-case")]
pub enum Command {
    /// Run a graph, streaming envelopes to stdout.
    Run(RunArgs),
    /// Check a graph without running it.
    Validate(ValidateArgs),
    /// Fire a scheduled member now.
    Trigger(MemberArgs),
    /// Restart a resettable schedule's clock.
    ResetTimer(MemberArgs),
    /// Cancel a run, or one member of it.
    Cancel(CancelArgs),
    /// List past runs, or show one record.
    History(HistoryArgs),
    /// Per-identity binding, utilization, and reset, read from oneharness data.
    Health,
    /// Spend one real harness turn in a throwaway directory.
    Smoke(SmokeArgs),
    /// Scaffold or check a persona.
    Persona(PersonaArgs),
}

/// `oneagentgraph run`.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct RunArgs {
    /// The graph config, by path or URL.
    pub graph: String,
    /// The task prose to give every member that takes one.
    #[arg(long)]
    pub task: Option<String>,
    /// Read the task prose from a file instead.
    #[arg(long)]
    pub task_file: Option<PathBuf>,
    /// The directory to run in.
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Stamp an extra label on every event, as `k=v`. Repeatable.
    #[arg(long, value_name = "k=v")]
    pub label: Vec<String>,
    /// Override one config field, as `members.worker.agent.model=NAME`.
    /// Repeatable.
    #[arg(long, value_name = "PATH=VALUE")]
    pub set: Vec<String>,
    /// How to render the events. `text` is a deterministic rendering of the same
    /// events, never separate content.
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub output: OutputFormat,
    /// Print `{run_id, events_path, pid}` and exit 0 instead of streaming.
    #[arg(long)]
    pub detach: bool,
}

/// How `run` renders its events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum OutputFormat {
    /// One NDJSON envelope per line.
    Json,
    /// A deterministic rendering of those same envelopes.
    Text,
}

/// `oneagentgraph validate`.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct ValidateArgs {
    /// The graph config, by path or URL.
    pub graph: String,
}

/// `oneagentgraph trigger` and `oneagentgraph reset-timer`.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct MemberArgs {
    /// The run.
    pub run: String,
    /// The member within it.
    pub member: String,
}

/// `oneagentgraph cancel`.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct CancelArgs {
    /// The run.
    pub run: String,
    /// One member of it; omit to cancel the whole run.
    pub member: Option<String>,
    /// Terminate rather than asking the member to stop.
    #[arg(long)]
    pub kill: bool,
}

/// `oneagentgraph history`.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct HistoryArgs {
    /// The run whose records to list; omit for every run.
    pub run: Option<String>,
    /// Show one record instead of listing.
    #[command(subcommand)]
    pub command: Option<HistoryCommand>,
}

/// The `history` subcommands.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum HistoryCommand {
    /// Show one record.
    Show {
        /// The record's id.
        id: String,
    },
}

/// `oneagentgraph smoke`.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct SmokeArgs {
    /// Where to spend the turn; a throwaway directory by default.
    #[arg(long)]
    pub dir: Option<PathBuf>,
}

/// `oneagentgraph persona`.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct PersonaArgs {
    /// What to do with a persona.
    #[command(subcommand)]
    pub command: PersonaCommand,
}

/// The `persona` subcommands.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum PersonaCommand {
    /// Scaffold a new persona.
    New {
        /// The persona's name.
        name: String,
    },
    /// Check a persona file.
    Validate {
        /// The persona file's path.
        path: PathBuf,
    },
}
