//! The persisted contracts: their goldens, and how the CLI treats one this build
//! did not write.
//!
//! A run record outlives the binary that wrote it: `history` reads runs from
//! months ago, and an operator's tooling reads the same file. So its shape is
//! versioned, and both versions this build understands are committed here as
//! files rather than described in prose — a golden is the only kind of schema
//! documentation that fails when the code stops matching it.
//!
//! * `record.v1.json` — the shape written before the version field existed. It
//!   must keep reading, unchanged, forever, or upgrading loses history.
//! * `record.v2.json` — the first version that declared every member up front.
//! * `record.v3.json` — what this build writes, byte for byte.
//! * `control.v1.json` — the three shapes of `control.json`, the turn-control
//!   record a run writes into a member's scratch and a *separate*
//!   `oneagentgraph interrupt` process reads back, possibly from a later build.
//! * `member-died.json` — the two shapes of the one *event* payload that
//!   changed when onejudge became a library. It is not in `record.json`, but it
//!   is in the `events.jsonl` a record points at, which outlives the run exactly
//!   as the record does, and onepipeline compiles against it.
//!
//! Regenerating an old record golden to make a failure go away is the mistake this
//! guards against: if the bytes changed, either the change was meant — in which
//! case it is a version bump and a new golden beside the old one — or it was not.
//!
//! The journeys at the end plant a record by hand and then drive the compiled
//! binary against it. That is the only way to reach their subject: what the verbs
//! do with a record they would never have written — a hostile `run_id`, a stream
//! path outside the run store — and the real one a user meets it with comes from
//! a hand-edit, a torn write, or another version. They need no graph and no
//! harness, which is why they sit here with the rest of the record's contract
//! rather than among the command-surface journeys.

use std::collections::BTreeMap;

use oneagentgraph::control::{Address, Record as ControlRecord, Turn, CONTROL_SCHEMA_VERSION};
use oneagentgraph::event::{Cause, Disposition, MemberDied, ENVELOPE_VERSION};
use oneagentgraph::member::Rule;
use oneagentgraph::resolve::ResolvedRef;
use oneagentgraph::run::{MemberOutcome, Record, RunId, RECORD_SCHEMA_VERSION};

/// The record both goldens describe, at the current version.
fn golden_record() -> Record {
    Record {
        schema_version: RECORD_SCHEMA_VERSION,
        run_id: RunId::parse("node-scope-1786171301679-1447994").expect("a run id"),
        graph: "./graph.yaml".into(),
        name: "node-scope".into(),
        started_ms: 1_786_171_301_679,
        finished_ms: Some(1_786_171_308_421),
        exit_code: Some(0),
        members: BTreeMap::from([
            ("reporter".into(), MemberOutcome::Died(Rule::Activity)),
            ("worker".into(), MemberOutcome::Settled),
        ]),
        declared_members: vec!["reporter".into(), "worker".into()],
        refs: vec![ResolvedRef {
            origin: "./graph.yaml".into(),
            sha256: "3b1f2c4d5e6a7b8c9d0e1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e".into(),
            bytes: 412,
        }],
        events_path: "/state/node-scope-1786171301679-1447994/events.jsonl".into(),
    }
}

/// What this build writes is byte-for-byte the committed golden, and reading it
/// back gives the same record.
#[test]
fn the_current_golden_is_exactly_what_this_build_writes() {
    let golden = include_str!("golden/record.v3.json");
    let written = format!(
        "{}\n",
        serde_json::to_string_pretty(&golden_record()).expect("a record serializes")
    );
    assert_eq!(
        written, golden,
        "record.json's shape changed. If that was deliberate, bump \
         RECORD_SCHEMA_VERSION and commit a new golden beside the old one — do not \
         overwrite it, or nothing proves the old shape still reads."
    );

    let read: Record = serde_json::from_str(golden).expect("the golden reads");
    assert_eq!(read, golden_record(), "the golden did not round-trip");
    assert_eq!(read.schema_version, RECORD_SCHEMA_VERSION);
}

#[test]
fn a_version_two_record_still_reads() {
    let record: Record = serde_json::from_str(include_str!("golden/record.v2.json"))
        .expect("a version 2 record still reads");
    assert_eq!(record.schema_version, 2);
    assert_eq!(record.members["worker"], MemberOutcome::Settled);
}

/// A record written before the version field existed still reads, as version 1.
///
/// This is the whole point of defaulting it: every field added since is optional
/// and absent from that file, so it has to parse without them and report the
/// version it was actually written under.
#[test]
fn a_record_from_before_the_version_field_still_reads_as_version_one() {
    let v1: Record = serde_json::from_str(include_str!("golden/record.v1.json"))
        .expect("a version 1 record still reads");

    assert_eq!(v1.schema_version, 1, "an unversioned record is version 1");
    assert!(
        v1.declared_members.is_empty(),
        "a version 1 record declares no members; it predates the field"
    );
    // Everything version 1 *did* carry is unchanged, so upgrading does not lose
    // a run's history.
    assert_eq!(v1.run_id.as_str(), "node-scope-1786171301679-1447994");
    assert_eq!(v1.exit_code, Some(0));
    assert_eq!(v1.members[&"worker".to_string()], MemberOutcome::Settled);
    assert_eq!(
        v1.members[&"reporter".to_string()],
        MemberOutcome::Died(Rule::Activity)
    );
    assert_eq!(v1.refs[0].bytes, 412);
}

/// Every optional field is absent from the JSON when it is empty, and survives a
/// round trip when it is not.
///
/// This is what keeps a reader of an older record unaffected: the keys a newer
/// build added are simply not there when there is nothing to say, so the
/// document a version 1 reader sees is the document it always saw.
#[test]
fn an_empty_optional_field_is_omitted_and_a_filled_one_round_trips() {
    let mut record = golden_record();
    record.finished_ms = None;
    record.exit_code = None;
    record.members = BTreeMap::new();
    record.declared_members = Vec::new();
    record.refs = Vec::new();

    let value = serde_json::to_value(&record).expect("serializes");
    let keys: Vec<&str> = value
        .as_object()
        .expect("a mapping")
        .keys()
        .map(String::as_str)
        .collect();
    for absent in ["finished_ms", "exit_code", "declared_members"] {
        assert!(
            !keys.contains(&absent),
            "{absent} is empty and must be omitted, not written: {keys:?}"
        );
    }
    // The version is not optional: a record that named none would read as 1.
    assert!(keys.contains(&"schema_version"), "{keys:?}");
    assert_eq!(
        serde_json::from_value::<Record>(value).expect("round-trips"),
        record,
        "a record with every optional field empty did not round-trip"
    );

    // And the same field, filled, survives the trip rather than being dropped.
    let filled = golden_record();
    assert_eq!(
        serde_json::from_str::<Record>(&serde_json::to_string(&filled).expect("serializes"))
            .expect("round-trips")
            .declared_members,
        vec!["reporter".to_string(), "worker".to_string()]
    );
}

/// The two `member-died` payloads this build writes, in the order the golden
/// commits them: the member that really was a child process, then the one this
/// process drove in-library.
fn golden_deaths() -> Vec<MemberDied> {
    vec![
        MemberDied {
            rule: "provider-failure".into(),
            cause: Cause::Exited,
            detail: "harness failed (quota)".into(),
            truncated: false,
            exit_code: Some(2),
            disposition: Some(Disposition::Exited),
            stderr_tail: "harness failed (quota)".to_string().into(),
        },
        MemberDied {
            rule: "provider-failure".into(),
            cause: Cause::Quota,
            detail: "provider error (respond): the subscription is exhausted".into(),
            truncated: false,
            exit_code: None,
            disposition: None,
            stderr_tail: None,
        },
    ]
}

/// `member-died`'s two shapes are byte-for-byte the committed golden.
///
/// This payload is the one wire shape the onejudge library conversion changed,
/// and both halves of the change are here because both have to keep holding. A
/// member that really was a child process still reports `exit_code`,
/// `disposition`, and `stderr_tail` — dropping those would narrow what a
/// single-sided member's death says. One driven in this process reports none of
/// them, and says the same thing through `cause` and `detail` instead; writing
/// them as `null` would have a consumer read a process that never existed.
///
/// It rides envelope version 1, unchanged: `v` is the shared envelope's own
/// version, duplicated verbatim into every producer in this stack, so bumping it
/// here alone would desynchronize the very field a consumer reads *before* it
/// knows whether it can decode the rest.
#[test]
fn the_member_died_goldens_are_exactly_what_this_build_writes() {
    assert_eq!(ENVELOPE_VERSION, 1, "the envelope this payload rides");
    let golden = include_str!("golden/member-died.json");
    let written = format!(
        "{}\n",
        serde_json::to_string_pretty(&golden_deaths()).expect("the payloads serialize")
    );
    assert_eq!(
        written, golden,
        "member-died's shape changed. If that was deliberate it is a change to \
         docs/contract.md and to every consumer compiled against it — amend the contract \
         and commit the new bytes here in the same change."
    );

    let read: Vec<MemberDied> = serde_json::from_str(golden).expect("the golden reads");
    assert_eq!(read, golden_deaths(), "the golden did not round-trip");
}

/// The three turn-control records this build writes, in the order the golden
/// commits them: an address whose store the report named, one that left
/// oneharness's own default, and an ask that was refused.
fn golden_controls() -> Vec<ControlRecord> {
    let session = "node-scope-1786171301679-1447994-worker-skill".to_string();
    let cwd = std::path::PathBuf::from("/state/node-scope-1786171301679-1447994/members/worker");
    vec![
        ControlRecord {
            schema_version: CONTROL_SCHEMA_VERSION,
            turn: Turn::Open {
                address: Address {
                    session: session.clone(),
                    session_dir: Some("/state/oneharness/sessions".into()),
                    cwd: cwd.clone(),
                },
            },
        },
        ControlRecord {
            schema_version: CONTROL_SCHEMA_VERSION,
            turn: Turn::Open {
                address: Address {
                    session,
                    session_dir: None,
                    cwd,
                },
            },
        },
        ControlRecord {
            schema_version: CONTROL_SCHEMA_VERSION,
            turn: Turn::Unavailable {
                reason: "harness `qwen` has no out-of-band turn control, so --control cannot be \
                         honored"
                    .into(),
            },
        },
    ]
}

/// `control.json`'s three shapes are byte-for-byte the committed golden.
///
/// It is a persisted contract like the record beside it: a run writes it and a
/// *different* process — `oneagentgraph interrupt`, minutes or hours later, and
/// possibly a different build — reads it back. So the shape is versioned and
/// committed rather than described, and `session_dir` is absent when the run left
/// oneharness's own default, because an `interrupt` that says nothing resolves
/// the same store and a written `null` would claim otherwise.
#[test]
fn the_control_goldens_are_exactly_what_this_build_writes() {
    let golden = include_str!("golden/control.v1.json");
    let written = format!(
        "{}\n",
        serde_json::to_string_pretty(&golden_controls()).expect("the records serialize")
    );
    assert_eq!(
        written, golden,
        "control.json's shape changed. If that was deliberate, bump \
         CONTROL_SCHEMA_VERSION and commit a new golden *beside* control.v1.json — a run in \
         flight was written by the build before this one."
    );

    let read: Vec<ControlRecord> = serde_json::from_str(golden).expect("the golden reads");
    assert_eq!(read, golden_controls(), "the golden did not round-trip");
}

/// The three fields only a child process has are omitted when it had none, and
/// survive the trip when it did.
///
/// This is the half a consumer branches on. A build that wrote them as `null`
/// for an in-process member would make "no exit status" and "exit status null"
/// the same document, and a build that dropped them from a child's death would
/// lose the evidence that death carries.
#[test]
fn a_child_process_s_own_fields_are_omitted_when_empty_and_round_trip_when_not() {
    let (child, in_process) = {
        let mut deaths = golden_deaths().into_iter();
        (deaths.next().expect("child"), deaths.next().expect("lib"))
    };

    let value = serde_json::to_value(&in_process).expect("serializes");
    let keys: Vec<&str> = value
        .as_object()
        .expect("a mapping")
        .keys()
        .map(String::as_str)
        .collect();
    for absent in ["exit_code", "disposition", "stderr_tail", "truncated"] {
        assert!(
            !keys.contains(&absent),
            "{absent} is empty and must be omitted, not written as null: {keys:?}"
        );
    }
    // The three that answer for *every* member are never optional: a death that
    // named no rule or no cause is one a supervisor cannot branch on at all.
    for present in ["rule", "cause", "detail"] {
        assert!(keys.contains(&present), "{present} is missing: {keys:?}");
    }
    assert_eq!(
        serde_json::from_value::<MemberDied>(value).expect("round-trips"),
        in_process
    );

    let value = serde_json::to_value(&child).expect("serializes");
    assert_eq!(value["exit_code"], serde_json::json!(2));
    assert_eq!(value["disposition"], serde_json::json!("exited"));
    assert_eq!(
        value["stderr_tail"],
        serde_json::json!("harness failed (quota)")
    );
    assert_eq!(
        serde_json::from_value::<MemberDied>(value).expect("round-trips"),
        child
    );
}

/// A `member-died` payload carrying a field this build never heard of is
/// refused rather than silently dropped.
///
/// The envelope is external input — a consumer of this crate's library reads
/// streams this build did not write — and `docs/contract.md`'s trust-boundary
/// rule is that a typo fails loudly. Adding three optional fields is exactly the
/// change that would quietly turn `deny_unknown_fields` off if the derive were
/// lost, and nothing else here would notice.
#[test]
fn a_member_died_payload_with_an_unknown_field_is_rejected() {
    let mut value = serde_json::to_value(&golden_deaths()[1]).expect("serializes");
    value["exit_status"] = serde_json::json!(2);
    let err = serde_json::from_value::<MemberDied>(value)
        .expect_err("an unknown member-died field must not be silently dropped");
    assert!(
        err.to_string().contains("exit_status"),
        "the refusal did not name the field: {err}"
    );
}

/// A record from a *newer* build is refused for its version, not for a key.
///
/// `Record` denies unknown fields, so a later build's new key would otherwise be
/// what the error named — leaving an operator to guess that the run was simply
/// recorded by something newer.
#[test]
fn a_record_from_a_newer_build_is_refused_by_its_version() {
    let state = tempfile::tempdir().expect("a state dir");
    let run = state.path().join("node-scope-1786171301679-1447994");
    std::fs::create_dir_all(&run).expect("mkdir");

    let mut ahead = serde_json::to_value(golden_record()).expect("a record");
    ahead["schema_version"] = serde_json::json!(u64::from(RECORD_SCHEMA_VERSION) + 1);
    ahead["something_this_build_never_heard_of"] = serde_json::json!(true);
    std::fs::write(run.join("record.json"), ahead.to_string()).expect("write");

    let err = oneagentgraph::history::show(state.path(), "node-scope-1786171301679-1447994")
        .expect_err("a newer record is refused");
    let message = err.to_string();
    assert!(
        message.contains("newer than this build reads"),
        "the refusal did not name the version: {message}"
    );
    assert!(
        !message.contains("something_this_build_never_heard_of"),
        "the refusal blamed a key rather than the version: {message}"
    );
    // And such a record is skipped in a listing rather than hiding the rest.
    assert!(oneagentgraph::history::list(state.path()).is_empty());
}

/// One state directory, and the compiled binary pointed at it.
struct Store {
    root: tempfile::TempDir,
}

/// What one invocation produced.
struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Store {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().expect("a state directory"),
        }
    }

    fn state(&self) -> &std::path::Path {
        self.root.path()
    }

    /// Plant a record for `run_id`, exactly as given.
    fn plant(&self, run_id: &str, record: &serde_json::Value) -> std::path::PathBuf {
        let dir = self.state().join(run_id);
        std::fs::create_dir_all(&dir).expect("a run directory");
        std::fs::write(dir.join("record.json"), record.to_string()).expect("write the record");
        dir
    }

    fn run(&self, args: &[&str]) -> Ran {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_oneagentgraph"))
            .args(args)
            .env("ONEAGENTGRAPH_STATE_DIR", self.state())
            .output()
            .expect("the binary runs");
        Ran {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

impl Ran {
    fn expect_code(&self, code: i32) -> &Self {
        assert_eq!(
            self.code, code,
            "expected exit {code}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout, self.stderr
        );
        self
    }
}
/// A run id is a *path component* every verb joins onto the state directory,
/// and it arrives out of `record.json` as well as off the argv — so a record
/// carrying a traversal in that field is refused rather than followed.
///
/// `cancel` is the verb this matters most for: it is the one that *writes*
/// through the joined path, so a record naming `../..` would have it create a
/// directory and drop a stop signal outside the run store.
#[test]
fn a_record_naming_a_path_outside_the_run_store_is_refused() {
    let store = Store::new();
    let escapee = store.state().join("escapee");
    store.plant(
        "hostile",
        &serde_json::json!({
            "run_id": format!("../../{}", escapee.file_name().expect("a name").to_string_lossy()),
            "graph": "./graph.yaml",
            "name": "hostile",
            "started_ms": 1_u64,
            "events_path": "/state/hostile/events.jsonl",
        }),
    );

    let cancelled = store.run(&["cancel", "hostile"]);
    cancelled.expect_code(2);
    assert!(
        cancelled.stderr.contains("is not a run id"),
        "{}",
        cancelled.stderr
    );
    assert!(
        !escapee.exists(),
        "cancel wrote through a record's run id and escaped the run store: {}",
        escapee.display()
    );

    // And the same record never shows up in a listing, rather than listing as
    // a run whose id nothing else would accept.
    let listed = store.run(&["history"]);
    listed.expect_code(0);
    assert!(!listed.stdout.contains("hostile"), "{}", listed.stdout);
}

/// `events_path` is reported, never followed. A record naming a stream outside
/// the run store does not move where `trigger` and `reset-timer` write: those
/// derive the signal directory from the run's id, as `cancel` does.
///
/// The record here is otherwise valid — a well-formed run id, a readable record
/// — so what is under test is the *use* of the path rather than the parse that
/// already refuses a malformed id.
#[test]
fn a_record_naming_a_stream_elsewhere_does_not_move_where_a_signal_is_written() {
    let store = Store::new();
    let elsewhere = store.state().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("elsewhere");
    let run = store.plant(
        "planted-1-1",
        &serde_json::json!({
            "run_id": "planted-1-1",
            "graph": "./graph.yaml",
            "name": "planted",
            "started_ms": 1_u64,
            "members": {"worker": "settled"},
            "events_path": elsewhere.join("events.jsonl").display().to_string(),
        }),
    );

    let triggered = store.run(&["trigger", "planted-1-1", "worker"]);
    triggered.expect_code(0);
    assert!(
        run.join("signals").join("worker.trigger").exists(),
        "the signal did not land in the run's own directory: {}",
        triggered.stdout
    );
    assert!(
        !elsewhere.join("signals").exists(),
        "trigger wrote through the record's events_path and escaped the run store: {}",
        elsewhere.display()
    );
}
