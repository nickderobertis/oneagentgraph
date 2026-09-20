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

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

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

/// How `run` renders its events, and how `sweep` writes its report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum OutputFormat {
    /// One NDJSON envelope per line; for `sweep`, one JSON object.
    Json,
    /// A deterministic rendering of those same envelopes; for `sweep`, the
    /// report an operator reads.
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
    /// Leave scratch written inside this many hours alone — a non-negative
    /// decimal, so `0.5` is thirty minutes; `0` sweeps whatever is provably
    /// dead.
    #[arg(
        long,
        value_name = "HOURS",
        default_value_t = MinAgeHours::DEFAULT,
        // Or a negative floor would be refused as an unknown flag rather than
        // as the bad value it is.
        allow_negative_numbers = true
    )]
    pub min_age_hours: MinAgeHours,
    /// How to write the report: `text` for an operator, `json` for one object
    /// carrying the same report.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

/// The default [`SweepArgs::min_age_hours`], which is
/// [`crate::sweep::DEFAULT_MIN_AGE`] said in the unit an operator types.
///
/// Derived rather than spelled, so the flag's default and the floor the library
/// applies cannot come to disagree.
pub const DEFAULT_MIN_AGE_HOURS: u64 = crate::sweep::DEFAULT_MIN_AGE.as_secs() / 3600;

/// A non-negative number of hours, as `--min-age-hours` reads one.
///
/// Decimal rather than whole, because the floor is one value an operator passes
/// to this verb and to `onevcs sweep` alike, and a flag that refused the `0.5`
/// its sibling takes made every caller pick the integer the stricter one
/// accepts. Parsed here, at the argument, so a negative or non-numeric floor is
/// refused with the same argument refusal as any other bad value — before a
/// family is examined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MinAgeHours(Duration);

impl MinAgeHours {
    /// [`crate::sweep::DEFAULT_MIN_AGE`], as the flag's default.
    pub const DEFAULT: Self = Self(crate::sweep::DEFAULT_MIN_AGE);

    /// The floor as the library applies it.
    #[must_use]
    pub const fn as_duration(self) -> Duration {
        self.0
    }

    /// The floor as a number of hours — what the report writes back.
    #[must_use]
    pub fn hours(self) -> f64 {
        self.0.as_secs_f64() / 3600.0
    }
}

impl FromStr for MinAgeHours {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let hours: f64 = text
            .parse()
            .map_err(|_| format!("`{text}` is not a number of hours"))?;
        // A NaN is neither negative nor not, and is refused with the negatives
        // rather than let through as a floor of no hours at all.
        if hours.is_nan() || hours < 0.0 {
            return Err(format!("`{text}` is not a non-negative number of hours"));
        }
        Duration::try_from_secs_f64(hours * 3600.0)
            .map(Self)
            .map_err(|_| format!("`{text}` is more hours than a floor can hold"))
    }
}

impl fmt::Display for MinAgeHours {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.hours())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The floor's grammar: a non-negative decimal number of hours, and nothing
    /// that is not one — the e2e journey drives `-1` and `abc` through the
    /// binary, and this pins the edges a command line rarely reaches.
    #[test]
    fn a_floor_is_a_non_negative_decimal_number_of_hours() {
        assert_eq!(
            "0.5"
                .parse::<MinAgeHours>()
                .expect("half an hour")
                .as_duration(),
            Duration::from_secs(30 * 60)
        );
        assert_eq!(
            "0".parse::<MinAgeHours>().expect("no floor").as_duration(),
            Duration::ZERO
        );
        assert_eq!(
            "24".parse::<MinAgeHours>().expect("a day"),
            MinAgeHours::DEFAULT
        );
        for refused in ["-1", "-0.5", "abc", "", "nan", "inf", "1e300"] {
            let err = refused
                .parse::<MinAgeHours>()
                .expect_err("a floor that is not a non-negative number of hours");
            assert!(err.contains("hours"), "{refused:?}: {err}");
        }
    }

    /// The flag's rendered default is the integer the contract states, and the
    /// hours a report writes back are the ones that were typed.
    #[test]
    fn the_default_renders_as_the_documented_integer_and_hours_round_trip() {
        assert_eq!(
            MinAgeHours::DEFAULT.to_string(),
            DEFAULT_MIN_AGE_HOURS.to_string()
        );
        let half = "0.5".parse::<MinAgeHours>().expect("half an hour");
        assert_eq!(half.to_string(), "0.5");
        assert!((half.hours() - 0.5).abs() < f64::EPSILON);
    }

    /// A negative floor reaches the value parser rather than being read as an
    /// unknown flag, so it is refused as the bad value it is.
    #[test]
    fn a_negative_floor_is_refused_as_a_value() {
        let err = Cli::try_parse_from(["oneagentgraph", "sweep", "--min-age-hours", "-1"])
            .expect_err("a negative floor");
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation, "{err}");
        assert!(err.to_string().contains("non-negative"), "{err}");
    }
}
