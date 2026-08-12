//! Construction shared by every harness turn this process launches directly.

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
