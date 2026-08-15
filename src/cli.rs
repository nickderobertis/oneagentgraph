//! The command-line argument surface.
//!
//! Exactly the commands, positionals, and flags `docs/contract.md` lists —
//! parsing only. Nothing here runs, validates, cancels, or reports anything; the
//! binary parses one of these and refuses.

// llmlint: ignore-file[invalid_states_unrepresentable] every identifier and boundary
// value here is the argument `docs/contract.md` spells, and this is the parsing layer
// only. A `RunId`/`MemberName`/`Label` newtype would be a public item the contract does
// not name, and parsing `k=v` or `PATH=VALUE` into one is the implementation the
// interface-only stage forbids (see AGENTS.md). `HistoryArgs` keeps two independent
// `Option`s because that is what `history [RUN] | history show ID` is in clap; making the
// pair exclusive in the type is a follow-up the contract has to settle first.

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
    /// Redirect a member's in-flight turn instead of ending it.
    Interrupt(InterruptArgs),
    /// List past runs, or show one record.
    History(HistoryArgs),
    /// Per-identity binding, utilization, and reset, read from oneharness data.
    Health,
    /// Spend one real harness turn in a throwaway directory.
    Smoke(SmokeArgs),
    /// Report every family of this crate's own scratch, and reclaim what is
    /// provably dead.
    Sweep(SweepArgs),
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
    /// Keep only the events a filter admits, as a file path or inline JSON.
    /// Wins over the graph's own `events.filter`.
    #[arg(long, value_name = "SPEC")]
    pub event_filter: Option<String>,
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

/// `oneagentgraph interrupt`.
///
/// The same addressing as [`CancelArgs`] — a run and a member of it — because the
/// two verbs reach the same live member and differ only in intent. `MEMBER` is
/// required here where `cancel` leaves it optional: there is no whole-run turn to
/// redirect.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct InterruptArgs {
    /// The run.
    pub run: String,
    /// The member within it.
    pub member: String,
    /// What the turn should do instead, delivered with the stop as one
    /// operation. Omit to only stop the turn.
    #[arg(long)]
    pub input: Option<String>,
    /// Read that redirection from a file instead.
    #[arg(long)]
    pub input_file: Option<PathBuf>,
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

/// `oneagentgraph sweep`.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct SweepArgs {
    /// Report what would be reclaimed without removing anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Leave scratch written inside this many hours alone; `0` sweeps whatever
    /// is provably dead.
    #[arg(long, value_name = "HOURS", default_value_t = DEFAULT_MIN_AGE_HOURS)]
    pub min_age_hours: u64,
}

/// The default [`SweepArgs::min_age_hours`], which is
/// [`crate::sweep::DEFAULT_MIN_AGE`] said in the unit an operator types.
///
/// Derived rather than spelled, so the flag's default and the floor the library
/// applies cannot come to disagree.
pub const DEFAULT_MIN_AGE_HOURS: u64 = crate::sweep::DEFAULT_MIN_AGE.as_secs() / 3600;

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
