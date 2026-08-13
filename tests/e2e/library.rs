//! The live library surface, driven through a real graph and process tree.

// llmlint: ignore-file[e2e_not_mocked] the only fake is the paid model process at
// oneharness's own binary override; the library scheduler, onejudge engine,
// oneharness process, event stream, cancellation signal, and reaper are real.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use oneagentgraph::config::ConfigRef;
use oneagentgraph::event::{Envelope, EventKind};
use oneagentgraph::run::{self, Request};

use crate::support::{fake_harness, oneharness_bin, Workspace};

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
        "fake:complete-now fake:started={} fake:hold={}",
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
        &format!(
            "  XDG_STATE_HOME: {}\n",
            workspace.session_store().display()
        ),
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
        &format!(
            "  XDG_STATE_HOME: {}\n",
            workspace.session_store().display()
        ),
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
