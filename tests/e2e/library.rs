//! The live library surface, driven through a real graph and process tree.

// llmlint: ignore-file[e2e_not_mocked] the only fake is the paid model process at
// oneharness's own binary override; the library scheduler, onejudge engine,
// oneharness process, event stream, cancellation signal, and reaper are real.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use oneagentgraph::config::ConfigRef;
use oneagentgraph::event::{Envelope, EventKind};
use oneagentgraph::run::{self, MemberName, Request, Signal};

use crate::support::{fake_harness, graph_with, oneharness_bin, Workspace, FAKE_HARNESS_KEY};
// The one Unix-only journey's own surface and helper: on a platform without a
// unix domain socket it compiles away, and an import left behind is a
// `-D warnings` build failure rather than dead weight.
#[cfg(unix)]
use crate::support::until;
#[cfg(unix)]
use oneagentgraph::control::{self, Delivery};

static LIBRARY_RUN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A caller receives an event while its member is observably still running,
/// cancels that real member tree, then learns the same failed-run exit status.
/// Comparing the channel to `events.jsonl` also pins content and ordering to the
/// merged stream the CLI consumes.
#[test]
fn a_library_caller_watches_cancels_and_waits_for_a_live_graph() {
    let _serial = LIBRARY_RUN.lock().expect("library journey lock");
    let workspace = Workspace::new();
    let began = workspace.at("began");
    let release = workspace.at("release");
    let task = format!(
        "fake:complete-now fake:entered={} fake:hold={}",
        began.display(),
        release.display()
    );
    let mut env = BTreeMap::new();
    env.insert(
        "XDG_STATE_HOME".to_string(),
        workspace.session_store().display().to_string(),
    );
    let request = Request {
        graph: ConfigRef(workspace.at("graph.yaml").display().to_string()),
        task: Some(task),
        dir: workspace.dir(),
        labels: Vec::new(),
        overrides: Vec::new(),
        state_dir: workspace.state(),
        oneharness_bin: oneharness_bin(),
    };

    // The graph itself selects the sanctioned provider double. Keeping that at
    // the graph boundary exercises the same export path a CLI run uses.
    workspace.graph(&crate::support::two_party_graph(
        &fake_harness(),
        &[(
            "XDG_STATE_HOME",
            workspace.session_store().display().to_string(),
        )],
    ));

    let running = run::start(&request, &env).expect("the graph starts");
    assert_eq!(
        running.started().events_path,
        workspace
            .state()
            .join(&running.started().run_id)
            .join(run::EVENTS_FILE)
            .display()
            .to_string()
    );
    let mut streamed = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while (!began.exists()
        || !streamed
            .iter()
            .any(|event: &Envelope| event.kind == EventKind::MemberStarted))
        && Instant::now() < deadline
    {
        if let Some(event) = running
            .recv_timeout(Duration::from_millis(100))
            .expect("the live stream remains connected")
        {
            streamed.push(event);
        }
    }
    assert!(
        began.exists(),
        "the real member never entered its held turn"
    );
    assert!(
        streamed
            .iter()
            .any(|event| event.kind == EventKind::MemberStarted),
        "no member event arrived while the held run was still in progress"
    );
    assert!(
        !release.exists(),
        "the held run completed before cancellation"
    );
    while let Some(event) = running
        .recv_timeout(Duration::from_millis(20))
        .expect("the live stream remains connected")
    {
        streamed.push(event);
    }

    assert!(running.cancel().expect("the run is cancellable") > 0);
    loop {
        match running.recv_timeout(Duration::from_secs(2)) {
            Ok(Some(event)) => streamed.push(event),
            Ok(None) => panic!("the cancelled run stopped publishing without ending"),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(error) => panic!("the live stream failed: {error}"),
        }
    }
    assert_eq!(running.wait().expect("the scheduler reports an exit"), 1);
    assert!(streamed.iter().any(|event| {
        event.kind == EventKind::MemberDied
            && event.payload.get("cause").and_then(|value| value.as_str()) == Some("cancelled")
    }));
    assert_eq!(
        streamed.last().map(|event| event.kind),
        Some(EventKind::GraphSettled)
    );

    let on_disk: Vec<Envelope> = std::fs::read_to_string(running_path(&workspace))
        .expect("the merged stream remains on disk")
        .lines()
        .map(|line| serde_json::from_str(line).expect("a typed envelope"))
        .collect();
    assert_eq!(streamed, on_disk);
}

fn running_path(workspace: &Workspace) -> std::path::PathBuf {
    std::fs::read_dir(workspace.state())
        .expect("run state")
        .next()
        .expect("one run")
        .expect("run directory")
        .path()
        .join(run::EVENTS_FILE)
}

#[test]
fn starting_an_invalid_graph_returns_the_scheduler_error() {
    let _serial = LIBRARY_RUN.lock().expect("library journey lock");
    let workspace = Workspace::new();
    workspace.graph("version: 1\nname: invalid\nmembers: {}\n");
    let request = Request {
        graph: ConfigRef(workspace.at("graph.yaml").display().to_string()),
        task: None,
        dir: workspace.dir(),
        labels: Vec::new(),
        overrides: Vec::new(),
        state_dir: workspace.state(),
        oneharness_bin: oneharness_bin(),
    };

    let error = run::start(&request, &BTreeMap::new())
        .err()
        .expect("an empty graph cannot start");
    assert!(error.to_string().contains("has no members"), "{error}");
}

#[test]
fn a_successful_live_run_returns_the_blocking_exit_status() {
    let _serial = LIBRARY_RUN.lock().expect("library journey lock");
    let workspace = Workspace::new();
    workspace.graph(&crate::support::two_party_graph(
        &fake_harness(),
        &[(
            "XDG_STATE_HOME",
            workspace.session_store().display().to_string(),
        )],
    ));
    let request = Request {
        graph: ConfigRef(workspace.at("graph.yaml").display().to_string()),
        task: Some("fake:complete-now".into()),
        dir: workspace.dir(),
        labels: Vec::new(),
        overrides: Vec::new(),
        state_dir: workspace.state(),
        oneharness_bin: oneharness_bin(),
    };

    let running = run::start(&request, &BTreeMap::new()).expect("the graph starts");
    assert_eq!(running.wait().expect("the graph settles"), 0);
}

/// A caller redirects a member's in-flight turn without spawning this crate's
/// binary, and the member does the new work.
///
/// The same journey `tests/e2e/interrupt.rs` drives through the CLI, reached
/// through the library instead: the run is [`run::start`]'s, the address is the
/// one that run wrote down, and the delivery is real `oneharness interrupt`
/// against the socket a real controlled turn bound. The two files are the whole
/// assertion — the first turn's `did-work` proves it never got past its park,
/// the redirected turn's proves it did the operator's own prose instead.
///
/// Unix-only for the reason the CLI journey is: oneharness's turn-control socket
/// is a unix domain socket.
#[cfg(unix)]
#[test]
fn a_library_caller_redirects_a_members_in_flight_turn() {
    let _serial = LIBRARY_RUN.lock().expect("library journey lock");
    let workspace = Workspace::new();
    workspace.graph(&crate::support::two_party_graph(
        &fake_harness(),
        &[(
            "XDG_STATE_HOME",
            workspace.session_store().display().to_string(),
        )],
    ));
    let started = workspace.at("turn-started");
    let original_work = workspace.at("did-original-work");
    let redirected_work = workspace.at("did-redirected-work");
    let mut env = BTreeMap::new();
    env.insert(
        "XDG_STATE_HOME".to_string(),
        workspace.session_store().display().to_string(),
    );
    let request = Request {
        graph: ConfigRef(workspace.at("graph.yaml").display().to_string()),
        task: Some(format!(
            "fake:park fake:started={} fake:did-work={}",
            started.display(),
            original_work.display()
        )),
        dir: workspace.dir(),
        labels: Vec::new(),
        overrides: Vec::new(),
        state_dir: workspace.state(),
        oneharness_bin: oneharness_bin(),
    };

    let running = run::start(&request, &env).expect("the graph starts");
    let run_id = running.started().run_id.clone();
    let member = MemberName::parse("worker").expect("a member name");

    // The double's own marker, which is the whole synchronization — nothing here
    // waits on a file inside the run's state. A run writes down where a member's
    // turn is addressed *before* it drives the conversation, so a turn that has
    // begun is one this call can already reach; and only a **controlled** turn
    // writes this marker, which makes the wait a check on the control ask too. A
    // timeout here means oneharness refused `--control` and the turn ran with no
    // lever to pull.
    until(
        "the member's turn to be in flight — only a controlled turn writes this",
        || started.exists(),
    );

    let redirection = format!(
        "fake:complete-now fake:did-work={} stop and write the summary instead",
        redirected_work.display()
    );
    let delivery = control::interrupt(
        &workspace.state(),
        &run_id,
        &member,
        Some(&redirection),
        &oneharness_bin(),
    )
    .expect("the run and its member are addressable");
    assert_eq!(delivery, Delivery::Delivered, "{delivery:?}");

    assert_eq!(
        running.wait().expect("the redirected member settles"),
        0,
        "the redirected member did not settle"
    );
    assert!(
        !original_work.exists(),
        "the interrupted turn went on and did the work it was parked before — the turn was not \
         stopped, only added to"
    );
    let did = std::fs::read_to_string(&redirected_work)
        .expect("the redirected turn never did the new work");
    assert!(
        did.contains("stop and write the summary instead"),
        "the turn that ran after the interrupt did not do what the caller asked: {did:?}"
    );

    // The same lever on the same member now that it is over: the run's own
    // record says it settled, so the answer is that fact rather than a socket
    // asked about a turn nobody is running. A caller reads it off the value
    // instead of off an exit code.
    let late = control::interrupt(
        &workspace.state(),
        &run_id,
        &member,
        Some("one more thing"),
        &oneharness_bin(),
    )
    .expect("a settled member is still addressable");
    assert!(
        matches!(&late, Delivery::NoTurn(reason) if reason.contains("already settled")),
        "{late:?}"
    );

    // And a member this run never had is refused rather than addressed: the
    // caller mistyped, which is not an answer about any turn.
    let ghost = MemberName::parse("ghost").expect("a member name");
    let refused = control::interrupt(
        &workspace.state(),
        &run_id,
        &ghost,
        Some("do this instead"),
        &oneharness_bin(),
    )
    .expect_err("a member the run never had cannot be addressed");
    assert!(
        refused.to_string().contains("has no member \"ghost\""),
        "{refused}"
    );
}

/// A caller restarts a scheduled member's clock, and the live run publishes the
/// reset it picked up.
///
/// The assertion is the run's own `cron-reset` on the same stream the CLI
/// consumes: a call that wrote a file nothing ever read would leave the caller
/// with a success and a member whose clock never moved, which is exactly the
/// failure the member check exists to prevent — driven here as the second half,
/// where a name this run never declared is refused and leaves no file behind.
#[test]
fn a_library_caller_resets_a_scheduled_members_timer() {
    let _serial = LIBRARY_RUN.lock().expect("library journey lock");
    let workspace = Workspace::new();
    // The `keeper` holds the run open while the reset is delivered and picked
    // up: a graph of nothing but the scheduled member settles every non-cron
    // member at once, and its cron thread stops with them. It waits on `anchor`
    // so it lands in the *second* wave — a held member in the first would keep
    // that wave from finishing, and a schedule taking its first turn at t=0 —
    // which is what a version 2 document's schedule does — hands its clock over
    // only once that turn has settled.
    let release = workspace.at("cron-keeper-release");
    workspace.graph(&graph_with(
        concat!(
            "version: 2\nname: node-scope\n",
            "env: {}\n",
            "members:\n  reporter:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "    schedule: {every: 3600, resettable: true}\n",
            "  anchor:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "  keeper:\n    kind: onejudge\n    base_config: ./base.yaml\n",
            "    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n    deps: [anchor]\n",
        ),
        &[
            (FAKE_HARNESS_KEY, fake_harness()),
            (
                "members.keeper.task",
                format!("fake:complete-now fake:hold={}", release.display()),
            ),
        ],
    ));
    let request = Request {
        graph: ConfigRef(workspace.at("graph.yaml").display().to_string()),
        task: Some("fake:complete-now: scheduled".into()),
        dir: workspace.dir(),
        labels: Vec::new(),
        overrides: Vec::new(),
        state_dir: workspace.state(),
        oneharness_bin: oneharness_bin(),
    };

    let running = run::start(&request, &BTreeMap::new()).expect("the graph starts");
    let run_id = running.started().run_id.clone();
    let member = MemberName::parse("reporter").expect("a member name");
    // The clock this call restarts only exists once the member has settled its
    // first firing and its schedule is counting.
    assert!(
        drain_until(&running, |event| event.kind == EventKind::MemberSettled
            && event.labels.member.as_deref() == Some("reporter")),
        "the scheduled member never settled its first firing"
    );

    // A name this run never declared is refused, and leaves nothing behind for
    // the run to read.
    let ghost = MemberName::parse("ghost").expect("a member name");
    let refused = run::signal(&workspace.state(), &run_id, &ghost, Signal::Reset)
        .expect_err("a member the run never had cannot be signalled");
    assert!(
        refused.to_string().contains("has no member \"ghost\""),
        "{refused}"
    );
    assert!(
        !workspace
            .state()
            .join(run_id.as_str())
            .join(run::SIGNAL_DIR)
            .join("ghost.reset")
            .exists(),
        "a refused signal still left its file behind"
    );

    run::signal(&workspace.state(), &run_id, &member, Signal::Reset)
        .expect("the run's own member is signallable");
    assert!(
        drain_until(&running, |event| event.kind == EventKind::CronReset
            && event.labels.member.as_deref() == Some("reporter")),
        "the run never picked the reset up"
    );

    std::fs::write(&release, "release").expect("release the keeper");
    assert_eq!(running.wait().expect("the run ends"), 0);
}

/// Read the live stream until an envelope satisfies `wanted`, or the run stops
/// producing them.
fn drain_until(running: &run::Running, wanted: impl Fn(&Envelope) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(240);
    while Instant::now() < deadline {
        match running.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(event)) if wanted(&event) => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}
