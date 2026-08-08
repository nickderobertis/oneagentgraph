//! The rest of the command surface, end to end.
//!
//! Ported from ai-orchestrator's `test_history_e2e.py`, `test_real_harness_smoke_e2e.py`,
//! `test_smoke_contention_e2e.py`, `test_environment_isolation_e2e.py`, and the
//! persona half of `test_dispatch_e2e.py`.

use crate::support::{fake_harness, two_party_graph, until, Workspace, CHAIN};

/// `validate` reads every ref the graph names, so a pass means the graph could
/// be launched — not merely that it parses.
#[test]
fn validate_reads_every_ref_the_graph_names() {
    let workspace = Workspace::new();
    workspace.run(&["validate", "./graph.yaml"]).expect_code(0);

    workspace.graph(
        &two_party_graph(&fake_harness(), "").replace("./oneharness.judge.toml", "./nowhere.toml"),
    );
    let run = workspace.run(&["validate", "./graph.yaml"]);
    run.expect_code(2);
    assert!(run.stderr.contains("nowhere.toml"), "{}", run.stderr);
}

/// A graph from another schema version, or one that could never run, is refused
/// by `validate` with the reason — not discovered at launch.
#[test]
fn validate_refuses_a_graph_that_could_never_run() {
    let workspace = Workspace::new();
    for (document, expected) in [
        ("version: 2\nname: g\nmembers: {}\n", "it reads version 1"),
        ("version: 1\nname: g\nmembers: {}\n", "has no members"),
        (
            concat!(
                "version: 1\nname: g\nmembers:\n  a:\n    kind: oneharness\n",
                "    oneharness_config: ./oneharness.toml\n    deps: [ghost]\n",
            ),
            "no member called",
        ),
        (
            concat!(
                "version: 1\nname: g\nmembers:\n  a:\n    kind: oneharness\n",
                "    oneharness_config: ./oneharness.toml\n    typo: 3\n",
            ),
            "typo",
        ),
    ] {
        workspace.graph(document);
        let run = workspace.run(&["validate", "./graph.yaml"]);
        run.expect_code(2);
        assert!(run.stderr.contains(expected), "{document}: {}", run.stderr);
    }
}

/// `--output text` is a deterministic rendering of the same events — same count,
/// same order, no separate content.
#[test]
fn text_output_renders_the_same_events_as_json() {
    let workspace = Workspace::new();
    let json = workspace.run_task("complete-now: render me");
    json.expect_code(0);

    let text = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "complete-now: render me",
        "--dir",
        &workspace.dir().display().to_string(),
        "--output",
        "text",
    ]);
    text.expect_code(0);

    let rendered: Vec<&str> = text
        .stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    for kind in [
        "graph-started",
        "member-started",
        "member-settled",
        "graph-settled",
    ] {
        assert!(
            rendered.iter().any(|line| line.contains(kind)),
            "the text rendering never carried {kind}: {rendered:?}"
        );
    }
    // Every line is one event: no blank lines, and each starts with its own
    // timestamp.
    for line in &rendered {
        assert!(
            line.starts_with("20"),
            "a text line is not an event: {line:?}"
        );
    }
}

/// `--detach` prints `{run_id, events_path, pid}` and exits 0, and the run it
/// left behind really does produce that stream.
#[test]
fn detach_prints_where_to_watch_the_run_it_left_behind() {
    let workspace = Workspace::new();
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "complete-now: detached",
        "--dir",
        &workspace.dir().display().to_string(),
        "--detach",
    ]);
    run.expect_code(0);

    let started: serde_json::Value =
        serde_json::from_str(run.stdout.trim()).unwrap_or_else(|err| {
            panic!(
                "--detach did not print one JSON object ({err}): {:?}",
                run.stdout
            )
        });
    let events = started["events_path"]
        .as_str()
        .expect("an events path")
        .to_string();
    assert!(started["run_id"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(started["pid"].as_u64().is_some_and(|pid| pid > 0));

    until("the detached run to settle", || {
        std::fs::read_to_string(&events).is_ok_and(|stream| stream.contains("\"graph-settled\""))
    });
    let stream = std::fs::read_to_string(&events).expect("the stream");
    for line in stream.lines().filter(|l| !l.trim().is_empty()) {
        serde_json::from_str::<serde_json::Value>(line).expect("every line is an envelope");
    }
}

/// `history` lists what ran, `history show` reads one record back, and a run
/// that is not there is named rather than reported as an empty list.
#[test]
fn history_lists_runs_and_shows_one_record() {
    let workspace = Workspace::new();
    workspace
        .run_task("complete-now: recorded once")
        .expect_code(0);

    let listed = workspace.run(&["history"]);
    listed.expect_code(0);
    let run_id = listed
        .stdout
        .lines()
        .next()
        .and_then(|line| line.split('\t').next())
        .expect("a listed run")
        .to_string();
    assert!(listed.stdout.contains("node-scope"), "{}", listed.stdout);

    let shown = workspace.run(&["history", "show", &run_id]);
    shown.expect_code(0);
    let record: serde_json::Value = serde_json::from_str(&shown.stdout).expect("a JSON record");
    assert_eq!(record["run_id"], serde_json::json!(run_id));
    assert_eq!(record["exit_code"], serde_json::json!(0));
    assert_eq!(record["members"]["worker"], serde_json::json!("settled"));

    let missing = workspace.run(&["history", "show", "no-such-run"]);
    missing.expect_code(2);
    assert!(missing.stderr.contains("no-such-run"), "{}", missing.stderr);
}

/// `health` forwards what oneharness knows about each identity, and says why
/// there is no answer when there is none.
#[test]
fn health_reads_oneharness_data_and_names_a_missing_binary() {
    let workspace = Workspace::new();
    let run = workspace.run(&["health"]);
    run.expect_code(0);
    serde_json::from_str::<serde_json::Value>(&run.stdout).expect("health answers JSON");

    let missing = workspace.run_with(
        &["health"],
        &[(
            "ONEAGENTGRAPH_ONEHARNESS_BIN",
            "oneharness-that-is-not-installed",
        )],
    );
    missing.expect_code(2);
    assert!(
        missing.stderr.contains("has to be on PATH"),
        "{}",
        missing.stderr
    );
}

/// `smoke` spends one turn through the real chain and names the identity that
/// ran it. A chain that reached nothing fails, naming each candidate.
///
/// Ported from `test_real_smoke_surfaces_timeout` and
/// `test_smoke_still_fails_when_every_launch_under_load_fails`.
#[test]
fn smoke_spends_one_turn_and_names_the_identity_that_ran_it() {
    let workspace = Workspace::new();
    let dir = workspace.at("smoke");
    std::fs::create_dir_all(&dir).expect("smoke dir");
    std::fs::write(dir.join("oneharness.toml"), CHAIN).expect("chain");

    let run = workspace.run_with(
        &["smoke", "--dir", &dir.display().to_string()],
        &[("ONEHARNESS_BIN_CLAUDE_CODE", &fake_harness())],
    );
    run.expect_code(0);
    assert!(
        run.stdout.contains("smoke: passed via claude-code"),
        "{}",
        run.stdout
    );

    let refused = workspace.run_with(
        &["smoke", "--dir", &dir.display().to_string()],
        &[
            ("ONEHARNESS_BIN_CLAUDE_CODE", &fake_harness()),
            ("FAKE_HARNESS_REFUSAL", "auth"),
        ],
    );
    refused.expect_code(1);
    assert!(
        refused.stderr.contains("claude-code [auth]"),
        "{}",
        refused.stderr
    );
}

/// A candidate that never ran the turn is the chain doing its job: `smoke` names
/// it on its own line, above the verdict, and still passes.
///
/// Ported from `test_smoke_survives_a_harness_that_refuses_a_launch_under_a_live_load`.
/// The `rate_limit` half of the same rule — a record carrying work the provider
/// already billed for, which a chain does **not** step past — is judged by
/// `smoke::judge` against a real report shape rather than here, because
/// oneharness stops the chain on one and so never produces a `fell_through`
/// entry a journey could construct.
#[test]
fn smoke_names_the_candidate_the_chain_stepped_past() {
    let workspace = Workspace::new();
    let dir = workspace.at("smoke");
    std::fs::create_dir_all(&dir).expect("smoke dir");
    std::fs::write(
        dir.join("oneharness.toml"),
        "run_mode = \"fallback\"\nharnesses = [\"claude-code\", \"codex\"]\n",
    )
    .expect("chain");
    let refusing = workspace.write(
        "refusing.sh",
        "#!/bin/sh\necho '401 Unauthorized: no credentials' >&2\nexit 1\n",
    );
    executable(&refusing);

    let run = workspace.run_with(
        &["smoke", "--dir", &dir.display().to_string()],
        &[
            (
                "ONEHARNESS_BIN_CLAUDE_CODE",
                &refusing.display().to_string(),
            ),
            ("ONEHARNESS_BIN_CODEX", &fake_harness()),
        ],
    );
    run.expect_code(0);
    assert!(
        run.stdout
            .contains("smoke: fell through claude-code (auth)"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("smoke: passed via codex"),
        "{}",
        run.stdout
    );
}

/// `persona new` scaffolds a document that validates once its two required keys
/// are filled in, and a name that would escape its catalog is refused before
/// anything is written.
///
/// Ported from `test_subdir_persona_scaffolding_and_recursive_validation_cli` and
/// `test_new_persona_cli_rejects_unsafe_names`.
#[test]
fn persona_new_scaffolds_and_refuses_a_name_that_escapes_its_catalog() {
    let workspace = Workspace::new();
    let created = workspace.run(&["persona", "new", "crozier/corpus"]);
    created.expect_code(0);
    assert!(
        workspace.at("crozier/corpus.yaml").exists(),
        "{}",
        created.stdout
    );

    // The scaffold is a real document: it validates as written.
    workspace
        .run(&["persona", "validate", "crozier/corpus.yaml"])
        .expect_code(0);

    for unsafe_name in ["../escape", "/absolute", "Engineer", "a b"] {
        let refused = workspace.run(&["persona", "new", unsafe_name]);
        refused.expect_code(2);
        assert!(
            refused.stderr.contains("invalid persona name"),
            "{unsafe_name}: {}",
            refused.stderr
        );
    }
    let again = workspace.run(&["persona", "new", "crozier/corpus"]);
    again.expect_code(2);
    assert!(again.stderr.contains("already exists"), "{}", again.stderr);
}

/// `persona validate` walks a catalog recursively, names the failing file, and
/// skips the `_`-prefixed template a catalog scaffolds from.
///
/// Ported from `test_recursive_validation_cli_reports_qualified_persona_name`.
#[test]
fn persona_validate_walks_a_catalog_and_names_the_failing_file() {
    let workspace = Workspace::new();
    workspace.write(
        "catalog/_template.yaml",
        "agent:\n  instructions: ''\nuser:\n  persona: ''\n",
    );
    workspace.write(
        "catalog/good.yaml",
        "agent:\n  instructions: role\nuser:\n  persona: lead\n",
    );
    workspace.write("catalog/nested/bad.yaml", "agent:\n  instructions: role\n");
    let run = workspace.run(&["persona", "validate", "catalog"]);
    run.expect_code(2);
    assert!(run.stderr.contains("nested/bad.yaml"), "{}", run.stderr);
    assert!(
        run.stderr.contains("user.persona is required"),
        "{}",
        run.stderr
    );
    // The template is scaffolding, not a persona, so it is never judged.
    assert!(!run.stderr.contains("_template.yaml"), "{}", run.stderr);

    std::fs::remove_file(workspace.at("catalog/nested/bad.yaml")).expect("remove");
    workspace
        .run(&["persona", "validate", "catalog"])
        .expect_code(0);
}

/// Every persona this crate ships validates through the same verb a user's own
/// file goes through — the contract's own acceptance criterion, driven at the
/// binary rather than asserted in a unit test.
#[test]
fn every_shipped_persona_validates_through_the_cli() {
    let workspace = Workspace::new();
    for (name, document) in oneagentgraph::persona::SHIPPED_PERSONAS {
        workspace.write(&format!("shipped/{name}.yaml"), document);
    }
    workspace
        .run(&["persona", "validate", "shipped"])
        .expect_code(0);
}

/// A shipped persona is reachable by name, with nothing to resolve — a graph can
/// say `persona: engineer` and get one.
#[test]
fn a_shipped_persona_is_reachable_by_name() {
    let workspace = Workspace::new();
    let record = workspace.at("prompts.txt");
    workspace.graph(
        &two_party_graph(&fake_harness(), "").replace("persona: engineer", "persona: reviewer"),
    );
    let run = workspace.run_task(&format!(
        "complete-now: shipped fake:record-prompt={}",
        record.display()
    ));
    run.expect_code(0);
    assert_eq!(
        crate::support::labels(&run.of_kind("member-started")[0])["persona"],
        "reviewer"
    );
    assert!(
        std::fs::read_to_string(&record)
            .expect("prompts")
            .contains("You specialize in review, not authoring."),
        "the shipped persona's role never reached the agent"
    );
}

/// A cron member fires again on `trigger`, and `reset-timer` restarts a
/// resettable clock. `cancel` is what ends it.
#[test]
fn a_cron_member_fires_on_trigger_and_stops_on_cancel() {
    let workspace = Workspace::new();
    workspace.graph(&format!(
        concat!(
            "version: 1\nname: node-scope\n",
            "env:\n  ONEHARNESS_BIN_CLAUDE_CODE: {fake}\n",
            "members:\n  reporter:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "    schedule: {{every: 3600, resettable: true}}\n",
        ),
        fake = fake_harness(),
    ));
    let state = workspace.state();
    let handle = {
        let dir = workspace.path().to_path_buf();
        let state = state.clone();
        std::thread::spawn(move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_oneagentgraph"))
                .args([
                    "run",
                    "./graph.yaml",
                    "--task",
                    "complete-now: scheduled",
                    "--dir",
                    &dir.join("work").display().to_string(),
                ])
                .current_dir(&dir)
                .env("ONEAGENTGRAPH_STATE_DIR", &state)
                .env("ONEAGENTGRAPH_ONEJUDGE_BIN", crate::support::onejudge_bin())
                .env(
                    "ONEAGENTGRAPH_ONEHARNESS_BIN",
                    crate::support::oneharness_bin(),
                )
                .env_remove("ONEHARNESS_HARNESSES")
                .output()
                .expect("the run finishes")
        })
    };

    until("the run to record itself", || run_id(&state).is_some());
    let id = run_id(&state).expect("a run");
    let events = state.join(&id).join("events.jsonl");
    until("the first settle", || {
        std::fs::read_to_string(&events).is_ok_and(|s| s.contains("member-settled"))
    });

    workspace
        .run(&["reset-timer", &id, "reporter"])
        .expect_code(0);
    until("the clock to restart", || {
        std::fs::read_to_string(&events).is_ok_and(|s| s.contains("cron-reset"))
    });

    workspace.run(&["trigger", &id, "reporter"]).expect_code(0);
    until("the scheduled member to fire", || {
        std::fs::read_to_string(&events).is_ok_and(|s| s.contains("cron-fired"))
    });

    workspace.run(&["cancel", &id]).expect_code(0);
    let output = handle.join().expect("the run thread");
    assert!(
        output.status.code().is_some(),
        "the cancelled run never exited"
    );

    let stream = std::fs::read_to_string(&events).expect("the stream");
    let fired = stream.matches("\"cron-fired\"").count();
    assert!(fired >= 1, "the trigger never fired the member: {stream}");
}

/// `trigger` and `reset-timer` name a member the run does not have rather than
/// leaving a signal nothing will ever read.
#[test]
fn a_signal_for_an_unknown_member_is_refused_by_name() {
    let workspace = Workspace::new();
    workspace.run_task("complete-now: signalled").expect_code(0);
    let id = run_id(&workspace.state()).expect("a run");

    let refused = workspace.run(&["trigger", &id, "ghost"]);
    refused.expect_code(2);
    assert!(
        refused.stderr.contains("no member \"ghost\""),
        "{}",
        refused.stderr
    );
    assert!(refused.stderr.contains("worker"), "{}", refused.stderr);

    let missing = workspace.run(&["reset-timer", "no-such-run", "worker"]);
    missing.expect_code(2);
    assert!(missing.stderr.contains("no-such-run"), "{}", missing.stderr);
}

/// The one run a state directory holds, once one has recorded itself.
fn run_id(state: &std::path::Path) -> Option<String> {
    std::fs::read_dir(state)
        .ok()?
        .flatten()
        .find(|entry| entry.path().join("record.json").exists())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
}

/// Mark a generated helper script executable, so oneharness can spawn it.
fn executable(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    #[cfg(not(unix))]
    let _ = path;
}
