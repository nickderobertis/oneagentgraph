//! `interrupt`: redirecting a member's in-flight turn instead of ending it.
//!
//! `cancel` is the only other way to change what a running member is doing, and
//! it discards the turn — the worker's whole accumulated context goes with it and
//! the replacement re-derives from scratch. These journeys are the difference:
//! the turn keeps its session and does something else.
//!
//! Nothing about the mechanism is faked. The binary under test writes the address
//! down, real `oneharness` opens the control socket and serves the interrupt over
//! claude-code's own stdin control protocol, and the double at the far end is the
//! paid harness — the one seam this suite is allowed. What the double does is
//! answer that protocol: it parks, it is aborted, and the redirection arrives as
//! the next turn.

use std::path::PathBuf;

use crate::support::{as_env, until, Workspace};

/// One run whose member is parked on a controllable turn, and the run id an
/// operator addresses it by.
struct Parked {
    /// The run process, still going.
    run: std::thread::JoinHandle<std::process::Output>,
    /// The run an `interrupt` names.
    id: String,
}

impl Parked {
    /// What the run exited with, once it is over.
    fn settled(self) -> std::process::Output {
        self.run.join().expect("the run thread")
    }
}

/// Start the default graph on a task that parks its agent turn, and wait until
/// that turn is really in flight.
///
/// The wait is on a marker only a *controlled* turn writes, so what follows is
/// addressed at a turn oneharness has already opened a socket for — waiting for
/// the process instead would race the bind.
fn parked(workspace: &Workspace, task: &str) -> Parked {
    let run = {
        let workspace_dir = workspace.path().to_path_buf();
        let state = workspace.state();
        let dir = workspace.dir();
        let xdg = workspace.at("xs");
        let task = task.to_string();
        std::thread::spawn(move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_oneagentgraph"))
                .args([
                    "run",
                    "./graph.yaml",
                    "--task",
                    &task,
                    "--dir",
                    &dir.display().to_string(),
                ])
                .current_dir(&workspace_dir)
                .env("ONEAGENTGRAPH_STATE_DIR", &state)
                .env(
                    "ONEAGENTGRAPH_ONEHARNESS_BIN",
                    crate::support::oneharness_bin(),
                )
                .env("XDG_STATE_HOME", &xdg)
                .env_remove("ONEHARNESS_HARNESSES")
                .env_remove("ONEHARNESS_MODEL")
                .output()
                .expect("the run finishes")
        })
    };
    let started = workspace.at("turn-started");
    until("the member's turn to be in flight", || started.exists());
    Parked {
        run,
        id: workspace.record()["run_id"]
            .as_str()
            .expect("the run recorded its id")
            .to_string(),
    }
}

/// The task that parks a member's agent turn, writing the marker [`parked`]
/// waits for.
fn parking_task(workspace: &Workspace, did_work: Option<&PathBuf>) -> String {
    let mut task = format!(
        "fake:park fake:started={}",
        workspace.at("turn-started").display()
    );
    if let Some(path) = did_work {
        task.push_str(&format!(" fake:did-work={}", path.display()));
    }
    task
}

/// The one journey the verb exists for: an in-flight turn stops what it was
/// doing and does the new work instead.
///
/// The two files are the whole assertion. `did-work` is appended by a controlled
/// turn *after* it is past its park and past any abort, so the first turn's copy
/// proves it never got there and the redirected turn's proves it did — and the
/// second is the operator's own prose, delivered with the stop as one operation.
/// A member that had merely been cancelled and restarted would have written
/// neither: there would be no second turn on that session at all.
///
/// Unix-only because oneharness's turn-control socket is a unix domain socket,
/// which is also why a Windows member reports no controllable turn — the journey
/// below drives that answer.
#[cfg(unix)]
#[test]
fn an_interrupt_stops_the_in_flight_turn_and_the_member_does_the_new_work() {
    let workspace = Workspace::new();
    let original_work = workspace.at("did-original-work");
    let redirected_work = workspace.at("did-redirected-work");

    let member = parked(&workspace, &parking_task(&workspace, Some(&original_work)));
    let run_id = member.id.clone();

    // The address the run wrote down while the turn is live. It is this crate's
    // own answer — the report that carries oneharness's cannot arrive until the
    // member settles — so the same file is read again at the end and the two are
    // held to each other below.
    let control = workspace
        .state()
        .join(&run_id)
        .join("members")
        .join("worker")
        .join("control.json");
    until("the run to record where its turn is addressed", || {
        control.exists()
    });
    let live: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&control).expect("the control record"))
            .expect("the control record is JSON");
    assert_eq!(live["turn"]["state"], "open", "{live}");

    // A redirection oneharness will not take is refused *before* the lever is
    // pulled: exit 2 like every other bad argument, nothing on stdout, and the
    // turn still there for the interrupt below. Driven through the real refusal
    // rather than a restatement of it — a control character is not message text,
    // because it reaches a harness inside a protocol frame.
    let unusable = workspace.run(&[
        "interrupt",
        &run_id,
        "worker",
        "--input",
        "do this\u{1b}[2Kinstead",
    ]);
    unusable.expect_code(2);
    assert!(
        unusable.stderr.contains("not message text"),
        "{}",
        unusable.stderr
    );
    assert!(
        unusable.stdout.is_empty(),
        "a redirection that was never delivered published an event: {}",
        unusable.stdout
    );

    let redirection = format!(
        "fake:complete-now fake:did-work={} stop and write the summary instead",
        redirected_work.display()
    );
    let interrupted = workspace.run(&["interrupt", &run_id, "worker", "--input", &redirection]);
    interrupted.expect_code(0);

    // The verb answers on the stream every other producer in this stack answers
    // on, so a supervisor watching a run sees the lever being pulled.
    let events = interrupted.of_kind("turn-interrupted");
    assert_eq!(events.len(), 1, "{}", interrupted.stdout);
    assert_eq!(events[0]["payload"]["member"], "worker");
    assert_eq!(events[0]["payload"]["delivered"], true);
    assert_eq!(
        events[0]["payload"]["input_bytes"],
        redirection.len() as u64
    );
    assert!(
        events[0]["payload"].get("reason").is_none(),
        "a delivered interrupt carried a reason: {}",
        interrupted.stdout
    );
    assert_eq!(events[0]["labels"]["run_id"], run_id.as_str());

    let output = member.settled();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "the redirected member did not settle\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stderr)
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
        "the turn that ran after the interrupt did not do what the operator asked: {did:?}"
    );

    // The drift gate under the live address: once the member settles, the record
    // is rewritten from onejudge's report — the handle oneharness *actually*
    // bound, and the store it bound it in. If the two ever disagreed, an
    // interrupt sent while the turn was live would be addressing nothing, and
    // this is the assertion that would say so.
    let settled: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&control).expect("the control record"))
            .expect("the control record is JSON");
    assert_eq!(
        settled["turn"]["address"]["session"], live["turn"]["address"]["session"],
        "the session a live interrupt addresses is not the one the report says was bound"
    );
    assert!(
        settled["turn"]["address"]["session_dir"].is_string(),
        "the report's own store directory did not replace the default: {settled}"
    );

    // And the same lever on the same member, now that it is over: the run's own
    // record says it settled, so the answer is that fact rather than a socket
    // asked about a turn nobody is running.
    let late = workspace.run(&["interrupt", &run_id, "worker", "--input", "one more thing"]);
    late.expect_code(3);
    assert!(
        late.of_kind("turn-interrupted")[0]["payload"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("already settled")),
        "{}",
        late.stdout
    );
}

/// A member on a harness with no out-of-band turn control is a **fact**, not a
/// failure: exit 3, naming which of the exit-3 causes applies.
///
/// `qwen` declares no control mechanism, so `oneharness run --control` refuses
/// the ask before it spawns anything, onejudge retries the same call without it,
/// and the member runs exactly as it always did — the refusal reaches the report
/// as `control: null` with the reason, which is what this verb reports. Nothing
/// about which harnesses have a lever is decided here.
#[test]
fn a_member_with_no_control_mechanism_is_reported_as_a_fact_rather_than_a_failure() {
    let workspace = Workspace::new();
    workspace.write("oneharness.toml", QWEN_CHAIN);
    workspace.write("oneharness.judge.toml", QWEN_CHAIN);
    workspace.graph(&uncontrollable_graph(&crate::support::fake_harness()));

    let ran = workspace.run_task("fake:complete-now: a member with no lever");
    ran.expect_code(0);
    let run_id = workspace.record()["run_id"]
        .as_str()
        .expect("the run recorded its id")
        .to_string();

    let interrupted =
        workspace.run(&["interrupt", &run_id, "worker", "--input", "do this instead"]);
    interrupted.expect_code(3);
    let events = interrupted.of_kind("turn-interrupted");
    assert_eq!(events.len(), 1, "{}", interrupted.stdout);
    assert_eq!(events[0]["payload"]["delivered"], false);
    let reason = events[0]["payload"]["reason"]
        .as_str()
        .expect("a delivery that did not land names why");
    assert!(
        reason.contains("no out-of-band turn control"),
        "exit 3 did not say which of its causes applied: {reason}"
    );
    assert!(
        interrupted.stderr.is_empty(),
        "a fact was reported as a failure on stderr: {}",
        interrupted.stderr
    );
}

/// A single-sided member opens no controllable turn at all, and says so rather
/// than reaching for a socket nothing ever bound.
#[test]
fn a_single_sided_member_says_it_opens_no_controllable_turn() {
    let workspace = Workspace::new();
    workspace.graph(&crate::support::single_sided_graph(
        &crate::support::fake_harness(),
    ));
    workspace
        .run_task("fake:complete-now: one agent")
        .expect_code(0);
    let run_id = workspace.record()["run_id"]
        .as_str()
        .expect("the run recorded its id")
        .to_string();

    let interrupted = workspace.run(&["interrupt", &run_id, "reporter", "--input", "instead"]);
    interrupted.expect_code(3);
    let events = interrupted.of_kind("turn-interrupted");
    assert!(
        events[0]["payload"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("opened no controllable turn")),
        "{}",
        interrupted.stdout
    );
}

/// Everything a caller can type that cannot work, refused with the contract's
/// exit 2 and nothing on stdout — a caller reads a line there as an event, so a
/// refusal must not produce one.
#[test]
fn every_unusable_interrupt_is_refused_by_name() {
    let workspace = Workspace::new();
    workspace
        .run_task("fake:complete-now: something to address")
        .expect_code(0);
    let run_id = workspace.record()["run_id"]
        .as_str()
        .expect("the run recorded its id")
        .to_string();
    let input_file = workspace.write("redirect.md", "do this instead\n");

    let cases: Vec<(Vec<String>, &str)> = vec![
        (
            vec![
                "interrupt".into(),
                "no-such-run".into(),
                "worker".into(),
                "--input".into(),
                "x".into(),
            ],
            "no-such-run",
        ),
        (
            vec![
                "interrupt".into(),
                run_id.clone(),
                "ghost".into(),
                "--input".into(),
                "x".into(),
            ],
            "has no member",
        ),
        (
            vec![
                "interrupt".into(),
                run_id.clone(),
                "../escape".into(),
                "--input".into(),
                "x".into(),
            ],
            "outside the run's own directory",
        ),
        (
            vec![
                "interrupt".into(),
                run_id.clone(),
                "worker".into(),
                "--input".into(),
                "x".into(),
                "--input-file".into(),
                input_file.display().to_string(),
            ],
            "give at most one",
        ),
        (
            vec![
                "interrupt".into(),
                run_id.clone(),
                "worker".into(),
                "--input".into(),
                "   ".into(),
            ],
            "redirect the turn at nothing",
        ),
        (
            vec![
                "interrupt".into(),
                run_id.clone(),
                "worker".into(),
                "--input-file".into(),
                "nowhere.md".into(),
            ],
            "cannot read --input-file",
        ),
    ];
    for (args, expected) in cases {
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let refused = workspace.run(&borrowed);
        refused.expect_code(2);
        assert!(
            refused.stderr.contains(expected),
            "{args:?} did not say {expected:?}: {}",
            refused.stderr
        );
        assert!(
            refused.stdout.is_empty(),
            "{args:?} wrote an event while refusing: {}",
            refused.stdout
        );
    }
}

/// A delivery that was attempted and failed is exit 1 — the lever broke, which is
/// not the same answer as a turn that was simply not there. The turn survives it,
/// and a **stop-only** interrupt then ends it without redirecting it at anything.
///
/// The failure is driven at the one seam a journey can reach it through: the
/// `oneharness` the verb shells out to is named by the environment, and one that
/// is not there is the same failure a host with a broken install has.
#[cfg(unix)]
#[test]
fn a_delivery_that_could_not_be_attempted_is_a_failure_rather_than_an_absent_turn() {
    let workspace = Workspace::new();
    let member = parked(&workspace, &parking_task(&workspace, None));
    let run_id = member.id.clone();

    let missing = [(
        "ONEAGENTGRAPH_ONEHARNESS_BIN",
        "oneagentgraph-no-such-oneharness".to_string(),
    )];
    let broken = as_env(&missing);
    let failed = workspace.run_with(
        &["interrupt", &run_id, "worker", "--input", "do this instead"],
        &broken,
    );
    failed.expect_code(1);
    assert!(
        failed.stderr.contains("oneagentgraph-no-such-oneharness"),
        "{}",
        failed.stderr
    );
    let events = failed.of_kind("turn-interrupted");
    assert_eq!(events[0]["payload"]["delivered"], false);

    // And the turn is still there for an install that works, which is the point
    // of telling the two answers apart. With no `--input`, which the contract
    // allows: the turn stops and nothing takes its place.
    let stopped = workspace.run(&["interrupt", &run_id, "worker"]);
    stopped.expect_code(0);
    let events = stopped.of_kind("turn-interrupted");
    assert_eq!(events[0]["payload"]["delivered"], true);
    assert_eq!(
        events[0]["payload"]["input_bytes"], 0,
        "an interrupt that only stops the turn carried a redirection: {}",
        stopped.stdout
    );
    assert_eq!(member.settled().status.code(), Some(0));
}

/// The redirection may come from a file, which is how an operator sends one too
/// long or too shell-hostile to type on an argv.
#[cfg(unix)]
#[test]
fn a_redirection_read_from_a_file_reaches_the_turn_the_same_way() {
    let workspace = Workspace::new();
    let redirected_work = workspace.at("did-redirected-work");
    let member = parked(&workspace, &parking_task(&workspace, None));

    let redirection = format!(
        "fake:complete-now fake:did-work={}\nwrite the summary instead\n",
        redirected_work.display()
    );
    let file = workspace.write("redirect.md", &redirection);
    let interrupted = workspace.run(&[
        "interrupt",
        &member.id,
        "worker",
        "--input-file",
        &file.display().to_string(),
    ]);
    interrupted.expect_code(0);
    let events = interrupted.of_kind("turn-interrupted");
    assert_eq!(events[0]["payload"]["delivered"], true);
    assert_eq!(
        events[0]["payload"]["input_bytes"],
        redirection.len() as u64,
        "the file's own bytes are what was delivered: {}",
        interrupted.stdout
    );

    assert_eq!(member.settled().status.code(), Some(0));
    let did = std::fs::read_to_string(&redirected_work)
        .expect("the redirected turn never did the new work");
    assert!(
        did.contains("write the summary instead"),
        "the turn that ran after the interrupt did not do what the file asked: {did:?}"
    );
}

/// An interrupt aimed at a run whose state this build cannot act on reports which
/// fact applies rather than failing: a turn that has gone, a record torn in half,
/// and one a later build wrote.
///
/// The records are planted, because that is the only way to reach the subject —
/// exactly as `tests/record.rs` plants a run record for the same reason. What
/// this verb does with a run that recorded an open turn and then went away is the
/// state an operator meets *after* the incident they reach for `interrupt`
/// during: a killed launcher, a host that rebooted, a torn write, or a run
/// started by the version they are about to upgrade past. No sequence of commands
/// produces those, and a journey that only drove healthy runs would leave the
/// answers an operator meets in anger unproven. Real `oneharness` is what answers
/// the first of them, and its own refusal is what this asserts on.
///
/// llmlint: ignore-block[tests_mirror_real_usage] see the paragraph above: the
/// planted files are the *subject*, not a shortcut around one — every command
/// under test is still the compiled binary reached the way a user reaches it.
#[cfg(unix)]
#[test]
fn an_interrupt_aimed_at_a_run_this_build_cannot_act_on_says_so_rather_than_failing() {
    let workspace = Workspace::new();
    let run_id = "node-scope-1786171301679-1447994";
    let member = workspace
        .state()
        .join(run_id)
        .join("members")
        .join("worker");
    std::fs::create_dir_all(&member).expect("the member's scratch");
    std::fs::write(
        workspace.state().join(run_id).join("record.json"),
        serde_json::json!({
            "schema_version": 2,
            "run_id": run_id,
            "graph": "./graph.yaml",
            "name": "node-scope",
            "started_ms": 1_786_171_301_679_u64,
            "declared_members": ["worker"],
            "refs": [],
            "events_path": format!("/state/{run_id}/events.jsonl"),
        })
        .to_string(),
    )
    .expect("a run record with no outcome yet");

    let gone = serde_json::json!({
        "schema_version": 1,
        "turn": {"state": "open", "address": {
            "session": format!("{run_id}-worker-skill"),
            "cwd": member.display().to_string(),
        }},
    })
    .to_string();
    let from_a_later_build = serde_json::json!({
        "schema_version": 99,
        "turn": {"state": "open", "address": {
            "session": format!("{run_id}-worker-skill"),
            "cwd": member.display().to_string(),
        }},
    })
    .to_string();
    for (record, expected) in [
        (gone.as_str(), "already ended"),
        ("{ torn in half", "not one this build can read"),
        (from_a_later_build.as_str(), "schema version 99"),
    ] {
        std::fs::write(member.join("control.json"), record).expect("a control record");
        // With no `--input` at all, which the contract allows: an interrupt that
        // only asks the turn to stop. It carries no redirection, and says so.
        let interrupted = workspace.run(&["interrupt", run_id, "worker"]);
        interrupted.expect_code(3);
        let events = interrupted.of_kind("turn-interrupted");
        assert_eq!(events[0]["payload"]["input_bytes"], 0);
        assert!(
            events[0]["payload"]["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains(expected)),
            "{record}: did not say {expected:?}: {}",
            interrupted.stdout
        );
    }
}
// llmlint: ignore-end[tests_mirror_real_usage]

/// A one-identity `qwen` chain: the harness with no out-of-band turn control that
/// takes the same wire shape the double already speaks, so a member on it runs
/// for real and only the lever is missing.
const QWEN_CHAIN: &str = "run_mode = \"fallback\"\nharnesses = [\"qwen\"]\n";

/// The default two-party graph, on a chain whose harness has no control
/// mechanism.
fn uncontrollable_graph(fake: &str) -> String {
    format!(
        concat!(
            "version: 1\nname: node-scope\n",
            "env:\n  ONEHARNESS_BIN_QWEN: {fake}\n",
            "members:\n  worker:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n",
        ),
        fake = fake,
    )
}
