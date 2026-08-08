//! The committed contract drives the public types.
//!
//! Every fixture here is read out of `docs/contract.md` at compile time rather
//! than copied beside it, so the document and this crate's wire shapes cannot
//! drift: edit one without the other and this suite fails.
//!
//! The document's JSON example is a *shape* illustration — four of its scalars
//! are angle-bracketed placeholders or an alternation of the legal values — so
//! the envelope test substitutes exactly those four, asserting each placeholder
//! is still there before it does. A doc edit that renames a placeholder fails
//! here rather than silently skipping the substitution.

use std::collections::BTreeSet;

use oneagentgraph::config::{
    AgentSide, ConfigRef, GraphConfig, JudgeSide, Member, OneharnessMember, OnejudgeMember,
    Schedule,
};
use oneagentgraph::error::{Error, EXIT_INVALID_CONFIG, EXIT_MEMBER_FAILED, EXIT_SUCCESS};
use oneagentgraph::event::{
    Artifact, Disposition, Envelope, EventKind, FallbackAdvanced, MemberDied, Source, TurnActivity,
    TurnCompleted, Usage, ENVELOPE_VERSION, MAX_ACTIVITY_DETAIL_CHARS, MAX_PAYLOAD_TEXT_BYTES,
};
use oneagentgraph::liveness::{
    DEFAULT_HEARTBEAT_TIMEOUT, DEFAULT_STALL_TIMEOUT, HEARTBEAT_TIMEOUT_ENV, OWNER_LOCK_FILE,
    STALL_TIMEOUT_ENV,
};
use oneagentgraph::run::{RunId, Started};
use serde_json::{json, Value};

/// The approved contract itself.
const CONTRACT: &str = include_str!("../docs/contract.md");

/// Every variant of [`EventKind`], so the doc-derived list below is checked in
/// both directions: a kind the document forgets, and a kind the crate forgets.
const ALL_EVENT_KINDS: &[EventKind] = &[
    EventKind::GraphStarted,
    EventKind::MemberStarted,
    EventKind::TurnStarted,
    EventKind::TurnActivity,
    EventKind::TurnCompleted,
    EventKind::MemberHeartbeat,
    EventKind::FallbackAdvanced,
    EventKind::MemberDied,
    EventKind::CronFired,
    EventKind::CronReset,
    EventKind::MemberSettled,
    EventKind::GraphSettled,
];

/// The fenced blocks in the contract carrying the given info string. The CLI
/// usage block carries none, so an empty `language` selects it — which means the
/// scanner has to track whether it is *inside* a block rather than matching the
/// opening fence, or every closing fence would open an unlabelled one.
fn fenced_blocks(language: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut open: Option<String> = None;
    let mut body = String::new();
    for line in CONTRACT.lines() {
        match &open {
            Some(info) => {
                if line.trim_end() == "```" {
                    if info == language {
                        blocks.push(std::mem::take(&mut body));
                    }
                    body.clear();
                    open = None;
                } else {
                    body.push_str(line);
                    body.push('\n');
                }
            }
            None => {
                if let Some(info) = line.trim_end().strip_prefix("```") {
                    open = Some(info.trim().to_string());
                    body.clear();
                }
            }
        }
    }
    assert!(open.is_none(), "unterminated ``` block in docs/contract.md");
    blocks
}

/// The one fenced block in the contract carrying the given info string.
fn fenced_block(language: &str) -> String {
    let blocks = fenced_blocks(language);
    assert_eq!(
        blocks.len(),
        1,
        "expected exactly one ```{language} block in docs/contract.md, found {}",
        blocks.len()
    );
    blocks.into_iter().next().expect("one block")
}

/// Every `` `backticked` `` token in the contract.
fn backticked() -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = CONTRACT;
    while let Some(open) = rest.find('`') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('`') else { break };
        out.push(rest[..close].to_string());
        rest = &rest[close + 1..];
    }
    out
}

/// Replace one placeholder scalar in the doc's envelope example, asserting it
/// was there to replace.
fn substitute(example: &mut Value, field: &str, placeholder: &str, concrete: Value) {
    let slot = example
        .get_mut(field)
        .unwrap_or_else(|| panic!("the envelope example has no `{field}` field"));
    assert_eq!(
        slot,
        &Value::String(placeholder.to_string()),
        "the envelope example's `{field}` placeholder moved; update this substitution"
    );
    *slot = concrete;
}

/// The doc's envelope example with its four placeholder scalars made concrete.
fn envelope_example() -> Value {
    let mut example: Value =
        serde_json::from_str(&fenced_block("json")).expect("the envelope example is not JSON");
    substitute(
        &mut example,
        "ts",
        "<RFC3339, millisecond, UTC>",
        json!("2026-08-07T12:34:56.789Z"),
    );
    substitute(
        &mut example,
        "stream",
        "<unique id per producing process>",
        json!("oneagentgraph-4f2a"),
    );
    substitute(
        &mut example,
        "source",
        "agentgraph|vcs|pipeline",
        json!("agentgraph"),
    );
    substitute(
        &mut example,
        "kind",
        "<event kind>",
        json!("member-started"),
    );
    example
}

#[test]
fn the_documented_envelope_round_trips_through_the_public_type() {
    let example = envelope_example();

    let envelope: Envelope =
        serde_json::from_value(example.clone()).expect("the documented envelope does not parse");

    assert_eq!(envelope.v, ENVELOPE_VERSION);
    assert_eq!(envelope.seq, 42);
    assert_eq!(envelope.source, Source::Agentgraph);
    assert_eq!(envelope.kind, EventKind::MemberStarted);
    assert_eq!(envelope.labels.run_id.as_deref(), Some("R"));
    assert_eq!(envelope.labels.round, Some(2));
    assert_eq!(envelope.labels.node.as_deref(), Some("service"));
    assert_eq!(envelope.labels.step.as_deref(), Some("implement"));
    assert_eq!(envelope.labels.member.as_deref(), Some("worker"));
    assert_eq!(envelope.labels.persona.as_deref(), Some("engineer"));
    assert!(envelope.labels.extra.is_empty(), "no extras in the example");
    assert!(
        envelope.payload.is_empty(),
        "the example's payload is empty"
    );
    assert_eq!(
        envelope.artifacts,
        vec![Artifact {
            id: "a-91".to_string(),
            kind: "log".to_string(),
            bytes: 21_400,
        }]
    );

    let round_tripped = serde_json::to_value(&envelope).expect("the envelope does not serialize");
    assert_eq!(
        round_tripped, example,
        "serializing the parsed envelope must reproduce the documented shape"
    );
}

#[test]
fn free_form_labels_survive_a_round_trip_beside_the_reserved_ones() {
    let mut example = envelope_example();
    example["labels"]["workstream"] = json!("contract");

    let envelope: Envelope =
        serde_json::from_value(example.clone()).expect("an extra label must be accepted");
    assert_eq!(
        envelope.labels.extra.get("workstream"),
        Some(&json!("contract")),
        "an unreserved label belongs in `extra`, not dropped"
    );
    assert_eq!(
        serde_json::to_value(&envelope).expect("serializes"),
        example,
        "an enricher's extra label must come back out unrewritten"
    );
}

#[test]
fn an_envelope_with_an_unknown_field_is_rejected() {
    let mut example = envelope_example();
    example["stage"] = json!("verify");

    let error = serde_json::from_value::<Envelope>(example)
        .expect_err("an unknown envelope field must not be silently dropped");
    assert!(
        error.to_string().contains("stage"),
        "the error should name the offending field, got: {error}"
    );
}

#[test]
fn the_source_alternation_names_exactly_the_source_variants() {
    let documented: BTreeSet<String> = "agentgraph|vcs|pipeline"
        .split('|')
        .map(str::to_string)
        .collect();
    assert!(
        CONTRACT.contains("\"source\": \"agentgraph|vcs|pipeline\""),
        "the contract no longer alternates the sources where this test reads them"
    );

    let implemented: BTreeSet<String> = [Source::Agentgraph, Source::Vcs, Source::Pipeline]
        .into_iter()
        .map(|source| {
            serde_json::to_value(source)
                .expect("serializes")
                .as_str()
                .expect("a string")
                .to_string()
        })
        .collect();

    assert_eq!(documented, implemented);
    for name in &documented {
        serde_json::from_value::<Source>(json!(name))
            .unwrap_or_else(|_| panic!("documented source `{name}` does not parse"));
    }
}

#[test]
fn the_event_kinds_paragraph_names_exactly_the_event_kind_variants() {
    // The kinds are the only kebab-case-with-a-dash tokens in the contract's
    // backticks; the payload fields beside them (`rule`, `exit_code`,
    // `stderr_tail`) are single words or snake_case.
    let documented: BTreeSet<String> = backticked()
        .into_iter()
        .filter(|token| {
            token.contains('-')
                && token.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                && !token.starts_with('-')
                && !token.ends_with('-')
        })
        .collect();

    let implemented: BTreeSet<String> = ALL_EVENT_KINDS
        .iter()
        .map(|kind| {
            serde_json::to_value(kind)
                .expect("serializes")
                .as_str()
                .expect("a string")
                .to_string()
        })
        .collect();

    assert_eq!(
        documented, implemented,
        "every documented event kind must be a variant, and every variant documented"
    );
    for name in &documented {
        let kind: EventKind = serde_json::from_value(json!(name))
            .unwrap_or_else(|_| panic!("documented kind `{name}` does not parse"));
        assert_eq!(
            serde_json::to_value(kind).expect("serializes"),
            json!(name),
            "`{name}` must round-trip"
        );
    }
}

#[test]
fn a_turn_activity_payload_carries_the_documented_bounded_summary() {
    assert!(
        CONTRACT.contains("(bounded tool summary: kind, name, 160-char detail)"),
        "the contract no longer describes the turn-activity payload this test pins"
    );

    let within_bounds = TurnActivity {
        kind: "tool".to_string(),
        name: "Bash".to_string(),
        detail: "cargo test --locked".to_string(),
        truncated: false,
    };
    assert_eq!(
        serde_json::to_value(&within_bounds).expect("serializes"),
        json!({"kind": "tool", "name": "Bash", "detail": "cargo test --locked"}),
        "an untruncated summary must not carry a `truncated` key"
    );

    let cut = TurnActivity {
        detail: "x".repeat(MAX_ACTIVITY_DETAIL_CHARS),
        truncated: true,
        ..within_bounds.clone()
    };
    let serialized = serde_json::to_value(&cut).expect("serializes");
    assert_eq!(
        serialized["truncated"],
        json!(true),
        "a summary cut to its bound must say so"
    );
    assert_eq!(
        serde_json::from_value::<TurnActivity>(serialized).expect("parses"),
        cut,
        "the turn-activity payload must round-trip"
    );
}

#[test]
fn a_turn_completed_payload_carries_the_documented_usage() {
    assert!(
        CONTRACT.contains("(usage: tokens in/out, cache r/w, cost, duration)"),
        "the contract no longer describes the turn-completed payload this test pins"
    );

    let completed = TurnCompleted {
        usage: Usage {
            tokens_in: 12_000,
            tokens_out: 900,
            cache_read: 8_000,
            cache_write: 1_500,
            cost: 0.42,
            duration: 61.5,
        },
    };
    let serialized = serde_json::to_value(&completed).expect("serializes");
    assert_eq!(
        serialized,
        json!({"usage": {
            "tokens_in": 12_000,
            "tokens_out": 900,
            "cache_read": 8_000,
            "cache_write": 1_500,
            "cost": 0.42,
            "duration": 61.5,
        }})
    );
    assert_eq!(
        serde_json::from_value::<TurnCompleted>(serialized).expect("parses"),
        completed
    );
}

#[test]
fn a_fallback_advanced_payload_names_the_identity_and_the_classified_reason() {
    assert!(
        CONTRACT.contains("(identity, classified reason)"),
        "the contract no longer describes the fallback-advanced payload this test pins"
    );

    let advanced = FallbackAdvanced {
        identity: "codex".to_string(),
        reason: "quota".to_string(),
    };
    let serialized = serde_json::to_value(&advanced).expect("serializes");
    assert_eq!(serialized, json!({"identity": "codex", "reason": "quota"}));
    assert_eq!(
        serde_json::from_value::<FallbackAdvanced>(serialized).expect("parses"),
        advanced
    );
}

#[test]
fn a_member_died_payload_carries_every_documented_field() {
    for field in ["rule", "exit_code", "stderr_tail"] {
        assert!(
            backticked().iter().any(|token| token == field),
            "the contract no longer names the member-died field `{field}`"
        );
    }

    let exited = MemberDied {
        rule: "activity-watchdog".to_string(),
        exit_code: Some(1),
        disposition: Disposition::Exited,
        stderr_tail: "harness failed (quota)".to_string(),
        truncated: false,
    };
    let serialized = serde_json::to_value(&exited).expect("serializes");
    assert_eq!(
        serialized,
        json!({
            "rule": "activity-watchdog",
            "exit_code": 1,
            "disposition": "exited",
            "stderr_tail": "harness failed (quota)",
        }),
        "an exited member reports its code, and an uncut tail says nothing about truncation"
    );
    assert_eq!(
        serde_json::from_value::<MemberDied>(serialized).expect("parses"),
        exited
    );

    let signaled = MemberDied {
        exit_code: None,
        disposition: Disposition::Signaled,
        stderr_tail: "x".repeat(MAX_PAYLOAD_TEXT_BYTES),
        truncated: true,
        ..exited
    };
    let serialized = serde_json::to_value(&signaled).expect("serializes");
    assert_eq!(
        serialized["disposition"],
        json!("signaled"),
        "a signalled member has no exit code to report"
    );
    assert!(
        serialized.get("exit_code").is_none(),
        "an absent exit code is omitted rather than written as null"
    );
    assert_eq!(serialized["truncated"], json!(true));
    assert_eq!(
        serde_json::from_value::<MemberDied>(serialized).expect("parses"),
        signaled
    );
}

#[test]
fn the_failure_modes_report_which_member_and_why() {
    let invalid =
        Error::InvalidConfig("members.worker.agent: missing oneharness_config".to_string());
    assert_eq!(
        invalid.to_string(),
        "invalid config: members.worker.agent: missing oneharness_config"
    );

    let failed = Error::MemberFailed {
        member: "worker".to_string(),
        reason: "the activity watchdog fired".to_string(),
    };
    assert_eq!(
        failed.to_string(),
        "member 'worker' failed: the activity watchdog fired",
        "the summary a caller sees must name the member the stream blames"
    );
}

#[test]
fn the_documented_dispositions_are_the_ones_a_member_died_payload_takes() {
    let documented = backticked()
        .into_iter()
        .find(|token| token.starts_with("disposition:"))
        .expect("the contract no longer states the member-died dispositions");
    let values: Vec<String> = documented
        .trim_start_matches("disposition:")
        .trim()
        .split('|')
        .map(str::to_string)
        .collect();

    let parsed: Vec<Disposition> = values
        .iter()
        .map(|value| {
            serde_json::from_value(json!(value))
                .unwrap_or_else(|_| panic!("documented disposition `{value}` does not parse"))
        })
        .collect();
    assert_eq!(parsed, vec![Disposition::Exited, Disposition::Signaled]);
}

#[test]
fn the_documented_payload_bounds_are_the_ones_the_crate_declares() {
    assert!(
        CONTRACT.contains(&format!("truncate at {MAX_PAYLOAD_TEXT_BYTES} bytes")),
        "the payload text bound in docs/contract.md and MAX_PAYLOAD_TEXT_BYTES disagree"
    );
    assert!(
        CONTRACT.contains(&format!("{MAX_ACTIVITY_DETAIL_CHARS}-char detail")),
        "the turn-activity detail bound in docs/contract.md and \
         MAX_ACTIVITY_DETAIL_CHARS disagree"
    );
    assert!(
        CONTRACT.contains(&format!("\"v\": {ENVELOPE_VERSION},")),
        "the envelope version in docs/contract.md and ENVELOPE_VERSION disagree"
    );
}

#[test]
fn the_documented_exit_codes_are_the_ones_the_crate_declares() {
    assert!(
        CONTRACT.contains(&format!(
            "Exit {EXIT_SUCCESS} = every member settled successfully"
        )),
        "the success exit code in docs/contract.md and EXIT_SUCCESS disagree"
    );
    assert!(
        CONTRACT.contains(&format!("{EXIT_MEMBER_FAILED} = a member failed or died")),
        "the failure exit code in docs/contract.md and EXIT_MEMBER_FAILED disagree"
    );
    assert!(
        CONTRACT.contains(&format!("{EXIT_INVALID_CONFIG} = invalid config")),
        "the invalid-config exit code in docs/contract.md and EXIT_INVALID_CONFIG disagree"
    );
}

/// The two copies of the exit codes that live outside the contract and outside
/// the crate, tied back to the constants.
///
/// Both exist for a reason and neither can read the contract where it is used:
/// the README is the first place a reader meets the codes, and
/// `scripts/smoke-published.sh` is deliberately toolchain-free bash that holds an
/// *installed* artifact to them, on a machine with no checkout. A restated
/// number with nothing checking it is the copy that goes stale — silently, since
/// a wrong code in the smoke script means the smoke passes on the wrong
/// behavior.
#[test]
fn every_restatement_of_the_exit_codes_matches_the_crate() {
    let readme = squeeze(include_str!("../README.md"));
    let sentence = format!(
        "Exit `{EXIT_SUCCESS}` means every member settled, `{EXIT_MEMBER_FAILED}` that one \
         failed or died, `{EXIT_INVALID_CONFIG}` that the config is invalid."
    );
    assert!(
        readme.contains(&sentence),
        "README.md no longer states the exit codes the crate declares — expected: {sentence}"
    );

    // The published smoke's only exit-code comparison is against the contract's
    // invalid-config code: it refuses graphs, it never runs one.
    let script = include_str!("../scripts/smoke-published.sh");
    let compared: Vec<&str> = script
        .split("-ne ")
        .skip(1)
        .map(|rest| {
            rest.split(|c: char| c.is_whitespace() || c == ']')
                .next()
                .unwrap_or_default()
        })
        .collect();
    assert!(
        !compared.is_empty(),
        "scripts/smoke-published.sh no longer compares an exit code at all"
    );
    for code in &compared {
        assert_eq!(
            *code,
            EXIT_INVALID_CONFIG.to_string(),
            "scripts/smoke-published.sh checks for exit {code}, which is not \
             EXIT_INVALID_CONFIG ({EXIT_INVALID_CONFIG}); every code it asserts must be one the \
             crate declares"
        );
    }
}

/// One block of prose as a single line, so a gate on a sentence is not a gate on
/// where the paragraph happened to wrap.
fn squeeze(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The published smoke checks an *installed* binary against the same command
/// surface this document spells, and it is toolchain-free bash that cannot read
/// the document to find it.
///
/// This is the one place the contract's CLI block is parsed. `tests/e2e/main.rs`
/// holds `--help` to the script's list rather than to this document, so the
/// document has a single reader and the two lists cannot drift apart behind a
/// second parser.
#[test]
fn the_published_smoke_checks_the_same_commands_the_contract_documents() {
    let script = include_str!("../scripts/smoke-published.sh");
    let loop_line = script
        .lines()
        .find(|line| line.trim_start().starts_with("for command in "))
        .expect("scripts/smoke-published.sh no longer loops over the command list");
    let mut checked: Vec<String> = loop_line
        .trim()
        .trim_start_matches("for command in ")
        .trim_end_matches("; do")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    checked.sort();
    checked.dedup();

    let mut documented = documented_commands();
    documented.sort();
    documented.dedup();
    assert!(
        documented.len() >= 9,
        "the contract's CLI block stopped parsing: {documented:?}"
    );
    assert_eq!(
        checked, documented,
        "scripts/smoke-published.sh and docs/contract.md disagree about the command surface"
    );
}

/// Every command the contract's CLI usage block spells.
fn documented_commands() -> Vec<String> {
    // Only the usage block — the document's prose says "oneagentgraph owns no
    // harness logic", and a scan of the whole file would read `owns` as a command
    // nobody typed.
    let usage = CONTRACT
        .split("```")
        .find(|block| block.starts_with("\noneagentgraph run GRAPH"))
        .expect("the contract's CLI usage block moved");
    usage
        .lines()
        .filter_map(|line| line.strip_prefix("oneagentgraph "))
        .filter_map(|rest| rest.split_whitespace().next())
        .filter(|word| word.chars().all(|c| c.is_ascii_lowercase() || c == '-'))
        .map(str::to_string)
        .collect()
}

#[test]
fn the_documented_liveness_bounds_are_the_ones_the_crate_declares() {
    assert!(
        CONTRACT.contains(&format!(
            "default deadline {}s",
            DEFAULT_HEARTBEAT_TIMEOUT.as_secs()
        )),
        "the heartbeat deadline in docs/contract.md and DEFAULT_HEARTBEAT_TIMEOUT disagree"
    );
    assert!(
        CONTRACT.contains(&format!("default {}s", DEFAULT_STALL_TIMEOUT.as_secs())),
        "the stall deadline in docs/contract.md and DEFAULT_STALL_TIMEOUT disagree"
    );
    for name in [HEARTBEAT_TIMEOUT_ENV, STALL_TIMEOUT_ENV, OWNER_LOCK_FILE] {
        assert!(
            CONTRACT.contains(name),
            "docs/contract.md no longer names `{name}`"
        );
    }
}

#[test]
fn the_documented_graph_round_trips_through_the_config_schema() {
    let yaml = fenced_block("yaml");
    let graph: GraphConfig =
        serde_norway::from_str(&yaml).expect("the documented graph does not parse");

    assert_eq!(graph.version, 1);
    assert_eq!(graph.name, "node-scope");
    assert_eq!(graph.env.get("MY_VAR").map(String::as_str), Some("value"));
    assert_eq!(
        graph.members.keys().collect::<Vec<_>>(),
        vec!["reporter", "worker"]
    );

    let Some(Member::Onejudge(worker)) = graph.members.get("worker") else {
        panic!("`worker` must deserialize as a onejudge member");
    };
    assert_eq!(
        worker,
        &OnejudgeMember {
            base_config: ConfigRef("./onejudge.base.yaml".to_string()),
            persona: Some(ConfigRef("https://example.com/engineer.yaml".to_string())),
            task: None,
            agent: AgentSide {
                oneharness_config: ConfigRef("./oneharness.toml".to_string()),
                model: None,
                stream: true,
            },
            judge: JudgeSide::Harness(oneagentgraph::config::JudgeHarness {
                oneharness_config: ConfigRef("./oneharness.judge.toml".to_string()),
                model: None,
            }),
            mode: "bypass".to_string(),
            max_turns: None,
        }
    );

    let Some(Member::Oneharness(reporter)) = graph.members.get("reporter") else {
        panic!("`reporter` must deserialize as a oneharness member");
    };
    assert_eq!(
        reporter,
        &OneharnessMember {
            oneharness_config: ConfigRef("./oneharness.toml".to_string()),
            persona: Some(ConfigRef("./reporter.yaml".to_string())),
            schedule: Some(Schedule {
                every: 1800,
                resettable: true,
            }),
            deps: Vec::new(),
        }
    );

    let reserialized = serde_norway::to_string(&graph).expect("the graph does not serialize");
    let reparsed: GraphConfig =
        serde_norway::from_str(&reserialized).expect("the serialized graph does not parse back");
    assert_eq!(reparsed, graph, "the graph schema must round-trip");
}

#[test]
fn a_graph_with_an_unknown_field_is_rejected() {
    let yaml = format!("{}\nnodes: []\n", fenced_block("yaml").trim_end());
    let error = serde_norway::from_str::<GraphConfig>(&yaml)
        .expect_err("an unknown graph field must not be silently dropped");
    assert!(
        error.to_string().contains("nodes"),
        "the error should name the offending field, got: {error}"
    );
}

#[test]
fn a_member_of_either_kind_rejects_an_unknown_field() {
    // A misspelled member field is the boundary failure a graph author actually
    // hits, and it is inside an internally-tagged enum variant — where a
    // `deny_unknown_fields` that quietly did nothing would leave the typo
    // silently dropped and the member running with a default. Both variants are
    // driven: they carry the attribute independently, so one can regress while
    // the other holds.
    let graph = fenced_block("yaml");
    for (kind, from, to, typo) in [
        ("onejudge", "    mode: bypass", "    moed: bypass", "moed"),
        ("oneharness", "    deps: []", "    dpes: []", "dpes"),
    ] {
        let yaml = graph.replace(from, to);
        assert_ne!(yaml, graph, "the contract no longer shows `{from}`");

        let error = serde_norway::from_str::<GraphConfig>(&yaml).expect_err(&format!(
            "a `{kind}` member's unknown field must not be silently dropped"
        ));
        assert!(
            error.to_string().contains(typo),
            "the error should name the offending `{kind}` field, got: {error}"
        );
    }
}

#[test]
fn the_documented_command_provider_judge_parses() {
    let documented = backticked()
        .into_iter()
        .find(|token| token.starts_with("judge: {command:"))
        .expect("the contract no longer shows a command-provider judge");
    let side = documented.trim_start_matches("judge:").trim().to_string();

    let judge: JudgeSide =
        serde_norway::from_str(&side).expect("the documented command judge does not parse");
    assert_eq!(
        judge,
        JudgeSide::Command(oneagentgraph::config::JudgeCommand {
            command: vec!["...".to_string()],
        })
    );
}

#[test]
fn an_agent_side_streams_unless_the_graph_turns_it_off() {
    assert!(
        CONTRACT.contains("stream: true                    # default true; false = report-only"),
        "the contract no longer states the streaming default this test pins"
    );

    let defaulted: AgentSide = serde_norway::from_str("oneharness_config: ./oneharness.toml\n")
        .expect("an agent side without `stream` must parse");
    assert!(defaulted.stream, "streaming is on unless turned off");

    let off: AgentSide =
        serde_norway::from_str("oneharness_config: ./oneharness.toml\nstream: false\n")
            .expect("an agent side with `stream: false` must parse");
    assert!(!off.stream, "report-only is what `stream: false` asks for");
}

#[test]
fn the_documented_cli_names_every_command_the_binary_accepts() {
    let usage = fenced_block("");
    for command in [
        "run",
        "validate",
        "trigger",
        "reset-timer",
        "cancel",
        "history",
        "health",
        "smoke",
        "persona",
    ] {
        assert!(
            usage.contains(&format!("oneagentgraph {command}")),
            "the contract's CLI block no longer documents `{command}`"
        );
    }
    for flag in [
        "--task",
        "--task-file",
        "--dir",
        "--label",
        "--set",
        "--output",
        "--detach",
        "--kill",
    ] {
        assert!(
            usage.contains(flag),
            "the contract's CLI block no longer documents `{flag}`"
        );
    }
}

/// `--detach` prints `{run_id, events_path, pid}` and exits 0 — three keys the
/// contract names, and the shape a caller parses to find the run it just left
/// behind. Driven through the type so a rename here fails against the document.
#[test]
fn the_detach_answer_carries_the_three_keys_the_contract_names() {
    let sentence = CONTRACT
        .lines()
        .find(|line| line.contains("--detach` prints"))
        .expect("the contract no longer describes what --detach prints");
    for key in ["run_id", "events_path", "pid"] {
        assert!(
            sentence.contains(key),
            "the contract's --detach answer no longer names `{key}`"
        );
    }

    let started = Started {
        run_id: RunId::parse("node-scope-1786171301679-1447994").expect("a run id"),
        events_path: "/state/node-scope/events.jsonl".into(),
        pid: 1_447_994,
    };
    let rendered = serde_json::to_value(&started).expect("an answer");
    let keys: BTreeSet<&str> = rendered
        .as_object()
        .expect("a mapping")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["run_id", "events_path", "pid"]
            .into_iter()
            .collect::<BTreeSet<_>>()
    );
    // And it round-trips, because a caller that reads it back has to get the
    // same three values it was handed.
    assert_eq!(
        serde_json::from_value::<Started>(rendered).expect("round-trips"),
        started
    );
}
