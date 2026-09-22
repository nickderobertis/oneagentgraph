//! The conversion inventory is driven by the surfaces it describes.
//!
//! `docs/oneharness-library.md` says the `oneharness run` hop is converted, and
//! rests that claim on names it does not own: `oneharness-core`'s public API on
//! one side, `docs/contract.md`'s wire schema and this crate's manifest on the
//! other. Neither copy can be generated — the document is prose written for a
//! reader — so this suite holds it against them. Every upstream field the
//! document names is resolved by the compiler against the real type, every wire
//! name it restates is built as the type this crate serializes, and the version
//! it credits is read out of `Cargo.toml`.
//!
//! One test here is not about the document at all and lives here because its
//! subject is the same engine:
//! [`the_graph_resolves_one_oneharness_core_and_it_is_the_one_the_manifest_takes`]
//! reads `Cargo.lock`, because how many copies of that engine a build links is a
//! question only the resolution can answer.
//!
//! The load-bearing one is [`the_seam_the_conversion_rests_on_is_still_there`]:
//! the grouping hooks are the entire reason this conversion was safe to make, and
//! a build that quietly lost them would leave the activity watchdog looking at an
//! empty tree and a killed run's paid harnesses billing. It is written so that
//! disappearing upstream is a failure here rather than a silent regression there.

use std::collections::BTreeSet;
use std::process::{Child, Command};

use oneagentgraph::event::{Cause, Disposition, MemberDied, Runner};
use oneharness_core::domain::history::HistoryId;
use oneharness_core::domain::report::{FallbackReport, RunReport, RunResult};
use oneharness_core::io::cancel::CancelToken;
use oneharness_core::io::run::{RunControls, RunOutcome, RunRequest};
use oneharness_core::io::runner::ProcessSupervisor;

/// The inventory itself, read at compile time rather than copied beside this.
const INVENTORY: &str = include_str!("../docs/oneharness-library.md");
/// The approved contract, which owns every wire name the inventory restates.
const CONTRACT: &str = include_str!("../docs/contract.md");
/// The manifest, which owns the `oneharness-core` version the inventory names.
const MANIFEST: &str = include_str!("../Cargo.toml");
/// The committed lockfile, which owns how many `oneharness-core`s the graph
/// actually resolves — a question the manifest cannot answer, because the second
/// copy was never this crate's own requirement.
///
/// Shipped to consumers too: `cargo package` carries `Cargo.lock` because this
/// crate has binaries, so the test below runs in a published crate as well as here.
const LOCKFILE: &str = include_str!("../Cargo.lock");

/// Both halves of the grouping seam, recorded here as an implementation of the
/// upstream trait.
///
/// `src/harness.rs`'s own implementation is the real one; this exists so the
/// *shape* of the seam is asserted by a test rather than only by a module that
/// happens to compile. A hook renamed, dropped, or given a different signature
/// upstream fails here with the reason attached.
struct BothHooks {
    /// Set by `spawning`, so the assertion below is about a hook that ran rather
    /// than one that merely exists.
    prepared: std::sync::atomic::AtomicBool,
}

impl ProcessSupervisor for BothHooks {
    fn spawning(&self, command: &mut Command) {
        // The last look before the fork, and it is the `Command` actually
        // spawned — which is what lets `scratch::Group::prepare` put the POSIX
        // ownership stamp and the Windows `CREATE_SUSPENDED` flag on the harness
        // itself.
        command.env("ONEAGENTGRAPH_INVENTORY_PROBE", "1");
        self.prepared
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn spawned(&self, child: &Child) {
        // A shared reference, and on Windows a still-suspended process: read the
        // id and return. `scratch::Group::join` is what this does for real, and
        // it deliberately does *not* resume — see the inventory.
        let _ = child.id();
    }
}

/// The entry point that takes a caller's claim on each harness child, as the
/// signature `src/harness.rs` calls it by.
///
/// Written as a type so the assertion below is a coercion the compiler checks:
/// a parameter added, dropped or reordered upstream fails to compile here.
type Supervised = fn(
    &RunRequest,
    RunControls<'_>,
    Option<&dyn ProcessSupervisor>,
) -> Result<RunOutcome, oneharness_core::errors::OneharnessError>;

/// The seam the conversion rests on: a supervisor with both hooks, handed to a
/// run through the entry point that takes one.
///
/// Every name resolved by the compiler. `run_supervised` taken as a function
/// value rather than called, so the signature is checked without spawning a paid
/// harness — the suite proves the *behaviour* end to end in
/// `tests/e2e/liveness.rs`, against a real grouped tree.
#[test]
fn the_seam_the_conversion_rests_on_is_still_there() {
    let hooks = BothHooks {
        prepared: std::sync::atomic::AtomicBool::new(false),
    };
    let mut command = Command::new("does-not-run");
    hooks.spawning(&mut command);
    assert!(
        hooks.prepared.load(std::sync::atomic::Ordering::SeqCst),
        "the pre-fork hook did not run"
    );

    let entry: Supervised = oneharness_core::io::run::run_supervised;
    let supervised: Option<&dyn ProcessSupervisor> = Some(&hooks);
    assert!(
        supervised.is_some(),
        "the supervisor is the caller's to pass"
    );
    let _ = entry;

    assert!(
        INVENTORY.contains("run_supervised") && INVENTORY.contains("ProcessSupervisor"),
        "the inventory stopped naming the seam it rests on"
    );
}

/// The pointer line and its reader, as the signatures a consumer of an
/// in-process turn resolves them by.
///
/// Written as coercions so the assertion is one the compiler checks, the way
/// [`Supervised`] is: a lock resolved below the release that carries them —
/// `oneharness-core` 0.17.0 — does not build this test, and the requirement
/// cannot slide back to a core that writes no line without this file saying so.
/// Nothing here writes a pointer or reads a real one: `src/harness.rs` sets no
/// `history_pointer_file` of its own, because the environment is how a consumer
/// names the file and the core reads it there, and the journey in
/// `tests/e2e/session.rs` is what drives that through a real member. What is
/// exercised at run time is only the reader's documented answer for a file that
/// does not exist — empty, nothing skipped — because a consumer polls the file
/// before the first run has begun.
type PointerReader = fn(
    &std::path::Path,
) -> Result<
    oneharness_core::io::history::HistoryPointers,
    oneharness_core::errors::OneharnessError,
>;
type PointerId = fn(&oneharness_core::domain::history::HistoryPointer) -> HistoryId;

#[test]
fn the_pointer_line_and_its_reader_are_in_the_linked_core() {
    let read: PointerReader = oneharness_core::io::history::read_pointers;
    let id: PointerId = oneharness_core::domain::history::HistoryPointer::history_id;
    let _ = id;

    let scratch = tempfile::tempdir().expect("a scratch directory");
    let pointers = read(&scratch.path().join("never-written.jsonl"))
        .expect("a pointer file nobody has written yet reads as empty, not as an error");
    assert!(
        pointers.pointers.is_empty() && pointers.skipped == 0,
        "a missing pointer file read as something: {pointers:?}"
    );
}

/// `RunControls` is still exactly the four side channels `src/harness.rs` builds
/// a literal for.
///
/// Exhaustive on purpose — no `..` — because that literal is exhaustive too: a
/// field added upstream breaks this compile *and* that one, and the two have to
/// be reconciled together. Upstream states this is why the supervisor took its
/// own entry point rather than a field here.
#[test]
fn the_controls_the_conversion_builds_are_still_these_four() {
    let controls = RunControls {
        events: None,
        cancel: CancelToken::new(),
        signal_cancel: false,
        version: None,
    };
    let RunControls {
        events,
        cancel,
        signal_cancel,
        version,
    } = controls;
    assert!(events.is_none(), "the sink is the caller's to supply");
    assert!(!cancel.is_cancelled(), "a fresh token is uncancelled");
    assert!(
        !signal_cancel,
        "an embedder handles its own signals — this process is many members"
    );
    assert!(version.is_none(), "the engine names itself by default");
}

/// The name of an upstream field, produced from the very tokens the compiler
/// resolves it by.
///
/// `offset_of!` needs the type and the field but no value, so this reaches fields
/// of types nothing here can cheaply build — `RunReport` has two dozen required
/// members — while `stringify!` makes the string a product of the same tokens
/// rather than a second copy of them. A field renamed or dropped upstream fails to
/// compile; a string that disagrees with the field is unwritable.
macro_rules! field {
    ($type:ty, $field:ident) => {{
        let _ = std::mem::offset_of!($type, $field);
        concat!(stringify!($type), "::", stringify!($field))
    }};
}

/// Every `RunRequest`/`RunControls`/`RunOutcome`/`RunReport` field the inventory
/// names as the replacement for something the spawn provided.
///
/// The document argues from these by name; a field renamed or dropped upstream
/// turns an argument into a dangling reference. Each entry below is resolved by
/// the compiler against the real type, so this test's only remaining job is to
/// reject a name in the document that nobody put on the list.
#[test]
fn every_upstream_field_the_inventory_names_is_still_there() {
    let proven: BTreeSet<String> = [
        field!(RunRequest, config),
        field!(RunRequest, cwd),
        field!(RunRequest, events),
        field!(RunRequest, stream),
        field!(RunRequest, prompt),
        field!(RunRequest, control),
        field!(RunRequest, no_config),
        field!(RunRequest, bin),
        field!(RunControls<'_>, events),
        field!(RunControls<'_>, cancel),
        field!(RunControls<'_>, signal_cancel),
        field!(RunControls<'_>, version),
        field!(RunOutcome, exit_code),
        field!(RunOutcome, failure_summary),
        field!(RunReport, control),
        field!(RunReport, fallback),
        field!(RunReport, results),
        field!(FallbackReport, ran),
        field!(RunResult, usage),
        field!(RunResult, observed_model),
        field!(RunResult, harness_id),
        field!(RunResult, failure_kind),
        field!(RunResult, error),
    ]
    .into_iter()
    // `RunControls<'_>` stringifies with its lifetime; the document writes the
    // type by name.
    .map(|name| name.replace("<'_>", ""))
    .collect();

    let named = qualified_fields_named_in(INVENTORY);
    assert!(!named.is_empty(), "the inventory names upstream fields");
    let unproven: Vec<&String> = named.difference(&proven).collect();
    assert!(
        unproven.is_empty(),
        "the inventory names upstream fields nothing here holds against the real type: {unproven:?}"
    );

    // The argument table's own column, which is written as bare field names.
    assert_eq!(
        mapped_request_fields(),
        BTreeSet::from(["config", "cwd", "events", "prompt", "stream"]),
        "the argument table maps onto different `RunRequest` fields than it did"
    );
}

/// The status block credits a *version* of `oneharness-core` with the seam, and
/// the manifest is what decides which one is linked.
///
/// A bump that leaves this document naming the version it was written against
/// would read as a claim about the linked engine that nobody checked — which is
/// exactly how a fixed blocker went unnoticed for two dispatches.
#[test]
fn the_status_block_names_the_version_the_manifest_takes() {
    let linked = version_the_manifest_takes();

    let (_, rest) = INVENTORY
        .split_once("`oneharness-core` **")
        .expect("the status block still credits the dependency by version");
    let (named, _) = rest
        .split_once("**")
        .expect("the version the status block names is emphasized");
    assert_eq!(
        named, linked,
        "the inventory credits `oneharness-core` {named} with a seam the {linked} the manifest \
         takes is what provides"
    );
}

/// The `oneharness-core` release this crate's own manifest takes.
///
/// One reader for both tests below: the version the document is held against and
/// the version the graph is held against have to be the same one, or the two
/// could agree with each other while agreeing with nothing that runs.
fn version_the_manifest_takes() -> &'static str {
    let (_, rest) = MANIFEST
        .split_once("\noneharness-core = \"")
        .expect("the manifest still takes `oneharness-core` by version");
    let (linked, _) = rest.split_once('"').expect("the requirement is quoted");
    linked
}

/// The graph resolves **one** `oneharness-core`, and it is the one the manifest
/// takes.
///
/// This crate used to ask for 0.10 while `onejudge` asked for 0.8, so the
/// lockfile carried both and every oneharness fix released between them reached
/// only the half `onejudge` does not drive — a fix nobody had, on the side that
/// needed it. Nothing else in the repository notices that: `cargo build` is happy
/// with two copies, `deny.toml` sets `multiple-versions = "allow"` for the
/// ordinary duplicates a real graph has, and the manifest cannot see the second
/// one because it was never this crate's own requirement. Only the resolution can
/// answer it, so the resolution is what is asserted.
///
/// Equality with the manifest is exact rather than semver-compatible on purpose:
/// every sibling requirement here is a full `x.y.z` carrying a dated, measured
/// reason in `Cargo.toml`, and `just upgrade` moves the manifest with the lock.
/// A lock that drifted a patch ahead of the prose is the prose going stale, which
/// is the same failure this file's other tests exist to catch.
#[test]
fn the_graph_resolves_one_oneharness_core_and_it_is_the_one_the_manifest_takes() {
    let resolved: Vec<&str> = LOCKFILE
        .split("name = \"oneharness-core\"\nversion = \"")
        .skip(1)
        .map(|rest| {
            rest.split_once('"')
                .expect("a locked package's version is quoted")
                .0
        })
        .collect();

    assert_eq!(
        resolved,
        [version_the_manifest_takes()],
        "the graph should carry exactly one `oneharness-core`, the one the manifest takes; \
         a second entry means a dependency — `onejudge` is the one that drives it — asks for \
         an incompatible range again, and the two requirements have to move together"
    );
}

/// The graph resolves **one** `onejudge`, and it is the one the manifest takes.
///
/// The same assertion for the engine that drives every two-party member, and
/// for the same reason it is made against the lock rather than the manifest: a
/// second copy could only arrive through a dependency's own requirement, which
/// the manifest cannot see. Exact for the reason above — the requirement here is
/// a full `x.y.z` with a measured reason beside it, and a lock that drifted past
/// the prose is the prose going stale.
#[test]
fn the_graph_resolves_one_onejudge_and_it_is_the_one_the_manifest_takes() {
    let (_, rest) = MANIFEST
        .split_once("\nonejudge = { version = \"")
        .expect("the manifest still takes `onejudge` by version");
    let (required, _) = rest.split_once('"').expect("the requirement is quoted");

    let resolved: Vec<&str> = LOCKFILE
        .split("name = \"onejudge\"\nversion = \"")
        .skip(1)
        .map(|rest| {
            rest.split_once('"')
                .expect("a locked package's version is quoted")
                .0
        })
        .collect();

    assert_eq!(
        resolved,
        [required],
        "the graph should carry exactly one `onejudge`, the one the manifest takes"
    );
}

/// Every version of `package` the committed lockfile resolves.
fn locked(package: &str) -> Vec<&'static str> {
    LOCKFILE
        .split(&format!("name = \"{package}\"\nversion = \""))
        .skip(1)
        .map(|rest| {
            rest.split_once('"')
                .expect("a locked package's version is quoted")
                .0
        })
        .collect()
}

/// The graph resolves **one** `onemessagebus`, the one the manifest takes — the
/// same bus `onejudge` links — and **no** `onemessagebus-agent` at all.
///
/// A second copy is two sets of bus types that look alike and are not the same
/// type: a note this crate re-exports from `onejudge` and one it builds from the
/// bus would stop being interchangeable, and an envelope one side emits would be
/// a different `Envelope` from the one the other reads. The agent profile is the
/// same hazard by another route — its envelope and note were the ones this crate
/// used to re-export, and the vocabulary is this crate's own now — so a graph
/// that carried it again would carry two of each.
#[test]
fn the_graph_resolves_one_bus_and_it_is_the_one_the_manifest_takes() {
    let (_, rest) = MANIFEST
        .split_once("\nonemessagebus = \"")
        .expect("the manifest still takes `onemessagebus` by version");
    let (required, _) = rest.split_once('"').expect("the requirement is quoted");
    assert_eq!(
        locked("onemessagebus"),
        [required],
        "the graph should carry exactly one `onemessagebus`, the one the manifest takes"
    );
    assert!(
        !MANIFEST.contains("\nonemessagebus-agent = "),
        "the agent profile is retired; the vocabulary is this crate's own"
    );
    assert_eq!(
        locked("onemessagebus-agent"),
        Vec::<&str>::new(),
        "nothing in the graph may pull the retired agent profile back in"
    );
}

/// The note this crate re-exports *is* the note `onejudge` declares: a value
/// obtained through one path is accepted by a function typed by the other, in
/// both directions, and the in-process channel `onejudge` hands its engine is
/// the bus core's own sender and inbox over those types.
///
/// Compiled rather than asserted at run time, because a second copy of the bus
/// in the graph would make these mismatched types — this test would not build.
#[test]
fn a_note_this_crate_re_exports_is_the_note_onejudge_declares() {
    fn judge_note(note: onejudge::note::Note) -> onejudge::note::Note {
        note
    }
    fn crate_note(note: oneagentgraph::note::Note) -> oneagentgraph::note::Note {
        note
    }

    let through_onejudge =
        oneagentgraph::note::Note::new(oneagentgraph::note::Addressee::Worker, "one note type")
            .expect("a note");
    let through_the_library = judge_note(through_onejudge.clone());
    assert_eq!(crate_note(through_the_library), through_onejudge);

    let (notes, inbox): (
        onemessagebus::Sender<onejudge::note::Note, onejudge::note::Accepted>,
        onemessagebus::Inbox<onejudge::note::Note, onejudge::note::Accepted>,
    ) = onejudge::note::Notes::channel();
    let sending = std::thread::spawn(move || notes.send(through_onejudge));
    inbox
        .take_within(std::time::Duration::from_secs(10))
        .expect("the note arrives")
        .answer(oneagentgraph::note::Accepted::Queued);
    assert_eq!(
        sending.join().expect("the sender"),
        Ok(onejudge::note::Accepted::Queued)
    );
}

/// The wire names both documents share still name the types this crate
/// serializes.
///
/// A vocabulary check, not a boundary gate: `runner: library`'s
/// three fields are what a member of either kind now publishes, and the three
/// the contract scopes to a member that *was* a process stay declared so a
/// consumer reading an older stream still parses one.
#[test]
fn the_wire_names_the_inventory_restates_are_the_types_this_crate_serializes() {
    let library = Runner::Library {
        engine: "oneharness".to_owned(),
        config: "oneharness.toml".to_owned(),
        worktree: "/tmp/work".to_owned(),
    };
    let died = MemberDied {
        rule: "member-died".to_owned(),
        cause: Cause::Exited,
        detail: String::new(),
        truncated: false,
        exit_code: Some(1),
        disposition: Some(Disposition::Exited),
        stderr_tail: Some(String::new()),
        candidates: Vec::new(),
    };
    assert!(matches!(library, Runner::Library { .. }));
    assert_eq!(died.exit_code, Some(1));

    for name in [
        "runner",
        "library",
        "process",
        "engine",
        "worktree",
        "exit_code",
        "disposition",
        "stderr_tail",
    ] {
        assert!(
            INVENTORY.contains(name),
            "the inventory stopped naming the contract's `{name}`"
        );
        assert!(
            CONTRACT.contains(name),
            "the inventory attributes `{name}` to a contract that has no such name"
        );
    }
}

/// The `field` column of the argument table, as bare `RunRequest` field names.
///
/// The `--compact` row maps onto nothing by design and carries no backticked
/// field, so it drops out here rather than needing to be named.
fn mapped_request_fields() -> BTreeSet<&'static str> {
    let (_, rest) = INVENTORY
        .split_once("| argument | field |")
        .expect("the argument table is still in the inventory");
    let (table, _) = rest.split_once("\n\n").expect("the table ends");
    table
        .lines()
        .filter_map(|line| line.split('|').nth(2))
        .flat_map(backticked)
        // `stream: Some(true)` names the field and the value it takes.
        .map(|cell| cell.split(':').next().unwrap_or(cell).trim())
        .filter(|cell| !cell.is_empty())
        .collect()
}

/// Every `Type::field` the document names, for the five upstream types it argues
/// from.
///
/// A span may go on past the field — `RunRequest::stream: Some(true)` names the
/// value it takes as well — so each is cut back to the path itself.
fn qualified_fields_named_in(text: &'static str) -> BTreeSet<String> {
    backticked(text)
        .filter(|span| {
            [
                "RunRequest::",
                "RunControls::",
                "RunOutcome::",
                "RunReport::",
                "RunResult::",
            ]
            .iter()
            .any(|prefix| span.starts_with(prefix))
        })
        .map(|span| {
            span.trim_end_matches(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
                .split(": ")
                .next()
                .unwrap_or(span)
                .to_owned()
        })
        .collect()
}

/// The contents of each non-empty backtick-delimited span in `text`.
///
/// A slice cut out of the document can end inside a span, so the unterminated
/// tail is dropped rather than counted as a name.
fn backticked(text: &'static str) -> impl Iterator<Item = &'static str> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .filter(|span| !span.is_empty())
}
