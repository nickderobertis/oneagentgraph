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
pub const DEFAULT_STALL_TIMEOUT: Duration = Duration::from_secs(600);

/// The lock file, inside a scratch directory, that proves who owns it.
pub const OWNER_LOCK_FILE: &str = "owner.lock";
