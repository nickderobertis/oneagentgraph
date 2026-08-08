//! End-to-end journeys against the compiled binary.
//!
//! Every test here spawns the real `oneagentgraph` executable as a subprocess
//! and asserts on its exit code, stdout, and stderr — the way a user reaches it.
//! Nothing is stubbed: at the interface-only stage the product *is* the argument
//! surface and the refusal, so that is what these drive.

use assert_cmd::Command;
use predicates::prelude::*;

/// The exit code the interface-only build refuses with, distinct from every code
/// the contract assigns (`0`, `1`, `2`).
const NOT_IMPLEMENTED: i32 = 3;

/// clap's exit code for a usage error — a command line the surface does not
/// accept, rejected before anything is attempted.
const USAGE_ERROR: i32 = 2;

/// The compiled binary under test, resolved by cargo rather than by PATH.
fn oneagentgraph() -> Command {
    Command::new(env!("CARGO_BIN_EXE_oneagentgraph"))
}

/// Every command the contract documents, with a minimal legal invocation.
const COMMANDS: &[(&str, &[&str])] = &[
    ("run", &["run", "graph.yaml"]),
    ("validate", &["validate", "graph.yaml"]),
    ("trigger", &["trigger", "run-1", "worker"]),
    ("reset-timer", &["reset-timer", "run-1", "reporter"]),
    ("cancel", &["cancel", "run-1"]),
    ("history", &["history"]),
    ("health", &["health"]),
    ("smoke", &["smoke"]),
    ("persona", &["persona", "new", "engineer"]),
];

#[test]
fn help_lists_every_documented_command() {
    let assert = oneagentgraph().arg("--help").assert().success();
    let help = String::from_utf8(assert.get_output().stdout.clone()).expect("help is UTF-8");

    for (name, _) in COMMANDS {
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
fn every_command_parses_and_then_refuses_loudly() {
    for (name, argv) in COMMANDS {
        let assert = oneagentgraph()
            .args(*argv)
            .assert()
            .code(NOT_IMPLEMENTED)
            .stderr(predicate::str::contains("NOT IMPLEMENTED"))
            .stderr(predicate::str::contains(*name))
            .stderr(predicate::str::contains("ACTION:"));

        assert!(
            assert.get_output().stdout.is_empty(),
            "`{name}` wrote to stdout; a caller must never read a refusal as an event stream"
        );
    }
}

#[test]
fn run_accepts_every_documented_flag_before_refusing() {
    oneagentgraph()
        .args([
            "run",
            "https://example.com/graph.yaml",
            "--task",
            "do the thing",
            "--task-file",
            "task.md",
            "--dir",
            ".",
            "--label",
            "run_id=R",
            "--label",
            "round=2",
            "--set",
            "members.worker.agent.model=some-model",
            "--output",
            "text",
            "--detach",
        ])
        .assert()
        .code(NOT_IMPLEMENTED)
        .stderr(predicate::str::contains("`run`"));
}

#[test]
fn history_takes_either_a_run_or_a_record_to_show() {
    for argv in [vec!["history", "run-1"], vec!["history", "show", "rec-9"]] {
        oneagentgraph()
            .args(&argv)
            .assert()
            .code(NOT_IMPLEMENTED)
            .stderr(predicate::str::contains("`history`"));
    }
}

#[test]
fn cancel_takes_an_optional_member_and_the_kill_flag() {
    oneagentgraph()
        .args(["cancel", "run-1", "worker", "--kill"])
        .assert()
        .code(NOT_IMPLEMENTED)
        .stderr(predicate::str::contains("`cancel`"));
}

#[test]
fn persona_validate_takes_a_path() {
    oneagentgraph()
        .args(["persona", "validate", "personas/engineer.yaml"])
        .assert()
        .code(NOT_IMPLEMENTED)
        .stderr(predicate::str::contains("`persona`"));
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
fn a_subcommands_help_documents_its_own_flags() {
    oneagentgraph()
        .args(["run", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--task-file"))
        .stdout(predicate::str::contains("--detach"));
}
