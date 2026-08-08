//! Dispatch journeys, ported from `ai-orchestrator/tests/e2e/test_dispatch_e2e.py`.
//!
//! Each drives the real `oneagentgraph` binary against a real graph, through the
//! real `onejudge` loop and the real `oneharness` selection, with only the paid
//! harness process replaced at oneharness's own seam. What is asserted is the
//! stream: the contract says exit 1 means "a member failed or died (the stream
//! says which and why)", so a journey that only checked the code would leave the
//! part a supervisor reads unproven.

// llmlint: ignore-file[e2e_not_mocked] see tests/e2e/support.rs: the paid harness
// process is the single sanctioned double, replaced at oneharness's own
// `ONEHARNESS_BIN_<ID>` seam, with real onejudge and real oneharness in between.

use crate::support::{
    as_env, bounds, fake_harness, fake_provider, labels, two_party_graph, Workspace, BASE, CHAIN,
};

/// The whole happy path: a two-party member completes, and the stream carries
/// the lifecycle a consumer renders.
///
/// Ported from `test_dispatch_completes_via_supervisor_loop` and
/// `test_dispatch_complete_now_single_turn`.
#[test]
fn a_member_completes_through_the_real_supervisor_loop() {
    let workspace = Workspace::new();
    let run = workspace.run_task("complete-now: write the thing");
    run.expect_code(0);

    let kinds = run.kinds();
    for expected in [
        "graph-started",
        "member-started",
        "turn-started",
        "turn-activity",
        "turn-completed",
        "member-settled",
        "graph-settled",
    ] {
        assert!(
            kinds.iter().any(|kind| kind == expected),
            "the stream never carried {expected}: {kinds:?}"
        );
    }
    assert_eq!(kinds.first().map(String::as_str), Some("graph-started"));
    assert_eq!(kinds.last().map(String::as_str), Some("graph-settled"));

    let settled = run.of_kind("member-settled");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["payload"]["completed"], serde_json::json!(true));
    // The full report is an artifact and the verdict is inline — the contract's
    // own split between what a stream carries and what it references.
    assert_eq!(
        settled[0]["artifacts"][0]["kind"],
        serde_json::json!("report")
    );
    assert!(settled[0]["artifacts"][0]["bytes"].as_u64().unwrap_or(0) > 0);
    assert!(settled[0]["payload"]["verdict"].is_array());
}

/// A member that never reaches its bar settles incomplete, and the run exits 1
/// with the stream saying which member and why.
///
/// Ported from `test_dispatch_hits_turn_cap_when_never_done` and
/// `test_run_onejudge_returns_incomplete_report_for_exit_one`.
#[test]
fn a_member_that_never_completes_exits_one_and_the_stream_says_which() {
    let workspace = Workspace::new();
    let run = workspace.run_task("should-fail: never reach the bar");
    run.expect_code(1);

    let settled = run.of_kind("member-settled");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["payload"]["completed"], serde_json::json!(false));
    assert_eq!(labels(&settled[0])["member"], "worker");
    assert_eq!(
        workspace.record()["members"]["worker"],
        serde_json::json!("incomplete")
    );
    assert_eq!(workspace.record()["exit_code"], serde_json::json!(1));
}

/// Every event carries the member and persona it came from, and the run id every
/// event in the run shares — the reserved labels a consumer joins on.
///
/// Ported from the label half of `test_real_dispatch_delivers_exact_task_to_agent_history`.
#[test]
fn every_event_carries_the_labels_a_consumer_joins_on() {
    let workspace = Workspace::new();
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "complete-now: label me",
        "--dir",
        &workspace.dir().display().to_string(),
        "--label",
        "round=2",
        "--label",
        "node=service",
    ]);
    run.expect_code(0);

    let events = run.events();
    let run_id = labels(&events[0])["run_id"].clone();
    assert!(!run_id.is_empty());
    for event in &events {
        let stamped = labels(event);
        assert_eq!(stamped["run_id"], run_id, "{event}");
        assert_eq!(stamped["round"], "2", "{event}");
        assert_eq!(stamped["node"], "service", "{event}");
        assert_eq!(event["v"], serde_json::json!(1));
        assert_eq!(event["source"], serde_json::json!("agentgraph"));
    }
    let member_events: Vec<_> = events
        .iter()
        .filter(|event| labels(event).contains_key("member"))
        .collect();
    assert!(!member_events.is_empty());
    for event in member_events {
        assert_eq!(labels(event)["member"], "worker");
        assert_eq!(labels(event)["persona"], "engineer", "{event}");
    }
}

/// `seq` is monotonic per stream with no gaps, which is how a consumer detects
/// loss.
#[test]
fn seq_is_monotonic_with_no_gaps() {
    let workspace = Workspace::new();
    let run = workspace.run_task("complete-now: number me");
    run.expect_code(0);

    let events = run.events();
    let stream = events[0]["stream"].clone();
    for (expected, event) in events.iter().enumerate() {
        assert_eq!(event["seq"], serde_json::json!(expected as u64), "{event}");
        assert_eq!(event["stream"], stream, "{event}");
    }
}

/// The task reaches the agent side exactly as it was given — including the
/// metacharacters a shell would have eaten.
///
/// Ported from `test_real_dispatch_delivers_exact_task_to_agent_history` and
/// `test_just_run_plan_preserves_metacharacter_laden_arguments`.
#[test]
fn the_exact_task_reaches_the_agent_side() {
    let workspace = Workspace::new();
    let record = workspace.at("prompts.txt");
    let task = format!(
        "complete-now: $(touch /tmp/pwned) `id` \"quoted\" 'single' | & ; \\ \
         fake:record-prompt={}",
        record.display()
    );
    workspace.run_task(&task).expect_code(0);

    let delivered = std::fs::read_to_string(&record).expect("the agent recorded its prompt");
    assert!(
        delivered.contains("$(touch /tmp/pwned) `id` \"quoted\" 'single' | & ;"),
        "the task was mangled on its way to the agent: {delivered}"
    );
    assert!(
        !std::path::Path::new("/tmp/pwned").exists(),
        "the task was evaluated as a shell"
    );
}

/// The persona's role is appended after the base's shared preamble, and both
/// reach the agent — the merge the contract's `persona` field buys.
///
/// Ported from `test_dispatch_subdir_qualified_persona_via_real_onejudge`.
#[test]
fn the_base_preamble_and_the_persona_role_both_reach_the_agent() {
    let workspace = Workspace::new();
    workspace.write(
        "roles/lead.yaml",
        concat!(
            "agent:\n  name: lead\n  instructions: |\n    Role marker: you lead.\n",
            "user:\n  persona: |\n    Supervisor marker: push hard.\n",
        ),
    );
    workspace.graph(
        &two_party_graph(&fake_harness(), "")
            .replace("persona: engineer", "persona: ./roles/lead.yaml"),
    );

    let record = workspace.at("prompts.txt");
    let run = workspace.run_task(&format!(
        "complete-now: merged fake:record-prompt={}",
        record.display()
    ));
    run.expect_code(0);

    let delivered = std::fs::read_to_string(&record).expect("prompts");
    assert!(
        delivered.contains("Standing bar: verify before you claim done."),
        "{delivered}"
    );
    assert!(delivered.contains("Role marker: you lead."), "{delivered}");
    assert!(
        delivered.contains("Supervisor marker: push hard."),
        "{delivered}"
    );
    // The persona's own name is what the events are labelled with.
    assert_eq!(labels(&run.of_kind("member-started")[0])["persona"], "lead");
}

/// A graph's `env` reaches every member process, `${VAR}` expanded — and that is
/// the seam the whole suite reaches the double through, so it is proven rather
/// than assumed.
///
/// Ported from `test_dispatch_forwards_validated_environment_to_real_provider`.
#[test]
fn the_graphs_env_reaches_the_member_process_expanded() {
    let workspace = Workspace::new();
    let marker = workspace.at("marker");
    workspace.graph(&two_party_graph(
        &fake_harness(),
        "  ONEAGENTGRAPH_TEST_MARKER: ${E2E_SOURCE}/leaf\n",
    ));
    let record = workspace.at("env.txt");
    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            &format!("complete-now: env fake:record-env={}", record.display()),
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[("E2E_SOURCE", &marker.display().to_string())],
    );
    run.expect_code(0);

    let recorded = std::fs::read_to_string(&record).expect("env");
    let first: serde_json::Value =
        serde_json::from_str(recorded.lines().next().expect("a line")).expect("json");
    assert_eq!(
        first["ONEAGENTGRAPH_TEST_MARKER"],
        serde_json::json!(format!("{}/leaf", marker.display())),
        "the graph's env did not reach the member expanded: {recorded}"
    );
}

/// A member's `mode` reaches both sides as the approval posture, because
/// onejudge's own config schema has no key for it.
#[test]
fn the_members_mode_reaches_the_harness_process() {
    let workspace = Workspace::new();
    let record = workspace.at("env.txt");
    workspace
        .graph(&two_party_graph(&fake_harness(), "").replace("mode: bypass", "mode: read-only"));
    workspace
        .run_task(&format!(
            "complete-now: mode fake:record-env={}",
            record.display()
        ))
        .expect_code(0);

    let recorded = std::fs::read_to_string(&record).expect("env");
    for line in recorded.lines() {
        let seen: serde_json::Value = serde_json::from_str(line).expect("json");
        assert_eq!(
            seen["ONEHARNESS_MODE"],
            serde_json::json!("read-only"),
            "{line}"
        );
    }
}

/// A command judge runs the whole member through onejudge's `split` provider:
/// the agent side stays on the real harness path, the supervisor is the command.
///
/// This is the contract's `judge: {command: [...]}` alternative.
#[test]
fn a_command_judge_supervises_through_the_split_provider() {
    let workspace = Workspace::new();
    workspace.graph(&format!(
        concat!(
            "version: 1\nname: node-scope\n",
            "env:\n  ONEHARNESS_BIN_CLAUDE_CODE: {fake}\n",
            "members:\n  worker:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      command: [\"{provider}\"]\n",
            "    mode: bypass\n",
        ),
        fake = fake_harness(),
        provider = fake_provider(),
    ));
    // A base carrying evals and an assessment reaches every operation the
    // protocol has, so the whole command-provider surface is driven rather than
    // just the completion decision.
    workspace.write(
        "base.yaml",
        concat!(
            "provider:\n  kind: oneharness\n",
            "agent:\n  instructions: |\n    Standing bar: verify before you claim done.\n",
            "user:\n  done_when: \"the task is complete\"\n  max_turns: 4\n",
            "evals:\n",
            "  - criterion: \"the change is well-scoped\"\n    kind: numeric\n    scale: [1, 5]\n",
            "assessment: \"Name the follow-up work this run left out of scope.\"\n",
        ),
    );
    let run = workspace.run_task("complete-now: judged by a command");
    run.expect_code(0);
    assert_eq!(
        run.of_kind("member-settled")[0]["payload"]["completed"],
        serde_json::json!(true)
    );

    // And the other half of the same supervisor: a member that never reaches its
    // bar is asked for another turn until the cap, then settles incomplete.
    let incomplete = workspace.run_task("should-fail: judged by a command");
    incomplete.expect_code(1);
    assert_eq!(
        incomplete.of_kind("member-settled")[0]["payload"]["completed"],
        serde_json::json!(false)
    );
}

/// A run with no task at all refuses before it launches anything, rather than
/// spending a turn on a member with nothing to do.
#[test]
fn a_member_with_no_task_refuses_before_it_launches() {
    let workspace = Workspace::new();
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(2);
    assert!(run.stderr.contains("no task"), "{}", run.stderr);
    assert!(
        run.stdout.is_empty(),
        "a refusal must not read as an event stream"
    );
}

/// A persona that does not satisfy the delta contract refuses the run, naming
/// what is wrong — before a paid turn is spent on it.
///
/// Ported from `test_dispatch_unknown_persona_raises` and
/// `test_dispatch_rejects_unsafe_persona_names`.
#[test]
fn an_unusable_persona_refuses_the_run_before_anything_starts() {
    let workspace = Workspace::new();
    workspace.write(
        "roles/empty.yaml",
        "agent:\n  instructions: '  '\nuser:\n  persona: ''\n",
    );
    let cases = [
        ("./roles/empty.yaml", "agent.instructions is required"),
        ("./roles/nowhere.yaml", "cannot read"),
        ("./roles/typo.yaml", "unknown field"),
    ];
    workspace.write("roles/typo.yaml", "agent:\n  instrucions: typo\n");
    for (reference, expected) in cases {
        workspace.graph(
            &two_party_graph(&fake_harness(), "")
                .replace("persona: engineer", &format!("persona: {reference}")),
        );
        let run = workspace.run_task("complete-now: never gets here");
        run.expect_code(2);
        assert!(run.stderr.contains(expected), "{reference}: {}", run.stderr);
    }
}

/// A graph whose base config merges to something incomplete refuses, naming the
/// field the base never supplied.
#[test]
fn an_incomplete_base_config_refuses_the_run() {
    let workspace = Workspace::new();
    workspace.write("base.yaml", "provider:\n  kind: oneharness\n");
    let run = workspace.run_task("complete-now: incomplete base");
    run.expect_code(2);
    assert!(run.stderr.contains("user.done_when"), "{}", run.stderr);
}

/// `--set` reaches the member's own field, and a path naming nothing refuses
/// rather than running a member on a setting nobody applied.
///
/// Ported from `test_dispatch_provider_override`.
#[test]
fn a_set_override_reaches_the_member_and_a_bad_one_refuses() {
    let workspace = Workspace::new();
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "complete-now: overridden",
        "--dir",
        &workspace.dir().display().to_string(),
        "--set",
        "members.worker.mode=read-only",
    ]);
    run.expect_code(0);

    let refused = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "complete-now: never",
        "--dir",
        &workspace.dir().display().to_string(),
        "--set",
        "members.ghost.mode=read-only",
    ]);
    refused.expect_code(2);
    assert!(refused.stderr.contains("no ghost"), "{}", refused.stderr);

    // A `--set` value arrives as text, but the field it lands on has a type in
    // the graph document. Overriding a number with a quoted string would change
    // the document's shape rather than its value, and the schema would then
    // refuse a graph the caller thought they had only retuned. So the graph here
    // spells the two typed fields out, which the default one leaves unset.
    workspace.graph(&format!(
        concat!(
            "version: 1\nname: node-scope\n",
            "env:\n  ONEHARNESS_BIN_CLAUDE_CODE: {fake}\n",
            "members:\n  worker:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    max_turns: 4\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n      stream: true\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n",
        ),
        fake = fake_harness(),
    ));
    let numeric = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "complete-now: retuned",
        "--dir",
        &workspace.dir().display().to_string(),
        "--set",
        "members.worker.max_turns=3",
    ]);
    numeric.expect_code(0);

    for (assignment, expected) in [
        ("members.worker.max_turns=soon", "not a number"),
        ("members.worker.agent.stream=maybe", "not a boolean"),
    ] {
        let mistyped = workspace.run(&[
            "run",
            "./graph.yaml",
            "--task",
            "complete-now: never",
            "--dir",
            &workspace.dir().display().to_string(),
            "--set",
            assignment,
        ]);
        mistyped.expect_code(2);
        assert!(
            mistyped.stderr.contains(expected),
            "{assignment}: {}",
            mistyped.stderr
        );
    }
}

/// `--task-file` is the other way in, and naming both is a refusal rather than a
/// silent preference.
#[test]
fn the_task_arrives_by_file_and_naming_both_ways_refuses() {
    let workspace = Workspace::new();
    let task = workspace.write("task.md", "complete-now: from a file\n");
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task-file",
        &task.display().to_string(),
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(0);

    let both = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "one",
        "--task-file",
        &task.display().to_string(),
    ]);
    both.expect_code(2);
    assert!(both.stderr.contains("exactly one"), "{}", both.stderr);
}

/// `deps` decides the order: a dependant's first event follows its dependency's
/// settle, in the one merged stream.
#[test]
fn a_dependant_member_starts_only_after_its_dependency_settles() {
    let workspace = Workspace::new();
    workspace.graph(&format!(
        concat!(
            "version: 1\nname: node-scope\n",
            "env:\n  ONEHARNESS_BIN_CLAUDE_CODE: {fake}\n",
            "members:\n",
            "  build:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "  report:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    deps: [build]\n",
        ),
        fake = fake_harness(),
    ));
    let run = workspace.run_task("complete-now: ordered");
    run.expect_code(0);

    let events = run.events();
    let position = |member: &str, kind: &str| {
        events
            .iter()
            .position(|event| {
                event["kind"] == kind && labels(event).get("member").is_some_and(|m| m == member)
            })
            .unwrap_or_else(|| panic!("no {kind} for {member}"))
    };
    assert!(
        position("build", "member-settled") < position("report", "member-started"),
        "the dependant started before its dependency settled"
    );
}

/// A run under a base config the member cannot even read refuses with the path,
/// not a parse trace.
#[test]
fn an_unreadable_ref_refuses_with_the_path_it_could_not_read() {
    let workspace = Workspace::new();
    workspace.graph(&two_party_graph(&fake_harness(), "").replace("./base.yaml", "./nowhere.yaml"));
    let run = workspace.run_task("complete-now: unreadable");
    run.expect_code(2);
    assert!(run.stderr.contains("nowhere.yaml"), "{}", run.stderr);
}

/// A member whose `onejudge` binary is not there dies as `unstartable`, with the
/// stream saying so — rather than the run reporting a settled graph.
#[test]
fn a_member_that_cannot_start_is_a_death_the_stream_names() {
    let workspace = Workspace::new();
    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "complete-now: unstartable",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[(
            "ONEAGENTGRAPH_ONEJUDGE_BIN",
            "onejudge-that-is-not-installed",
        )],
    );
    run.expect_code(1);
    let died = run.of_kind("member-died");
    assert_eq!(died.len(), 1, "{died:?}");
    assert_eq!(died[0]["payload"]["rule"], serde_json::json!("unstartable"));
    assert!(
        died[0]["payload"]["stderr_tail"]
            .as_str()
            .unwrap_or_default()
            .contains("onejudge-that-is-not-installed"),
        "{died:?}"
    );
}

/// The base and the persona are recorded content-addressed, so a replay is
/// checked against what was read rather than against a path or a URL.
#[test]
fn every_config_a_run_read_is_recorded_content_addressed() {
    let workspace = Workspace::new();
    workspace.run_task("complete-now: recorded").expect_code(0);

    let record = workspace.record();
    let refs = record["refs"].as_array().expect("refs").clone();
    let origins: Vec<&str> = refs.iter().filter_map(|r| r["origin"].as_str()).collect();
    for expected in [
        "./graph.yaml",
        "./base.yaml",
        "./oneharness.toml",
        "./oneharness.judge.toml",
    ] {
        assert!(
            origins.contains(&expected),
            "{origins:?} is missing {expected}"
        );
    }
    for reference in &refs {
        let digest = reference["sha256"].as_str().expect("a digest");
        assert_eq!(digest.len(), 64, "{reference}");
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()), "{reference}");
    }
    // The two sides name the same file, and content addressing is what makes
    // that one entry rather than two that merely look alike.
    assert_eq!(
        refs.iter()
            .filter(|r| r["origin"] == "./oneharness.toml")
            .count(),
        1
    );
}

/// A `kind: oneharness` member is single-sided: one agent, no judge, and the
/// same stream shape.
#[test]
fn a_single_sided_member_runs_one_agent_with_no_judge() {
    let workspace = Workspace::new();
    workspace.write("oneharness.toml", CHAIN);
    workspace.graph(&format!(
        concat!(
            "version: 1\nname: node-scope\n",
            "env:\n  ONEHARNESS_BIN_CLAUDE_CODE: {fake}\n",
            "members:\n  reporter:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
        ),
        fake = fake_harness(),
    ));
    let run = workspace.run_task("complete-now: single sided");
    run.expect_code(0);
    assert_eq!(
        labels(&run.of_kind("member-settled")[0])["member"],
        "reporter"
    );
    assert!(
        !run.of_kind("turn-activity").is_empty(),
        "{:?}",
        run.kinds()
    );
}

/// The base every journey merges onto is the one the suite asserts against, so a
/// change to it is a change to what these journeys prove.
#[test]
fn the_shared_base_is_the_one_these_journeys_assert_against() {
    assert!(BASE.contains("Standing bar: verify before you claim done."));
    assert!(!bounds("1", "1").is_empty());
    assert!(!as_env(&bounds("1", "1")).is_empty());
}
