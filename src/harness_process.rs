//! Construction shared by every harness turn this process launches directly.
//!
//! **Why this is still a process, against `oneharness_core::io::run::run`.** Not
//! for want of an entrypoint: that call returns the report, takes an event sink,
//! and takes a cancel token — `docs/contract.md`'s own collapse condition, met
//! since oneharness 0.7.0. One guarantee holds the hop open, on Windows only:
//! membership of a member's tree is the named job object [`crate::scratch::Group`]
//! opens, joining one needs the `Child` an in-process call never yields, and
//! the linked `oneharness-core`'s `RunControls` exposes no hook between building a
//! harness process and having one. The harness would sit outside its member's
//! group, leaving [`crate::scratch::work`] — the activity watchdog's only Windows
//! evidence — empty, and [`crate::scratch::reap`] nothing to end for a
//! `cancel --kill`.
//!
//! `docs/oneharness-library.md` is the rest: every other guarantee this spawn
//! provides with the seam that replaces it, the upstream proposal that one gap is,
//! and why the conversion is not done for POSIX alone in the meantime. Its
//! argument table is where the argv this module launches is held against a
//! `RunRequest` field by field — including the one flag the two callers here
//! decide rather than always pass, which is whether the turn streams.

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
