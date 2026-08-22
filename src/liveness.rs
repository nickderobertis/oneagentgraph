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
/// **Half an hour, because the two errors this number chooses between are
/// different sizes.** Waiting out a member that really is wedged costs minutes,
/// and the heartbeat rule and `cancel` still answer for it; condemning one that
/// is working destroys everything it has not yet emitted, and a member writing a
/// long report has emitted nothing until it lands. Five were killed at 601
/// seconds — the ten-minute bound this replaces, plus the probe that established
/// it — each of them mid-report; one, relaunched under a 2400-second bound, then
/// went quiet for 14m43s and finished.
///
/// Streamed provider output is deliberately not counted as activity;
/// [`crate::member::Stall`] records that decision and what it rests on.
//
// llmlint: ignore-block[changed_behavior_has_e2e] the *window* between this
// value and the one it replaces has no journey, because observing it means
// holding a member silent and idle for more than ten minutes — longer than the
// whole suite runs. Both facts either side of it are driven end to end in
// `tests/e2e/liveness.rs`: that a run with no override is supervised under this
// constant, by `the_documented_defaults_are_what_a_run_supervises_under`, which
// fails with the incident's own `rule: activity` death when this number is
// shrunk; and that the rule still condemns an idle member, by
// `a_member_whose_child_is_alive_but_idle_is_still_condemned`. The rule is
// driven at this constant itself in
// `member::tests::the_default_bound_condemns_an_idle_tree_long_after_the_bound_it_replaces`.
pub const DEFAULT_STALL_TIMEOUT: Duration = Duration::from_secs(1800);
// llmlint: ignore-end[changed_behavior_has_e2e]

/// The lock file, inside a scratch directory, that proves who owns it.
pub const OWNER_LOCK_FILE: &str = "owner.lock";
