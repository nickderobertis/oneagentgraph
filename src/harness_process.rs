//! Construction shared by every harness turn this process launches directly.
//!
//! # Why this is still a process, against `oneharness_core::io::run::run`
//!
//! Not for want of an entrypoint. That call returns the report, takes an event
//! sink, and takes a cancel token — the three things `docs/contract.md` names as
//! collapsing this hop — and every argument the two callers here build maps onto
//! a `RunRequest` field:
//!
//! | argument | field |
//! |---|---|
//! | `--config <p>` | `config` |
//! | `--cwd <d>` | `cwd` |
//! | `--events` | `events` |
//! | `--stream` | `stream: Some(true)` |
//! | `--prompt <text>` | `prompt` |
//! | `--compact` | none, by design: it is how the CLI *prints* a report |
//!
//! Three things hold the hop open, none of them on the argv:
//!
//! 1. **A per-member environment.** `crate::member` spawns with
//!    `env_remove(`[`crate::invoke::PROCESS_WIDE_HARNESS_ENV`]`)`. A library run
//!    reads *this* process's environment, which every member shares, and
//!    `RunRequest::no_config` discards `RunRequest::config` along with the
//!    `ONEHARNESS_*` layer — so there is no way to keep the graph's file and drop
//!    the variable that overrides it. Converting without this spends another
//!    subscription silently. *Proposal:* an env-layer opt-out that keeps `config`.
//! 2. **A tree to hold.** [`crate::scratch::Group`] is joined at the spawn, and
//!    `run::EventSink` is the crate's only public trait. On POSIX the group is the
//!    [`crate::scratch::SCRATCH_ENV`] stamp and `RunRequest::env` carries it; on
//!    Windows it is a job object, which needs the `Child` an in-process call never
//!    yields. *Proposal:* a spawn hook on `RunControls`, mirroring
//!    `onejudge::SpawnHook` — which is how `crate::judge` groups an in-process
//!    two-party member today.
//! 3. **The contract's child-process facts.** `runner: process` with its
//!    `program`/`args`/`cwd`, and `exit_code`/`disposition`/`stderr_tail` on
//!    `member-died`, which `docs/contract.md` scopes to a member that was one.
//!    That is a proposal to the contract's owner, not to oneharness.

use std::process::{Command, Stdio};

/// A harness-turn command which cannot consume the caller's input.
///
/// Keeping this in the constructor means a new direct launch starts safe. A
/// call site has to deliberately replace stdin to make a turn interactive.
pub(crate) fn command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.stdin(Stdio::null());
    command
}
