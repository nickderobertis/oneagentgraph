//! Liveness journeys, ported from ai-orchestrator's `test_liveness_e2e.py`,
//! `test_oneharness_timeout_e2e.py`, and the dispatch-scratch and leak-guard
//! halves of `test_scratch_e2e.py` / `test_leak_guard_e2e.py`.
//!
//! What is being held here is `docs/contract.md`'s liveness sentence — the
//! heartbeat wrapper, the activity watchdog, scratch ownership, descendant
//! reaping, and the successor contract. It is cited rather than copied: the
//! bounds and names it fixes are gated against this crate's constants by
//! `tests/contract.rs`, and a second prose copy here would only be free to drift
//! from both.
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
// Where a verb answers for a rule, the journey drives the verb: `cancel --kill`
// reports what it signalled, and `sweep` reports what it kept and why. The
// journeys that still read `oneagentgraph::scratch` directly do so to *arrange*
// or *observe* what no verb can — a group holding an orphan, a process table
// checked between two commands — never to stand in for the verb itself.

use std::path::Path;

use crate::support::{
    as_env, bounds, fake_harness, graph_with, labels, until, Workspace, FAKE_HARNESS_KEY,
};
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
    // A member the watchdog condemned did not choose to stop: it was cancelled,
    // and that is what separates it from a member that failed on its own terms.
    assert_eq!(payload["cause"], serde_json::json!("cancelled"));
    assert!(
        payload["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("activity")),
        "a death with no evidence: {payload}"
    );
    // This member was never a process, so it reports none of a process's facts
    // rather than reporting them as null.
    for absent in ["exit_code", "disposition", "stderr_tail"] {
        assert!(
            payload.get(absent).is_none(),
            "an in-process member reported {absent}: {payload}"
        );
    }
    assert_eq!(labels(&died[0])["member"], "worker");
    assert_eq!(
        workspace.record()["members"]["worker"],
        serde_json::json!("died (activity)")
    );
}

/// A member with **live work under it** is not condemned for publishing
/// nothing, however long the silence runs.
///
/// This is the failure the rule was: a supervisory member whose turn is one long
/// child — a whole round — publishes nothing for far longer than the bound while
/// being entirely healthy, and the watchdog killed it and took the live worker
/// underneath it with it. Losing a supervisor is recoverable by adoption; losing
/// the dispatch under it is not.
///
/// The pair with the journey below is the whole proof, and the two differ in one
/// thing only: whether the child does anything. This one's provider consumes CPU
/// behind the barrier and publishes not one line until it is released — three
/// stall bounds later — and the member has to still be alive to answer.
#[test]
fn a_member_whose_child_is_working_is_not_condemned_for_its_silence() {
    let workspace = Workspace::new();
    workspace.graph(&single_sided_graph());
    let release = workspace.at("release");
    let env = bounds("60", &STALL.as_secs_f64().to_string());

    let releaser = {
        let release = release.clone();
        std::thread::spawn(move || {
            std::thread::sleep(SILENT_FOR);
            std::fs::write(&release, "go").expect("release");
        })
    };
    let started = std::time::Instant::now();
    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            &format!(
                "fake:complete-now work without publishing anything fake:work={}",
                release.display()
            ),
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &as_env(&env),
    );
    releaser.join().expect("releaser");
    run.expect_code(0);

    assert!(
        run.of_kind("member-died").is_empty(),
        "a member whose child was working was condemned anyway: {:?}",
        run.of_kind("member-died")
    );
    // The bound really did expire, several times over, while the member was
    // silent — otherwise this journey never reached the state it asserts on.
    assert!(
        started.elapsed() > SILENT_FOR,
        "the member answered before its silence outlasted the bound, so nothing was tested"
    );
    // And it was a real turn either side of that silence, rather than a member
    // that never started: the provider published its first line only once it was
    // released.
    assert!(
        !run.of_kind("turn-activity").is_empty(),
        "the member published nothing at all, so it never took the turn: {:?}",
        run.kinds()
    );
    assert_eq!(
        workspace.record()["members"]["worker"],
        serde_json::json!("settled")
    );
}

/// A member with **no** live work under it is still condemned — even though a
/// process of its own is alive the whole time.
///
/// The other half of the pair above, and the one that stops the fix from being
/// read as switching the watchdog off. Its provider is a real process, running
/// and reachable for the whole bound; what it is not is *working*. A rule that
/// spared any member with a live child would spare this one, and a wedged member
/// is exactly a member whose harness is alive and will never answer.
#[test]
fn a_member_whose_child_is_alive_but_idle_is_still_condemned() {
    let workspace = Workspace::new();
    workspace.graph(&single_sided_graph());
    let entered = workspace.at("provider.started");
    let env = bounds("60", &STALL.as_secs_f64().to_string());

    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            &format!(
                "fake:hang doing nothing at all fake:record-prompt={}",
                entered.display()
            ),
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &as_env(&env),
    );
    run.expect_code(1);

    // The provider really was there to be spared: it records itself on the way
    // to the wait it never returns from. Without this, a condemnation could be
    // of a member whose tree had not started yet — which proves nothing about a
    // rule that reads the tree.
    assert!(
        entered.exists(),
        "the provider never started, so this journey never had a live child to spare"
    );
    let died = run.of_kind("member-died");
    assert_eq!(died.len(), 1, "{:?}", run.kinds());
    assert_eq!(died[0]["payload"]["rule"], serde_json::json!("activity"));
    assert_eq!(
        workspace.record()["members"]["worker"],
        serde_json::json!("died (activity)")
    );
}

/// One member across the whole startup window: spared while nothing is stamped
/// for it, and condemned once its tree is there to ask and does nothing.
///
/// Driven at `Stall` rather than through a verb because no graph can hold a
/// member's first spawn open on demand — that window is the member's own
/// in-process startup, which this suite's one sanctioned double sits downstream
/// of, and it is milliseconds here against the seconds it takes on Windows. The
/// clock is one clock for both halves, because the transition is the subject:
/// the first tree a silent member gets is a baseline, not a verdict.
#[test]
fn a_member_with_no_tree_to_ask_is_spared_and_a_wedged_one_is_still_condemned() {
    use oneagentgraph::member::Stall;
    use oneagentgraph::scratch::Group;

    let root = tempfile::tempdir().expect("a workspace");
    let scratch = root.path().join("oneagentgraph-starting");
    std::fs::create_dir_all(&scratch).expect("the scratch");
    let bound = std::time::Duration::from_secs(1);

    // A member that has started and launched nothing yet: the group exists,
    // because its launcher made it, and it holds nothing at all.
    let group = Group::open(&scratch).expect("a group");
    let started = std::time::Instant::now();
    let mut stall = Stall::new(bound, started);
    while started.elapsed() < bound * 2 {
        assert!(
            !stall.condemns(0, &scratch),
            "a member that had not started its tree yet was condemned for the silence, {:?} in",
            started.elapsed()
        );
        std::thread::sleep(POLL);
    }

    // And now there is a tree to ask, and it is doing nothing: a real process,
    // alive and reachable for the whole bound, that will never answer.
    let mut child = parked_in(&group, &scratch);

    // The same clock and the same silence: what changed is that there is now
    // something to ask. A wedged member is exactly this — a live harness that
    // will never answer — and it is still condemned.
    let found = std::time::Instant::now();
    let mut condemned = false;
    while !condemned && found.elapsed() < bound * 8 {
        condemned = stall.condemns(0, &scratch);
        std::thread::sleep(POLL);
    }
    assert!(
        condemned,
        "a member whose live tree did nothing for {:?} was spared",
        found.elapsed()
    );

    group.terminate();
    let _ = child.kill();
    let _ = child.wait();
}

/// A member whose tree is found and then **vanishes** is not condemned on the
/// sample it left behind, and the tree that replaces it is judged from a
/// baseline of its own.
///
/// The shape is a supervisory member mid-round: its turn is a succession of
/// children, so between two of them nothing at all is stamped for it — while it
/// is already well past its stall bound, because publishing nothing for a whole
/// round is what made this rule necessary. Folded to a reading of zero, that gap
/// is a tree charged no CPU next to a sample of the child that just exited, and
/// the rule condemns a member whose round is running exactly to plan.
///
/// So the gap decides nothing *and* the sample does not outlive the tree: the
/// two readings are of different processes — the successor's accounting starts
/// at zero — so their difference is a rate of nothing. What the successor gets
/// is what any first look gets, a baseline, and the verdict comes from the
/// comparison after it. That last phase is what keeps the sparing honest: this
/// member is still condemned, just on evidence about the tree it actually has.
///
/// Driven at `Stall` for the reason the journey above is: no graph can hold a
/// member's tree open, empty, and full again on demand. Everything the rule
/// reads is real — real processes, the platform's own enumeration, the kernel's
/// own CPU accounting, one clock throughout — and only the succession is staged.
#[test]
fn a_member_whose_tree_vanishes_is_not_condemned_on_the_sample_it_left() {
    use oneagentgraph::member::Stall;
    use oneagentgraph::scratch::Group;

    let root = tempfile::tempdir().expect("a workspace");
    let scratch = root.path().join("oneagentgraph-mid-round");
    std::fs::create_dir_all(&scratch).expect("the scratch");
    let group = Group::open(&scratch).expect("a group");

    // The child this round is on, and the sample the next phase must not
    // inherit: a real process, alive and charged nothing, looked at twice — an
    // idle tree by this rule's own reckoning, and the bound has not expired yet.
    let mut child = parked_in(&group, &scratch);
    let started = std::time::Instant::now();
    let mut stall = Stall::new(VANISH_BOUND, started);
    while started.elapsed() < VANISH_BOUND * 5 / 6 {
        assert!(
            !stall.condemns(0, &scratch),
            "a member was condemned {:?} into a bound of {VANISH_BOUND:?}",
            started.elapsed()
        );
        std::thread::sleep(POLL);
    }

    // The child exits, and the member is between children: nothing is stamped
    // for it, and it goes on publishing nothing right through the bound. This is
    // the whole subject — every look from here on is into the gap, and the first
    // of them is what the old rule condemned this member on.
    child.kill().expect("the child stops");
    child.wait().expect("the child is reaped");
    until("the scratch to be named by nothing at all", || {
        oneagentgraph::scratch::stamped_for(&scratch.display().to_string()).is_empty()
    });
    while started.elapsed() < VANISH_BOUND * 2 {
        assert!(
            !stall.condemns(0, &scratch),
            "a member between two children was condemned for the tree that exited, {:?} in",
            started.elapsed()
        );
        std::thread::sleep(POLL);
    }

    // The next child starts, and the member is silent past twice its bound — so
    // a rule that had kept the exited child's reading would condemn on the very
    // first look at this one. It gets a baseline instead.
    let mut child = parked_in(&group, &scratch);
    std::thread::sleep(POLL * 2);
    assert!(
        !stall.condemns(0, &scratch),
        "the first look at a member's next child condemned it on the one before"
    );

    // And then the rule is what it always was, on evidence about the tree this
    // member actually has: this child does nothing either, and it is condemned.
    let found = std::time::Instant::now();
    let mut condemned = false;
    while !condemned && found.elapsed() < VANISH_BOUND * 2 {
        condemned = stall.condemns(0, &scratch);
        std::thread::sleep(POLL);
    }
    assert!(
        condemned,
        "a member whose next child did nothing for {:?} was spared",
        found.elapsed()
    );

    group.terminate();
    let _ = child.kill();
    let _ = child.wait();
}

/// The stall bound the journey above supervises under.
///
/// Long enough that the rule takes its two looks at the first child, and the
/// gap opens, before the bound expires — the order the production shape has, and
/// the one that makes the sparing during the gap mean anything.
const VANISH_BOUND: std::time::Duration = std::time::Duration::from_secs(6);

/// A real parked process in `group`, returned once the platform names it under
/// `scratch`.
///
/// The tree half of a wedged member, which is what both journeys above need one
/// of: alive, reachable, and charged no CPU at all.
fn parked_in(group: &oneagentgraph::scratch::Group, scratch: &Path) -> std::process::Child {
    let mut parked = std::process::Command::new(fake_harness());
    parked
        .args(["-p", "fake:hang"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = group.spawn(&mut parked).expect("the parked process starts");
    until("the scratch to be named by a live process", || {
        !oneagentgraph::scratch::stamped_for(&scratch.display().to_string()).is_empty()
    });
    child
}

/// How often the two watchdog loops in this crate look at a quiet member, which
/// is the cadence the journeys above drive the rule at rather than one of their
/// own.
const POLL: std::time::Duration = oneagentgraph::member::HEARTBEAT_INTERVAL;

/// The same guarantee for a **two-party** member, which is supervised by a
/// different loop — and is the shape the production failure had.
///
/// The member condemned in anger was a two-party supervisor: its last act was a
/// call that blocked for a whole round, so its own conversation published
/// nothing while the work it had dispatched ran underneath it. `crate::judge`
/// has its own copy of the stall check — a member driven in this process cannot
/// be killed the way a child can — so a rule wired into one supervisor and not
/// the other is a real regression that the single-sided pair above cannot see.
///
/// Its condemned twin is [`a_member_that_publishes_nothing_is_condemned_by_the_activity_watchdog`],
/// which drives this same member kind onto a provider that does nothing at all.
#[test]
fn a_two_party_member_whose_conversation_is_working_is_not_condemned_for_its_silence() {
    let workspace = Workspace::new();
    let release = workspace.at("release");
    let env = bounds("60", &STALL.as_secs_f64().to_string());

    let releaser = {
        let release = release.clone();
        std::thread::spawn(move || {
            std::thread::sleep(SILENT_FOR);
            std::fs::write(&release, "go").expect("release");
        })
    };
    let started = std::time::Instant::now();
    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            &format!(
                "fake:complete-now hold the whole round open fake:work={}",
                release.display()
            ),
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &as_env(&env),
    );
    releaser.join().expect("releaser");
    run.expect_code(0);

    assert!(
        run.of_kind("member-died").is_empty(),
        "a two-party member whose agent side was working was condemned anyway: {:?}",
        run.of_kind("member-died")
    );
    assert!(
        started.elapsed() > SILENT_FOR,
        "the conversation answered before its silence outlasted the bound"
    );
    assert_eq!(
        workspace.record()["members"]["worker"],
        serde_json::json!("settled")
    );
}

/// The stall bound both watchdog journeys above supervise under.
///
/// Shortened from the contract's ten minutes, because what is under test is the
/// rule rather than the number — the default is asserted separately below. Not
/// *too* short: the rule now takes two observations of the member's tree to
/// establish that it is idle, so a bound near the supervisor's own cadence would
/// be measuring the probe interval instead.
const STALL: std::time::Duration = std::time::Duration::from_secs(4);

/// How long the working member stays silent before it is released: three whole
/// stall bounds, so a run that survives it has outlived the rule repeatedly
/// rather than raced it once.
const SILENT_FOR: std::time::Duration = std::time::Duration::from_secs(12);

/// A member whose harness process exits without publishing a report dies as a
/// provider failure, carrying the **typed** cause and the detail that names it.
///
/// This is the distinction `member-died` exists for: provider throttling, an OOM
/// kill, and a genuine crash otherwise all reach a supervisor as the same dead
/// member. It is also the half the library conversion changed — the classification
/// now comes from onejudge's own `ProviderErrorKind`, which is oneharness's
/// normalized `failure_kind`, instead of being read out of a stderr tail.
#[test]
fn a_provider_failure_carries_its_typed_cause_and_detail() {
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
    // A classified cause, drawn from the closed set the contract lists — never a
    // bare string this build invented, and never the two dispositions only a
    // child process has.
    let cause = payload["cause"].as_str().unwrap_or_default();
    assert!(
        [
            "auth",
            "rate_limit",
            "model_not_found",
            "quota",
            "overloaded",
            "timeout",
            "cancelled",
            "spawn",
            "protocol",
            "other",
            "unclassified",
        ]
        .contains(&cause),
        "a member driven in-process reported the cause {cause:?}: {payload}"
    );
    let detail = payload["detail"].as_str().unwrap_or_default();
    assert!(!detail.is_empty(), "a death with no evidence: {payload}");
    assert!(
        detail.len() <= 4096,
        "the detail outgrew its documented bound"
    );
    for absent in ["exit_code", "disposition", "stderr_tail"] {
        assert!(
            payload.get(absent).is_none(),
            "an in-process member reported {absent}: {payload}"
        );
    }
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
/// The turn hangs rather than completing, so the watchdog is the only thing that
/// can end this member — which is what makes the `cancelled` cause an assertion
/// rather than a coin toss. A member that *could* have finished would race the
/// deadline and settle on its own terms on an idle host.
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
    assert_eq!(died[0]["payload"]["cause"], serde_json::json!("cancelled"));
}

/// A member the **activity watchdog** condemns takes its descendants with it.
///
/// The `member-died` event above is the supervisor's *decision*; this is the
/// outcome an operator is actually promised. A member is `onejudge` with
/// `oneharness` under it and the paid provider under that, and condemning the
/// one this supervisor holds leaves the other two running — still billing
/// whoever owns the subscription, with nothing left watching them.
///
/// **What this journey and its heartbeat twin can and cannot isolate.** They
/// *cover* the guarantee on every platform, and that is what they are for; they
/// cannot be made to fail by compiling the Windows platform layer out, and no
/// reader should spend an afternoon trying. The reason is structural rather
/// than a weakness in the journeys: a condemnation kills the member this
/// supervisor holds, and the chain below it — `onejudge`, then `oneharness`,
/// then the provider — tears itself down from that alone, so the tree dies
/// whether or not a job object also reached it. The guarantee the *group* adds
/// on top is only observable where that containment cannot apply, which is a
/// descendant with nothing above it left to end it. That case has its own
/// journey — [`a_group_reaps_a_descendant_whose_parent_has_already_exited`] —
/// and *that* one does fail with the layer removed. So: these two for coverage,
/// that one for isolation. Do not contort these into failing, and do not delete
/// them for failing to.
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
/// It covers rather than isolates, for the reason spelled out on the journey
/// above.
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
/// Both condemnation journeys use it for the margin. A condemnation races
/// process startup: the rule fires on a clock that starts with the member, and
/// the tree has to be up by then. The heartbeat rule leaves half a second — it
/// is only reachable below the supervisor's refresh cadence — and a two-party
/// chain, three real CLIs deep, spends 67–90ms of that reaching its first turn
/// on a `windows-latest` runner. The two-party tree is condemned by the journeys
/// above and torn down by the cancel journeys below; what these two want is the
/// tree that clears the window by the widest margin.
fn single_sided_graph() -> String {
    graph_with(
        concat!(
            "version: 1\nname: node-scope\n",
            "env: {}\n",
            "members:\n  worker:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
        ),
        &[(FAKE_HARNESS_KEY, fake_harness())],
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

/// The same proof through the verb an operator actually runs: `sweep` leaves a
/// live run's scratch alone, and says which directory it kept and why.
///
/// The journey above holds the *rule*; this holds the thing that acts on it. A
/// sweep is invoked under disk pressure, which is exactly when runs are in
/// flight, so "reclaims nothing a run is working in" is the property that makes
/// the verb safe to hand an operator at all.
#[test]
fn a_sweep_leaves_a_live_run_s_scratch_alone() {
    let workspace = Workspace::new();
    let release = workspace.at("release");
    let state = workspace.state();
    let temp = workspace.at("tmp");
    std::fs::create_dir_all(&temp).expect("mkdir");

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
                .env(
                    "ONEAGENTGRAPH_ONEHARNESS_BIN",
                    crate::support::oneharness_bin(),
                )
                .env_remove("ONEHARNESS_HARNESSES")
                .output()
                .expect("the run finishes")
        })
    };

    until("the run to claim its scratch", || {
        first_lock(&state).is_some()
    });
    let root = first_lock(&state)
        .expect("a lock")
        .parent()
        .expect("the run root")
        .to_path_buf();

    // No floor and no dry run: everything this sweep can prove dead, it takes.
    let swept = workspace.run_with(
        &["sweep", "--min-age-hours", "0"],
        &[("TMPDIR", &temp.display().to_string())],
    );
    swept.expect_code(0);
    assert!(
        root.is_dir(),
        "a sweep took a live run's scratch:\n{}",
        swept.stdout
    );
    // On the *lock*, named: a live run also has processes stamped for it, so a
    // sweep with the ownership proof taken out would keep this directory anyway
    // and pass an assertion that only checked it survived. The reason is what
    // separates the two, and the run holds its claim from before it launches
    // anything until its process exits, so this one is not a race.
    assert!(
        swept
            .stdout
            .contains(&format!("{} is still locked by its owner", root.display())),
        "a sweep that kept a live run's scratch, but not because a run owns it:\n{}",
        swept.stdout
    );

    std::fs::write(&release, "go").expect("release");
    let output = handle.join().expect("the run thread");
    assert_eq!(output.status.code(), Some(0));
}

/// Scratch a **live process still names** survives a sweep, even once its owner
/// is gone.
///
/// The other half of "never reclaim what is in use", and the half the ownership
/// lock cannot answer: a supervisor that died leaves its lock free and its
/// recorded identity stale, while a descendant it started keeps running — a paid
/// harness reparented to init, still writing into the tree below that directory.
/// Removing it would destroy live work and leave the harness billing, so the
/// stamp the kernel fixed at `exec` is proof enough to keep it. Ending it stays
/// `cancel --kill`'s job; a sweep that killed would be a teardown wearing a
/// report's name.
#[test]
fn a_sweep_leaves_scratch_a_live_process_still_names_alone() {
    use oneagentgraph::scratch::{Group, SCRATCH_ENV};

    let workspace = Workspace::new();
    let state = workspace.state();
    let temp = workspace.at("tmp");
    std::fs::create_dir_all(&temp).expect("mkdir");

    // A directory whose *ownership* proof clears completely: the lock is free,
    // and the identity recorded in it names this process's number with a start
    // token nobody holds — the recycled number the token exists to see through.
    // Without the stamp below, this is a directory the sweep would take.
    let abandoned = state.join("node-scope-abandoned");
    std::fs::create_dir_all(&abandoned).expect("mkdir");
    std::fs::write(
        abandoned.join(oneagentgraph::liveness::OWNER_LOCK_FILE),
        format!("{} 1\n", std::process::id()),
    )
    .expect("write");

    // Through a group, because that is what membership *is* on Windows: a bare
    // spawn carrying the environment stamp is invisible to `stamped_for` there,
    // and this journey would pass for the wrong reason.
    let group = Group::open(&abandoned).expect("a group");
    let mut parked = std::process::Command::new(fake_harness());
    parked
        .args(["-p", "fake:hang"])
        .env(SCRATCH_ENV, &abandoned)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = group.spawn(&mut parked).expect("the parked process starts");
    until("the scratch to be named by a live process", || {
        !oneagentgraph::scratch::stamped_for(&abandoned.display().to_string()).is_empty()
    });

    let swept = workspace.run_with(
        &["sweep", "--min-age-hours", "0"],
        &[("TMPDIR", &temp.display().to_string())],
    );
    swept.expect_code(0);
    assert!(
        abandoned.is_dir(),
        "a sweep took scratch a live process is still writing into:\n{}",
        swept.stdout
    );
    assert!(
        swept.stdout.contains("still named by"),
        "a sweep kept it without saying which proof kept it:\n{}",
        swept.stdout
    );

    group.terminate();
    let _ = child.kill();
    let _ = child.wait();
}

/// A two-party member puts **every process its engine spawns** into the group its
/// own scratch names — both sides of the conversation, in the same group.
///
/// This is what the three cancel journeys below rest on, and it stopped being
/// free when a two-party member stopped being a child process. While a member
/// *was* `onejudge run`, this crate spawned one process into a group and
/// everything below joined by inheritance. In-process, the `oneharness run` for
/// each side is spawned by the supervisor itself — so unless it hands onejudge
/// the group, a worker and a judge sit in whatever group the supervisor is in,
/// and `cancel --kill` has no tree to name. On Windows that is not a degradation
/// but an absence: membership there *is* the job object, so an ungrouped member
/// is one nothing can reap.
///
/// Asserted on the report rather than on a process table because that is where
/// the answer is honest on every platform: onejudge records the group a hook
/// named, and names none when no hook ran. So a member whose engine was handed no
/// group fails here on Linux and macOS too, rather than only on the platform the
/// consequence bites.
#[test]
fn a_two_party_member_groups_both_sides_of_its_conversation() {
    let workspace = Workspace::new();
    let run = workspace.run_task("fake:complete-now: group both sides");
    run.expect_code(0);

    let settled = run.of_kind("member-settled");
    assert_eq!(settled.len(), 1, "{:?}", run.kinds());
    let path = settled[0]["payload"]["report_path"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("the stored report"))
            .expect("the stored report is JSON");
    let processes = report["processes"].as_array().cloned().unwrap_or_default();
    assert!(
        !processes.is_empty(),
        "the engine reported spawning nothing: {report}"
    );

    // The group is the member's own scratch, which is exactly the string
    // `cancel --kill` and the end-of-run reap are given.
    let scratch = member_scratch(&workspace.state()).expect("a member scratch");
    let group = scratch.display().to_string();
    for process in &processes {
        assert_eq!(
            process["group"].as_str(),
            Some(group.as_str()),
            "a spawned process outside the member's group: {process}"
        );
    }

    // Both sides, not just the one under evaluation: onejudge installs the hook
    // on both backends of a `split`, and a worker reaped without its judge is
    // half a leaked tree.
    let roles: std::collections::BTreeSet<&str> = processes
        .iter()
        .filter_map(|process| process["role"].as_str())
        .collect();
    assert_eq!(
        roles,
        ["agent", "judge"].into_iter().collect(),
        "only one side of the conversation was grouped: {processes:?}"
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

/// A cancelled run reaps a **single-sided** member's harness — a process this
/// crate never spawned, and the one every other cancel journey here misses.
///
/// Without the grouping hooks the harness carries no stamp, so this hangs rather
/// than passing quietly.
#[test]
fn a_cancelled_run_reaps_a_single_sided_members_harness() {
    let workspace = Workspace::new();
    workspace.graph(&single_sided_graph());
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

    until("the member's harness to be stamped for its group", || {
        member_scratch(&state).is_some_and(|scratch| {
            !oneagentgraph::scratch::stamped_for(&scratch.display().to_string()).is_empty()
        })
    });
    let scratch = member_scratch(&state).expect("a member scratch");
    // The stamped process is the *harness*, not this crate's child: there is no
    // `oneharness` process in this member's turn to have carried it.
    assert!(
        !oneagentgraph::scratch::stamped_for(&scratch.display().to_string()).is_empty(),
        "the harness oneharness spawned in this process carries no group stamp"
    );

    let run_id = run_id(&state);
    let cancelled = workspace.run(&["cancel", &run_id, "--kill"]);
    cancelled.expect_code(0);
    assert!(
        cancelled.stdout.contains("signalled") && !cancelled.stdout.contains("0 process(es)"),
        "a cancel of a single-sided member signalled nothing, so its harness is \
         still billing: {}",
        cancelled.stdout
    );

    until("the stamped harness to be gone", || {
        oneagentgraph::scratch::stamped_for(&scratch.display().to_string()).is_empty()
    });

    let status = member.wait().expect("the run exits");
    assert!(
        status.code().is_some(),
        "the cancelled run never exited: its harness still holds its streams"
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
        &[("FAKE_HARNESS_IGNORE_TERM", "1")],
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
/// Which makes this the one journey that **fails without the platform layer**
/// and passes with it — the isolating test for the job object, where the two
/// above are covering ones. Keep it that way: if a change here makes it pass
/// with the layer removed, it has stopped testing the layer.
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

    until("the detached ticker to start", || {
        tick_bytes_written(&ticks) > 0
    });

    // Orphaned on purpose: the process that started the ticker goes first, so
    // nothing above the ticker is left to end it.
    child.kill().expect("the parked process is killed");
    child.wait().expect("the parked process is reaped");
    let orphaned = tick_bytes_written(&ticks);
    std::thread::sleep(SETTLE);
    assert!(
        tick_bytes_written(&ticks) > orphaned,
        "the ticker stopped when its parent did, so this journey never reached the orphan it \
         asserts on"
    );

    // The group is the only thing that can still reach it.
    let reaped = group.terminate();
    assert!(
        reaped > 0,
        "the group reported reaping nothing, so an orphaned descendant is beyond it"
    );
    let ended = tick_bytes_written(&ticks);
    std::thread::sleep(SETTLE);
    assert_eq!(
        tick_bytes_written(&ticks),
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
        let ticked = tick_bytes_written(&ticks);
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
            tick_bytes_written(&ticks),
            ticked,
            "attempt {attempt}: the {rule} rule condemned the member and reported it, but its \
             descendant is still running — the provider under a condemned member keeps billing \
             with nothing left watching it\n--- stdout ---\n{}",
            run.stdout
        );
        return;
    }
}

/// How many bytes of ticks the descendant has written so far, or zero when it
/// never wrote. A byte count rather than a tick count on purpose: every caller
/// asks only whether the file *grew*, and growth is what proves the descendant is
/// still running without this having to know a tick's size.
fn tick_bytes_written(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

/// The first `owner.lock` under a state directory, once a run has claimed one.
fn first_lock(state: &Path) -> Option<std::path::PathBuf> {
    // A lock that *exists* is not yet a lock that records anything:
    // `scratch::Owned::claim` creates the file, takes the kernel lock, and only
    // then writes the identity into it. A caller polling for existence can read
    // the empty file in between and see a claim that identifies nobody — which
    // is a race in the observer, not in the claim. So the file counts once it has
    // content, and a caller waiting on this waits for a claim it can read.
    let lock = std::fs::read_dir(state)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join("owner.lock"))
        .find(|path| std::fs::metadata(path).is_ok_and(|recorded| recorded.len() > 0))?;
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
