//! The blocked-conversion inventory is driven by the surfaces it quotes.
//!
//! `docs/oneharness-library.md` says the `oneharness run` hop is blocked, and
//! rests that claim on names it does not own: `oneharness-core`'s public API on
//! one side, `docs/contract.md`'s wire schema on the other. Neither copy can be
//! generated — the document is prose written for a reader — so this suite is the
//! drift gate instead. Every upstream name in the document is held against the
//! real type, and every contract name against the real contract, so a rename or
//! an addition on either side fails here rather than leaving the document quietly
//! wrong about why the conversion cannot run.
//!
//! The load-bearing one is
//! [`the_inventorys_blocker_is_still_the_shape_of_run_controls`]: it destructures
//! [`RunControls`] exhaustively, so the day upstream adds the spawn seam the
//! document asks for, this file stops compiling. That failure *is* the signal —
//! the blocker is lifted and the status block at the top of the document is stale.

use std::collections::BTreeSet;

use oneagentgraph::event::{Cause, Disposition, MemberDied, Runner};
use oneharness_core::io::cancel::CancelToken;
use oneharness_core::io::run::{RunControls, RunRequest};

/// The inventory itself, read at compile time rather than copied beside this.
const INVENTORY: &str = include_str!("../docs/oneharness-library.md");
/// The approved contract, which owns every wire name the inventory restates.
const CONTRACT: &str = include_str!("../docs/contract.md");

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

/// Every `RunRequest`/`RunControls`/`RunOutcome`/`RunReport` field the inventory
/// names as the replacement for something the spawn provides today.
///
/// The document argues from these by name; a field renamed or dropped upstream
/// turns an argument into a dangling reference. The literals below are what makes
/// the check real — the compiler rejects a name that no longer exists, and this
/// test rejects a name in the document that nobody wrote down here.
#[test]
fn every_upstream_field_the_inventory_names_is_still_there() {
    // Naming them in a literal is the proof they exist; `..default()` keeps an
    // unrelated upstream addition from failing this crate's gate.
    let request = RunRequest {
        config: None,
        cwd: None,
        events: false,
        stream: None,
        prompt: Vec::new(),
        env: Vec::new(),
        control: false,
        no_config: false,
        bin: Vec::new(),
        ..RunRequest::default()
    };
    assert!(
        !request.no_config,
        "the inventory reasons about the default"
    );

    let proven: BTreeSet<String> = [
        "RunRequest::config",
        "RunRequest::cwd",
        "RunRequest::events",
        "RunRequest::stream",
        "RunRequest::prompt",
        "RunRequest::env",
        "RunRequest::control",
        "RunRequest::no_config",
        "RunRequest::bin",
        // `RunControls`'s own fields are destructured exhaustively above.
        "RunControls::events",
        "RunControls::cancel",
        "RunControls::signal_cancel",
        "RunControls::version",
        // Read off the returned values in `src/member.rs`'s place.
        "RunOutcome::exit_code",
        "RunOutcome::failure_summary",
        "RunReport::control",
        "RunReport::fallback",
    ]
    .into_iter()
    .map(str::to_owned)
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
