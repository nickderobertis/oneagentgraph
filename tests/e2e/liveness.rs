//! Liveness journeys, ported from ai-orchestrator's `test_liveness_e2e.py`,
//! `test_oneharness_timeout_e2e.py`, and the dispatch-scratch and leak-guard
//! halves of `test_scratch_e2e.py` / `test_leak_guard_e2e.py`.
//!
//! What is being held here is the contract's own sentence: "heartbeat wrapper
//! (default deadline 60s, `ONEAGENTGRAPH_HEARTBEAT_TIMEOUT`), activity watchdog
//! (default 600s, `ONEAGENTGRAPH_STALL_TIMEOUT`), scratch ownership via
//! `owner.lock` flock + pid-with-start-token, descendant reaping, successor
//! contract for processes meant to outlive their launcher."
//!
//! Every one of these was learned from an incident, so each journey drives the
//! failure rather than the happy path: a member that stops publishing, one that
//! is killed, a sweep racing a live run, and processes left behind by a member
//! whose parent is already gone.

use std::path::Path;

use crate::support::{as_env, fake_harness, labels, until, Workspace};

/// A member that publishes nothing is condemned by the activity watchdog, and
/// the death says which rule fired and what the process left behind.
///
/// The stall bound is shortened rather than waited out: what is under test is
/// the rule, and the contract's own default is asserted separately below.
#[test]
fn a_member_that_publishes_nothing_is_condemned_by_the_activity_watchdog() {
    let workspace = Workspace::new();
    let env = vec![
        ("ONEAGENTGRAPH_STALL_TIMEOUT", "2".to_string()),
        ("ONEAGENTGRAPH_HEARTBEAT_TIMEOUT", "60".to_string()),
    ];
    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "fake:hang and never answer",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &as_env(&env),
    );
    run.expect_code(1);

    let died = run.of_kind("member-died");
    assert_eq!(died.len(), 1, "{:?}", run.kinds());
    let payload = &died[0]["payload"];
    assert_eq!(payload["rule"], serde_json::json!("activity"));
    // A member the watchdog killed did not choose to stop, so the disposition
    // is what separates it from an exit status the member itself returned.
    assert_eq!(payload["disposition"], serde_json::json!("signaled"));
    assert!(payload.get("stderr_tail").is_some(), "{payload}");
    assert_eq!(labels(&died[0])["member"], "worker");
    assert_eq!(
        workspace.record()["members"]["worker"],
        serde_json::json!("died (activity)")
    );
}

/// A member whose harness process exits without publishing a report dies as a
/// provider failure, carrying the exit code and the stderr that names it.
///
/// This is the distinction `worker-died` exists for: provider throttling, an OOM
/// kill, and a genuine crash otherwise all reach a supervisor as the same dead
/// process.
#[test]
fn a_provider_failure_carries_its_exit_code_and_stderr_tail() {
    let workspace = Workspace::new();
    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "complete-now: the provider dies",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[("FAKE_HARNESS_CRASH", "1")],
    );
    run.expect_code(1);

    let died = run.of_kind("member-died");
    assert_eq!(died.len(), 1, "{:?}", run.kinds());
    let payload = &died[0]["payload"];
    assert_eq!(payload["rule"], serde_json::json!("provider-failure"));
    assert_eq!(payload["disposition"], serde_json::json!("exited"));
    assert_eq!(payload["exit_code"], serde_json::json!(2));
    let tail = payload["stderr_tail"].as_str().unwrap_or_default();
    assert!(!tail.is_empty(), "a death with no evidence: {payload}");
    assert!(
        tail.len() <= 4096,
        "the stderr tail outgrew its documented bound"
    );
}

/// A live member publishes a heartbeat, so a consumer can tell a working member
/// from a dead one across a turn that produces nothing else.
#[test]
fn a_live_member_publishes_a_heartbeat_while_its_turn_runs() {
    let workspace = Workspace::new();
    let release = workspace.at("release");
    let env = vec![("ONEAGENTGRAPH_HEARTBEAT_TIMEOUT", "4".to_string())];

    let releaser = {
        let release = release.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(3));
            std::fs::write(&release, "go").expect("release");
        })
    };
    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            &format!("complete-now: fake:hold={}", release.display()),
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &as_env(&env),
    );
    releaser.join().expect("releaser");
    run.expect_code(0);
    assert!(
        !run.of_kind("member-heartbeat").is_empty(),
        "a member held mid-turn published no heartbeat: {:?}",
        run.kinds()
    );
}

/// A bound nobody meant refuses the run rather than supervising under it — and
/// the refusal names the variable, so an operator knows which one to fix.
#[test]
fn an_unusable_liveness_bound_refuses_the_run_by_name() {
    let workspace = Workspace::new();
    for (variable, value) in [
        ("ONEAGENTGRAPH_STALL_TIMEOUT", "0"),
        ("ONEAGENTGRAPH_STALL_TIMEOUT", "soon"),
        ("ONEAGENTGRAPH_HEARTBEAT_TIMEOUT", "-1"),
    ] {
        let run = workspace.run_with(
            &[
                "run",
                "./graph.yaml",
                "--task",
                "complete-now: never gets here",
                "--dir",
                &workspace.dir().display().to_string(),
            ],
            &[(variable, value)],
        );
        run.expect_code(2);
        assert!(
            run.stderr.contains(variable),
            "{variable}={value}: {}",
            run.stderr
        );
        assert!(
            run.stderr.contains("positive number of seconds"),
            "{}",
            run.stderr
        );
    }
}

/// A run holds an exclusive `owner.lock` on its scratch for as long as it is
/// live, which is the proof a sweeper asks for — and the pid-with-start-token
/// beside it is what stops a recycled number from pinning it forever.
#[cfg(unix)]
#[test]
fn a_live_run_holds_its_scratch_against_a_sweep() {
    let workspace = Workspace::new();
    let release = workspace.at("release");
    let state = workspace.state();

    let handle = {
        let workspace_dir = workspace.path().to_path_buf();
        let state = state.clone();
        let release = release.clone();
        std::thread::spawn(move || {
            let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_oneagentgraph"));
            command
                .args([
                    "run",
                    "./graph.yaml",
                    "--task",
                    &format!("complete-now: fake:hold={}", release.display()),
                    "--dir",
                    &workspace_dir.join("work").display().to_string(),
                ])
                .current_dir(&workspace_dir)
                .env("ONEAGENTGRAPH_STATE_DIR", &state)
                .env("ONEAGENTGRAPH_ONEJUDGE_BIN", crate::support::onejudge_bin())
                .env(
                    "ONEAGENTGRAPH_ONEHARNESS_BIN",
                    crate::support::oneharness_bin(),
                )
                .env_remove("ONEHARNESS_HARNESSES");
            command.output().expect("the run finishes")
        })
    };

    until("the run to claim its scratch", || {
        first_lock(&state).is_some()
    });
    let lock = first_lock(&state).expect("a lock");

    // The kernel's own answer to "can anything still be using this?": while the
    // run holds the directory, a sweeper's non-blocking exclusive acquisition
    // is refused.
    assert!(
        oneagentgraph::scratch::reclaimable(lock.parent().expect("the run root")).is_err(),
        "a live run's scratch was offered up to a sweep"
    );
    let recorded = std::fs::read_to_string(&lock).expect("the lock records its owner");
    let mut parts = recorded.split_whitespace();
    let pid: i32 = parts.next().expect("a pid").parse().expect("a number");
    let token: u64 = parts
        .next()
        .expect("a start token")
        .parse()
        .expect("a number");
    assert!(
        pid > 0 && token > 0,
        "the lock recorded {recorded:?}, which identifies nothing"
    );

    std::fs::write(&release, "go").expect("release");
    let output = handle.join().expect("the run thread");
    assert_eq!(output.status.code(), Some(0));

    // Once the run is gone, both proofs clear and the directory is reclaimable —
    // which is what makes an unattended sweep safe rather than destructive.
    assert_eq!(
        oneagentgraph::scratch::reclaimable(lock.parent().expect("the run root")),
        Ok(())
    );
}

/// A member's descendants carry the run's scratch stamp, and `cancel --kill`
/// reaps exactly those — the evidence the kernel fixes at `exec`, which reaches
/// a descendant whose parent has already exited.
#[cfg(unix)]
#[test]
fn a_cancelled_run_reaps_the_processes_stamped_for_it() {
    let workspace = Workspace::new();
    let release = workspace.at("release");
    let state = workspace.state();

    let handle = {
        let workspace_dir = workspace.path().to_path_buf();
        let state = state.clone();
        let release = release.clone();
        std::thread::spawn(move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_oneagentgraph"))
                .args([
                    "run",
                    "./graph.yaml",
                    "--task",
                    &format!("complete-now: fake:hold={}", release.display()),
                    "--dir",
                    &workspace_dir.join("work").display().to_string(),
                ])
                .current_dir(&workspace_dir)
                .env("ONEAGENTGRAPH_STATE_DIR", &state)
                .env("ONEAGENTGRAPH_ONEJUDGE_BIN", crate::support::onejudge_bin())
                .env(
                    "ONEAGENTGRAPH_ONEHARNESS_BIN",
                    crate::support::oneharness_bin(),
                )
                .env_remove("ONEHARNESS_HARNESSES")
                .output()
                .expect("the run finishes")
        })
    };

    until("the member's own scratch to be stamped", || {
        member_scratch(&state).is_some_and(|scratch| {
            !oneagentgraph::scratch::stamped_for(&scratch.display().to_string()).is_empty()
        })
    });
    let scratch = member_scratch(&state).expect("a member scratch");
    let before = oneagentgraph::scratch::stamped_for(&scratch.display().to_string());
    assert!(!before.is_empty(), "no process carried the member's stamp");

    let run_id = run_id(&state);
    let cancelled = workspace.run(&["cancel", &run_id, "worker", "--kill"]);
    cancelled.expect_code(0);
    assert!(
        cancelled.stdout.contains("signalled"),
        "{}",
        cancelled.stdout
    );

    until("the stamped processes to be gone", || {
        oneagentgraph::scratch::stamped_for(&scratch.display().to_string()).is_empty()
    });

    // The run itself ends rather than hanging on a member nobody will release.
    std::fs::write(&release, "go").ok();
    let output = handle.join().expect("the run thread");
    assert!(
        output.status.code().is_some(),
        "the cancelled run never exited"
    );
}

/// A finished run leaves nothing of its own running: the reap on the way out is
/// what stops one run's leavings from polluting the next.
#[cfg(unix)]
#[test]
fn a_finished_run_leaves_nothing_stamped_for_it() {
    let workspace = Workspace::new();
    workspace
        .run_task("complete-now: leave nothing behind")
        .expect_code(0);
    let scratch = member_scratch(&workspace.state()).expect("a member scratch");
    assert!(
        oneagentgraph::scratch::stamped_for(&scratch.display().to_string()).is_empty(),
        "a finished run left processes carrying its stamp"
    );
}

/// The contract's defaults are what a run uses when nothing overrides them.
#[test]
fn the_documented_defaults_are_what_a_run_supervises_under() {
    use oneagentgraph::liveness::{
        DEFAULT_HEARTBEAT_TIMEOUT, DEFAULT_STALL_TIMEOUT, HEARTBEAT_TIMEOUT_ENV, STALL_TIMEOUT_ENV,
    };
    assert_eq!(DEFAULT_HEARTBEAT_TIMEOUT.as_secs(), 60);
    assert_eq!(DEFAULT_STALL_TIMEOUT.as_secs(), 600);
    assert_eq!(HEARTBEAT_TIMEOUT_ENV, "ONEAGENTGRAPH_HEARTBEAT_TIMEOUT");
    assert_eq!(STALL_TIMEOUT_ENV, "ONEAGENTGRAPH_STALL_TIMEOUT");
    assert!(!fake_harness().is_empty());
}

/// The first `owner.lock` under a state directory, once a run has claimed one.
fn first_lock(state: &Path) -> Option<std::path::PathBuf> {
    let lock = std::fs::read_dir(state)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join("owner.lock"))
        .find(|path| path.exists())?;
    Some(lock)
}

/// The `worker` member's own scratch under a state directory.
fn member_scratch(state: &Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(state)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join("members").join("worker"))
        .find(|path| path.exists())
}

/// The one run a state directory holds.
fn run_id(state: &Path) -> String {
    std::fs::read_dir(state)
        .expect("state")
        .flatten()
        .find(|entry| entry.path().join("record.json").exists())
        .expect("a recorded run")
        .file_name()
        .to_string_lossy()
        .into_owned()
}
