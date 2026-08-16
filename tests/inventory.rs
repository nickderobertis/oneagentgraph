//! The conversion inventory is driven by the surfaces it describes.
//!
//! `docs/oneharness-library.md` says the `oneharness run` hop is converted, and
//! rests that claim on names it does not own: `oneharness-core`'s public API on
//! one side, `docs/contract.md`'s wire schema and this crate's manifest on the
//! other. Neither copy can be generated — the document is prose written for a
//! reader — so this suite is the drift gate instead. Every upstream field the
//! document names is resolved by the compiler against the real type, every
//! sentence it quotes is matched against the document it quotes, and the version
//! it credits is read out of `Cargo.toml`.
//!
//! The load-bearing one is [`the_seam_the_conversion_rests_on_is_still_there`]:
//! the grouping hooks are the entire reason this conversion was safe to make, and
//! a build that quietly lost them would leave the activity watchdog looking at an
//! empty tree and a killed run's paid harnesses billing. It is written so that
//! disappearing upstream is a failure here rather than a silent regression there.

use std::collections::BTreeSet;
use std::process::{Child, Command};

use oneagentgraph::event::{Cause, Disposition, MemberDied, Runner};
use oneharness_core::domain::report::RunReport;
use oneharness_core::io::cancel::CancelToken;
use oneharness_core::io::run::{RunControls, RunOutcome, RunRequest};
use oneharness_core::io::runner::ProcessSupervisor;

/// The inventory itself, read at compile time rather than copied beside this.
const INVENTORY: &str = include_str!("../docs/oneharness-library.md");
/// The approved contract, which owns every wire name the inventory restates.
const CONTRACT: &str = include_str!("../docs/contract.md");
/// The manifest, which owns the `oneharness-core` version the inventory names.
const MANIFEST: &str = include_str!("../Cargo.toml");

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

    // The entry point, by signature: request, controls, and the caller's claim.
    let entry: Supervised = oneharness_core::io::run::run_supervised;
    let supervised: Option<&dyn ProcessSupervisor> = Some(&hooks);
    assert!(
        supervised.is_some(),
        "the supervisor is the caller's to pass"
    );
    // Named so an unused binding is not what keeps the signature honest.
    let _ = entry;

    assert!(
        INVENTORY.contains("run_supervised") && INVENTORY.contains("ProcessSupervisor"),
        "the inventory stopped naming the seam it rests on"
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
    let (_, rest) = MANIFEST
        .split_once("\noneharness-core = \"")
        .expect("the manifest still takes `oneharness-core` by version");
    let (linked, _) = rest.split_once('"').expect("the requirement is quoted");

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

/// The wire names the inventory restates belong to `docs/contract.md`, and its
/// quotations of that document have to still be quotations.
///
/// Two of the three quoted sentences are the *stale* ones the inventory reports
/// as a correction the contract owner still owes. That is the direction this
/// gate is meant to run in: when the owner makes that correction, this fails, and
/// the inventory's follow-up list has to be updated in the same change rather
/// than left claiming a debt that has been paid.
#[test]
fn the_contract_still_says_what_the_inventory_quotes_it_saying() {
    // Both documents are hard-wrapped prose, so a quotation is matched against
    // the reflowed text rather than against whichever line it happened to break
    // on. A rewrap on either side is not drift.
    let inventory = reflowed(INVENTORY);
    let contract = reflowed(CONTRACT);
    for quotation in [
        "is still `oneharness run`, a child process",
        "neither returns the report nor accepts an event sink",
        "when oneharness grows a non-printing run entrypoint or an event-sink parameter",
        "onejudge's `ProviderErrorKind`, which is oneharness's own normalized `failure_kind`, \
         mapped totally",
    ] {
        assert!(
            inventory.contains(quotation),
            "the inventory no longer quotes: {quotation}"
        );
        assert!(
            contract.contains(quotation),
            "the inventory quotes the contract on something it no longer says: {quotation}"
        );
    }

    // `runner: library`'s three fields — what a single-sided member now
    // publishes — and the three the contract scopes to a member that *was* a
    // process, which no member is. Both are still declared types: a consumer
    // reading an older stream still parses one.
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

/// Every `Type::field` the document names, for the four upstream types it argues
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

/// `text` with every run of whitespace collapsed to one space, so a hard-wrapped
/// sentence is one string again.
fn reflowed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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
