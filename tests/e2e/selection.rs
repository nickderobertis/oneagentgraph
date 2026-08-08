//! Per-side selection journeys, ported from ai-orchestrator's
//! `test_harness_side_selection_e2e.py` and `test_quota_fallthrough_e2e.py`.
//!
//! This crate owns **no** harness or model selection: the graph names an
//! oneharness config per side, and oneharness decides everything past that. What
//! these journeys hold is the seam — that each side really runs under the config
//! it was given, that a model override reaches only the side it was written on,
//! and that the pairing rule refuses a graph that could not degrade.
//!
//! Both halves are money hazards rather than style points. A side that quietly
//! inherited the other's selection runs on a subscription nobody chose; an
//! unpaired model reaches whichever candidate the chain settles on and kills the
//! member on a provider rejection instead of falling through.

// llmlint: ignore-file[e2e_not_mocked] see tests/e2e/support.rs: the paid harness
// process is the single sanctioned double. The wrapper scripts here are that same
// double under a second `ONEHARNESS_BIN_*` key, because a fall-through needs two
// candidates and neither may be a real subscription.

use crate::support::{
    fake_harness, two_party_graph, Workspace, CHAIN, FALLBACK_CHAIN, MIXED_CHAIN,
};

// llmlint: ignore-block[tests_mirror_real_usage] the assertions in this block read a
// file the doubled harness wrote recording the prompt, environment, or argv it was
// given, and that is the observation point *because* it is the subject: what
// arrived at the far end of the invocation this crate builds. Nothing a user reads
// carries it — the stream reports that a member settled, not what the harness was
// asked, and a run that delivered a mangled task or spent the wrong subscription
// exits 0 with a stream identical to a correct one. Asserting only on the stream
// would leave the whole delivery path unproven, which is the path these journeys
// were ported to cover; ai-orchestrator's originals record at the same point for
// the same reason. The recorder is the single sanctioned double, and every
// assertion about behaviour — exit codes, event kinds, the run record — is still
// made through the CLI.
/// A model named on one side is stamped into that side's own config, and reaches
/// the harness — while the other side keeps the model its own config pins.
///
/// Ported from `test_each_side_runs_the_model_it_was_given_on_one_shared_identity`
/// and `test_a_model_given_to_one_side_never_reaches_the_other`.
#[test]
fn a_model_reaches_only_the_side_it_was_written_on() {
    let workspace = Workspace::new();
    workspace.write(
        "oneharness.toml",
        &format!("{CHAIN}\n[harness.claude-code]\nmodel = \"agent-side-default\"\n"),
    );
    workspace.write(
        "oneharness.judge.toml",
        &format!("{CHAIN}\n[harness.claude-code]\nmodel = \"judge-side-default\"\n"),
    );
    workspace.graph(&two_party_graph(&fake_harness(), "").replace(
        "    agent:\n      oneharness_config: ./oneharness.toml\n",
        "    agent:\n      oneharness_config: ./oneharness.toml\n      model: agent-only-model\n",
    ));
    // Asserted at the provider boundary rather than in the generated config: the
    // argv is where a model actually reaches a harness, so this is what proves
    // the side it was written on is the side that got it.
    let argv = workspace.at("argv.txt");
    workspace
        .run_task(&format!(
            "complete-now: per-side model fake:record-argv={}",
            argv.display()
        ))
        .expect_code(0);

    let spawned = std::fs::read_to_string(&argv).expect("the harness recorded its argv");
    let lines: Vec<&str> = spawned.lines().collect();
    assert!(
        lines.len() >= 2,
        "not every side reached the double: {spawned}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("--model agent-only-model")),
        "the agent side never got its override: {spawned}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("--model judge-side-default")),
        "the judge side did not keep the model its own config pins: {spawned}"
    );
    assert!(
        !spawned.contains("agent-side-default"),
        "the override did not replace the pinned model: {spawned}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.contains("agent-only-model"))
            .count(),
        1,
        "the agent's model reached the judge: {spawned}"
    );
}

// llmlint: ignore-end[tests_mirror_real_usage]

/// The pairing rule: a model paired with a chain spanning two harness families
/// refuses the run before it starts, naming both families.
///
/// Ported from `test_a_model_the_pairing_rule_forbids_refuses_the_dispatch_before_it_starts`.
#[test]
fn a_model_on_a_chain_of_two_families_refuses_before_anything_starts() {
    let workspace = Workspace::new();
    workspace.write("oneharness.toml", MIXED_CHAIN);
    workspace.graph(&two_party_graph(&fake_harness(), "").replace(
        "    agent:\n      oneharness_config: ./oneharness.toml\n",
        "    agent:\n      oneharness_config: ./oneharness.toml\n      model: claude-opus-5\n",
    ));
    let run = workspace.run_task("complete-now: never gets here");
    run.expect_code(2);
    assert!(run.stderr.contains("claude-code, codex"), "{}", run.stderr);
    assert!(run.stderr.contains("one harness family"), "{}", run.stderr);
    assert!(
        run.stdout.is_empty(),
        "a refusal must not read as an event stream"
    );
}

// llmlint: ignore-block[tests_mirror_real_usage] the assertions in this block read a
// file the doubled harness wrote recording the prompt, environment, or argv it was
// given, and that is the observation point *because* it is the subject: what
// arrived at the far end of the invocation this crate builds. Nothing a user reads
// carries it — the stream reports that a member settled, not what the harness was
// asked, and a run that delivered a mangled task or spent the wrong subscription
// exits 0 with a stream identical to a correct one. Asserting only on the stream
// would leave the whole delivery path unproven, which is the path these journeys
// were ported to cover; ai-orchestrator's originals record at the same point for
// the same reason. The recorder is the single sanctioned double, and every
// assertion about behaviour — exit codes, event kinds, the run record — is still
// made through the CLI.
/// The model *value* is forwarded unchecked — the contract's deliberate
/// asymmetry with the identity, which only the config selects.
#[test]
fn the_model_value_itself_is_forwarded_unchecked() {
    let workspace = Workspace::new();
    workspace.graph(&two_party_graph(&fake_harness(), "").replace(
        "    agent:\n      oneharness_config: ./oneharness.toml\n",
        "    agent:\n      oneharness_config: ./oneharness.toml\n      model: no-such-model-anywhere\n",
    ));
    let argv = workspace.at("argv.txt");
    workspace
        .run_task(&format!(
            "complete-now: unchecked model fake:record-argv={}",
            argv.display()
        ))
        .expect_code(0);
    assert!(
        std::fs::read_to_string(&argv)
            .expect("argv")
            .contains("--model no-such-model-anywhere"),
        "a model this crate never checked did not reach the harness"
    );
}

/// The chain the member's own config declares is what selects the identity — a
/// process-wide `ONEHARNESS_HARNESSES` in the launching environment does not
/// move a side that was given its own config.
///
/// Ported from `test_each_side_runs_the_provider_it_was_given_over_a_process_wide_selection`.
// A POSIX shell stands in for a provider here, which is a platform
// capability like the others this suite gates on: the behaviour under test
// needs one *identity* to answer differently from another, and a script is
// how that is expressed without a second compiled binary per case.
#[cfg(unix)]
#[test]
fn each_side_runs_the_config_it_was_given() {
    let workspace = Workspace::new();
    let record = workspace.at("env.txt");

    // If the ambient selection *did* win, the run would resolve `codex` — so
    // that identity is bound to a script that refuses rather than left to reach
    // a real paid provider. The refusal is the safety net; the assertion is that
    // it never runs.
    let never = workspace.write(
        "never.sh",
        "#!/bin/sh\necho 'the ambient selection reached a side that had its own config' >&2\nexit 1\n",
    );
    make_executable(&never);

    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            &format!(
                "complete-now: selection fake:record-env={}",
                record.display()
            ),
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        // oneharness's own process-wide selection, exported by whatever launched
        // this run. Both sides still resolve their own config's chain, because
        // that is what the graph named.
        &[
            ("ONEHARNESS_HARNESSES", "codex"),
            ("ONEHARNESS_BIN_CODEX", &never.display().to_string()),
        ],
    );
    run.expect_code(0);
    // Every side reached the doubled `claude-code` candidate — if either had
    // resolved something else, the double would never have been spawned and the
    // record would be missing that side's line.
    let recorded = std::fs::read_to_string(&record).expect("env");
    assert!(
        recorded.lines().count() >= 2,
        "not every side reached the double: {recorded}"
    );
}

// llmlint: ignore-end[tests_mirror_real_usage]

/// A chain whose candidates all refuse on `auth` reports *every* step past as a
/// `fallback-advanced`, and then fails rather than settling.
///
/// The chain that reaches a working identity is the next journey down; this is
/// the other end of the same rule, and the end that once reported a member as
/// settled having run no turn at all.
///
/// Ported from `test_a_zero_work_subscription_429_hands_the_turn_to_the_next_identity`
/// and `test_agent_config_falls_back_to_codex_after_claude_auth_rejection`.
#[test]
fn a_chain_that_refuses_every_candidate_reports_each_one_and_fails() {
    let workspace = Workspace::new();
    workspace.write("oneharness.toml", FALLBACK_CHAIN);
    workspace.graph(&format!(
        concat!(
            "version: 1\nname: node-scope\n",
            "env:\n",
            // Both candidates are replaced, so the chain can reach no paid
            // provider by any path — including the fall-through under test.
            "  ONEHARNESS_BIN_CLAUDE_CODE: {fake}\n",
            "  ONEHARNESS_BIN_CODEX: {fake}\n",
            "  FAKE_HARNESS_REFUSAL: auth\n",
            "members:\n  reporter:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
        ),
        fake = fake_harness(),
    ));
    // Both candidates refuse, because one `FAKE_HARNESS_REFUSAL` reaches both —
    // which is the chain that reached nothing, and it must fail rather than
    // report a settled member.
    let run = workspace.run_task("complete-now: everything refuses");
    run.expect_code(1);

    // oneharness records *every* candidate it stepped past, including on a chain
    // that then reached nothing, so both are reported with their classification.
    // What must not happen is a settle: a chain that ran no turn has not done the
    // work, however many candidates it named on the way.
    let advanced = run.of_kind("fallback-advanced");
    let identities: Vec<&str> = advanced
        .iter()
        .filter_map(|event| event["payload"]["identity"].as_str())
        .collect();
    assert_eq!(identities, vec!["claude-code", "codex"], "{advanced:?}");
    for event in &advanced {
        assert_eq!(event["payload"]["reason"], serde_json::json!("auth"));
    }
    assert!(
        run.of_kind("member-settled").is_empty(),
        "a chain that ran no turn reported the member as settled: {:?}",
        run.kinds()
    );
    let died = run.of_kind("member-died");
    assert_eq!(died.len(), 1, "{:?}", run.kinds());
}

/// The other half of the same chain: when a later candidate *can* run, the step
/// past is reported with the identity and oneharness's own classification.
// A POSIX shell stands in for a provider here, which is a platform
// capability like the others this suite gates on: the behaviour under test
// needs one *identity* to answer differently from another, and a script is
// how that is expressed without a second compiled binary per case.
#[cfg(unix)]
#[test]
fn a_chain_that_reaches_a_working_identity_reports_the_step_past() {
    let workspace = Workspace::new();
    workspace.write("oneharness.toml", FALLBACK_CHAIN);
    let refusing = workspace.write(
        "refusing.sh",
        // The zero-work 429 a subscription out of quota really answers with: a
        // terminal record that reads as a success and declares the rejection
        // only through `terminal_reason` and an embedded `api_error_status`,
        // having spent nothing. The accounting is what oneharness classifies on,
        // which is why every counter in it is zero.
        &format!(
            "#!/bin/sh\nFAKE_HARNESS_REFUSAL=quota exec {} \"$@\"\n",
            fake_harness()
        ),
    );
    make_executable(&refusing);
    workspace.graph(&format!(
        concat!(
            "version: 1\nname: node-scope\n",
            "env:\n",
            "  ONEHARNESS_BIN_CLAUDE_CODE: {refusing}\n",
            "  ONEHARNESS_BIN_CODEX: {fake}\n",
            "members:\n  reporter:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
        ),
        refusing = refusing.display(),
        fake = fake_harness(),
    ));
    let run = workspace.run_task("complete-now: fall through to the next identity");
    run.expect_code(0);

    let advanced = run.of_kind("fallback-advanced");
    assert_eq!(advanced.len(), 1, "{:?}", run.kinds());
    assert_eq!(
        advanced[0]["payload"]["identity"],
        serde_json::json!("claude-code")
    );
    assert_eq!(advanced[0]["payload"]["reason"], serde_json::json!("quota"));
    assert!(!run.of_kind("member-settled").is_empty());
}

/// Mark a generated helper script executable, so oneharness can spawn it.
#[cfg(unix)]
fn make_executable(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    #[cfg(not(unix))]
    let _ = path;
}
