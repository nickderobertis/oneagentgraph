//! The live library surface, driven through a real graph and process tree.

// llmlint: ignore-file[e2e_not_mocked] the only fake is the paid model process at
// oneharness's own binary override; the library scheduler, onejudge engine,
// oneharness process, event stream, cancellation signal, and reaper are real.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use oneagentgraph::config::ConfigRef;
use oneagentgraph::event::{Envelope, EventFilter, EventKind, Matcher};
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
        filter: None,
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
        filter: None,
        state_dir: workspace.state(),
        oneharness_bin: oneharness_bin(),
    };

    let error = run::start(&request, &BTreeMap::new())
        .err()
        .expect("an empty graph cannot start");
    assert!(error.to_string().contains("has no members"), "{error}");
}

/// A library caller's own filter narrows the live stream it receives, and one it
/// could not honour is the scheduler's refusal rather than a run that starts.
///
/// The library path is the one `onepipeline` will reach through when it stops
/// spawning this binary, and it is not the CLI's: nothing here parses a `SPEC`,
/// so a filter that only the flag consulted would leave this caller unfiltered
/// with no way to tell.
#[test]
fn a_library_callers_own_filter_narrows_the_stream_it_receives() {
    let _serial = LIBRARY_RUN.lock().expect("library journey lock");
    let workspace = Workspace::new();
    workspace.graph(&crate::support::two_party_graph(
        &fake_harness(),
        &[(
            "XDG_STATE_HOME",
            workspace.session_store().display().to_string(),
        )],
    ));
    let request = |filter: Option<EventFilter>| Request {
        graph: ConfigRef(workspace.at("graph.yaml").display().to_string()),
        task: Some("fake:complete-now".into()),
        dir: workspace.dir(),
        labels: Vec::new(),
        overrides: Vec::new(),
        filter,
        state_dir: workspace.state(),
        oneharness_bin: oneharness_bin(),
    };
    let matching = |kind: &str| Matcher {
        kind: Some(kind.to_string()),
        ..Matcher::default()
    };

    // A matcher that names no field matches everything, so one in `exclude`
    // silences the stream — refused with the matcher named, before the run.
    let error = run::start(
        &request(Some(EventFilter {
            include: Vec::new(),
            exclude: vec![Matcher::default()],
        })),
        &BTreeMap::new(),
    )
    .err()
    .expect("a filter the run cannot honour stops it starting");
    assert!(error.to_string().contains("exclude[0] {}"), "{error}");
    assert!(
        std::fs::read_dir(workspace.state())
            .expect("the state directory")
            .next()
            .is_none(),
        "a refused filter still left a run behind"
    );

    let running = run::start(
        &request(Some(EventFilter {
            include: Vec::new(),
            exclude: vec![matching("turn-*")],
        })),
        &BTreeMap::new(),
    )
    .expect("the graph starts");
    let mut streamed = Vec::new();
    loop {
        match running.recv_timeout(Duration::from_secs(10)) {
            Ok(Some(event)) => streamed.push(event),
            Ok(None) => panic!("the run stopped publishing without ending"),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(error) => panic!("the live stream failed: {error}"),
        }
    }
    assert_eq!(running.wait().expect("the graph settles"), 0);
    assert!(
        !streamed
            .iter()
            .any(|event| event.kind.as_str().starts_with("turn-")),
        "the caller's filter did not reach the stream it receives: {:?}",
        streamed.iter().map(|e| e.kind).collect::<Vec<_>>()
    );
    assert!(
        streamed
            .iter()
            .any(|event| event.kind == EventKind::MemberSettled),
        "filtering the turns took the settlement with them"
    );
    // The events file is the same merged stream, so the two agree — a caller
    // that read the file back would otherwise see what its filter removed.
    let on_disk: Vec<Envelope> = std::fs::read_to_string(running_path(&workspace))
        .expect("the merged stream remains on disk")
        .lines()
        .map(|line| serde_json::from_str(line).expect("a typed envelope"))
        .collect();
    assert_eq!(streamed, on_disk);
}

/// A filter that omits `graph-started` still yields a live handle, and the run
/// under it starts, schedules, and settles exactly as it would have.
///
/// `graph-started` is the envelope this handle used to be *built* from, so a
/// caller narrowing the stream to the events it cares about was answered with a
/// refusal for a graph that had started perfectly well. What the handle reports
/// is where the run is, which no filter touches; what the run does internally is
/// unfiltered by construction, which the settlement and the record prove.
#[test]
fn a_run_whose_filter_omits_graph_started_still_starts_and_settles() {
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
        filter: Some(EventFilter {
            include: Vec::new(),
            exclude: vec![Matcher {
                kind: Some("graph-started".to_string()),
                ..Matcher::default()
            }],
        }),
        state_dir: workspace.state(),
        oneharness_bin: oneharness_bin(),
    };

    let running = run::start(&request, &BTreeMap::new())
        .expect("a filter that omits graph-started does not stop the graph starting");
    let started = running.started().clone();
    assert!(
        !started.run_id.as_str().is_empty(),
        "the handle named no run"
    );
    assert_eq!(
        started.events_path,
        workspace
            .state()
            .join(&started.run_id)
            .join(run::EVENTS_FILE)
            .display()
            .to_string(),
        "the handle points at a stream that is not this run's"
    );

    let mut streamed = Vec::new();
    loop {
        match running.recv_timeout(Duration::from_secs(10)) {
            Ok(Some(event)) => streamed.push(event),
            Ok(None) => panic!("the run stopped publishing without ending"),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(error) => panic!("the live stream failed: {error}"),
        }
    }
    // Settle detection is internal and reads what `emit` returns, not what the
    // stream carried: the run settles successfully with the envelope missing.
    assert_eq!(running.wait().expect("the graph settles"), 0);
    assert!(
        !streamed
            .iter()
            .any(|event| event.kind == EventKind::GraphStarted),
        "the excluded envelope reached the stream after all"
    );
    // Scheduling ran the wave and the member settled, both of which the stream
    // still says — the filter took one kind, not the run's account of itself.
    for expected in [EventKind::MemberStarted, EventKind::MemberSettled] {
        assert!(
            streamed.iter().any(|event| event.kind == expected),
            "{expected:?} never arrived: {:?}",
            streamed.iter().map(|e| e.kind).collect::<Vec<_>>()
        );
    }
    assert_eq!(
        streamed.last().map(|event| event.kind),
        Some(EventKind::GraphSettled)
    );
    // And `seq` still numbers from zero with no gap, so the missing envelope is
    // not readable as a dropped one.
    assert_eq!(
        streamed.iter().map(|event| event.seq).collect::<Vec<_>>(),
        (0..streamed.len() as u64).collect::<Vec<_>>()
    );

    let record = workspace.record();
    assert_eq!(record["members"]["worker"], serde_json::json!("settled"));
    assert_eq!(record["exit_code"], serde_json::json!(0));
    assert_eq!(
        record["run_id"],
        serde_json::json!(started.run_id.as_str()),
        "the handle and the record disagree about which run this is"
    );
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
        filter: None,
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
        filter: None,
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
        filter: None,
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

/// Every member's harness runs where the graph told it to, and the process
/// hosting them all stays where it started.
///
/// A member's directory is a **value** on the way to a harness — a single-sided
/// member's `oneharness run --cwd`, a two-party member's
/// `invoke::JudgeLaunch::worktree`, which onejudge puts on the same flag — because
/// one process hosts every member: a `set_current_dir` anywhere in the run path
/// moves the members that never asked, and moves them mid-run. This is the only
/// suite that can say so. The CLI journeys already prove where each harness ran
/// (`tests/e2e/dispatch.rs`), but they watch a process that has since exited, and
/// its working directory with it; here the run happens in *this* process, so the
/// directory it kept is readable on both sides of the run — and a side that
/// inherits rather than being told one reports it from inside.
///
/// It is also the invariant the last `oneharness run` hop has to keep when it
/// collapses into `oneharness_core::io::run::run`, whose `cwd` is a `RunRequest`
/// field rather than a process state for exactly this reason — see
/// `src/harness_process.rs`, which carries that boundary inventory.
#[test]
fn the_hosting_process_directory_never_moves_for_a_member_that_works_elsewhere() {
    let _serial = LIBRARY_RUN.lock().expect("library journey lock");
    let workspace = Workspace::new();
    let own = workspace.dir().join("api");
    std::fs::create_dir_all(&own).expect("the member's own directory");
    let single_sided = workspace.at("reporter.cwd");
    let two_party = workspace.at("worker.cwd");

    // Both member kinds in one graph, because they reach a directory by
    // different routes and only one of them is a child process today.
    workspace.graph(&graph_with(
        concat!(
            "version: 3\nname: node-scope\n",
            "env: {}\n",
            "members:\n",
            "  reporter:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "    dir: ./api\n",
            "  worker:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n",
        ),
        &[
            (FAKE_HARNESS_KEY.to_string(), fake_harness()),
            (
                "env.XDG_STATE_HOME".to_string(),
                workspace.session_store().display().to_string(),
            ),
            (
                "members.reporter.task".to_string(),
                format!(
                    "fake:complete-now report in. fake:record-cwd={}",
                    single_sided.display()
                ),
            ),
        ],
    ));
    let request = Request {
        graph: ConfigRef(workspace.at("graph.yaml").display().to_string()),
        task: Some(format!(
            "fake:complete-now drive this run to settlement. fake:record-cwd={}",
            two_party.display()
        )),
        dir: workspace.dir(),
        labels: Vec::new(),
        overrides: Vec::new(),
        filter: None,
        state_dir: workspace.state(),
        oneharness_bin: oneharness_bin(),
    };

    let before = std::env::current_dir().expect("the hosting process has a working directory");
    let running = run::start(&request, &BTreeMap::new()).expect("the graph starts");
    assert_eq!(running.wait().expect("the graph settles"), 0);
    assert_eq!(
        std::env::current_dir().expect("the hosting process still has a working directory"),
        before,
        "the run moved the working directory of the process that hosts every member"
    );

    // And the members really did work elsewhere, so the assertion above is about
    // a directory something was asked to change rather than one nobody used.
    assert_eq!(recorded(&single_sided), vec![canonical(&own)]);

    // The two-party member's own turns are told the graph's directory — and the
    // sides onejudge tells nothing (its judge side takes no `--cwd`, having its
    // config by name) inherit this process's, which is what makes the invariant
    // observable from *inside* the run: a `set_current_dir` that moved the host
    // mid-run is recorded here by a harness that started after it, where reading
    // `current_dir()` afterwards would be fooled by one that put it back.
    let sides = recorded(&two_party);
    let told = canonical(&workspace.dir());
    let inherited = canonical(&before);
    assert!(
        sides.contains(&told),
        "no side of the two-party member ran in the directory the graph named: {sides:?}"
    );
    assert!(
        sides.iter().all(|dir| *dir == told || *dir == inherited),
        "a side ran somewhere neither the graph nor this process named: {sides:?}"
    );
}

/// Every directory a harness recorded through `fake:record-cwd`, canonical so a
/// host whose temporary directory is a symlink (macOS: `/var` → `/private/var`)
/// compares equal to what the graph named.
fn recorded(path: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|err| {
            panic!(
                "no harness recorded a directory in {}: {err}",
                path.display()
            )
        })
        .lines()
        .map(|line| canonical(std::path::Path::new(line.trim())))
        .collect()
}

fn canonical(path: &std::path::Path) -> std::path::PathBuf {
    path.canonicalize()
        .unwrap_or_else(|err| panic!("{} cannot be resolved: {err}", path.display()))
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
