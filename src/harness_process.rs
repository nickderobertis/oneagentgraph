//! Construction shared by every harness turn this process launches directly.
//!
//! # Why this is still a process, against `oneharness_core::io::run::run`
//!
//! `AGENTS.md` requires a hop that stays to name what the library does not
//! expose, in the code, at the site. This is that site, and the answer is no
//! longer "oneharness has no entrypoint": since 0.7.0 it has exactly the one
//! `docs/contract.md` said the hop would collapse to —
//! [`oneharness_core::io::run::run`], which **returns** the report instead of
//! printing it, takes a `RunControls::events` sink so events arrive as they
//! occur, and takes a `RunControls::cancel` that tears the harness tree down.
//! This crate is on 0.8.0 and calls it nowhere. Both remaining callers were
//! re-checked against that release.
//!
//! **Every argument maps. There is no argv-shaped gap.** `crate::invoke` builds
//! `run --config <p> --cwd <d> --events --stream --prompt <text>` for a
//! single-sided member, and `crate::smoke` builds `run --cwd <d> --compact
//! --prompt <text>`:
//!
//! | argument | `RunRequest` field |
//! |---|---|
//! | `--config <p>` | `config` |
//! | `--cwd <d>` | `cwd` |
//! | `--events` | `events` |
//! | `--stream` | `stream: Some(true)` |
//! | `--prompt <text>` | `prompt` |
//! | `--compact` | none, **by design** — it is how the CLI *prints* a report, and an in-process caller is handed the value |
//!
//! What keeps the hop is not on the argv at all. It is the two things a
//! *subprocess* gave this crate for free, and one thing that is not oneharness's
//! to give:
//!
//! 1. **A per-member environment.** `crate::member` spawns with
//!    `env_remove(`[`crate::invoke::PROCESS_WIDE_HARNESS_ENV`]`)`, because that
//!    one variable beats the per-side config a graph named and the launcher this
//!    crate was extracted from *exports* it around everything it dispatches. A
//!    library run reads the environment of **this** process, which is shared with
//!    every other member and cannot be unset for one of them. `RunRequest` has no
//!    equivalent: `no_config` drops the `ONEHARNESS_*` layer but discards
//!    `RunRequest::config` with it, so there is no way to keep the file the graph
//!    named and drop the environment that overrides it. Converting without this
//!    does not fail a run — it silently spends a different subscription, which is
//!    the one failure class this crate refuses to make invisible.
//!    **Proposal to oneharness:** let a run opt out of the environment layer while
//!    keeping an explicit `config` — an `ignore_env` beside `no_config`, or a
//!    `RunRequest::env` that *replaces* the inherited environment rather than
//!    adding to it.
//! 2. **A tree to hold.** `crate::scratch::Group` is what `cancel --kill`, the
//!    activity watchdog, and `sweep`'s reap all reach a member's descendants
//!    through, and it is joined at the spawn. oneharness spawns each harness
//!    itself, as its own POSIX group leader, and exposes no hook over that: the
//!    only public trait in the crate is `run::EventSink`. On POSIX the loss is
//!    recoverable — the group *is* the [`crate::scratch::SCRATCH_ENV`] stamp, and
//!    `RunRequest::env` carries it to each harness process — but on Windows the
//!    group is a job object, and assigning one needs the `std::process::Child`
//!    that never reaches this crate. A converted single-sided member would be
//!    ungroupable there, which `crate::member` deliberately refuses to start.
//!    **Proposal to oneharness:** a spawn hook on `RunControls`, mirroring
//!    `onejudge::SpawnHook` — which is precisely how `crate::judge` keeps both
//!    sides of an in-process two-party member inside one group today.
//! 3. **The contract, which is not oneharness's to change.** `docs/contract.md`
//!    says a `kind: oneharness` member "is still `oneharness run`, a child
//!    process", publishes `member-started` with `runner: process` and its
//!    `program`/`args`/`cwd`, and dies with `exit_code`, `disposition`, and
//!    `stderr_tail` — "a **child process's** facts, present only for a member that
//!    was one". Collapsing the hop deletes all three from that member's death and
//!    changes `tests/golden/member-died.json` with them. The doc is committed as
//!    approved and the code is written to match it, so this is a proposal to the
//!    planner who owns it rather than an edit made on the way past.
//!
//! `crate::control`'s `oneharness interrupt` is a separate hop with a separate
//! reason, recorded there: the control socket is version-*equal*, so the client
//! has to be the same build as the run that bound it.

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
