//! End-to-end journeys against the compiled binary.
//!
//! Every journey here spawns the real `oneagentgraph` executable as a subprocess
//! and asserts on its exit code, stdout, and stderr — the way a user reaches it.
//! Behind it run the real `onejudge` CLI and the real `oneharness` CLI, also as
//! subprocesses. The **only** thing standing in for something real is the paid
//! harness process, replaced at oneharness's own `ONEHARNESS_BIN_<ID>` seam by
//! this crate's `test-doubles` stand-in, because a model turn is the one genuinely
//! external thing a gate cannot run for free.
//!
//! The journeys themselves are ported from `ai-orchestrator/tests/e2e/`, which is
//! where the accumulated failure knowledge of this system lives — each is named
//! after the thing that once broke.

mod dispatch;
mod liveness;
mod origin;
mod remote;
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

/// Every command the surface is expected to carry, taken from the published
/// smoke script's own list.
///
/// Read from the script rather than from `docs/contract.md` on purpose: this is
/// one link in a chain, and `tests/contract.rs` owns the other — it holds that
/// script's list to the contract document. So the document is parsed in exactly
/// one place, the script in exactly one place, and `--help` is held to both
/// transitively. A second parser of the contract here is the drift this whole
/// arrangement exists to prevent.
fn expected_commands() -> Vec<String> {
    let script = include_str!("../../scripts/smoke-published.sh");
    let loop_line = script
        .lines()
        .find(|line| line.trim_start().starts_with("for command in "))
        .expect("scripts/smoke-published.sh no longer loops over the command list");
    loop_line
        .trim()
        .trim_start_matches("for command in ")
        .trim_end_matches("; do")
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

#[test]
fn help_lists_every_documented_command() {
    let assert = oneagentgraph().arg("--help").assert().success();
    let help = String::from_utf8(assert.get_output().stdout.clone()).expect("help is UTF-8");

    let expected = expected_commands();
    assert!(
        expected.len() >= 9,
        "the published smoke's command list stopped parsing: {expected:?}"
    );
    for name in &expected {
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

/// The help surface, not the flags' behaviour: each of these is driven for real
/// by the journeys in `dispatch.rs`, `verbs.rs`, and `liveness.rs`. What is left
/// unproven by those is that `run --help` still *tells* a caller they exist.
#[test]
fn run_help_lists_every_documented_flag() {
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

/// The published smoke, run for real over the binary this repo just compiled.
///
/// `release.yml` and `published-smoke.yml` run this same file over an artifact
/// installed from PyPI or npm, where a failure is a release that already shipped.
/// Running it here is what makes that a gate rather than a report: the script's
/// own assertions — the version it reports, the command list, the refusals and
/// their exit codes, and the graph `validate` must read — are held to the
/// compiled binary on every `just check`.
///
/// Unix-only because the script is bash and the release legs that run it on
/// Windows do so under a bash CI shell rather than a bare shell this test can
/// assume.
#[cfg(unix)]
#[test]
fn the_published_smoke_passes_against_the_binary_this_repo_compiled() {
    let path_dir = tempfile::tempdir().expect("a PATH directory");
    // The script reaches its subject as `oneagentgraph` on PATH — that is the
    // thing it is written to check — so the compiled binary is put there under
    // that name rather than the script being told where to look.
    std::os::unix::fs::symlink(
        env!("CARGO_BIN_EXE_oneagentgraph"),
        path_dir.path().join("oneagentgraph"),
    )
    .expect("link the binary onto a PATH");

    let existing = std::env::var("PATH").unwrap_or_default();
    let output = std::process::Command::new("bash")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/smoke-published.sh"
        ))
        .args(["--expect-version", env!("CARGO_PKG_VERSION")])
        .args(["--label", "the binary just compiled"])
        .env("PATH", format!("{}:{existing}", path_dir.path().display()))
        .output()
        .expect("bash runs the published smoke");

    assert!(
        output.status.success(),
        "scripts/smoke-published.sh failed against this build\n--- stdout ---\n{}\n--- stderr \
         ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("surface smoke test passed"),
        "the smoke exited 0 without reporting a pass: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
