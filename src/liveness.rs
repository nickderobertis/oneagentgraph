//! The liveness contract, ported from `ai-orchestrator` intact.
//!
//! The bounds and names below are the contract; the wrapper, the watchdog, the
//! lock, the reaper, and the successor `exec` are not implemented here.
//!
//! - A **heartbeat wrapper** requires a member to say it is alive within
//!   [`DEFAULT_HEARTBEAT_TIMEOUT`], overridable by [`HEARTBEAT_TIMEOUT_ENV`].
//! - An **activity watchdog** requires it to record something within
//!   [`DEFAULT_STALL_TIMEOUT`], overridable by [`STALL_TIMEOUT_ENV`].
//! - **Scratch ownership** is an [`OWNER_LOCK_FILE`] `flock` plus a
//!   pid-with-start-token, so a stamp for a dead dispatch cannot be mistaken for
//!   a live one.
//! - **Descendant reaping** terminates what a finished member left running.
//! - The **successor contract** is how a process meant to outlive its launcher
//!   sheds an inherited stamp: it forks and `exec`s under a scratch directory it
//!   claims itself, and is judged by its own liveness thereafter.

use std::time::Duration;

/// Overrides [`DEFAULT_HEARTBEAT_TIMEOUT`].
pub const HEARTBEAT_TIMEOUT_ENV: &str = "ONEAGENTGRAPH_HEARTBEAT_TIMEOUT";

/// How long a member may go without a heartbeat before it is declared dead.
pub const DEFAULT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(60);

/// Overrides [`DEFAULT_STALL_TIMEOUT`].
pub const STALL_TIMEOUT_ENV: &str = "ONEAGENTGRAPH_STALL_TIMEOUT";

/// How long a member may record nothing before the activity watchdog fires.
///
/// Half an hour, because the two errors this number chooses between are
/// different sizes: over-waiting on a wedged member costs minutes, and the
/// heartbeat rule and `cancel` still answer for it, while condemning a working
/// one destroys everything it has not emitted — and a member writing a long
/// report has emitted nothing until it lands. Ten minutes sat inside that, where
/// five members were condemned mid-report.
///
/// Streamed provider output is deliberately not counted as activity;
/// [`crate::member::Stall`] records that decision and what it rests on.
//
// llmlint: ignore-block[changed_behavior_has_e2e] observing the window between
// this value and the one it replaces means holding a member silent for over ten
// minutes, which outlasts the whole suite. Either side of it is covered:
// `tests/e2e/liveness.rs` drives both a run supervised under this constant and a
// member the rule still condemns, and `member::tests` drives the rule at this
// constant itself.
pub const DEFAULT_STALL_TIMEOUT: Duration = Duration::from_secs(1800);
// llmlint: ignore-end[changed_behavior_has_e2e]

/// The lock file, inside a scratch directory, that proves who owns it.
pub const OWNER_LOCK_FILE: &str = "owner.lock";
