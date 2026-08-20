//! The published persona format drives the real types, and the roles this crate
//! ships still merge to what they always did.
//!
//! Two things are held here that nothing else can hold. `docs/persona-format.md`
//! is what an author writes a persona from and what a consumer migrates by, so
//! both of its examples are read out of the document at compile time and driven
//! through [`Persona::parse`] — the accepted one accepted, the refused one
//! refused, naming the fields the document says to write instead.
//!
//! And `tests/golden/persona-merge.v1.json` is the effective config each shipped
//! role merged to **before** the format changed, captured from the previous
//! spelling and compared against what the current one produces. The whole point
//! of the change is that only the spelling moved; this is the proof, and a role
//! that quietly says something different to its agent fails here.

use std::collections::BTreeMap;

use oneagentgraph::persona::{merge, Persona, MEMBER_OWNED, SHIPPED_PERSONAS};
use serde_json::Value;

/// The published format, read rather than restated.
const FORMAT: &str = include_str!("../docs/persona-format.md");

/// The effective configs the shipped roles merged to before the format changed.
const GOLDEN: &str = include_str!("golden/persona-merge.v1.json");

/// The base the golden was captured against, in the spelling that ships now.
///
/// Every rule the merge has is exercised by it: a preamble to append a role
/// after, a shared review bar to compose with, a turn cap for a role to
/// override, a `task` that must not leak, and two fields (`skill`, `assessment`)
/// that pass through untouched. Its previous spelling differed in exactly one
/// place — this `system_prompt` was an `agent: {instructions: …}` block — which
/// is what makes the comparison below mean anything.
const BASE: &str = concat!(
    "provider:\n  kind: oneharness\n",
    "system_prompt: |\n  Standing bar: verify before you claim done.\n",
    "user:\n  done_when: \"the task is complete\"\n  max_turns: 4\n",
    "skill: ./skills/shared\n",
    "assessment: \"Name the follow-up work this run left out of scope.\"\n",
    "task: leaked\n",
);

/// Every fenced ```yaml block in the format document, in the order it appears.
fn yaml_blocks() -> Vec<String> {
    let mut blocks = Vec::new();
    let mut body: Option<String> = None;
    for line in FORMAT.lines() {
        match &mut body {
            Some(open) => {
                if line.trim_end() == "```" {
                    blocks.push(std::mem::take(open));
                    body = None;
                } else {
                    open.push_str(line);
                    open.push('\n');
                }
            }
            None => {
                if line.trim_end() == "```yaml" {
                    body = Some(String::new());
                }
            }
        }
    }
    assert!(
        body.is_none(),
        "docs/persona-format.md has an unclosed fenced block"
    );
    blocks
}

/// Every row of the document's table whose header row starts with `header`, as
/// written — the header and its `| --- |` separator dropped.
fn table_rows(header: &str) -> Vec<String> {
    let mut lines = FORMAT.lines().skip_while(|line| !line.starts_with(header));
    assert!(
        lines.next().is_some(),
        "docs/persona-format.md has no table headed {header:?}"
    );
    assert!(lines.next().is_some(), "{header:?} has no separator row");
    lines
        .take_while(|line| line.starts_with('|'))
        .map(str::to_string)
        .collect()
}

/// A value with every mapping's keys sorted, so a comparison is about what the
/// merge produced rather than the order it happened to insert it in.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, Value> = map
                .iter()
                .map(|(key, held)| (key, canonical(held)))
                .collect();
            Value::Object(
                sorted
                    .into_iter()
                    .map(|(key, held)| (key.clone(), held))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

/// The document's example persona is a persona: it parses, and every key it
/// shows off survives into the effective config a member would be handed.
#[test]
fn the_documented_example_is_a_persona_the_crate_accepts() {
    let example = yaml_blocks()
        .first()
        .cloned()
        .expect("docs/persona-format.md shows an example persona");
    let persona = Persona::parse(&example, "docs/persona-format.md")
        .expect("the documented example is refused by the crate that documents it");
    assert_eq!(persona.label(), Some("lead"));

    let merged = merge(BASE, "base.yaml", &persona).expect("the example merges");
    // onejudge's own schema is the arbiter of the result, exactly as it is of
    // the fragment: what this crate writes, onejudge reads.
    serde_json::from_value::<onejudge::cli::Config>(merged.clone())
        .expect("the merged config is not a onejudge config");
    let prompt = merged["system_prompt"].as_str().expect("a prompt");
    assert!(prompt.starts_with("Standing bar:"), "{prompt}");
    assert!(prompt.contains("You lead the implementation."), "{prompt}");
    let bar = merged["user"]["done_when"].as_str().expect("a bar");
    assert!(bar.contains("the task is complete"), "{bar}");
    assert!(bar.contains("the task's acceptance criteria"), "{bar}");
    assert_eq!(merged["user"]["max_turns"], Value::from(8));
    assert_eq!(merged["evals"].as_array().expect("evals").len(), 1);
    // The two keys the document calls this crate's own are consumed by the
    // merge, and neither is anywhere in what onejudge is handed.
    assert!(merged.get("name").is_none(), "{merged}");
    assert!(
        merged["user"].get("done_when_replaces_base").is_none(),
        "{merged}"
    );
}

/// The document's refused example is refused, and the refusal says what the
/// document's rewrite table says.
#[test]
fn the_documented_refusal_is_what_the_crate_does() {
    let refused = yaml_blocks()
        .get(1)
        .cloned()
        .expect("docs/persona-format.md shows the refused spelling");
    let err = Persona::parse(&refused, "roles/lead.yaml")
        .expect_err("the previous spelling still loads")
        .to_string();
    assert!(err.contains("roles/lead.yaml"), "{err}");
    assert!(err.contains("`agent.instructions`"), "{err}");
    assert!(err.contains("`system_prompt`"), "{err}");
    assert!(err.contains("`agent.name`"), "{err}");
    assert!(err.contains("`name`"), "{err}");

    // And the table itself is the migration a consumer performs, so the fields
    // it says are unchanged are asserted unchanged rather than left to prose.
    for unchanged in [
        "user.persona",
        "user.done_when",
        "user.done_when_replaces_base",
        "user.max_turns",
    ] {
        assert!(
            FORMAT.contains(&format!("| `{unchanged}` | unchanged |")),
            "the rewrite table stopped saying {unchanged} is unchanged"
        );
    }
    let carried = Persona::parse(
        concat!(
            "system_prompt: r\n",
            "user:\n  persona: p\n  done_when: bar\n",
            "  done_when_replaces_base: true\n  max_turns: 3\n",
        ),
        "carried.yaml",
    )
    .expect("every field the table calls unchanged still loads");
    let merged = merge(BASE, "base.yaml", &carried).expect("it merges");
    assert_eq!(merged["user"]["done_when"], Value::from("bar"));
    assert_eq!(merged["user"]["max_turns"], Value::from(3));
}

/// The fields the document says are refused are exactly the fields the crate
/// refuses, in the same order and with the same reason.
///
/// `MEMBER_OWNED` is the list a persona is actually checked against, and this
/// document is where an author reads it — two places one fact has to agree in, so
/// the document's table is reconciled against the constant rather than written
/// beside it. A field added to the crate and not to the table, or a reason
/// reworded in one and not the other, fails here.
#[test]
fn the_document_names_exactly_the_fields_the_crate_refuses() {
    let expected: Vec<String> = MEMBER_OWNED
        .iter()
        .map(|(field, owner)| format!("| `{field}` | {owner} |"))
        .collect();
    assert_eq!(
        table_rows("| refused field |"),
        expected,
        "docs/persona-format.md and MEMBER_OWNED disagree about what a persona may not carry"
    );
}

/// Every role this crate ships merges to exactly the config it merged to before
/// the format changed.
///
/// The golden was captured from the previous spelling — the same five personas
/// with `agent: {name, instructions}` blocks, over the same base with an
/// `agent: {instructions}` preamble. Nothing here regenerates it: a role whose
/// effective config moved has to be looked at, not re-recorded.
///
/// One entry has been looked at and re-recorded since, deliberately and once:
/// `engineer`'s `user.persona`, whose review bar was corrected so that it demands
/// nothing a member cannot settle inside its own run. `tests/persona_bar.rs` is
/// what holds every shipped bar there, and says why. Every other entry is still
/// the original capture.
#[test]
fn the_shipped_roles_merge_to_what_they_did_before_the_format_changed() {
    let expected: BTreeMap<String, Value> =
        serde_json::from_str(GOLDEN).expect("the golden is JSON");
    let produced: BTreeMap<String, Value> = SHIPPED_PERSONAS
        .iter()
        .map(|(name, document)| {
            let persona = Persona::parse(document, name)
                .unwrap_or_else(|err| panic!("{name} no longer loads: {err}"));
            let merged = merge(BASE, "base.yaml", &persona)
                .unwrap_or_else(|err| panic!("{name} no longer merges: {err}"));
            ((*name).to_string(), canonical(&merged))
        })
        .collect();
    assert_eq!(
        produced.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "the shipped catalog changed, which the golden cannot answer for"
    );
    for (name, expected) in &expected {
        assert_eq!(
            &produced[name], expected,
            "{name} is layered differently than it was before the format changed"
        );
    }
}
