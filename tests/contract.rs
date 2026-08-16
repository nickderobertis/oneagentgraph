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

// llmlint: ignore-file[contracts_have_one_source_or_a_drift_gate] accurate, and
// open as blocker 1 in `docs/oneharness-library.md`: the contract sentence this
// contradicts is its owner's to correct, and no coupling test stands in for
// that approval.

use std::collections::BTreeSet;

use oneagentgraph::cli::DEFAULT_MIN_AGE_HOURS;
use oneagentgraph::config::{
    AgentSide, ConfigRef, GraphConfig, JudgeSide, Member, OneharnessMember, OnejudgeMember,
    Schedule, FIRST_EVENT_FILTER_VERSION, FIRST_PERSONA_CATALOG_VERSION, FIRST_SCHEMA_VERSION,
    FIRST_START_AFTER_VERSION, SCHEMA_VERSION,
};
use oneagentgraph::error::{
    Error, EXIT_INVALID_CONFIG, EXIT_MEMBER_FAILED, EXIT_NO_CONTROLLABLE_TURN, EXIT_SUCCESS,
};
use oneagentgraph::event::{
    Artifact, Cause, Disposition, Envelope, EventFilter, EventKind, FallbackAdvanced, Labels,
    Matcher, MemberDied, MemberStarted, Role, Runner, Source, TurnActivity, TurnCompleted,
    TurnInterrupted, Usage, ENVELOPE_VERSION, MAX_ACTIVITY_DETAIL_CHARS, MAX_PAYLOAD_TEXT_BYTES,
};
use oneagentgraph::liveness::{
    DEFAULT_HEARTBEAT_TIMEOUT, DEFAULT_STALL_TIMEOUT, HEARTBEAT_TIMEOUT_ENV, OWNER_LOCK_FILE,
    STALL_TIMEOUT_ENV,
};
use oneagentgraph::run::{RunId, Started};
use oneagentgraph::scratch::WORKING_PERCENT_OF_A_CORE;
use oneagentgraph::sweep::{families, RUNS_FAMILY, TEMP_FAMILY};
use serde_json::{json, Value};

/// The approved contract itself.
const CONTRACT: &str = include_str!("../docs/contract.md");
/// The package-front example must name the same current graph schema.
const README: &str = include_str!("../README.md");

/// Every variant of [`EventKind`], so the doc-derived list below is checked in
/// both directions: a kind the document forgets, and a kind the crate forgets.
const ALL_EVENT_KINDS: &[EventKind] = &[
    EventKind::GraphStarted,
    EventKind::MemberStarted,
    EventKind::TurnStarted,
    EventKind::TurnActivity,
    EventKind::TurnCompleted,
    EventKind::TurnInterrupted,
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
    backticked_in(CONTRACT)
}

/// The same, over one passage of it.
fn backticked_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
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

/// Every liveness rule this build can die by.
///
/// The one other closed set the contract spells in kebab case, so it has to be
/// separated from the event kinds below — and asserting it here is what makes the
/// separation honest rather than a filter that quietly drops a kind.
const ALL_RULES: &[oneagentgraph::member::Rule] = &[
    oneagentgraph::member::Rule::Unstartable,
    oneagentgraph::member::Rule::Signalled,
    oneagentgraph::member::Rule::ProviderFailure,
    oneagentgraph::member::Rule::Heartbeat,
    oneagentgraph::member::Rule::Activity,
];

#[test]
fn the_documented_rules_are_the_ones_a_member_can_die_by() {
    let documented: BTreeSet<String> = backticked()
        .into_iter()
        .filter(|token| ALL_RULES.iter().any(|rule| rule.as_str() == token))
        .collect();
    let taken: BTreeSet<String> = ALL_RULES
        .iter()
        .map(|rule| rule.as_str().to_string())
        .collect();
    assert_eq!(
        documented, taken,
        "the contract and this build disagree about the liveness rules a member dies by"
    );
    for rule in ALL_RULES {
        assert_eq!(
            oneagentgraph::member::Rule::named(rule.as_str()),
            Some(*rule),
            "a documented rule this build cannot read back is one a run record cannot carry"
        );
    }
}

#[test]
fn the_event_kinds_paragraph_names_exactly_the_event_kind_variants() {
    // The kinds are the kebab-case-with-a-dash tokens in the contract's
    // backticks; the payload fields beside them (`rule`, `cause`, `detail`,
    // `exit_code`, `stderr_tail`) are single words or snake_case. The liveness
    // rules are the one other kebab-case set, and they are checked against this
    // build in `the_documented_rules_are_the_ones_a_member_can_die_by` above —
    // so excluding them here drops nothing unproven.
    let documented: BTreeSet<String> = backticked()
        .into_iter()
        .filter(|token| {
            token.contains('-')
                && token.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                && !token.starts_with('-')
                && !token.ends_with('-')
                && !ALL_RULES.iter().any(|rule| rule.as_str() == token)
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

/// `turn-interrupted` carries the four fields the contract names, and `reason`
/// is present exactly when the redirection did not land — a served interrupt that
/// carried one would let a consumer read a success as a refusal.
#[test]
fn a_turn_interrupted_payload_names_the_member_and_whether_it_landed() {
    assert!(
        CONTRACT.contains(
            "(`member`, `delivered`, `input_bytes`, and the `reason` a delivery that did not land \
             names)"
        ),
        "the contract no longer describes the turn-interrupted payload this test pins"
    );

    let delivered = TurnInterrupted {
        member: "worker".to_string(),
        delivered: true,
        input_bytes: 31,
        reason: None,
    };
    let serialized = serde_json::to_value(&delivered).expect("serializes");
    assert_eq!(
        serialized,
        json!({"member": "worker", "delivered": true, "input_bytes": 31})
    );
    assert_eq!(
        serde_json::from_value::<TurnInterrupted>(serialized).expect("parses"),
        delivered
    );

    let refused = TurnInterrupted {
        member: "worker".to_string(),
        delivered: false,
        input_bytes: 0,
        reason: Some("the member is between turns".to_string()),
    };
    let serialized = serde_json::to_value(&refused).expect("serializes");
    assert_eq!(serialized["reason"], json!("the member is between turns"));
    assert_eq!(
        serde_json::from_value::<TurnInterrupted>(serialized).expect("parses"),
        refused
    );
}

/// The four exit codes `interrupt` assigns, and the one that is a *fact* rather
/// than an error — the distinction the whole verb rests on, since an operator's
/// script reads anything else as a lever that broke.
#[test]
fn the_documented_interrupt_exit_codes_are_the_ones_the_crate_declares() {
    let sentence = CONTRACT
        .lines()
        .find(|line| line.contains("Its exit codes:"))
        .expect("the contract no longer states what `interrupt` exits with");
    for (code, meaning) in [
        (EXIT_SUCCESS, "delivered"),
        (
            EXIT_NO_CONTROLLABLE_TURN,
            "the member has no controllable turn in flight",
        ),
        (EXIT_INVALID_CONFIG, "invalid arguments"),
        (
            EXIT_MEMBER_FAILED,
            "a delivery that was attempted and failed",
        ),
    ] {
        assert!(
            sentence.contains(&format!("`{code}` {meaning}")),
            "the contract's `interrupt` exit codes no longer say `{code}` {meaning}"
        );
    }
    assert!(
        CONTRACT.contains(&format!(
            "Exit `{EXIT_NO_CONTROLLABLE_TURN}` is **a fact, not an error**"
        )),
        "the contract no longer states that exit {EXIT_NO_CONTROLLABLE_TURN} is a fact"
    );
    // And every cause it can be, so an answer that says only "no turn" is a
    // document edit away from failing here.
    for cause in [
        "between turns",
        "already settled",
        "no out-of-band turn control",
        "opens no controllable turn at all",
    ] {
        assert!(
            CONTRACT.contains(cause),
            "the contract no longer names `{cause}` as an exit-{EXIT_NO_CONTROLLABLE_TURN} cause"
        );
    }
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
        role: None,
        turn: None,
    };
    let serialized = serde_json::to_value(&advanced).expect("serializes");
    assert_eq!(
        serialized,
        json!({"identity": "codex", "reason": "quota"}),
        "a single-sided member has one side, so it stamps neither of the two-party fields"
    );
    assert_eq!(
        serde_json::from_value::<FallbackAdvanced>(serialized).expect("parses"),
        advanced
    );

    // The two fields a two-party member's telemetry adds, which the document
    // names beside the pair above.
    assert!(
        CONTRACT.contains("role: agent|judge"),
        "the contract no longer names the side a fallback-advanced is attributed to"
    );
    let attributed = FallbackAdvanced {
        role: Some(Role::Judge),
        turn: Some(3),
        ..advanced
    };
    let serialized = serde_json::to_value(&attributed).expect("serializes");
    assert_eq!(serialized["role"], json!("judge"));
    assert_eq!(serialized["turn"], json!(3));
    assert_eq!(
        serde_json::from_value::<FallbackAdvanced>(serialized).expect("parses"),
        attributed
    );
}

#[test]
fn a_member_died_payload_carries_every_documented_field() {
    for field in ["rule", "cause", "detail", "exit_code", "stderr_tail"] {
        assert!(
            backticked().iter().any(|token| token == field),
            "the contract no longer names the member-died field `{field}`"
        );
    }

    let exited = MemberDied {
        rule: "activity-watchdog".to_string(),
        cause: Cause::Exited,
        detail: "harness failed (quota)".to_string(),
        truncated: false,
        exit_code: Some(1),
        disposition: Some(Disposition::Exited),
        stderr_tail: Some("harness failed (quota)".to_string()),
    };
    let serialized = serde_json::to_value(&exited).expect("serializes");
    assert_eq!(
        serialized,
        json!({
            "rule": "activity-watchdog",
            "cause": "exited",
            "detail": "harness failed (quota)",
            "exit_code": 1,
            "disposition": "exited",
            "stderr_tail": "harness failed (quota)",
        }),
        "an exited member reports its code, and an uncut detail says nothing about truncation"
    );
    assert_eq!(
        serde_json::from_value::<MemberDied>(serialized).expect("parses"),
        exited
    );

    let signaled = MemberDied {
        cause: Cause::Signaled,
        exit_code: None,
        disposition: Some(Disposition::Signaled),
        detail: "x".repeat(MAX_PAYLOAD_TEXT_BYTES),
        truncated: true,
        ..exited.clone()
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

    // The shape the conversion is *for*: a member driven in this process has no
    // exit status, no disposition, and no standard error, and says why it died
    // through the two fields that answer for every member instead.
    let in_process = MemberDied {
        rule: "provider-failure".to_string(),
        cause: Cause::Quota,
        detail: "provider error (respond): the subscription is exhausted".to_string(),
        truncated: false,
        exit_code: None,
        disposition: None,
        stderr_tail: None,
    };
    let serialized = serde_json::to_value(&in_process).expect("serializes");
    assert_eq!(
        serialized,
        json!({
            "rule": "provider-failure",
            "cause": "quota",
            "detail": "provider error (respond): the subscription is exhausted",
        }),
        "a member that was never a process must not report a process's facts as null"
    );
    assert_eq!(
        serde_json::from_value::<MemberDied>(serialized).expect("parses"),
        in_process
    );
}

/// Every `cause` the contract lists is one this build takes, and every one this
/// build takes is listed — so a category added on either side fails here.
#[test]
fn the_documented_causes_are_the_ones_a_member_died_payload_takes() {
    const ALL: &[Cause] = &[
        Cause::Auth,
        Cause::RateLimit,
        Cause::ModelNotFound,
        Cause::Quota,
        Cause::Overloaded,
        Cause::Timeout,
        Cause::Cancelled,
        Cause::Spawn,
        Cause::Protocol,
        Cause::Other,
        Cause::Exited,
        Cause::Signaled,
        Cause::Unclassified,
    ];
    let documented: BTreeSet<String> = backticked()
        .into_iter()
        .filter(|token| ALL.iter().any(|cause| cause.as_str() == token))
        .collect();
    let taken: BTreeSet<String> = ALL.iter().map(|cause| cause.as_str().to_string()).collect();
    assert_eq!(
        documented, taken,
        "the contract and this build disagree about the member-died causes"
    );
    for cause in ALL {
        let serialized = serde_json::to_value(cause).expect("serializes");
        assert_eq!(serialized, json!(cause.as_str()));
        assert_eq!(
            &serde_json::from_value::<Cause>(serialized).expect("parses"),
            cause
        );
    }
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
    // `interrupt`'s own code is restated there too, and it is the one a reader
    // most needs to be told is not a failure.
    let interrupted = format!(
        "Exit `{EXIT_NO_CONTROLLABLE_TURN}` means there was no controllable turn in flight, and \
         says which — a fact, not an error."
    );
    assert!(
        readme.contains(&interrupted),
        "README.md no longer states the exit code `interrupt` declares — expected: {interrupted}"
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
    // The activity watchdog's other number: the share of a core below which a
    // tree counts as idle. The document states it and this build applies it, so
    // neither may move alone — a threshold nobody wrote down is the calibration
    // this rule replaced.
    assert!(
        CONTRACT.contains(&format!("{WORKING_PERCENT_OF_A_CORE}% of one core")),
        "the idle threshold in docs/contract.md and WORKING_PERCENT_OF_A_CORE disagree"
    );
}

/// The families `sweep` names, and the floor it applies, are the ones the crate
/// declares.
///
/// The document's promise is that a family is always in one of two lists, which
/// is a promise about *names*: a family the code calls something else is one an
/// operator cannot find in the report, and a floor the document states and the
/// code does not apply is a directory taken from under them. Both are gated
/// against the constants rather than restated here.
#[test]
fn the_documented_sweep_names_the_families_and_the_floor_the_crate_applies() {
    for name in [RUNS_FAMILY, TEMP_FAMILY] {
        assert!(
            CONTRACT.contains(&format!("`{name}`")),
            "docs/contract.md no longer names the `{name}` scratch family"
        );
    }
    assert!(
        CONTRACT.contains(&format!("default {DEFAULT_MIN_AGE_HOURS}")),
        "the sweep floor in docs/contract.md and DEFAULT_MIN_AGE disagree"
    );
    // The families the document names are the families the binary sweeps: a
    // third one added to the crate and not to the document would be swept
    // without ever being described.
    let named: Vec<&str> = families("state".into(), "temp".into())
        .iter()
        .map(|family| family.name.as_str())
        .collect();
    assert_eq!(named, vec![RUNS_FAMILY, TEMP_FAMILY]);
}

#[test]
fn the_documented_graph_round_trips_through_the_config_schema() {
    let yaml = fenced_block("yaml");
    let graph: GraphConfig =
        serde_norway::from_str(&yaml).expect("the documented graph does not parse");

    assert_eq!(graph.version, SCHEMA_VERSION);
    oneagentgraph::config::validate(&graph).expect("the documented graph must validate");
    assert_eq!(graph.name, "node-scope");
    assert_eq!(graph.env.get("MY_VAR").map(String::as_str), Some("value"));
    assert_eq!(
        graph.personas.as_deref(),
        Some(std::path::Path::new("./personas"))
    );
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
            deps: Vec::new(),
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
            task: None,
            dir: None,
            schedule: Some(Schedule {
                every: 1800,
                start_after: Some(1800),
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

/// The documented `events.filter` is the shared grammar, and it decides the same
/// envelopes through the public type that the document says it does.
#[test]
fn the_documented_event_filter_is_the_grammar_the_crate_applies() {
    let graph: GraphConfig =
        serde_norway::from_str(&fenced_block("yaml")).expect("the documented graph parses");
    let filter = graph
        .events
        .as_ref()
        .and_then(|events| events.filter.as_ref())
        .expect("the documented graph names a filter");
    assert_eq!(
        filter,
        &EventFilter {
            include: vec![
                Matcher {
                    kind: Some("member-*".to_string()),
                    ..Matcher::default()
                },
                Matcher {
                    member: Some("worker".to_string()),
                    persona: Some("engineer".to_string()),
                    ..Matcher::default()
                },
            ],
            exclude: vec![Matcher {
                kind: Some("turn-activity".to_string()),
                ..Matcher::default()
            }],
        }
    );
    filter.validate().expect("the documented filter is usable");

    let worker = Labels {
        member: Some("worker".to_string()),
        persona: Some("engineer".to_string()),
        ..Labels::default()
    };
    // The glob admits every kind it spans, the labels admit the member's own
    // turns, and the exclusion beats both.
    assert!(filter.allows(Source::Agentgraph, "member-started", &worker));
    assert!(filter.allows(Source::Agentgraph, "member-settled", &worker));
    assert!(filter.allows(Source::Agentgraph, "turn-completed", &worker));
    assert!(!filter.allows(Source::Agentgraph, "turn-activity", &worker));
    // A `member-*` kind still passes on an envelope carrying neither label,
    // because a matcher list is a disjunction...
    assert!(filter.allows(Source::Agentgraph, "member-died", &Labels::default()));
    // ...and the graph's own events, which match no matcher, do not.
    assert!(!filter.allows(Source::Agentgraph, "graph-started", &Labels::default()));
}

/// A graph that names no `events` block serializes without one, so a document
/// written before the block existed round-trips byte-identically — and one that
/// declares an older schema and names it anyway is refused by the block's name.
#[test]
fn the_documented_event_block_is_omitted_when_unset_and_gated_when_set() {
    let documented = fenced_block("yaml");
    let mut graph: GraphConfig =
        serde_norway::from_str(&documented).expect("the documented graph parses");
    graph.events = None;
    let rendered = serde_norway::to_string(&graph).expect("the graph serializes");
    assert!(
        !rendered.contains("events:"),
        "an absent events block must stay absent for older consumers: {rendered}"
    );

    for older in FIRST_SCHEMA_VERSION..FIRST_EVENT_FILTER_VERSION {
        let older = documented.replace(
            &format!("version: {SCHEMA_VERSION}"),
            &format!("version: {older}"),
        );
        let mut graph: GraphConfig = serde_norway::from_str(&older).expect("it still parses");
        // The documented graph's `personas` catalog postdates every schema in
        // this loop too; dropping it leaves the block as the one thing refused.
        graph.personas = None;
        let error = oneagentgraph::config::validate(&graph)
            .expect_err("the block postdates this schema version");
        assert!(error.to_string().contains("`events`"), "{error}");
        assert!(
            error.to_string().contains(&format!(
                "requires graph schema version {FIRST_EVENT_FILTER_VERSION}"
            )),
            "{error}"
        );
    }
}

/// The documented persona catalog is optional, gated on the schema that has it,
/// and omitted from a graph that names none — so a document written before it
/// existed round-trips byte-identically and keeps its old resolution rule.
#[test]
fn the_documented_persona_catalog_is_gated_and_omitted_when_unset() {
    let documented = fenced_block("yaml");
    assert!(
        documented.contains("personas: ./personas"),
        "the documented graph must show its own persona catalog"
    );
    let mut graph: GraphConfig =
        serde_norway::from_str(&documented).expect("the documented graph parses");
    graph.personas = None;
    let rendered = serde_norway::to_string(&graph).expect("the graph serializes");
    assert!(
        !rendered.contains("personas:"),
        "an absent catalog must stay absent for older consumers: {rendered}"
    );

    for older in FIRST_SCHEMA_VERSION..FIRST_PERSONA_CATALOG_VERSION {
        let older = documented.replace(
            &format!("version: {SCHEMA_VERSION}"),
            &format!("version: {older}"),
        );
        let graph: GraphConfig = serde_norway::from_str(&older).expect("it still parses");
        let error = oneagentgraph::config::validate(&graph)
            .expect_err("the key postdates this schema version");
        assert!(error.to_string().contains("`personas`"), "{error}");
        assert!(
            error.to_string().contains(&format!(
                "requires graph schema version {FIRST_PERSONA_CATALOG_VERSION}"
            )),
            "{error}"
        );
    }
}

/// The document's rule for telling a catalog *name* from a path or URL is the
/// crate's own [`is_persona_name`], and every form the document names is decided
/// the way it says.
#[test]
fn the_documented_persona_names_are_the_ones_the_crate_looks_up() {
    for name in ["engineer", "crozier/crozier-corpus"] {
        assert!(
            oneagentgraph::persona::is_persona_name(name),
            "the document names {name:?} as a catalog name"
        );
    }
    for reference in [
        "./roles/lead.yaml",
        "./reporter.yaml",
        "https://example.com/engineer.yaml",
    ] {
        assert!(
            !oneagentgraph::persona::is_persona_name(reference),
            "{reference:?} is a ref, not a catalog name"
        );
    }
}

#[test]
fn the_readme_graph_uses_the_current_schema_version() {
    let expected = format!("```yaml\nversion: {SCHEMA_VERSION}\n");
    assert!(
        README.contains(&expected),
        "the README graph example must begin with {expected:?}"
    );
}

/// The contract documents `deps` on both member variants and both round-trip.
#[test]
fn the_documented_dependency_field_round_trips_on_both_member_kinds() {
    let document = fenced_block("yaml");
    assert_eq!(
        document.matches("deps: []").count(),
        2,
        "the documented graph must carry deps on both member variants"
    );
    let graph: GraphConfig =
        serde_norway::from_str(&document).expect("the documented graph parses");
    let Member::Onejudge(worker) = &graph.members["worker"] else {
        panic!("worker is onejudge")
    };
    assert!(worker.deps.is_empty());
    let Member::Oneharness(reporter) = &graph.members["reporter"] else {
        panic!("reporter is oneharness")
    };
    assert!(reporter.deps.is_empty());
    let round_trip = serde_norway::to_string(&graph).expect("graph serializes");
    assert!(
        !round_trip.contains("deps:"),
        "empty dependencies must remain omitted for older consumers: {round_trip}"
    );
    let reparsed: GraphConfig = serde_norway::from_str(&round_trip).expect("graph reparses");
    assert_eq!(reparsed, graph);
}

/// The contract documents a single-sided member's own `task` and `dir`, and a
/// member that carries neither serializes without either — so a graph document
/// written before they existed round-trips byte-identically.
///
/// The absence is the assertion that matters: these are optional fields on a
/// schema older consumers already read, and one that serialized as `task: null`
/// would be a document those consumers now reject.
#[test]
fn the_documented_member_job_fields_round_trip_and_stay_omitted_when_unset() {
    let document = fenced_block("yaml");
    for field in ["task: null", "dir: null"] {
        assert!(
            document.contains(field),
            "the documented reporter must show `{field}`"
        );
    }
    let graph: GraphConfig =
        serde_norway::from_str(&document).expect("the documented graph parses");
    let Member::Oneharness(reporter) = &graph.members["reporter"] else {
        panic!("reporter is oneharness")
    };
    assert_eq!(reporter.task, None);
    assert_eq!(reporter.dir, None);

    let round_trip = serde_norway::to_string(&graph).expect("graph serializes");
    assert!(
        !round_trip.contains("task:") && !round_trip.contains("dir:"),
        "a member with no job of its own must serialize without either field: {round_trip}"
    );
    let reparsed: GraphConfig = serde_norway::from_str(&round_trip).expect("graph reparses");
    assert_eq!(reparsed, graph);

    // And a member that *does* carry them keeps them across the round trip,
    // which is what a consumer writing a graph document depends on.
    let carried = document.replace(
        "    task: null                        # this member's own job; usually --task instead\n",
        "    task: send one status update\n",
    );
    let carried = carried.replace(
        "    dir: null                         # this member's own directory; default the run's \
         --dir\n",
        "    dir: ./api\n",
    );
    let graph: GraphConfig = serde_norway::from_str(&carried).expect("the carried graph parses");
    let Member::Oneharness(reporter) = &graph.members["reporter"] else {
        panic!("reporter is oneharness")
    };
    assert_eq!(reporter.task.as_deref(), Some("send one status update"));
    assert_eq!(reporter.dir.as_deref(), Some(std::path::Path::new("./api")));
    let reparsed: GraphConfig =
        serde_norway::from_str(&serde_norway::to_string(&graph).expect("serializes"))
            .expect("reparses");
    assert_eq!(reparsed, graph);
}

/// The contract documents `start_after`, a schedule naming none waits `every`
/// from the schema that has it, and one written before serializes without it.
///
/// The omission is the assertion that matters twice over: it is what every
/// schedule already in a consumer's repository is, and it must both keep its old
/// meaning under its old schema and round-trip byte-identically through this
/// crate.
#[test]
fn the_documented_start_after_defaults_to_every_and_stays_omitted_when_unset() {
    let document = fenced_block("yaml");
    assert!(
        document.contains("start_after: 1800"),
        "the documented schedule must show `start_after`"
    );
    let graph: GraphConfig =
        serde_norway::from_str(&document).expect("the documented graph parses");
    let Member::Oneharness(reporter) = &graph.members["reporter"] else {
        panic!("reporter is oneharness")
    };
    let documented = reporter.schedule.expect("the reporter is scheduled");
    assert_eq!(documented.start_after, Some(1800));
    assert_eq!(documented.first_turn_after(SCHEMA_VERSION), 1800);

    // A schedule naming none: one whole interval under this schema, and the
    // document round-trips without gaining a key an older consumer would reject.
    let unset = document.replace("start_after: 1800, ", "");
    let graph: GraphConfig = serde_norway::from_str(&unset).expect("the graph parses");
    let Member::Oneharness(reporter) = &graph.members["reporter"] else {
        panic!("reporter is oneharness")
    };
    let inherited = reporter.schedule.expect("the reporter is scheduled");
    assert_eq!(inherited.start_after, None);
    assert_eq!(inherited.first_turn_after(SCHEMA_VERSION), inherited.every);
    let round_trip = serde_norway::to_string(&graph).expect("graph serializes");
    assert!(
        !round_trip.contains("start_after"),
        "a schedule that named no start_after must serialize without one: {round_trip}"
    );
    let reparsed: GraphConfig = serde_norway::from_str(&round_trip).expect("graph reparses");
    assert_eq!(reparsed, graph);

    // The same schedule under every schema that predates the field takes its
    // first turn at t=0, which is what those documents have always done — and
    // naming the field there is refused rather than run under a delay nobody
    // asked for.
    for older in FIRST_SCHEMA_VERSION..FIRST_START_AFTER_VERSION {
        assert_eq!(inherited.first_turn_after(older), 0, "version {older}");
        let declared = unset.replacen(
            &format!("version: {SCHEMA_VERSION}"),
            &format!("version: {older}"),
            1,
        );
        let mut graph: GraphConfig =
            serde_norway::from_str(&declared).expect("an older graph parses");
        // The documented graph also carries an `events` block and a `personas`
        // catalog, both of which postdate every schema in this loop; dropping
        // them leaves the schedule as the one thing being read under the older
        // version.
        graph.events = None;
        graph.personas = None;
        oneagentgraph::config::validate(&graph)
            .unwrap_or_else(|err| panic!("version {older} must still validate: {err}"));
        let named = document.replacen(
            &format!("version: {SCHEMA_VERSION}"),
            &format!("version: {older}"),
            1,
        );
        let mut graph: GraphConfig = serde_norway::from_str(&named).expect("the graph parses");
        graph.events = None;
        graph.personas = None;
        let refused =
            oneagentgraph::config::validate(&graph).expect_err("start_after postdates this schema");
        assert!(
            refused.to_string().contains(&format!(
                "requires graph schema version {FIRST_START_AFTER_VERSION}"
            )),
            "version {older}: {refused}"
        );
    }

    // And `0`, the spelling that asks for the behaviour every schedule had.
    let at_once = document.replace("start_after: 1800", "start_after: 0");
    let graph: GraphConfig = serde_norway::from_str(&at_once).expect("the immediate graph parses");
    let Member::Oneharness(reporter) = &graph.members["reporter"] else {
        panic!("reporter is oneharness")
    };
    assert_eq!(
        reporter
            .schedule
            .expect("the reporter is scheduled")
            .first_turn_after(SCHEMA_VERSION),
        0
    );
}

/// The `member-started` payload the contract describes is the typed one this
/// build writes, for both runners and with `start_after` only where it belongs.
///
/// Asserted on the serialized event rather than on the struct: `runner` is the
/// field a consumer branches on, and the fields beside it are what that branch
/// then reads.
#[test]
fn a_member_started_payload_describes_its_runner_and_its_deferred_delay() {
    for named in [
        "runner: library|process",
        "engine",
        "config",
        "worktree",
        "program",
        "args",
        "cwd",
        "start_after",
    ] {
        assert!(
            CONTRACT.contains(&format!("`{named}`")),
            "the contract no longer names `{named}` on member-started"
        );
    }

    let library = MemberStarted {
        runner: Runner::Library {
            engine: "onejudge".to_string(),
            config: "/scratch/onejudge.yaml".to_string(),
            worktree: "/scratch".to_string(),
        },
        start_after: None,
    };
    let written = serde_json::to_value(&library).expect("serializes");
    assert_eq!(
        written,
        json!({
            "runner": "library",
            "engine": "onejudge",
            "config": "/scratch/onejudge.yaml",
            "worktree": "/scratch",
        }),
        "a member taking its turn now must publish no delay, and no field of the other runner"
    );
    assert_eq!(
        serde_json::from_value::<MemberStarted>(written).expect("reads back"),
        library
    );

    let deferred = MemberStarted {
        runner: Runner::Process {
            program: "oneharness".to_string(),
            args: vec![
                "run".to_string(),
                "--prompt".to_string(),
                "report".to_string(),
            ],
            cwd: "/work".to_string(),
        },
        start_after: Some(1800),
    };
    let written = serde_json::to_value(&deferred).expect("serializes");
    assert_eq!(
        written,
        json!({
            "runner": "process",
            "program": "oneharness",
            "args": ["run", "--prompt", "report"],
            "cwd": "/work",
            "start_after": 1800,
        })
    );
    assert_eq!(
        serde_json::from_value::<MemberStarted>(written).expect("reads back"),
        deferred
    );

    // A payload mixing the two runners is not a payload this build can read: a
    // consumer branching on `runner` must never meet a library member with an
    // argv.
    assert!(serde_json::from_value::<MemberStarted>(json!({
        "runner": "library",
        "engine": "onejudge",
        "config": "/scratch/onejudge.yaml",
        "worktree": "/scratch",
        "program": "oneharness",
    }))
    .is_err());
}

/// The contract documents `{task}` and its one escape, and both reach the member.
///
/// Asserted through the built invocation rather than through the expansion
/// function, because what the contract promises is what the member is *given*.
#[test]
fn the_documented_task_token_expands_into_a_members_own_task() {
    for spelling in ["`{task}`", "`{{task}}`"] {
        assert!(
            CONTRACT.contains(spelling),
            "the contract must document {spelling}"
        );
    }
    let workspace = tempfile::tempdir().expect("a workspace");
    std::fs::write(
        workspace.path().join("oneharness.toml"),
        "run_mode = \"fallback\"\nharnesses = [\"claude-code\"]\n",
    )
    .expect("a chain");
    let member: Member = serde_norway::from_str(concat!(
        "kind: oneharness\noneharness_config: ./oneharness.toml\n",
        "task: \"{task}\\n\\nand report it, using {{task}} to say so\"\n",
    ))
    .expect("a member");
    let scratch = workspace.path().join("scratch");
    let context = oneagentgraph::invoke::Context {
        dir: workspace.path(),
        scratch: &scratch,
        graph_dir: Some(workspace.path()),
        personas: None,
        task: Some("ship the release"),
        task_text: oneagentgraph::config::TaskText::under(SCHEMA_VERSION),
        session: "s",
        oneharness_bin: "oneharness",
    };
    let invocation = oneagentgraph::invoke::build(
        &member,
        &context,
        &mut oneagentgraph::resolve::Resolver::new(),
    )
    .expect("the member builds");
    let oneagentgraph::invoke::Launch::Harness(launch) = invocation.launch else {
        panic!("a single-sided member is driven through the oneharness library")
    };
    assert_eq!(
        launch.prompt,
        "ship the release\n\nand report it, using {task} to say so"
    );
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
        "interrupt",
        "history",
        "health",
        "smoke",
        "sweep",
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
        "--event-filter",
        "--output",
        "--detach",
        "--kill",
        "--dry-run",
        "--min-age-hours",
        "--input",
        "--input-file",
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

/// Where the contract names the path-valued keys a member's oneharness config
/// has anchored to its own directory.
const ANCHORED_PATHS_SENTENCE: &str = "The path-valued keys this applies to:";

/// The contract's list of anchored path keys and the crate's are one list.
///
/// The set is a contract in both directions: a key the document names and this
/// build does not anchor is a promise nothing keeps, and one this build anchors
/// without the document is a config value silently rewritten under its author.
/// Neither can be caught by the fenced-block tests above — the list is prose,
/// because it is a rule about a *neighbouring* tool's schema rather than a shape
/// of this crate's own — so it is checked here instead.
#[test]
fn the_documented_path_keys_are_the_ones_a_members_config_has_anchored() {
    let (_, rest) = CONTRACT
        .split_once(ANCHORED_PATHS_SENTENCE)
        .unwrap_or_else(|| panic!("docs/contract.md no longer says {ANCHORED_PATHS_SENTENCE:?}"));
    // The sentence alone: everything after it names what is *not* anchored.
    let sentence = rest.split_once(". ").map_or(rest, |(head, _)| head);
    let documented: BTreeSet<String> = backticked_in(sentence)
        .into_iter()
        // A key inside a table is named as its author writes it —
        // `[harness.<id>.variant.<name>] env_file` — and the key is the last word
        // of that.
        .filter_map(|token| token.split_whitespace().last().map(str::to_string))
        .collect();
    let anchored: BTreeSet<String> = oneagentgraph::invoke::ANCHORED_PATHS
        .iter()
        .copied()
        .chain(std::iter::once(
            oneagentgraph::invoke::ANCHORED_VARIANT_PATH,
        ))
        .map(str::to_string)
        .collect();
    assert_eq!(
        documented, anchored,
        "the contract and this build disagree about which paths in a member's \
         oneharness config are anchored to that config's own directory"
    );
}
