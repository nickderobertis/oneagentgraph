//! What this repository publishes, held to the schema that defines it.
//!
//! `release-targets.toml` at this root is written against the canonical
//! release-target schema — the one `onevcs`'s `docs/contract.md` defines and every
//! repository in this stack writes against. That schema has exactly one
//! implementation, `onevcs`'s own reader, and this suite *is* that reader:
//! [`onevcs::validate_release_declaration`] is what a consumer reading this
//! repository's declaration runs, so what passes here is what passes there.
//!
//! Deliberately not a copy of those rules. A schema restated in each repository
//! that writes against it is the duplication the shared format exists to remove:
//! the copies drift, and a document that passes the copy is still refused by the
//! tool that actually reads it. Nothing below decides what the schema says — every
//! refusal here is the reader's, quoted back.
//!
//! And the refusals are most of the value. A declaration is written once and then
//! read by machinery with no way to fix it, so what this gate is worth is what it
//! REFUSES. Each case below refuses a document built by taking this repository's
//! real one and changing exactly one thing about it, so every refusal is of a
//! document somebody could plausibly have written.
//!
//! Whether what the document says is *true* — whether a release really publishes
//! what it declares — is a different question with a different gate:
//! `npm/test/release-targets.test.mjs` holds it against the release configuration
//! itself, in both directions.

use onevcs::Declaration;
use toml_edit::DocumentMut;

/// The declaration itself, read at compile time rather than copied beside this —
/// the way `tests/contract.rs` reads the contract it drives, and why
/// `Cargo.toml`'s `include` list carries it.
const DECLARATION: &str = include_str!("../release-targets.toml");

/// What a refusal names the document by. A path rather than a URL, because that is
/// what somebody reading the failure has open.
const ORIGIN: &str = "release-targets.toml";

/// This repository's document, as the reader that defines the schema reads it.
fn declared() -> Declaration {
    onevcs::validate_release_declaration(DECLARATION, ORIGIN).unwrap_or_else(|failure| {
        panic!(
            "the canonical release-target reader refused this repository's declaration: {failure}"
        )
    })
}

/// The identifiers a plain TOML parse finds, in document order: each `[[target]]`'s
/// own `id`, then every identifier that target `covers`.
///
/// Read with an ordinary parser and no knowledge of this repository, which is how a
/// consumer that has never seen this file reads it. Comparing the canonical reader's
/// answer against this is what catches a document that was *accepted* with something
/// dropped on the way through — which an exit-code assertion cannot see.
fn identifiers_a_plain_parse_finds() -> Vec<String> {
    let document: DocumentMut = DECLARATION.parse().expect("the declaration is TOML");
    let targets = document["target"]
        .as_array_of_tables()
        .expect("the declaration's targets are [[target]] tables");
    let mut found = Vec::new();
    for target in targets {
        found.push(
            target["id"]
                .as_str()
                .expect("a target's id is a string")
                .to_owned(),
        );
        let Some(covers) = target.get("covers") else {
            continue;
        };
        for covered in covers.as_array().expect("`covers` is an array") {
            found.push(
                covered
                    .as_str()
                    .expect("a covered id is a string")
                    .to_owned(),
            );
        }
    }
    found
}

/// The declaration this repository ships is one the canonical reader accepts, whole.
#[test]
fn the_canonical_reader_accepts_this_repositorys_declaration() {
    let declaration = declared();

    let read: Vec<String> = declaration
        .targets
        .iter()
        .flat_map(|target| {
            std::iter::once(target.id.to_string())
                .chain(target.covers.iter().map(ToString::to_string))
        })
        .collect();
    assert_eq!(
        read,
        identifiers_a_plain_parse_finds(),
        "the reader's answer and the document disagree about what this repository ships"
    );
}

/// Every target is waited on by a short name, and a plain parse finds the same one.
///
/// The short name is what a host document and a consumer's plan name a target by, so
/// a reader that has never seen this repository has to be able to pair each name with
/// the artifact it stands for from the document alone.
#[test]
fn every_declared_target_pairs_a_short_name_with_the_artifact_it_names() {
    let declaration = declared();
    let document: DocumentMut = DECLARATION.parse().expect("the declaration is TOML");
    let targets = document["target"]
        .as_array_of_tables()
        .expect("the declaration's targets are [[target]] tables");

    let paired: Vec<(String, String)> = targets
        .iter()
        .map(|target| {
            (
                target["name"].as_str().expect("a short name").to_owned(),
                target["id"].as_str().expect("an identifier").to_owned(),
            )
        })
        .collect();
    assert_eq!(
        declaration
            .targets
            .iter()
            .map(|target| (target.name.to_string(), target.id.to_string()))
            .collect::<Vec<_>>(),
        paired,
    );
    // The probe a consumer runs to ask what a registry serves for one of those
    // identifiers, which the document names and the reader hands back as a path
    // into this checkout.
    assert_eq!(
        declaration
            .probe
            .as_ref()
            .map(|path| path.as_path().to_string_lossy().into_owned()),
        Some("scripts/release-probe.sh".to_owned()),
        "the declaration no longer names the probe that answers for its identifiers"
    );
}

/// One way a declaration can go wrong, and what the reader must say about it.
struct Refusal {
    /// What is wrong with the document this produces.
    because: &'static str,
    /// The single change that makes it wrong, applied to this repository's own file.
    mutate: fn(&mut DocumentMut),
    /// What the refusal has to name, so a failing gate points at the field rather
    /// than at the document.
    names: &'static str,
}

/// Each of the three ways a hand-written declaration goes wrong, refused by the
/// canonical reader rather than by anything in this repository.
#[test]
fn the_canonical_reader_refuses_a_declaration_that_has_gone_out_of_shape() {
    let refusals = [
        Refusal {
            because: "a target with no `what`, so a reader learns nothing from the entry",
            mutate: |document| {
                targets(document)
                    .get_mut(0)
                    .expect("a first target")
                    .remove("what");
            },
            names: "what",
        },
        Refusal {
            because: "an identifier with no registry, which names two artifacts at once",
            mutate: |document| {
                targets(document).get_mut(0).expect("a first target")["id"] =
                    toml_edit::value("oneagentgraph");
            },
            names: "names no registry",
        },
        Refusal {
            because: "two targets taking one short name, so a consumer's plan has two answers",
            mutate: |document| {
                let first = targets(document)
                    .get(0)
                    .expect("a first target")
                    .get("name")
                    .expect("a short name")
                    .clone();
                targets(document).get_mut(1).expect("a second target")["name"] = first;
            },
            names: "short name",
        },
    ];

    for refusal in refusals {
        let mut document: DocumentMut = DECLARATION.parse().expect("the declaration is TOML");
        (refusal.mutate)(&mut document);
        let failure = onevcs::validate_release_declaration(&document.to_string(), ORIGIN)
            .expect_err(&format!(
                "the canonical reader accepted a declaration with {}",
                refusal.because
            ));
        let said = failure.to_string();
        assert!(
            said.contains(refusal.names),
            "the refusal of {} does not name {:?}: {said}",
            refusal.because,
            refusal.names
        );
    }
}

/// The `[[target]]` tables of a document being mutated.
fn targets(document: &mut DocumentMut) -> &mut toml_edit::ArrayOfTables {
    document["target"]
        .as_array_of_tables_mut()
        .expect("the declaration's targets are [[target]] tables")
}
