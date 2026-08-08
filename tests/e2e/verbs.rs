//! The rest of the command surface, end to end.
//!
//! Ported from ai-orchestrator's `test_history_e2e.py`, `test_real_harness_smoke_e2e.py`,
//! `test_smoke_contention_e2e.py`, `test_environment_isolation_e2e.py`, and the
//! persona half of `test_dispatch_e2e.py`.

// llmlint: ignore-file[e2e_not_mocked] see tests/e2e/support.rs: the paid harness
// process is the single sanctioned double, and the wrapper scripts here are that
// same double reached under a second `ONEHARNESS_BIN_*` key.

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
        (
            "version: 1\nname: ' '\nmembers: {}\n",
            "a graph needs a name",
        ),
        // A member's name becomes a directory this run creates and a signal file
        // an operator writes, so one that would leave the run's own directory is
        // refused before anything is created.
        (
            concat!(
                "version: 1\nname: g\nmembers:\n  \"../escape\":\n    kind: oneharness\n",
                "    oneharness_config: ./oneharness.toml\n",
            ),
            "member name",
        ),
        // An `env:` key is exported to every member, and one the platform cannot
        // name is not a variable at all.
        (
            concat!(
                "version: 1\nname: g\nenv:\n  \"A=B\": v\n",
                "members:\n  a:\n    kind: oneharness\n",
                "    oneharness_config: ./oneharness.toml\n",
            ),
            "env key",
        ),
        (
            concat!(
                "version: 1\nname: g\nenv:\n  \"\": v\n",
                "members:\n  a:\n    kind: oneharness\n",
                "    oneharness_config: ./oneharness.toml\n",
            ),
            "env key",
        ),
        (
            concat!(
                "version: 1\nname: g\nmembers:\n  a:\n    kind: oneharness\n",
                "    oneharness_config: ./oneharness.toml\n    schedule: {every: 0}\n",
            ),
            "never stops firing",
        ),
        (
            concat!(
                "version: 1\nname: g\nmembers:\n  w:\n    kind: onejudge\n",
                "    base_config: ./base.yaml\n    mode: ' '\n",
                "    agent: {oneharness_config: ./oneharness.toml}\n",
                "    judge: {oneharness_config: ./oneharness.judge.toml}\n",
            ),
            "names none",
        ),
        (
            concat!(
                "version: 1\nname: g\nmembers:\n  w:\n    kind: onejudge\n",
                "    base_config: ./base.yaml\n    mode: bypass\n    max_turns: 0\n",
                "    agent: {oneharness_config: ./oneharness.toml}\n",
                "    judge: {oneharness_config: ./oneharness.judge.toml}\n",
            ),
            "no turn at all",
        ),
        (
            concat!(
                "version: 1\nname: g\nmembers:\n  w:\n    kind: onejudge\n",
                "    base_config: ./base.yaml\n    mode: bypass\n",
                "    agent: {oneharness_config: ./oneharness.toml}\n",
                "    judge: {command: []}\n",
            ),
            "needs a command to run",
        ),
    ] {
        workspace.graph(document);
        let run = workspace.run(&["validate", "./graph.yaml"]);
        run.expect_code(2);
        assert!(run.stderr.contains(expected), "{document}: {}", run.stderr);
    }
}

/// A graph, or a ref inside one, may be an `https` URL. A host that cannot be
/// reached is a refusal naming the URL — not a run that started against a
/// document nobody read.
///
/// The URL is under `.invalid`, which by RFC 6761 resolves nowhere, so this
/// journey drives the real fetch path without leaving the machine.
#[test]
fn an_https_ref_is_fetched_and_an_unreachable_one_is_refused_by_url() {
    let workspace = Workspace::new();
    let unreachable = "https://oneagentgraph.invalid/graph.yaml";

    let graph = workspace.run(&["validate", unreachable]);
    graph.expect_code(2);
    assert!(graph.stderr.contains(unreachable), "{}", graph.stderr);
    assert!(graph.stderr.contains("cannot fetch"), "{}", graph.stderr);

    // And the same for a ref *inside* a graph this build can read, so the
    // refusal is the fetch rather than the graph.
    workspace.graph(
        &two_party_graph(&fake_harness(), "")
            .replace("./base.yaml", "https://oneagentgraph.invalid/base.yaml"),
    );
    let inner = workspace.run(&["validate", "./graph.yaml"]);
    inner.expect_code(2);
    assert!(
        inner.stderr.contains("oneagentgraph.invalid/base.yaml"),
        "{}",
        inner.stderr
    );

    // `http` is refused before a byte leaves: a config fetched in the clear is
    // one an intermediary chooses.
    let cleartext = workspace.run(&["validate", "http://oneagentgraph.invalid/graph.yaml"]);
    cleartext.expect_code(2);
    assert!(
        cleartext.stderr.contains("must use https"),
        "{}",
        cleartext.stderr
    );
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

    // Every flag the run was given is forwarded to the process that actually
    // runs it: a detached run under a different task, label, or override would
    // otherwise be a different run from the one the caller asked for.
    let task_file = workspace.write("task.md", "complete-now: detached from a file\n");
    let forwarded = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task-file",
        &task_file.display().to_string(),
        "--dir",
        &workspace.dir().display().to_string(),
        "--label",
        "round=7",
        "--set",
        "members.worker.mode=read-only",
        "--detach",
    ]);
    forwarded.expect_code(0);
    let started: serde_json::Value =
        serde_json::from_str(forwarded.stdout.trim()).expect("one JSON object");
    let stream_path = started["events_path"].as_str().expect("a path").to_string();
    until("the forwarded run to settle", || {
        std::fs::read_to_string(&stream_path).is_ok_and(|s| s.contains("\"graph-settled\""))
    });
    let forwarded_stream = std::fs::read_to_string(&stream_path).expect("the stream");
    assert!(
        forwarded_stream.contains("\"round\":\"7\""),
        "{forwarded_stream}"
    );
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

/// The refusals the surface owes a caller who typed something that cannot work:
/// a task file that is not there, a `history RUN` naming no run, a persona
/// argument pointing at nothing, and a catalog holding none.
#[test]
fn every_verb_refuses_what_cannot_work_by_name() {
    let workspace = Workspace::new();

    let missing_file = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task-file",
        "./no-such-task.md",
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    missing_file.expect_code(2);
    assert!(
        missing_file.stderr.contains("cannot read --task-file"),
        "{}",
        missing_file.stderr
    );

    let no_run = workspace.run(&["history", "no-such-run"]);
    no_run.expect_code(2);
    assert!(no_run.stderr.contains("no run"), "{}", no_run.stderr);

    let nowhere = workspace.run(&["persona", "validate", "./nowhere"]);
    nowhere.expect_code(2);
    assert!(
        nowhere.stderr.contains("no persona at"),
        "{}",
        nowhere.stderr
    );

    std::fs::create_dir_all(workspace.at("empty-catalog")).expect("mkdir");
    let empty = workspace.run(&["persona", "validate", "empty-catalog"]);
    empty.expect_code(2);
    assert!(
        empty.stderr.contains("no personas under"),
        "{}",
        empty.stderr
    );

    // A file that is not YAML at all fails as itself, naming the file.
    workspace.write("catalog2/broken.yaml", "not: [a, mapping\n");
    let broken = workspace.run(&["persona", "validate", "catalog2"]);
    broken.expect_code(2);
    assert!(broken.stderr.contains("broken.yaml"), "{}", broken.stderr);
}

/// `history RUN` narrows the listing to one run, and `cancel` with no member
/// stops the whole run.
#[test]
fn history_narrows_to_one_run_and_cancel_stops_the_whole_run() {
    let workspace = Workspace::new();
    workspace.run_task("complete-now: one run").expect_code(0);
    let id = run_id(&workspace.state()).expect("a run");

    let narrowed = workspace.run(&["history", &id]);
    narrowed.expect_code(0);
    assert_eq!(narrowed.stdout.lines().count(), 1, "{}", narrowed.stdout);
    assert!(narrowed.stdout.starts_with(&id), "{}", narrowed.stdout);

    let cancelled = workspace.run(&["cancel", &id, "--kill"]);
    cancelled.expect_code(0);
    assert!(
        cancelled.stdout.contains("cancelled"),
        "{}",
        cancelled.stdout
    );
    assert!(
        !cancelled.stdout.contains("member"),
        "a whole-run cancel named a member: {}",
        cancelled.stdout
    );
}

/// `smoke` with no `--dir` spends its turn in a throwaway directory of its own,
/// so a caller never has to find one.
#[test]
fn smoke_makes_its_own_throwaway_directory() {
    let workspace = Workspace::new();
    let tmp = workspace.at("tmp");
    std::fs::create_dir_all(&tmp).expect("mkdir");
    let run = workspace.run_with(
        &["smoke"],
        &[
            ("TMPDIR", &tmp.display().to_string()),
            ("ONEHARNESS_BIN_CLAUDE_CODE", &fake_harness()),
            ("ONEHARNESS_HARNESSES", "claude-code"),
        ],
    );
    run.expect_code(0);
    assert!(
        run.stdout.contains("smoke: passed via claude-code"),
        "{}",
        run.stdout
    );
    assert!(
        std::fs::read_dir(&tmp)
            .expect("tmp")
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("oneagentgraph-smoke-")),
        "smoke did not make its own directory under TMPDIR"
    );
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

    // A binary that runs and refuses forwards its own diagnostic, and one that
    // answers something that is not a report is refused rather than forwarded:
    // a caller would read either as an answer about its identities.
    let refusing = workspace.write(
        "refuse.sh",
        "#!/bin/sh\necho 'no identities configured' >&2\nexit 1\n",
    );
    executable(&refusing);
    let refused = workspace.run_with(
        &["health"],
        &[(
            "ONEAGENTGRAPH_ONEHARNESS_BIN",
            &refusing.display().to_string(),
        )],
    );
    refused.expect_code(2);
    assert!(
        refused.stderr.contains("refused the probe"),
        "{}",
        refused.stderr
    );

    let babbling = workspace.write("babble.sh", "#!/bin/sh\necho 'not a report'\n");
    executable(&babbling);
    let babbled = workspace.run_with(
        &["health"],
        &[(
            "ONEAGENTGRAPH_ONEHARNESS_BIN",
            &babbling.display().to_string(),
        )],
    );
    babbled.expect_code(2);
    assert!(babbled.stderr.contains("not JSON"), "{}", babbled.stderr);

    // JSON that parses but describes nothing is the harder half: `7` and
    // `"none"` are valid documents, and forwarding one would let a caller read
    // it as an answer about its identities.
    for (name, script) in [
        ("scalar.sh", "#!/bin/sh\necho 7\n"),
        ("string.sh", "#!/bin/sh\necho '\"none\"'\n"),
    ] {
        let bare = workspace.write(name, script);
        executable(&bare);
        let answered = workspace.run_with(
            &["health"],
            &[("ONEAGENTGRAPH_ONEHARNESS_BIN", &bare.display().to_string())],
        );
        answered.expect_code(2);
        assert!(
            answered.stderr.contains("a bare value, not a report"),
            "{name}: {}",
            answered.stderr
        );
    }
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
    // The catalog on disk is what is compiled in and what a user reads, so it is
    // what the verb is pointed at — a suite that rebuilt it from a library
    // constant would pass on a file the crate does not ship.
    let catalog = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("personas");
    Workspace::new()
        .run(&["persona", "validate", &catalog.display().to_string()])
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

    // A member argument becomes a path — the signal file, and the scratch
    // `cancel --kill` reaps — so one that could leave the run's own directory is
    // refused before either is touched.
    for escape in ["../elsewhere", "a/b"] {
        let refused = workspace.run(&["trigger", &id, escape]);
        refused.expect_code(2);
        assert!(
            refused.stderr.contains("member"),
            "{escape}: {}",
            refused.stderr
        );

        let killed = workspace.run(&["cancel", &id, escape, "--kill"]);
        killed.expect_code(2);
        assert!(
            killed.stderr.contains("member"),
            "{escape}: {}",
            killed.stderr
        );
    }

    // A well-formed name that is simply not a member of this run is refused too,
    // by every verb that takes one — `cancel` included, which would otherwise
    // report having cancelled something that was never there.
    let cancelled = workspace.run(&["cancel", &id, "ghost"]);
    cancelled.expect_code(2);
    assert!(
        cancelled.stderr.contains("no member \"ghost\""),
        "{}",
        cancelled.stderr
    );

    // And a run id is a path component too.
    let traversal = workspace.run(&["history", "show", "../elsewhere"]);
    traversal.expect_code(2);
    assert!(
        traversal.stderr.contains("is not a run id"),
        "{}",
        traversal.stderr
    );
}

/// A typo is refused *while the run is in flight*, which is when these verbs are
/// actually used.
///
/// The run record fills its outcomes in as members settle, so for the whole of a
/// live run there are none — and a check that read only those accepted any name
/// at all, answering an operator's typo with exit 0 and a signal file nothing
/// would ever read. The graph's own member list is written before anything
/// launches for exactly this.
#[test]
fn a_signal_for_an_unknown_member_is_refused_while_the_run_is_still_running() {
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
                    "complete-now: in flight",
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
    let record = state.join(&id).join("record.json");

    // The record has the graph's members and no outcomes yet: exactly the state
    // the old check could say nothing about.
    let raw = std::fs::read_to_string(&record).expect("a record");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("the record is JSON");
    assert_eq!(
        parsed["members"],
        serde_json::json!({}),
        "this journey needs a run with no settled member yet: {raw}"
    );

    for verb in ["trigger", "reset-timer"] {
        let refused = workspace.run(&[verb, &id, "ghost"]);
        refused.expect_code(2);
        assert!(
            refused.stderr.contains("no member \"ghost\""),
            "{verb}: {}",
            refused.stderr
        );
        assert!(
            refused.stderr.contains("reporter"),
            "{verb}: {}",
            refused.stderr
        );
    }
    assert!(
        !state
            .join(&id)
            .join("signals")
            .join("ghost.trigger")
            .exists(),
        "a refused signal still left its file behind"
    );

    workspace.run(&["cancel", &id]).expect_code(0);
    handle.join().expect("the run thread");
}

/// A detached run that never records itself is a refusal that says where to
/// look, not an exit 0 handing back a run id nothing produced.
///
/// `--detach` reports on a child it cannot watch: it prints where the stream
/// will be and leaves. When the child dies immediately — an unreadable graph is
/// the ordinary way — the only thing the parent knows is that no record
/// appeared, and the caller must not be told a run started.
#[test]
fn detach_refuses_when_the_run_it_launched_never_records_itself() {
    let workspace = Workspace::new();
    let run = workspace.run(&[
        "run",
        "./no-such-graph.yaml",
        "--task",
        "complete-now: doomed",
        "--dir",
        &workspace.dir().display().to_string(),
        "--detach",
    ]);
    run.expect_code(2);
    assert!(
        run.stderr.contains("did not record itself"),
        "{}",
        run.stderr
    );
    assert!(
        run.stdout.trim().is_empty(),
        "a refusal printed a detach answer on stdout: {}",
        run.stdout
    );
}

/// `reset-timer` is accepted for any member, but only a schedule that declared
/// itself `resettable` restarts its clock.
///
/// The signal is left either way — an operator cannot know from outside which
/// schedules opted in, and refusing at the CLI would make the answer depend on a
/// record the caller cannot see. What differs is the run's response: a
/// non-resettable schedule keeps counting, and says nothing it did not do.
#[test]
fn reset_timer_leaves_a_non_resettable_schedule_counting() {
    let workspace = Workspace::new();
    workspace.graph(&format!(
        concat!(
            "version: 1\nname: node-scope\n",
            "env:\n  ONEHARNESS_BIN_CLAUDE_CODE: {fake}\n",
            "members:\n  reporter:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "    schedule: {{every: 3600, resettable: false}}\n",
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
                    "complete-now: unresettable",
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

    // A trigger after it, so the run is proven to have read the signal
    // directory past the reset rather than merely not reached it yet.
    workspace.run(&["trigger", &id, "reporter"]).expect_code(0);
    until("the scheduled member to fire", || {
        std::fs::read_to_string(&events).is_ok_and(|s| s.contains("cron-fired"))
    });

    workspace.run(&["cancel", &id]).expect_code(0);
    handle.join().expect("the run thread");

    let stream = std::fs::read_to_string(&events).expect("the stream");
    assert!(
        !stream.contains("cron-reset"),
        "a schedule that is not resettable reported restarting its clock: {stream}"
    );
}

/// The README tells a reader which verbs need `onejudge` and `oneharness` on
/// `PATH` and which need neither. This is that sentence, executed.
///
/// A prose prerequisite is the kind that rots quietly: nothing fails when a verb
/// quietly grows a dependency, until someone follows the README onto a machine
/// that has only what it told them to install. So every verb the README says
/// needs neither CLI is run here with both pointed at a path that does not
/// exist, and has to work anyway.
#[test]
fn the_verbs_the_readme_says_need_no_cli_run_without_one() {
    let workspace = Workspace::new();
    workspace.run_task("complete-now: recorded").expect_code(0);
    let id = run_id(&workspace.state()).expect("a run");

    let absent = workspace.at("no-such-binary").display().to_string();
    let without = [
        ("ONEAGENTGRAPH_ONEJUDGE_BIN", absent.as_str()),
        ("ONEAGENTGRAPH_ONEHARNESS_BIN", absent.as_str()),
    ];
    for args in [
        vec!["validate", "./graph.yaml"],
        vec!["history"],
        vec!["history", id.as_str()],
        // `persona validate` takes a path, per the contract; scaffolding one is
        // the other half of what the README says needs no CLI.
        vec!["persona", "new", "solo"],
        vec!["persona", "validate", "solo.yaml"],
        vec!["trigger", id.as_str(), "worker"],
        vec!["reset-timer", id.as_str(), "worker"],
        vec!["cancel", id.as_str()],
    ] {
        let run = workspace.run_with(&args, &without);
        assert_eq!(
            run.code,
            0,
            "`{}` needs a CLI the README says it does not:\n{}\n{}",
            args.join(" "),
            run.stdout,
            run.stderr
        );
    }
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
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    #[cfg(not(unix))]
    let _ = path;
}
