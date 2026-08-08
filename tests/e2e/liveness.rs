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
// journeys here are platform-gated because their *subject* is a facility only
// that platform has, not because the rule is weaker elsewhere: a descendant
// declining `SIGTERM` is POSIX-only, since Windows has no signal to decline, and
// the forged `owner.job` and the killed launcher are Windows-only, since both
// rest on the job object that stands in there for the environment stamp.
// `src/scratch.rs` documents every one of those mappings, including which
// direction each differs in. Every other journey runs on all three platforms.
// They also read
// `oneagentgraph::scratch` directly, because scratch ownership is a liveness rule
// the contract gives no CLI verb of its own — the sweep a future operator runs is
// this same library call, so this is the interface, not a reach past one. Where a
// verb does answer — `cancel --kill` reporting what it signalled — the journey
// asserts on that instead.

use std::path::Path;

use crate::support::{as_env, bounds, fake_harness, labels, until, Workspace};
// The one Unix-only journey's own helper: on a platform without `SIGTERM` it
// compiles away, and an import left behind is a `-D warnings` build failure
// rather than dead weight.
#[cfg(unix)]
use crate::support::two_party_graph;

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
            "fake:complete-now: the provider dies",
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
            &format!("fake:complete-now: fake:hold={}", release.display()),
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
///
/// The turn hangs rather than completing, and that is what makes the
/// *disposition* an assertion rather than a coin toss. The supervisor refreshes
/// on a 500ms interval, so the rule always fires after the first one — but a
/// task that finishes inside that window has already exited by the time the
/// kill lands, and `member-died` then truthfully reports `exited`. This is the
/// same margin the production default is wide for, read from the other side: on
/// a loaded host the member outlives the window and the kill is what ends it,
/// while on an idle one the turn wins the race. A member that cannot finish
/// leaves the watchdog as the only thing that can end it, on a host of any
/// speed.
#[test]
fn the_heartbeat_rule_condemns_a_member_whose_liveness_cannot_be_confirmed() {
    let workspace = Workspace::new();
    let env = vec![
        ("ONEAGENTGRAPH_HEARTBEAT_TIMEOUT", "0.05".to_string()),
        // Wide, so the rule that fires is the heartbeat and not this one.
        ("ONEAGENTGRAPH_STALL_TIMEOUT", "600".to_string()),
    ];
    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "fake:hang against too tight a margin",
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

/// A member the **activity watchdog** condemns takes its descendants with it.
///
/// The `member-died` event above is the supervisor's *decision*; this is the
/// outcome an operator is actually promised. A member is `onejudge` with
/// `oneharness` under it and the paid provider under that, and condemning the
/// one this supervisor holds leaves the other two running — still billing
/// whoever owns the subscription, with nothing left watching them.
#[test]
fn a_member_the_activity_watchdog_condemns_leaves_no_descendant_running() {
    // The stall bound is wider than the 2s the journey above uses, and the
    // difference is the point: that one asserts on the *event*, which needs only
    // the member, while this one asserts on the *tree*, which has to have
    // launched by the time the rule fires.
    a_condemned_member_leaves_no_descendant_running(
        Some(&single_sided_graph()),
        "activity",
        "60",
        "5",
    );
}

/// The same guarantee for the **heartbeat** rule, which condemns on its own
/// terms and through its own branch of the supervisor.
///
/// Two rules, two `kill_and_report` call sites: a teardown wired into one and
/// not the other is a real regression, and one journey cannot see it.
///
#[test]
fn a_member_the_heartbeat_rule_condemns_leaves_no_descendant_running() {
    a_condemned_member_leaves_no_descendant_running(
        Some(&single_sided_graph()),
        "heartbeat",
        "0.05",
        "600",
    );
}

/// The shallowest real member this crate builds: one agent, no judge, so the
/// doubled provider is the member's own child rather than its grandchild.
///
/// Both condemnation journeys use it, and that is forced rather than chosen. A
/// condemnation races process startup: the rule fires on a clock that starts
/// with the member, and the tree has to be up by then. The heartbeat rule leaves
/// half a second — it is only reachable below the supervisor's refresh cadence —
/// and the two-party chain, three real CLIs deep, did not reach its provider
/// inside fifteen *seconds* under this suite's own load on a CI runner. The
/// two-party tree is condemned by the journeys above and torn down by the cancel
/// journeys below; what these two need is a tree that is reliably *there*.
fn single_sided_graph() -> String {
    format!(
        concat!(
            "version: 1\nname: node-scope\n",
            "env:\n  ONEHARNESS_BIN_CLAUDE_CODE: {fake}\n",
            "members:\n  worker:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
        ),
        fake = fake_harness(),
    )
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
                "fake:complete-now: never gets here",
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
                    &format!("fake:complete-now: fake:hold={}", release.display()),
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
                    &format!("fake:complete-now: fake:hold={}", release.display()),
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
                    &format!("fake:complete-now: fake:hold={}", release.display()),
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

/// A cancelled run reaps a member that published **nothing at all**, and the
/// run itself then returns.
///
/// The other cancel journeys reach a member mid-turn, which is a member whose
/// tree has already announced itself up the pipe. This one never writes a byte:
/// its provider parks on its first turn and stays there, so the only thing that
/// can end the tree is the reap, and the only thing that can end the *run* is
/// the tree's pipes closing when it does. That is one failure on POSIX and two
/// on Windows, where a descendant inherits its launcher's pipe handles outright
/// — a cancel that reached the member alone would leave this supervisor blocked
/// forever on a stream a process it just cancelled is still holding open.
#[test]
fn a_cancelled_run_reaps_a_member_that_published_nothing() {
    let workspace = Workspace::new();
    let state = workspace.state();

    let mut member = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "fake:hang and publish nothing at all",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );

    until("the member's own scratch to be stamped", || {
        member_scratch(&state).is_some_and(|scratch| {
            !oneagentgraph::scratch::stamped_for(&scratch.display().to_string()).is_empty()
        })
    });
    let scratch = member_scratch(&state).expect("a member scratch");

    let run_id = run_id(&state);
    let cancelled = workspace.run(&["cancel", &run_id, "--kill"]);
    cancelled.expect_code(0);
    // The count, not just the word: `0 process(es) signalled` carries it too,
    // and a cancel that found nothing to reap is exactly the failure here.
    assert!(
        cancelled.stdout.contains("signalled") && !cancelled.stdout.contains("0 process(es)"),
        "a cancel of a silent member signalled nothing: {}",
        cancelled.stdout
    );

    until("the stamped processes to be gone", || {
        oneagentgraph::scratch::stamped_for(&scratch.display().to_string()).is_empty()
    });

    // Nothing releases this member, so a run that returns is one whose reap
    // reached every process holding its streams open.
    let status = member.wait().expect("the run exits");
    assert!(
        status.code().is_some(),
        "the cancelled run never exited: its member's tree still holds its streams"
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
                "fake:complete-now: fake:record-prompt={} fake:hold={}",
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

/// A group reaps a descendant **whose parent has already exited** — the process
/// no walk from the member's own pid would ever reach.
///
/// This is the case the whole grouping rule exists for, and the one the two
/// condemnation journeys above turn out not to reach. Measured, not assumed:
/// with the Windows layer compiled out, both of those still passed, because the
/// chain a member is contains its own tree — kill `onejudge` and it ends
/// `oneharness`, which ends the provider, and a detached grandchild goes with
/// them. So the guarantee the *group* adds is only visible where that
/// containment cannot apply: an orphan, with nothing above it left to end it.
///
/// Driven at [`Group`] rather than through a verb because no graph can produce
/// one on demand — the real CLIs decline to leak. The double is still a real
/// subprocess, the orphan is a real detached process, and the group is the same
/// one `run` puts every member in.
#[test]
fn a_group_reaps_a_descendant_whose_parent_has_already_exited() {
    use oneagentgraph::scratch::{Group, SCRATCH_ENV};

    let root = tempfile::tempdir().expect("a workspace");
    let scratch = root.path().join("oneagentgraph-orphaning");
    std::fs::create_dir_all(&scratch).expect("the scratch");
    let ticks = root.path().join("descendant.ticks");

    let group = Group::open(&scratch).expect("a group");
    let mut parked = std::process::Command::new(fake_harness());
    parked
        .args([
            "-p",
            &format!("fake:hang fake:spawn-ticker={}", ticks.display()),
        ])
        // The stamp the POSIX half of a group *is*, applied here the way
        // `member::run` applies it. On Windows the job the spawn goes into
        // carries the same membership, and neither platform needs the other's.
        .env(SCRATCH_ENV, &scratch)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = group.spawn(&mut parked).expect("the parked process starts");

    until("the detached ticker to start", || ticks_written(&ticks) > 0);

    // Orphaned on purpose: the process that started the ticker goes first, so
    // nothing above the ticker is left to end it.
    child.kill().expect("the parked process is killed");
    child.wait().expect("the parked process is reaped");
    let orphaned = ticks_written(&ticks);
    std::thread::sleep(SETTLE);
    assert!(
        ticks_written(&ticks) > orphaned,
        "the ticker stopped when its parent did, so this journey never reached the orphan it \
         asserts on"
    );

    // The group is the only thing that can still reach it.
    let reaped = group.terminate();
    assert!(
        reaped > 0,
        "the group reported reaping nothing, so an orphaned descendant is beyond it"
    );
    let ended = ticks_written(&ticks);
    std::thread::sleep(SETTLE);
    assert_eq!(
        ticks_written(&ticks),
        ended,
        "the group reported reaping {reaped} process(es), but the orphan is still running"
    );
}

/// A finished run leaves nothing of its own running: the reap on the way out is
/// what stops one run's leavings from polluting the next.
#[test]
fn a_finished_run_leaves_nothing_stamped_for_it() {
    let workspace = Workspace::new();
    workspace
        .run_task("fake:complete-now: leave nothing behind")
        .expect_code(0);
    let scratch = member_scratch(&workspace.state()).expect("a member scratch");
    assert!(
        oneagentgraph::scratch::stamped_for(&scratch.display().to_string()).is_empty(),
        "a finished run left processes carrying its stamp"
    );
}

/// A run killed outright still takes its member's tree with it.
///
/// Every other teardown here runs *through* this binary: a watchdog condemns a
/// member, or a `cancel` reaps one. This is the case where none of that can
/// happen — the supervisor is gone before it can do anything — and it is the one
/// that costs money, because a paid harness nobody is watching keeps billing.
///
/// Windows-only, and a strengthening rather than a rule POSIX also holds: a job
/// object is created `KILL_ON_JOB_CLOSE`, so the kernel ends the tree when the
/// last handle to it goes, and a killed process's handles go with it. There is
/// no POSIX equivalent — a `SIGKILL`ed launcher cannot reap, and its descendants
/// are reparented and left running — so asserting this on Unix would assert a
/// guarantee `src/scratch.rs` does not claim there.
#[cfg(windows)]
#[test]
fn a_run_killed_outright_does_not_leak_its_member_s_tree() {
    let workspace = Workspace::new();
    let state = workspace.state();

    // Nothing releases this member and no bound is shortened, so the only thing
    // that can end its tree is the run process going away.
    let mut run = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "fake:hang until this run is killed under it",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );

    until("the member's own scratch to be stamped", || {
        member_scratch(&state).is_some_and(|scratch| {
            !oneagentgraph::scratch::stamped_for(&scratch.display().to_string()).is_empty()
        })
    });
    let scratch = member_scratch(&state).expect("a member scratch");

    // Killed, not cancelled: no signal file, no reap, no chance to clean up.
    run.kill().expect("the run is killed");
    run.wait().expect("the killed run is reaped");

    until("the member's tree to die with its supervisor", || {
        oneagentgraph::scratch::stamped_for(&scratch.display().to_string()).is_empty()
    });
}

/// A forged `owner.job` cannot aim a reap at somebody else's process tree, and
/// the real group beside it still tears down.
///
/// On Windows a scratch directory records the job object its tree belongs to, so
/// that a second process — an operator's `cancel --kill` — can find it. That
/// record is a file, which makes it external input: read as the name to open,
/// its content would choose what `TerminateJobObject` is aimed at, and a record
/// written by anything else would point a cancel at any job object this user can
/// open. So the name is derived from the directory and the record is only
/// honoured when it agrees.
///
/// Both halves are asserted, because either alone passes for the wrong reason: a
/// reap that refused everything would satisfy the first, and one that trusted
/// the file would satisfy the second.
#[cfg(windows)]
#[test]
fn a_forged_group_record_cannot_redirect_a_reap() {
    use oneagentgraph::scratch::Group;

    let root = tempfile::tempdir().expect("a workspace");
    let legitimate = root.path().join("oneagentgraph-legitimate");
    let forged = root.path().join("oneagentgraph-forged");
    std::fs::create_dir_all(&legitimate).expect("the real scratch");
    std::fs::create_dir_all(&forged).expect("the forged scratch");

    // A real group with a real process parked in it: the doubled harness, driven
    // straight rather than through a run, because the subject here is the group
    // rather than anything a graph does with one.
    let group = Group::open(&legitimate).expect("a group");
    let mut parked = std::process::Command::new(fake_harness());
    parked
        .args(["-p", "fake:hang so the group has something to hold"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = group.spawn(&mut parked).expect("the parked process starts");

    let stamp = legitimate.display().to_string();
    until("the parked process to join its group", || {
        !oneagentgraph::scratch::stamped_for(&stamp).is_empty()
    });

    // The forgery: the real group's name, in a directory that has no claim to
    // it. This is the record `cancel --kill` would read on its way to a reap.
    let stolen =
        std::fs::read_to_string(legitimate.join("owner.job")).expect("the group recorded itself");
    std::fs::write(forged.join("owner.job"), &stolen).expect("plant the forged record");

    assert_eq!(
        oneagentgraph::scratch::reap(&forged),
        0,
        "a forged record was honoured, so a reap reached a tree it does not own"
    );
    assert!(
        !oneagentgraph::scratch::stamped_for(&stamp).is_empty(),
        "a reap of {} killed the tree {} owns",
        forged.display(),
        legitimate.display()
    );
    assert!(
        child
            .try_wait()
            .expect("the parked process is waitable")
            .is_none(),
        "the parked process was terminated through a directory that never launched it"
    );

    // And the directory that does own the group still tears it down, so what the
    // check refuses is the forgery rather than the mechanism.
    assert!(
        oneagentgraph::scratch::reap(&legitimate) > 0,
        "the real record was refused along with the forged one"
    );
    until("the real group's processes to be gone", || {
        oneagentgraph::scratch::stamped_for(&stamp).is_empty()
    });
    let status = child.wait().expect("the parked process is reaped");
    assert!(
        !status.success(),
        "a terminated process reported a clean exit: {status:?}"
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

/// How long a reaped descendant is watched for a tick it should no longer be
/// writing.
///
/// Thirty times the double's own 50ms cadence, so a descendant that survived has
/// had every chance to say so — the assertion below is an *absence*, and a
/// window near the cadence would read a slow host as a successful teardown.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(1_500);

/// How long may be spent *reaching* a live descendant before the journey gives
/// up and says it never saw one.
///
/// A budget rather than a count, because the two rules below cost very different
/// amounts per attempt — a stall bound is waited out and a heartbeat one fires
/// in half a second — and what should be shared is the patience, not the number.
const REACH_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// Condemn a member by `rule` and hold that its descendant stopped running.
///
/// The witness is a **detached** descendant, and both halves of that are load
/// bearing.
///
/// *Detached*, because the chain tears itself down without any help: kill
/// `onejudge` and it ends `oneharness`, which ends the double. A journey watching
/// those goes green whether or not this supervisor did anything, which is a test
/// that proves nothing — measured, not assumed: the first version of this one
/// watched the double and passed with the whole Windows layer compiled out. What
/// no cascade reaches is a process whose parent has already exited, so the double
/// leaves one behind, and that is also the real hazard — a harness that forks a
/// background worker and dies.
///
/// *A descendant*, because the alternatives cannot answer. A pid is not reachable
/// from outside the run, and this crate's own `stamped_for` is the facility under
/// test — on the platform this is newest on it answered "nothing is running"
/// whether or not anything was, so an assertion resting on it goes green against
/// exactly the leak it is written to catch. So the descendant answers for itself:
/// it appends to a file while it lives, and a file that stops growing once the
/// run returns is a tree that is gone.
///
/// The retry is how the *precondition* is reached, not tolerance for a flaky
/// assertion. A condemnation is a race against process startup by construction:
/// the rule fires on a clock that starts when the member does, and the tree this
/// watches has to exist by then. An attempt where it did not is an attempt that
/// never reached the state under test, so it is retried rather than passed —
/// quietly passing on one is exactly the vacuous green this journey exists to
/// avoid, and running out of budget says so instead of going green.
fn a_condemned_member_leaves_no_descendant_running(
    graph: Option<&str>,
    rule: &str,
    heartbeat: &str,
    stall: &str,
) {
    let give_up_at = std::time::Instant::now() + REACH_BUDGET;
    for attempt in 1.. {
        let workspace = Workspace::new();
        if let Some(document) = graph {
            workspace.graph(document);
        }
        let ticks = workspace.at("descendant.ticks");
        let env = bounds(heartbeat, stall);
        let run = workspace.run_with(
            &[
                "run",
                "./graph.yaml",
                "--task",
                &format!("fake:hang fake:spawn-ticker={}", ticks.display()),
                "--dir",
                &workspace.dir().display().to_string(),
            ],
            &as_env(&env),
        );
        run.expect_code(1);

        let died = run.of_kind("member-died");
        assert_eq!(died.len(), 1, "{:?}", run.kinds());
        assert_eq!(
            died[0]["payload"]["rule"],
            serde_json::json!(rule),
            "a {rule} bound condemned by another rule: {}",
            died[0]["payload"]
        );

        // Nothing to hold against the teardown: the descendant never launched
        // inside the window this rule condemns in, so this run is not evidence.
        let ticked = ticks_written(&ticks);
        if ticked == 0 {
            assert!(
                std::time::Instant::now() < give_up_at,
                "no descendant was live when the {rule} rule fired, across {attempt} runs — this \
                 journey asserts on a tree that was running, and never saw one"
            );
            continue;
        }

        std::thread::sleep(SETTLE);
        assert_eq!(
            ticks_written(&ticks),
            ticked,
            "attempt {attempt}: the {rule} rule condemned the member and reported it, but its \
             descendant is still running — the provider under a condemned member keeps billing \
             with nothing left watching it\n--- stdout ---\n{}",
            run.stdout
        );
        return;
    }
}

/// How much the descendant has written so far, or nothing when it never wrote.
fn ticks_written(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
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
