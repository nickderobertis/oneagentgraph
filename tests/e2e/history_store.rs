//! What recording a member's turn costs a history store that is already large,
//! and what reading it back through the published pointer costs.
//!
//! A `kind: oneharness` member runs its turn in this process through the linked
//! core's `io::run`, so the history writer that turn opens is the linked core's.
//! oneharness's `docs/history-index.md` is what that writer is held to here:
//! recording appends to the run's own session file and one entry per line to
//! its date's segments under `.index.d/` — `runs-` for the closing line,
//! `events-` for each event — reading at most one trailing byte per append, and
//! reads no index and lists no directory; a pointer-path read opens the one
//! session file the pointer names and nothing else. Both are proven off a trace
//! of the calls the processes made, because an outcome alone cannot tell a
//! writer that never scanned from one that scanned, failed, and carried on.

// llmlint: ignore-file[e2e_not_mocked] see tests/e2e/support.rs: the paid harness
// process is the single sanctioned double, replaced at oneharness's own
// `ONEHARNESS_BIN_<ID>` seam. The binary under test, the core it links, the
// history store it records into, and the reader that follows the pointer are
// all real.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use oneharness_core::domain::history_index::{SegmentKind, UtcDate};
use oneharness_core::io::history;
use serde_json::Value;

use crate::support::{fake_harness, graph_with, single_sided_graph, Workspace};

/// One turn, completed, so the member's run writes its closing record.
const TASK: &str = "fake:complete-now: say something worth reading back";

/// The other sessions already in the store: at least a thousand, across several
/// project directories, so a writer that walked the tree or a reader that
/// listed a project would have plenty to be caught opening.
const PROJECTS: usize = 6;
const SESSIONS_PER_PROJECT: usize = 200;

/// How much of a segment a recording writer may read each time it appends to
/// it: the one trailing byte `docs/history-index.md` names, to see whether an
/// interrupted writer left the segment without its final newline.
const SEGMENT_TAIL_BYTES: i64 = 1;

/// The legacy index files an older core kept at the store's root. This core
/// never opens them on either path, so each is left unopenable.
const LEGACY_INDEXES: [&str; 2] = [".index.jsonl", ".event-index.jsonl"];

/// The legacy lock an older core took around its index.
const LEGACY_LOCK: &str = ".index.lock";

/// One file-system call out of a trace, reduced to what the assertions read.
#[derive(Debug)]
struct Call {
    line: String,
    syscall: String,
    /// The path the call names: an open's resolved path, or the path strace
    /// annotated the descriptor a read or a listing used with.
    path: Option<PathBuf>,
    /// The call's return value, when it returned a number.
    returned: Option<i64>,
}

/// Every call recorded under `<trace>.<pid>`, across every process traced.
fn calls(trace: &Path) -> Vec<Call> {
    let dir = trace.parent().expect("the trace sits in a directory");
    let stem = format!(
        "{}.",
        trace
            .file_name()
            .and_then(|it| it.to_str())
            .expect("a plain trace name")
    );
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).expect("the trace directory lists") {
        let path = entry.expect("a trace entry").path();
        if !path
            .file_name()
            .and_then(|it| it.to_str())
            .is_some_and(|name| name.starts_with(&stem))
        {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a trace file reads");
        found.extend(text.lines().filter_map(parse));
    }
    assert!(
        !found.is_empty(),
        "nothing was traced under {}",
        trace.display()
    );
    found
}

/// One strace line, `name(args) = result`, with `-y` descriptor annotations.
fn parse(line: &str) -> Option<Call> {
    let (syscall, rest) = line.split_once('(')?;
    let syscall = syscall.trim().to_string();
    if syscall.is_empty()
        || !syscall
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let (args, result) = rest.rsplit_once(") = ")?;
    let returned = result
        .split_whitespace()
        .next()
        .and_then(|it| it.split('<').next())
        .and_then(|it| it.parse().ok());
    let path = match syscall.as_str() {
        "open" | "creat" => quoted(args).map(PathBuf::from),
        "openat" | "openat2" => {
            let named = PathBuf::from(quoted(args)?);
            if named.is_absolute() {
                Some(named)
            } else {
                Some(annotated(args)?.join(named))
            }
        }
        _ => annotated(args),
    };
    Some(Call {
        line: line.to_string(),
        syscall,
        path,
        returned,
    })
}

/// The first string argument of a call.
fn quoted(args: &str) -> Option<String> {
    let (_, rest) = args.split_once('"')?;
    Some(rest.split_once('"')?.0.to_string())
}

/// The path `-y` annotated the call's first descriptor argument with.
fn annotated(args: &str) -> Option<PathBuf> {
    let first = args.split(", ").next()?;
    let (_, rest) = first.split_once('<')?;
    Some(PathBuf::from(rest.strip_suffix('>')?))
}

fn is_open(call: &Call) -> bool {
    matches!(
        call.syscall.as_str(),
        "open" | "openat" | "openat2" | "creat"
    )
}

fn is_listing(call: &Call) -> bool {
    matches!(call.syscall.as_str(), "getdents" | "getdents64")
}

fn is_read(call: &Call) -> bool {
    matches!(
        call.syscall.as_str(),
        "read" | "pread64" | "readv" | "preadv" | "preadv2"
    )
}

/// The calls that name something inside the store — a failed one as much as a
/// successful one, since a writer that tried to open the legacy index and fell
/// back to walking the tree is exactly what this is here to catch.
fn in_store<'a>(calls: &'a [Call], store: &Path) -> Vec<&'a Call> {
    calls
        .iter()
        .filter(|call| call.path.as_deref().is_some_and(|it| it.starts_with(store)))
        .collect()
}

/// A store that already holds a thousand and more other sessions and the legacy
/// index files an older core left, with those files made unopenable. Returns
/// every file it wrote, with its bytes.
fn populate(store: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut written = BTreeMap::new();
    for project in 0..PROJECTS {
        let dir = store.join(format!("-home-someone-project-{project}"));
        std::fs::create_dir_all(&dir).expect("a project directory");
        for session in 0..SESSIONS_PER_PROJECT {
            let path = dir.join(format!("earlier-{project}-{session}.jsonl"));
            let bytes = format!(
                "{{\"schema_version\":\"1.0\",\"session\":\"earlier-{project}-{session}\"}}\n"
            )
            .into_bytes();
            std::fs::write(&path, &bytes).expect("an earlier session");
            written.insert(path, bytes);
        }
    }
    for name in LEGACY_INDEXES {
        let path = store.join(name);
        let bytes = format!("{{\"legacy\":\"{name}\"}}\n").into_bytes();
        std::fs::write(&path, &bytes).expect("a legacy index");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000))
            .expect("the legacy index made unopenable");
        written.insert(path, bytes);
    }
    let lock = store.join(LEGACY_LOCK);
    std::fs::write(&lock, b"").expect("the legacy lock");
    written.insert(lock, Vec::new());
    written
}

/// Every file `populate` wrote still holds exactly the bytes it wrote, and the
/// legacy indexes are still unopenable — read back by making each readable for
/// the comparison only.
fn assert_untouched(written: &BTreeMap<PathBuf, Vec<u8>>) {
    for (path, bytes) in written {
        let legacy = path
            .file_name()
            .and_then(|it| it.to_str())
            .is_some_and(|name| LEGACY_INDEXES.contains(&name));
        if legacy {
            let mode = std::fs::metadata(path).expect("the legacy index is still there");
            assert_eq!(
                mode.permissions().mode() & 0o777,
                0,
                "{} was re-permissioned",
                path.display()
            );
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400))
                .expect("the legacy index made readable for the comparison");
        }
        let now = std::fs::read(path)
            .unwrap_or_else(|err| panic!("{} is gone or unreadable: {err}", path.display()));
        assert_eq!(&now, bytes, "{} changed", path.display());
    }
}

/// A `kind: oneharness` member's turn records into a large store whose legacy
/// index cannot be opened, touching only its own session file and the dated
/// segments it appends to; the session the graph's published pointer names then
/// reads back by that pointer alone, without the index or any listing.
#[test]
fn recording_a_turn_and_reading_it_back_scan_no_history() {
    let workspace = Workspace::new();
    let store_dir = workspace.at("history");
    std::fs::create_dir_all(&store_dir).expect("the history store");
    let store = std::fs::canonicalize(&store_dir).expect("the store resolves");
    let written = populate(&store);
    assert!(written.len() > 1000, "{} files", written.len());
    let pointer_file = workspace.at("pointers.jsonl");
    workspace.graph(&graph_with(
        &single_sided_graph(&fake_harness()),
        &[
            ("env.ONEHARNESS_HISTORY", "1".to_string()),
            ("env.ONEHARNESS_HISTORY_DIR", store.display().to_string()),
            (
                "env.ONEHARNESS_HISTORY_POINTER_FILE",
                pointer_file.display().to_string(),
            ),
        ],
    ));
    let traces = tempfile::tempdir().expect("a trace directory");

    // Recording: the graph runs its member's turn under the trace.
    let record_trace = traces.path().join("record");
    let dir = workspace.dir().display().to_string();
    let run = workspace.run_traced(
        &record_trace,
        env!("CARGO_BIN_EXE_oneagentgraph"),
        &["run", "./graph.yaml", "--task", TASK, "--dir", &dir],
    );
    run.expect_code(0);
    assert_eq!(
        run.of_kind("member-settled").len(),
        1,
        "the member's turn did not complete: {:?}",
        run.kinds()
    );

    // The pointer the graph published, and the entry its run left in the one
    // segment named for its id's UTC date.
    let read = history::read_pointers(&pointer_file).expect("the pointer file reads");
    assert_eq!((read.pointers.len(), read.skipped), (1, 0), "{read:?}");
    let pointer = &read.pointers[0];
    let history_id = pointer.history_id();
    let date = UtcDate::of_history_id(history_id).expect("a run's id carries its date");
    let segment = store
        .join(".index.d")
        .join(SegmentKind::Runs.file_name(date));
    let entries = std::fs::read_to_string(&segment)
        .unwrap_or_else(|err| panic!("no segment {}: {err}", segment.display()));
    assert!(
        entries
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .any(|entry| entry["kind"] == "run" && entry["history_id"] == history_id.to_string()),
        "{} holds no run entry for {history_id}:\n{entries}",
        segment.display()
    );
    let session_file = store
        .join(pointer.history_project())
        .join(format!("{}.jsonl", pointer.history_session()));
    assert!(
        session_file.is_file(),
        "{} was not written",
        session_file.display()
    );

    // The run's events went to the other segment for the same date: each event
    // line is one entry there, as each closing run line is one entry here.
    let events_segment = store
        .join(".index.d")
        .join(SegmentKind::Events.file_name(date));
    let events = std::fs::read_to_string(&events_segment)
        .unwrap_or_else(|err| panic!("no segment {}: {err}", events_segment.display()));
    assert!(
        events
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .any(|entry| entry["kind"] == "event" && entry["run_id"] == history_id.to_string()),
        "the turn published no event into {}, so its per-event write went unproven:\n{events}",
        events_segment.display()
    );
    let segments = [segment.as_path(), events_segment.as_path()];

    let recorded = calls(&record_trace);
    let touched = in_store(&recorded, &store);
    for call in &touched {
        if is_open(call) {
            let path = call.path.as_deref().expect("an open names a path");
            assert!(
                path == session_file || segments.contains(&path),
                "recording opened a store file other than its session and its date's \
                 segments: {}",
                call.line
            );
        }
        assert!(
            !is_listing(call),
            "recording listed a directory in the store: {}",
            call.line
        );
    }
    for path in segments {
        let opened = touched
            .iter()
            .filter(|call| is_open(call) && call.path.as_deref() == Some(path))
            .count();
        let read: i64 = touched
            .iter()
            .filter(|call| is_read(call) && call.path.as_deref() == Some(path))
            .map(|call| call.returned.unwrap_or(0).max(0))
            .sum();
        assert!(
            read <= SEGMENT_TAIL_BYTES * i64::try_from(opened).expect("a count"),
            "recording read {read} bytes of {} over {opened} appends, past one trailing \
             byte each",
            path.display()
        );
    }
    assert!(
        touched.iter().any(|call| is_open(call)
            && call.path.as_deref() == Some(segment.as_path())
            && call.returned.is_some_and(|fd| fd >= 0)),
        "the trace saw no append to {} — it is not tracing the writer",
        segment.display()
    );

    // Reading: the consumer follows the published pointer under the trace.
    let read_trace = traces.path().join("read");
    let reader = workspace.run_traced(
        &read_trace,
        env!("CARGO_BIN_EXE_oneagentgraph-history-reader"),
        &[&pointer_file.display().to_string()],
    );
    reader.expect_code(0);
    let found: Value = serde_json::from_str(reader.stdout.trim())
        .unwrap_or_else(|err| panic!("the reader printed no record ({err}): {}", reader.stdout));
    assert_eq!(found["history_id"], history_id.to_string());
    assert_eq!(found["session_file"], session_file.display().to_string());
    assert!(
        found["prompt"].as_str().is_some_and(|it| it.contains(TASK)),
        "the record read back is not this turn's: {found}"
    );
    let read_calls = calls(&read_trace);
    let read_touched = in_store(&read_calls, &store);
    for call in &read_touched {
        if is_open(call) {
            assert_eq!(
                call.path.as_deref(),
                Some(session_file.as_path()),
                "the read opened a store file other than the session its pointer names: {}",
                call.line
            );
        }
        assert!(
            !is_listing(call),
            "the read listed a directory in the store: {}",
            call.line
        );
    }
    assert!(
        read_touched.iter().any(|call| is_open(call)),
        "the trace saw the read open nothing in the store — it is not tracing the reader"
    );

    // Nothing else in the store moved: the legacy lock was neither created nor
    // changed (it predates this run, and no call above opened it, `O_CREAT` or
    // not), and every other session's file and both legacy indexes hold the
    // bytes they held before.
    assert_untouched(&written);
}
