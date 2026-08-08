//! End-to-end journeys against the compiled binary.
//!
//! Every journey here spawns the real `oneagentgraph` executable as a subprocess
//! and asserts on its exit code, stdout, and stderr — the way a user reaches it.
//! Behind it run the real `onejudge` CLI and the real `oneharness` CLI, also as
//! subprocesses. The **only** thing standing in for something real is the paid
//! harness process, replaced at oneharness's own `ONEHARNESS_BIN_<ID>` seam by
//! this crate's `fake-provider` double, because a model turn is the one genuinely
//! external thing a gate cannot run for free.
//!
//! The journeys themselves are ported from `ai-orchestrator/tests/e2e/`, which is
//! where the accumulated failure knowledge of this system lives — each is named
//! after the thing that once broke.

mod dispatch;
mod liveness;
mod selection;
mod support;
mod verbs;

use assert_cmd::Command;
use predicates::prelude::*;

/// clap's exit code for a usage error — a command line the surface does not
/// accept, rejected before anything is attempted. It coincides with the
/// contract's `2` for an invalid config, and deliberately so: both are "this
/// invocation was never going to run."
const USAGE_ERROR: i32 = 2;

/// The compiled binary under test, resolved by cargo rather than by PATH.
fn oneagentgraph() -> Command {
    Command::new(env!("CARGO_BIN_EXE_oneagentgraph"))
}

/// Every command the contract documents.
const COMMANDS: &[&str] = &[
    "run",
    "validate",
    "trigger",
    "reset-timer",
    "cancel",
    "history",
    "health",
    "smoke",
    "persona",
];

#[test]
fn help_lists_every_documented_command() {
    let assert = oneagentgraph().arg("--help").assert().success();
    let help = String::from_utf8(assert.get_output().stdout.clone()).expect("help is UTF-8");

    for name in COMMANDS {
        assert!(
            help.contains(name),
            "`--help` does not mention `{name}`:\n{help}"
        );
    }
}

#[test]
fn version_reports_the_crate_version() {
    oneagentgraph()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn run_accepts_every_documented_flag() {
    oneagentgraph()
        .args(["run", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--task-file"))
        .stdout(predicate::str::contains("--label"))
        .stdout(predicate::str::contains("--set"))
        .stdout(predicate::str::contains("--output"))
        .stdout(predicate::str::contains("--detach"))
        .stdout(predicate::str::contains("--dir"));
}

#[test]
fn an_unknown_command_is_a_usage_error_that_names_it() {
    oneagentgraph()
        .arg("orchestrate")
        .assert()
        .code(USAGE_ERROR)
        .stderr(predicate::str::contains("orchestrate"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn a_missing_required_argument_is_a_usage_error_that_names_it() {
    oneagentgraph()
        .arg("validate")
        .assert()
        .code(USAGE_ERROR)
        .stderr(predicate::str::contains("GRAPH"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn a_missing_member_argument_is_a_usage_error() {
    oneagentgraph()
        .args(["trigger", "run-1"])
        .assert()
        .code(USAGE_ERROR)
        .stderr(predicate::str::contains("MEMBER"));
}

#[test]
fn an_unsupported_output_format_is_a_usage_error_naming_the_supported_ones() {
    oneagentgraph()
        .args(["run", "graph.yaml", "--output", "yaml"])
        .assert()
        .code(USAGE_ERROR)
        .stderr(predicate::str::contains("json"))
        .stderr(predicate::str::contains("text"));
}

#[test]
fn no_arguments_at_all_is_a_usage_error_that_shows_the_surface() {
    oneagentgraph()
        .assert()
        .code(USAGE_ERROR)
        .stderr(predicate::str::contains("Usage:"));
}

#[test]
fn cancel_takes_an_optional_member_and_the_kill_flag() {
    oneagentgraph()
        .args(["cancel", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--kill"))
        .stdout(predicate::str::contains("MEMBER"));
}
