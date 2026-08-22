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
/// **Half an hour, because the two errors this number chooses between are not
/// the same size.** Over-waiting on a member that really is wedged costs minutes
/// of one member's time, and it is not even the only thing watching: the
/// heartbeat rule and `cancel` both still answer for that member. Condemning one
/// that is working destroys everything it has not yet emitted, and a member
/// whose deliverable is a long written report has emitted *nothing* until it
/// finishes — a killed report leaves no partial artifact to recover, and the
/// members waiting on it are skipped along with it. The errors are minutes
/// against a whole dispatch, so occasional over-waiting is the cheaper one and
/// this bound is set past where the expensive one starts.
///
/// The number is where the measurement put it. Five members were condemned at
/// 601 seconds — the ten-minute bound this replaces, plus the probe that
/// established it — each with sixty to a hundred completed tool calls behind it
/// and a 20-30k-token report in flight, which is five to ten minutes of
/// continuous generation that publishes nothing. One of them, relaunched on the
/// same task under `ONEAGENTGRAPH_STALL_TIMEOUT=2400`, then went quiet for
/// 14m43s and finished. Half an hour clears the longest silence anyone has
/// measured a *working* member holding, with the margin the measurement's
/// small sample deserves.
///
/// **Streamed provider output is not counted as activity, because at this
/// crate's pins there is no such signal to count.** That decision, and the
/// versions it was read from, are recorded at [`crate::member::Stall`] — it
/// belongs beside the rule that would consume the signal rather than beside the
/// bound that stands in for it.
pub const DEFAULT_STALL_TIMEOUT: Duration = Duration::from_secs(1800);

/// The lock file, inside a scratch directory, that proves who owns it.
pub const OWNER_LOCK_FILE: &str = "owner.lock";
