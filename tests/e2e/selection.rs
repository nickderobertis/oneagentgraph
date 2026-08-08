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

use crate::support::{
    fake_harness, two_party_graph, Workspace, CHAIN, FALLBACK_CHAIN, MIXED_CHAIN,
};

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
    workspace
        .run_task("complete-now: per-side model")
        .expect_code(0);

    let members = workspace.state();
    let agent = read_side(&members, "oneharness.toml");
    let judge = read_side(&members, "oneharness.judge.toml");
    assert!(
        agent.contains("agent-only-model"),
        "the agent side kept its config's model: {agent}"
    );
    assert!(
        !agent.contains("agent-side-default"),
        "the override did not replace the pinned model: {agent}"
    );
    assert!(
        judge.contains("judge-side-default") && !judge.contains("agent-only-model"),
        "the agent's model reached the judge: {judge}"
    );
}

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

/// The model *value* is forwarded unchecked — the contract's deliberate
/// asymmetry with the identity, which only the config selects.
#[test]
fn the_model_value_itself_is_forwarded_unchecked() {
    let workspace = Workspace::new();
    workspace.graph(&two_party_graph(&fake_harness(), "").replace(
        "    agent:\n      oneharness_config: ./oneharness.toml\n",
        "    agent:\n      oneharness_config: ./oneharness.toml\n      model: no-such-model-anywhere\n",
    ));
    workspace
        .run_task("complete-now: unchecked model")
        .expect_code(0);
    assert!(read_side(&workspace.state(), "oneharness.toml").contains("no-such-model-anywhere"));
}

/// The chain the member's own config declares is what selects the identity — a
/// process-wide `ONEHARNESS_HARNESSES` in the launching environment does not
/// move a side that was given its own config.
///
/// Ported from `test_each_side_runs_the_provider_it_was_given_over_a_process_wide_selection`.
#[test]
fn each_side_runs_the_config_it_was_given() {
    let workspace = Workspace::new();
    let record = workspace.at("env.txt");
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
        &[("ONEAGENTGRAPH_TEST_AMBIENT", "set")],
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

/// A chain whose first candidate refuses on `auth` hands the turn to the next
/// identity, and the run reports each step past as a `fallback-advanced`.
///
/// Ported from `test_a_zero_work_subscription_429_hands_the_turn_to_the_next_identity`
/// and `test_agent_config_falls_back_to_codex_after_claude_auth_rejection`.
#[test]
fn a_refused_candidate_hands_the_turn_on_and_the_stream_says_so() {
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
#[test]
fn a_chain_that_reaches_a_working_identity_reports_the_step_past() {
    let workspace = Workspace::new();
    workspace.write("oneharness.toml", FALLBACK_CHAIN);
    let refusing = workspace.write(
        "refusing.sh",
        "#!/bin/sh\necho '401 Unauthorized: no credentials' >&2\nexit 1\n",
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
    assert_eq!(advanced[0]["payload"]["reason"], serde_json::json!("auth"));
    assert!(!run.of_kind("member-settled").is_empty());
}

/// One side's resolved config, as the run wrote it into that member's scratch.
fn read_side(state: &std::path::Path, name: &str) -> String {
    let members = std::fs::read_dir(state)
        .expect("state")
        .flatten()
        .map(|entry| entry.path().join("members").join("worker"))
        .find(|path| path.exists())
        .expect("the member's scratch");
    std::fs::read_to_string(members.join(name)).unwrap_or_default()
}

/// Mark a generated helper script executable, so oneharness can spawn it.
fn make_executable(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    #[cfg(not(unix))]
    let _ = path;
}
