//! The blocked-conversion inventory is driven by the surfaces it quotes.
//!
//! `docs/oneharness-library.md` says the `oneharness run` hop is blocked, and
//! rests that claim on names it does not own: `oneharness-core`'s public API on
//! one side, `docs/contract.md`'s wire schema and this crate's manifest on the
//! other. Neither copy can be generated — the document is prose written for a
//! reader — so this suite is the drift gate instead. Every upstream field the
//! document names is resolved by the compiler against the real type, every
//! sentence it quotes is matched against the document it quotes, and the version
//! it blames is read out of `Cargo.toml`.
//!
//! The load-bearing one is
//! [`the_inventorys_blocker_is_still_the_shape_of_run_controls`]: it destructures
//! [`RunControls`] exhaustively, so the day upstream adds the spawn seam the
//! document asks for, this file stops compiling. That failure *is* the signal —
//! the blocker is lifted and the status block at the top of the document is stale.

use std::collections::BTreeSet;

use oneagentgraph::event::{Cause, Disposition, MemberDied, Runner};
use oneharness_core::domain::report::RunReport;
use oneharness_core::io::cancel::CancelToken;
use oneharness_core::io::run::{RunControls, RunOutcome, RunRequest};

/// The inventory itself, read at compile time rather than copied beside this.
const INVENTORY: &str = include_str!("../docs/oneharness-library.md");
/// The approved contract, which owns every wire name the inventory restates.
const CONTRACT: &str = include_str!("../docs/contract.md");
/// The manifest, which owns the `oneharness-core` version the inventory names.
const MANIFEST: &str = include_str!("../Cargo.toml");

/// The blocker is that `RunControls` has no seam between building a harness
/// process and having one, so this crate cannot join each one to the member's
/// named job object. The exhaustive destructure below is what proves it: a field
/// added upstream — a `spawning`/`spawned` pair, or anything else — breaks this
/// compile, and the document's status block has to be rewritten before it builds
/// again.
#[test]
fn the_inventorys_blocker_is_still_the_shape_of_run_controls() {
    let controls = RunControls {
        events: None,
        cancel: CancelToken::new(),
        signal_cancel: false,
        version: None,
    };
    // Exhaustive on purpose — no `..` — so this is the drift gate and not a
    // sample of the fields that happened to be interesting.
    let RunControls {
        events,
        cancel,
        signal_cancel,
        version,
    } = controls;
    assert!(events.is_none(), "the sink is the caller's to supply");
    assert!(!cancel.is_cancelled(), "a fresh token is uncancelled");
    assert!(!signal_cancel, "an embedder handles its own signals");
    assert!(version.is_none(), "the engine names itself by default");

    let real: BTreeSet<&str> = ["events", "cancel", "signal_cancel", "version"]
        .into_iter()
        .collect();
    assert_eq!(
        listed_in_the_status_block(),
        real,
        "the status block names a different `RunControls` than the linked one"
    );
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
/// names as the replacement for something the spawn provides today.
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
        field!(RunRequest, env),
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

/// The status block attributes the blocker to a *version* of `oneharness-core`,
/// and the manifest is what decides which one that is.
///
/// A bump that leaves this document naming the version it was written against
/// would read as a claim about the linked engine that nobody checked — which is
/// exactly how a fixed blocker goes unnoticed.
#[test]
fn the_status_block_names_the_version_the_manifest_takes() {
    let (_, rest) = MANIFEST
        .split_once("\noneharness-core = \"")
        .expect("the manifest still takes `oneharness-core` by version");
    let (linked, _) = rest.split_once('"').expect("the requirement is quoted");

    let (_, rest) = INVENTORY
        .split_once("`oneharness-core` ")
        .expect("the status block still names the dependency");
    let (named, _) = rest
        .split_once("'s public API")
        .expect("the status block still attributes the blocker to its public API");
    assert_eq!(
        named, linked,
        "the inventory blames `oneharness-core` {named} for a blocker in the {linked} the manifest takes"
    );
}

/// The wire names the inventory restates belong to `docs/contract.md`, and its
/// quotations of that document have to still be quotations.
///
/// The inventory's last two sections argue from the contract — that a
/// platform-conditional conversion would make `runner` and the process facts
/// depend on the host, and that the contract's own sentence about this hop is
/// stale. Both arguments dissolve if the contract stops saying what is quoted
/// here, so the copies are checked against it rather than trusted.
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

    // `runner: library`'s three fields, and the three the contract scopes to a
    // member that was a process. Building each is what ties the name in the
    // document to the type this crate serializes.
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

/// The `RunControls` field names the status block says are all there is.
///
/// Read out of the sentence itself — from `exposes only` up to the next thing it
/// names — so rewording the claim without rewording the list fails above.
fn listed_in_the_status_block() -> BTreeSet<&'static str> {
    let (_, rest) = INVENTORY
        .split_once("exposes only")
        .expect("the status block still says what `RunControls` exposes");
    let (sentence, _) = rest
        .split_once("io::process::Process")
        .expect("the status block still goes on to the private spawn type");
    backticked(sentence).collect()
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
