//! Every bar this crate ships demands only what a member can settle inside its
//! own run — "What a bar may demand" in `docs/persona-format.md` gives the two
//! shapes that cannot be and why. This file holds the shipped roles there, by
//! the vocabulary of that class rather than one wording of it.
//!
//! ## The audit the list was written from
//!
//! Every shipped role was read against the class, and only `engineer` carried
//! it. The rest are recorded here because "already sound" is a finding a later
//! reader needs the reason for:
//!
//! * `docs-writer` — brings no completion criterion of its own, and its review
//!   contract asks only for accuracy against the code and prose a maintainer can
//!   use. Both are readable in the run that produced them.
//! * `planner` — its bar is a property of the plan it is handed: actionable
//!   subtasks, explicit dependencies, named interfaces.
//! * `researcher` — cited sources, facts held apart from inferences, and no
//!   project file modified. The last is about what the worker itself did.
//! * `reviewer` — findings tied to specific code and ranked, each verified
//!   against that code. Verifying a claim against the source is the work, not a
//!   tier someone else runs over it afterwards.

use oneagentgraph::persona::{merge, Persona, SHIPPED_PERSONAS};

/// A base bringing no supervisor and no bar of its own, so what the merge leaves
/// under `user:` is the shipped role's own bar rather than an operator's layered
/// under it.
///
/// The real [`merge`] rather than a hand-read of the YAML: what a judge is handed
/// is the merged config, and a bar that arrived some other way is not the one
/// under test.
const BARE_BASE: &str = "provider:\n  kind: oneharness\n";

struct Bar {
    persona: Option<String>,
    done_when: Option<String>,
}

impl Bar {
    fn clauses(&self) -> Vec<(&'static str, &str)> {
        [
            ("user.persona", self.persona.as_deref()),
            ("user.done_when", self.done_when.as_deref()),
        ]
        .into_iter()
        .filter_map(|(field, clause)| clause.map(|clause| (field, clause)))
        .collect()
    }
}

fn bar(name: &str, document: &str) -> Bar {
    let persona =
        Persona::parse(document, name).unwrap_or_else(|err| panic!("{name} does not load: {err}"));
    let merged = merge(BARE_BASE, "base.yaml", &persona)
        .unwrap_or_else(|err| panic!("{name} does not merge: {err}"));
    let user = merged.get("user").cloned().unwrap_or_default();
    let field = |key: &str| {
        user.get(key)
            .map(|value| {
                value
                    .as_str()
                    .unwrap_or_else(|| panic!("{name}'s `user.{key}` is not a string"))
                    .to_string()
            })
            .filter(|clause| !clause.trim().is_empty())
    };
    Bar {
        persona: field("persona"),
        done_when: field("done_when"),
    }
}

fn shipped_bars() -> Vec<(&'static str, Bar)> {
    SHIPPED_PERSONAS
        .iter()
        .map(|(name, document)| (*name, bar(name, document)))
        .collect()
}

/// Word prefixes no shipped bar may use, one group per shape.
///
/// A word naming a proof method or a check tier makes the bar decide how a
/// criterion is met, which only that criterion knows and which a role shipped to
/// an unseen repository cannot know exists. A word naming work that lands after
/// the run points at state no member can observe while it is still the one that
/// would have to satisfy it.
///
/// Matched as prefixes so every inflection is caught — `gate` has to catch
/// `gates` and `gated`. A prefix written in capitals is matched case-sensitively:
/// `CI` must find `CI` without finding `cite`, which is an ordinary word in a
/// research bar.
///
/// Three words the operator repository's own version of this list carries are
/// deliberately absent, because a *shipped* bar is not one clause shared by a
/// plan, a report, and a diff alike — it is a role, and a role's subject matter
/// is its own:
///
/// * `test` and `coverage` — an implementation role must keep demanding tests
///   that drive the real interface and coverage that does not regress. What it
///   may not do is name the tier that proves them, which is what the entries
///   below catch.
/// * `push` — every one of these bars is written in the stance of pushing the
///   worker, and the prefix cannot tell that from pushing a branch. The class it
///   would catch is already covered by `remote`, `merge`, `change request`,
///   `publish`, and `CI`.
const OUT_OF_REACH: &[&str] = &[
    // Decides how a criterion is proven.
    "end to end",
    "end-to-end",
    "gate",
    "pipeline",
    "suite",
    "green",
    "check",
    "lint",
    "inspection",
    "verification",
    "enforce",
    // Points at state that arrives after the run has ended.
    "remote",
    "merge",
    "pull request",
    "change request",
    "deploy",
    "publish",
    "approve",
    "approval",
    // Both at once.
    "CI",
];

/// Whether `clause` uses `stem` at the start of a word, in any inflection.
///
/// A word boundary and an open suffix rather than a bare substring search: a
/// search that got either wrong would fail a bar that is fine or pass one that is
/// not.
fn uses(clause: &str, stem: &str) -> bool {
    let cased = stem.chars().any(|letter| letter.is_ascii_uppercase());
    let haystack = if cased {
        clause.to_string()
    } else {
        clause.to_lowercase()
    };
    let needle = if cased {
        stem.to_string()
    } else {
        stem.to_lowercase()
    };
    haystack.match_indices(&needle).any(|(at, _)| {
        haystack[..at]
            .chars()
            .next_back()
            .is_none_or(|before| !before.is_alphanumeric())
    })
}

/// `clause` with every run of whitespace collapsed to one space.
///
/// A bar is a wrapped block scalar, so `pull request` and `end to end` are as
/// likely to arrive with a newline through the middle as with a space; a search
/// over the raw text would miss exactly the multi-word demands it exists to
/// catch.
fn flowing(clause: &str) -> String {
    clause.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn out_of_reach(clause: &str) -> Vec<&'static str> {
    let flowing = flowing(clause);
    OUT_OF_REACH
        .iter()
        .copied()
        .filter(|stem| uses(&flowing, stem))
        .collect()
}

#[test]
fn no_shipped_bar_uses_the_vocabulary_of_a_demand_beyond_a_members_reach() {
    for (name, bar) in shipped_bars() {
        for (field, clause) in bar.clauses() {
            let found = out_of_reach(clause);
            assert!(
                found.is_empty(),
                "personas/{name}.yaml's `{field}` uses {found:?}, so a role shipped to a \
                 repository this crate has never seen either decides how a criterion is proven or \
                 points at state that arrives after the run ends. State it in the task's own \
                 acceptance criteria instead:\n{clause}"
            );
        }
    }
}

/// The guard reads the class, not the sentence it was written from.
///
/// A bar carrying the demand in wording that shares nothing with the one removed
/// from `engineer` is still caught, and caught for the reason the class names —
/// asserted as the exact set of prefixes, so a matcher that stopped finding one
/// of them fails here rather than quietly narrowing the guard.
#[test]
fn the_guard_reads_the_class_rather_than_the_wording_it_was_written_from() {
    let reworded = concat!(
        "name: lead\n",
        "user:\n",
        "  persona: |\n",
        "    Hold the work open until the repository's whole verification pipeline has run over\n",
        "    the finished tree, and until the branch is on the remote with its change request\n",
        "    approved.\n",
        "  done_when: \"the suite is green and every gate the project enforces has passed\"\n",
    );
    let bar = bar("reworded", reworded);
    assert_eq!(
        out_of_reach(bar.persona.as_deref().expect("a stance")),
        [
            "pipeline",
            "verification",
            "remote",
            "change request",
            "approve"
        ],
        "the guard stopped recognising a reworded demand about state after the run"
    );
    assert_eq!(
        out_of_reach(bar.done_when.as_deref().expect("a criterion")),
        ["gate", "suite", "green", "enforce"],
        "the guard stopped recognising a reworded demand about how a criterion is proven"
    );

    // The two edges the prefixes rest on, stated rather than left to the shipped
    // bars that happen to exercise them: an inflection is caught, and a word that
    // merely starts with the same letters is not.
    assert!(uses("every gated tier", "gate") && uses("the CI run", "CI"));
    assert!(!uses("cite a specific source", "CI") && !uses("a delegated tier", "gate"));
}

/// Narrowed, not emptied: the corrected `engineer` bar still reviews for the
/// things it exists to review for.
///
/// Every stem test above is satisfied by a bar that says nothing, so this is the
/// half that keeps the correction from being a softening. Each phrase here is one
/// the class does not object to: it names a property of the change the member
/// itself produced, not a tier that has to run over it or a place it has to reach.
#[test]
fn the_engineer_bar_still_demands_what_it_is_for() {
    let stance = flowing(
        &bar(
            "engineer",
            oneagentgraph::persona::shipped("engineer").expect("shipped"),
        )
        .persona
        .expect("engineer reviews from a stance"),
    );
    for demand in [
        "correctness",
        "validated trust boundaries",
        "backward compatibility",
        "accessibility",
        "tests that drive the real interface rather than mocking the layer",
        "a failure or recovery path",
        "coverage gaps it opens",
        "schema-version bump",
        "acceptance criteria",
        "no regression is introduced in what it touched",
    ] {
        assert!(
            stance.contains(demand),
            "personas/engineer.yaml's `user.persona` no longer demands {demand:?}, so correcting \
             what it may not demand has softened what it must:\n{stance}"
        );
    }
}

/// Which roles bring no completion criterion of their own, recorded as a
/// decision rather than left to whatever the files happen to say.
///
/// A role with no `user.done_when` is judged against whatever bar the base config
/// a graph names carries, which is the operator's to write and not this crate's
/// to over-reach into — so there is nothing here for the guard to read, and that
/// is the correct shape for a role whose completion is the operator's call.
/// `engineer` is deliberately one of them: its correction narrowed the stance it
/// reviews from and did not add a second bar beside the operator's.
#[test]
fn the_roles_that_bring_no_completion_criterion_of_their_own_are_recorded_here() {
    let without: Vec<&str> = shipped_bars()
        .iter()
        .filter(|(_, bar)| bar.done_when.is_none())
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(
        without,
        ["docs-writer", "engineer"],
        "a shipped role gained or lost a completion criterion of its own; that decides whether \
         the operator's base bar stands alone for it, so say so here"
    );
    for (name, bar) in shipped_bars() {
        assert!(
            bar.persona.is_some(),
            "personas/{name}.yaml no longer hands its judge a stance to review from"
        );
    }
}
