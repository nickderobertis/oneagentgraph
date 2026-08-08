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

// llmlint: ignore-file[e2e_not_mocked, live_tier_compiles_and_requires_credential, tests_mirror_real_usage] see tests/e2e/support.rs for the single sanctioned double; three
// journeys here are `#[cfg(unix)]` because their *subject* is a kernel
// facility: an `flock` on `owner.lock`, a `/proc` environment stamp no process
// can shed, and a signal to a proven identity. `src/scratch.rs` documents the
// degraded contract on a platform without them, and compiling these there would
// assert behaviour this crate deliberately does not promise. They also read
// `oneagentgraph::scratch` directly, because scratch ownership is a liveness rule
// the contract gives no CLI verb of its own — the sweep a future operator runs is
// this same library call, so this is the interface, not a reach past one. Where a
// verb does answer — `cancel --kill` reporting what it signalled — the journey
// asserts on that instead.

use std::path::Path;

use crate::support::{as_env, fake_harness, labels, two_party_graph, until, Workspace};

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

/// The heartbeat rule is the other watchdog, and it fires on its own terms: a
/// member whose liveness this supervisor could not confirm inside the deadline
/// is declared dead, whatever it was publishing.
///
/// The deadline is set below the refresh cadence to reach the rule, which is
/// exactly why the production default is far above it: at the cadence itself,
/// the margin reaps healthy members under the load this crate creates.
#[test]
fn the_heartbeat_rule_condemns_a_member_whose_liveness_cannot_be_confirmed() {
    let workspace = Workspace::new();
    let env = vec![
        ("ONEAGENTGRAPH_HEARTBEAT_TIMEOUT", "0.05".to_string()),
        ("ONEAGENTGRAPH_STALL_TIMEOUT", "600".to_string()),
    ];
    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "complete-now: too tight a margin",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &as_env(&env),
    );
    run.expect_code(1);
    let died = run.of_kind("member-died");
    assert_eq!(died.len(), 1, "{:?}", run.kinds());
    assert_eq!(died[0]["payload"]["rule"], serde_json::json!("heartbeat"));
    assert_eq!(
        died[0]["payload"]["disposition"],
        serde_json::json!("signaled")
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

/// `cancel RUN --kill`, with no member named, reaps every process stamped for
/// the run — including one stamped for a member below it.
///
/// The whole-run reap is rooted at the run directory rather than a member's, so
/// it is a different walk from the named-member one below, and the failure it
/// guards against is a live member surviving the cancel of its own run.
#[cfg(unix)]
#[test]
fn a_whole_run_cancel_reaps_every_member_stamped_for_it() {
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

    // No member named: the reap is rooted at the *run*, so it has to reach a
    // process stamped for a member below it rather than only one stamped for
    // the run directory itself.
    let run_id = run_id(&state);
    let cancelled = workspace.run(&["cancel", &run_id, "--kill"]);
    cancelled.expect_code(0);
    assert!(
        cancelled.stdout.contains("signalled"),
        "a whole-run kill signalled nothing: {}",
        cancelled.stdout
    );
    assert!(
        !cancelled.stdout.contains("member"),
        "a whole-run cancel named a member: {}",
        cancelled.stdout
    );

    until("the stamped processes to be gone", || {
        oneagentgraph::scratch::stamped_for(&scratch.display().to_string()).is_empty()
    });

    std::fs::write(&release, "go").ok();
    let output = handle.join().expect("the run thread");
    assert!(
        output.status.code().is_some(),
        "the cancelled run never exited"
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

/// A descendant that refuses `SIGTERM` is stopped anyway: the reap asks once,
/// waits out its grace period, and kills whatever is still there.
///
/// Ported from `test_oneharness_timeout_e2e.py`, whose subject is a process tree
/// that outlives the CLI that returned. Asking is what a process is free to
/// decline, and every other reap journey here uses a double that goes quietly —
/// so the escalation past `SIGTERM`, which is the half that actually guarantees
/// the tree is gone, is only reachable through one that does not.
#[cfg(unix)]
#[test]
fn a_descendant_that_refuses_to_stop_is_killed_anyway() {
    let workspace = Workspace::new();
    // Through the graph's own `env:`, because that is the path a value takes to
    // the member process — and the turn refusing the signal is that process.
    workspace.graph(&two_party_graph(
        &fake_harness(),
        "  FAKE_HARNESS_IGNORE_TERM: \"1\"\n",
    ));
    let release = workspace.at("release");
    let started = workspace.at("turn-started");
    let state = workspace.state();

    let mut member = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            &format!(
                "complete-now: fake:record-prompt={} fake:hold={}",
                started.display(),
                release.display()
            ),
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );

    // The *turn* has to be live, not merely the run: onejudge and oneharness are
    // stamped within a moment of launch and both go quietly, so a reap timed
    // against them never meets the process that refuses. The double records its
    // prompt on its way to the barrier, which is the only signal that says the
    // signal-refusing process itself exists.
    until("the TERM-refusing turn to be parked", || started.exists());
    let scratch = member_scratch(&state).expect("a member scratch");
    assert!(
        !release.exists(),
        "the barrier was released before the reap, so nothing was holding"
    );

    let run_id = run_id(&state);
    workspace.run(&["cancel", &run_id, "--kill"]).expect_code(0);

    // Nothing here can have shut itself down on the first signal: the turn
    // holding this tree open declined it, so an empty stamp is the `SIGKILL`
    // after the grace period and nothing else.
    until("the TERM-refusing processes to be gone", || {
        oneagentgraph::scratch::stamped_for(&scratch.display().to_string()).is_empty()
    });

    // And the run itself returns rather than waiting on a member it just reaped.
    std::fs::write(&release, "go").ok();
    let status = member.wait().expect("the run exits");
    assert!(status.code().is_some(), "the cancelled run never exited");
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
