//! The conversation behind a member's turns: the label that names it, and the
//! oneharness record it was written into.
//!
//! Both are read off the stream a real member run produced. A hand-built
//! envelope would prove the serializer and nothing else — what these journeys
//! are about is whether an operator can get from an event to the transcript the
//! agent actually had, which is a claim about a file oneharness wrote on disk.

// llmlint: ignore-file[e2e_not_mocked] see tests/e2e/support.rs: the paid harness
// process is the single sanctioned double, replaced at oneharness's own
// `ONEHARNESS_BIN_<ID>` seam. Everything these journeys read — the real
// oneharness history store, the real onejudge telemetry the pointer is built
// from, and the binary under test — is the real thing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use oneharness_core::io::history;
use serde_json::Value;

#[cfg(unix)]
use crate::support::make_executable;
use crate::support::{
    as_env, assert_session_labels, bounds, fake_harness, graph_with, two_party_graph, Run,
    Workspace, FAKE_HARNESS_KEY,
};

/// The task these journeys run: one turn, completed, so a member reaches its
/// settle and both sides of the conversation write a history record.
const TASK: &str = "fake:complete-now: say something worth reading back";

fn sessions(run: &Run) -> Vec<Value> {
    run.of_kind("oneharness-session")
}

/// The session file a pointer's three path fields name, resolved the way the
/// consumer resolves it: oneharness's own library, step for step, rather than a
/// path this test rebuilt out of the same strings.
fn resolve(payload: &Value) -> PathBuf {
    let dir =
        history::resolve_dir(payload["history_dir"].as_str()).expect("the payload named a store");
    let path = history::find_session_path(
        &dir,
        payload["history_project"].as_str(),
        payload["history_session"]
            .as_str()
            .expect("the payload named a session"),
    )
    .expect("the history store is readable")
    .unwrap_or_else(|| panic!("no session file under {} for {payload}", dir.display()));
    // The layout `src/judge.rs` refuses a `history_file` outside of, held here
    // against the file oneharness really wrote — upstream keeps the extension
    // private, so this is what stops that constant from drifting off it.
    assert_eq!(
        path.extension().and_then(|it| it.to_str()),
        Some("jsonl"),
        "{}",
        path.display()
    );
    path
}

/// One record out of a session file, by the id the artifact names.
fn read_record(path: &Path, id: &Value) -> Value {
    let records = history::read_session_display(path).expect("the session file reads back");
    records
        .into_iter()
        .find(|record| &record["history_id"] == id)
        .unwrap_or_else(|| panic!("{} holds no record {id}", path.display()))
}

/// A settled member publishes one pointer per oneharness invocation, and the
/// three path fields on it resolve the session file that invocation wrote.
///
/// Resolved rather than compared: the point of publishing a store, a project and
/// a session name — instead of one path — is that a consumer reaches the file
/// through oneharness's own library, so the journey reaches it the same way and
/// reads the record whose id is the one the artifact names.
#[test]
fn a_settled_member_publishes_a_pointer_that_opens_the_conversation_it_had() {
    let workspace = Workspace::new();
    let run = workspace.run_task(TASK);
    run.expect_code(0);

    let published = sessions(&run);
    assert!(
        !published.is_empty(),
        "a settled member published no conversation to read: {:?}",
        run.kinds()
    );
    // A two-party member is two conversations: the side that does the work and
    // the side that supervises it, each with its own oneharness invocations.
    let roles: Vec<&str> = published
        .iter()
        .filter_map(|event| event["payload"]["role"].as_str())
        .collect();
    assert!(
        roles.contains(&"agent") && roles.contains(&"judge"),
        "one side of the conversation published nothing: {roles:?}"
    );

    for event in &published {
        let payload = &event["payload"];
        assert_eq!(event["labels"]["member"], "worker");
        assert!(
            payload["turn"].as_u64().is_some_and(|turn| turn >= 1),
            "a pointer named no turn: {payload}"
        );
        assert_eq!(
            payload["identity"], "claude-code",
            "a pointer named an identity that did not run: {payload}"
        );

        let artifacts = event["artifacts"].as_array().expect("an artifact list");
        assert_eq!(artifacts.len(), 1, "{event}");
        let artifact = &artifacts[0];
        assert_eq!(artifact["kind"], "oneharness_session");
        assert_eq!(
            artifact["id"], payload["history_id"],
            "the artifact names a record other than the one the payload points at"
        );

        // The consumer's own resolution, step for step: the configured store,
        // then the session file inside its project, then the records in it.
        let path = resolve(payload);
        let record = read_record(&path, &artifact["id"]);
        assert!(
            record["prompt"].as_str().is_some_and(|it| !it.is_empty()),
            "the record the pointer opened carries no conversation: {record}"
        );

        // The count was taken as the event was published, and the agent side
        // appends every later turn to the same file — so it is a real size that
        // the finished file is at least as large as, never a placeholder.
        let published_bytes = artifact["bytes"].as_u64().expect("a byte count");
        let on_disk = std::fs::metadata(&path).expect("the session file").len();
        assert!(
            published_bytes > 0 && published_bytes <= on_disk,
            "{published_bytes} bytes published against {on_disk} on disk"
        );
    }
}

/// A run that **failed** still names the conversation the side that ran had.
///
/// This is the case the pointer exists for: a member that produced no report at
/// all is exactly when an operator has nothing else to read, and the failed run's
/// telemetry is the only place the record is named. The agent side runs and
/// writes its history; the judge side's chain reaches no identity, so the member
/// dies — and the agent's transcript is still openable.
///
/// A POSIX shell stands in for the refusing identity, which is why this is
/// unix-only: what the journey needs is one identity answering differently from
/// another, and a script is how that is said without a second compiled double.
#[cfg(unix)]
#[test]
fn a_failed_run_still_names_the_conversation_the_side_that_ran_had() {
    let workspace = Workspace::new();
    // Only the judge side is moved: its chain is a single `codex` candidate, and
    // the binary behind that id refuses. The agent side is untouched, so it runs
    // its turn for real and writes the record this journey opens.
    workspace.write(
        "oneharness.judge.toml",
        "run_mode = \"fallback\"\nharnesses = [\"codex\"]\n",
    );
    let refusing = workspace.write(
        "refusing.sh",
        &format!(
            "#!/bin/sh\nFAKE_HARNESS_REFUSAL=quota exec {} \"$@\"\n",
            fake_harness()
        ),
    );
    make_executable(&refusing);
    workspace.graph(&two_party_graph(
        &fake_harness(),
        &[("ONEHARNESS_BIN_CODEX", refusing.display().to_string())],
    ));

    let run = workspace.run_task(TASK);
    run.expect_code(1);
    assert_eq!(run.of_kind("member-died").len(), 1, "{:?}", run.kinds());
    assert!(
        run.of_kind("member-settled").is_empty(),
        "the member settled, so this journey no longer drives a failed run"
    );

    let published = sessions(&run);
    let agent: Vec<&Value> = published
        .iter()
        .filter(|event| event["payload"]["role"] == "agent")
        .collect();
    assert!(
        !agent.is_empty(),
        "a failed run named no conversation at all: {:?}",
        run.kinds()
    );
    for event in agent {
        let payload = &event["payload"];
        let path = resolve(payload);
        assert!(
            read_record(&path, &event["artifacts"][0]["id"])["prompt"]
                .as_str()
                .is_some_and(|it| !it.is_empty()),
            "the record a failed run pointed at carries no conversation"
        );
    }
    // The judge side reached no identity, so it wrote nothing to point at — the
    // same rule as the journey below, on the other side of one member.
    assert!(
        published
            .iter()
            .all(|event| event["payload"]["role"] == "agent"),
        "a side that ran nothing published a conversation: {published:?}"
    );
    assert_session_labels(&run);
}

/// A member a watchdog **condemned** still names the conversations it had
/// before it stalled.
///
/// The third place a member's telemetry is published from, and the one whose
/// events are all an operator gets: this member never settled and never reported
/// a failure of its own — it was stopped — so the records of the turns it did
/// take are the only account of what it was doing when it stopped.
#[test]
fn a_condemned_member_still_names_the_conversations_it_had_before_it_stalled() {
    let workspace = Workspace::new();
    // The heartbeat deadline, tightened until the rule fires on a member whose
    // turn never answers; the stall bound is left wide so the rule that fires is
    // this one. The same shape `tests/e2e/liveness.rs` drives it under.
    let held = bounds("0.05", "600");
    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "fake:hang after saying something worth reading back",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &as_env(&held),
    );
    run.expect_code(1);
    assert_eq!(run.of_kind("member-died").len(), 1, "{:?}", run.kinds());

    let published = sessions(&run);
    assert!(
        !published.is_empty(),
        "a condemned member accounted for nothing it had done: {:?}",
        run.kinds()
    );
    for event in &published {
        let path = resolve(&event["payload"]);
        assert!(
            read_record(&path, &event["artifacts"][0]["id"])["prompt"]
                .as_str()
                .is_some_and(|it| !it.is_empty()),
            "the record a condemned member pointed at carries no conversation"
        );
    }
    assert_session_labels(&run);
}

/// Exactly the four kinds that name a turn carry a `session` label, and every
/// other kind the run put on its stream carries none.
///
/// The exclusion is the half that matters: a consumer reads a labelled envelope
/// that is not a turn's activity or interruption as one transcript turn, so a
/// label on the heartbeats or the settle would render a transcript of them.
#[test]
fn a_real_runs_turns_are_labelled_with_their_conversation_and_nothing_else_is() {
    let workspace = Workspace::new();
    let run = workspace.run_task(TASK);
    run.expect_code(0);

    let labelled = assert_session_labels(&run);
    assert_eq!(
        labelled,
        ["turn-activity", "turn-completed", "turn-started"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        "the turns of a settled member are not the events carrying a conversation: {:?}",
        run.kinds()
    );
    // The kinds that must never carry one, named rather than left to the
    // negative above: these are the ones a run really publishes, so a regression
    // that labelled them would be caught here even if the set above moved.
    for kind in ["member-started", "member-settled", "graph-settled"] {
        assert!(
            !run.of_kind(kind).is_empty(),
            "this journey no longer observes {kind}: {:?}",
            run.kinds()
        );
    }
    assert!(
        !sessions(&run).is_empty(),
        "this journey no longer observes an oneharness-session to exclude"
    );
}

/// A `session` an operator stamped by hand does not become the conversation:
/// the run replaces it on the turns and takes it off everything else.
///
/// `--label` reaches every envelope of a run, so this is the one way a stream
/// can arrive at a consumer with a session on a heartbeat — the exact thing that
/// would render as a transcript of heartbeats. The emitter owns the key, and
/// this is that ownership driven through the CLI that can contest it.
#[test]
fn an_operators_own_session_label_does_not_become_the_conversation() {
    let workspace = Workspace::new();
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        TASK,
        "--dir",
        &workspace.dir().display().to_string(),
        "--label",
        "session=theirs",
    ]);
    run.expect_code(0);

    assert!(
        !run.stdout.contains("\"session\":\"theirs\""),
        "an operator's own session reached the stream: {}",
        run.stdout
    );
    // And the label is still there on the turns, carrying the run's own value —
    // which `assert_session_labels` checks against each envelope's stream and
    // member.
    assert!(assert_session_labels(&run).contains("turn-started"));
}

/// A conversation stays within the bound however long the run is named, and two
/// members of one over-long run are still two conversations.
///
/// The bound is reachable from the outside: a graph names itself, that name is
/// most of the run id, and the run id is most of the session. Two things can go
/// wrong there, and both are here. A consumer that refuses an over-long
/// identifier refuses the transcript of a run whose only sin was a descriptive
/// name — and a label brought under the bound by cutting off its end drops the
/// member with it, which merges every member of that run into one conversation
/// the consumer has no way to tell apart. This graph is named past the bound and
/// has two members, so a label that stopped naming its member fails here.
#[test]
fn two_members_of_an_over_long_run_are_still_two_conversations() {
    let workspace = Workspace::new();
    let long = "supervised-implementation-of-the-quarterly-reporting-service-".repeat(3);
    workspace.graph(&graph_with(
        concat!(
            "version: 1\nname: named-in-full\n",
            "env: {}\n",
            "members:\n  worker:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "  reviewer:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
        ),
        &[
            (FAKE_HARNESS_KEY, fake_harness()),
            ("name", long.to_string()),
        ],
    ));

    let run = workspace.run_task(TASK);
    run.expect_code(0);

    // Each member's own conversation, off the turns it really took.
    let turns = run.of_kind("turn-started");
    let conversation = |member: &str| -> String {
        let sessions: BTreeSet<&str> = turns
            .iter()
            .filter(|event| event["labels"]["member"] == member)
            .map(|event| {
                event["labels"]["session"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a turn of {member} names no conversation: {event}"))
            })
            .collect();
        assert_eq!(
            sessions.len(),
            1,
            "{member} took its turns in {sessions:?} rather than in one conversation"
        );
        sessions.iter().next().expect("a conversation").to_string()
    };
    let worker = conversation("worker");
    let reviewer = conversation("reviewer");
    // The run's own id, which the half the two members share comes from.
    let stream = turns[0]["stream"]
        .as_str()
        .expect("every envelope names its stream");

    assert_ne!(
        worker, reviewer,
        "two members of one run answer to a single conversation, which a consumer \
         would render as one transcript of both"
    );
    for (member, session) in [("worker", &worker), ("reviewer", &reviewer)] {
        assert_eq!(
            session.chars().count(),
            128,
            "a run named past the bound was not brought under it: {session}"
        );
        // Brought under the bound, not mangled: what is left is still an
        // identifier, and it still names both the run and the member.
        assert!(
            session
                .chars()
                .all(|it| it.is_ascii_alphanumeric() || matches!(it, '-' | '_' | '.'))
                && !session.starts_with('.'),
            "{session} is not an identifier the consumer accepts"
        );
        assert!(
            session.contains(&format!(".{member}-")),
            "the conversation no longer names the member whose turns it holds: {session}"
        );
        let (from_stream, _) = session.split_once('.').expect("a stream and a member");
        assert!(
            stream.ends_with(from_stream),
            "the conversation no longer names the run it belongs to: {session}"
        );
    }
    assert_session_labels(&run);
}

/// The text rendering names the record too, so a terminal watching a run is not
/// the one view that cannot get to the transcript.
#[test]
fn the_text_rendering_names_the_record_a_pointer_opens() {
    let workspace = Workspace::new();
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        TASK,
        "--dir",
        &workspace.dir().display().to_string(),
        "--output",
        "text",
    ]);
    run.expect_code(0);

    let lines: Vec<&str> = run
        .stdout
        .lines()
        .filter(|line| line.contains("oneharness-session"))
        .collect();
    assert!(
        !lines.is_empty(),
        "no pointer was rendered:\n{}",
        run.stdout
    );
    for line in lines {
        let (_, detail) = line
            .split_once("oneharness-session ")
            .expect("a rendered pointer says which conversation it names");
        let mut parts = detail.split_whitespace();
        assert!(
            matches!(parts.next(), Some("agent" | "judge")),
            "a rendered pointer names no side: {line}"
        );
        assert_eq!(parts.next(), Some("turn"), "{line}");
        assert!(parts.next().is_some_and(|turn| turn.parse::<u64>().is_ok()));
        assert!(
            parts.next().is_some_and(|id| !id.is_empty()),
            "a rendered pointer names no record: {line}"
        );
    }
}

/// A member whose chain reached no identity publishes no pointer at all: there
/// is no conversation, and a pointer at a file nobody wrote is worse than
/// silence.
///
/// The failure path of the same telemetry the settled journey reads — a run that
/// produced no report at all, which is where `fallback-advanced` still speaks and
/// this must not.
#[test]
fn a_member_whose_chain_ran_nothing_publishes_no_conversation_to_point_at() {
    let workspace = Workspace::new();
    workspace.graph(&two_party_graph(
        &fake_harness(),
        &[("FAKE_HARNESS_REFUSAL", "quota")],
    ));
    let run = workspace.run_task(TASK);
    run.expect_code(1);

    assert!(
        !run.of_kind("fallback-advanced").is_empty(),
        "the refusing chain reported no candidate at all: {:?}",
        run.kinds()
    );
    assert!(
        sessions(&run).is_empty(),
        "a chain that ran no turn published a conversation: {:?}",
        sessions(&run)
    );
    assert_eq!(run.of_kind("member-died").len(), 1, "{:?}", run.kinds());
    assert_session_labels(&run);
}
