//! The rest of the command surface, end to end.
//!
//! Ported from ai-orchestrator's `test_history_e2e.py`, `test_real_harness_smoke_e2e.py`,
//! `test_smoke_contention_e2e.py`, `test_environment_isolation_e2e.py`, and the
//! persona half of `test_dispatch_e2e.py`.

// llmlint: ignore-file[e2e_not_mocked] see tests/e2e/support.rs: the paid harness
// process is the single sanctioned double, and the wrapper scripts here are that
// same double reached under a second `ONEHARNESS_BIN_*` key.

use oneagentgraph::config::{FIRST_SCHEMA_VERSION, SCHEMA_VERSION};

use crate::support::{
    fake_harness, graph_with, two_party_graph, until, Workspace, CHAIN, FAKE_HARNESS_KEY, NO_ENV,
};

/// `validate` reads every ref the graph names, so a pass means the graph could
/// be launched — not merely that it parses.
#[test]
fn validate_reads_every_ref_the_graph_names() {
    let workspace = Workspace::new();
    workspace.run(&["validate", "./graph.yaml"]).expect_code(0);

    workspace.graph(
        &two_party_graph(&fake_harness(), NO_ENV)
            .replace("./oneharness.judge.toml", "./nowhere.toml"),
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
    // A version past the ones this build reads, derived rather than typed: it
    // moves with every schema this crate adds, and a literal here made the next
    // bump refuse the schema it had just started reading.
    let ahead = SCHEMA_VERSION + 1;
    let document = format!("version: {ahead}\nname: g\nmembers: {{}}\n");
    workspace.graph(&document);
    let run = workspace.run(&["validate", "./graph.yaml"]);
    run.expect_code(2);
    assert!(
        run.stderr.contains(&format!(
            "reads versions {FIRST_SCHEMA_VERSION} through {SCHEMA_VERSION}"
        )),
        "{document}: {}",
        run.stderr
    );

    for (document, expected) in [
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
        // A graph of nothing but schedules quiesces as soon as its clocks tick,
        // so a deferred first turn in one never comes due: the member would
        // start, wait, and the run would exit 0 without it ever having run.
        // Refused instead, saying which field asks for the other behaviour.
        (
            concat!(
                "version: 4\nname: g\nmembers:\n  a:\n    kind: oneharness\n",
                "    oneharness_config: ./oneharness.toml\n    schedule: {every: 1800}\n",
            ),
            "never comes due",
        ),
        // A span longer than any run is a member that never fires and never says
        // why — refused on either of the two fields that can carry one.
        (
            concat!(
                "version: 4\nname: g\nmembers:\n  a:\n    kind: oneharness\n",
                "    oneharness_config: ./oneharness.toml\n",
                "    schedule: {every: 60, start_after: 18446744073709551615}\n",
            ),
            "longer than any run",
        ),
        (
            concat!(
                "version: 1\nname: g\nmembers:\n  a:\n    kind: oneharness\n",
                "    oneharness_config: ./oneharness.toml\n",
                "    schedule: {every: 18446744073709551615}\n",
            ),
            "longer than any run",
        ),
        // `start_after` postdates version 3, and a document declaring one is
        // refused by the field's name rather than run at t=0 — the delay it
        // asked for and the delay it would get are opposite answers.
        (
            concat!(
                "version: 3\nname: g\nmembers:\n  a:\n    kind: oneharness\n",
                "    oneharness_config: ./oneharness.toml\n",
                "    schedule: {every: 1800, start_after: 30}\n",
            ),
            "requires graph schema version 4",
        ),
        (
            concat!(
                "version: 1\nname: g\nmembers:\n  w:\n    kind: onejudge\n",
                "    base_config: ./base.yaml\n    mode: bypass\n    deps: [build]\n",
                "    agent: {oneharness_config: ./oneharness.toml}\n",
                "    judge: {oneharness_config: ./oneharness.judge.toml}\n",
            ),
            "requires graph schema version 2",
        ),
        // A member's own job is the same shape of refusal one schema later: a
        // document that declares version 2 and then gives a member a task of its
        // own is told which schema has that field, rather than silently running
        // that member on the graph's task instead.
        (
            concat!(
                "version: 2\nname: g\nmembers:\n  a:\n    kind: oneharness\n",
                "    oneharness_config: ./oneharness.toml\n    task: its own job\n",
            ),
            "requires graph schema version 3",
        ),
        (
            concat!(
                "version: 2\nname: g\nmembers:\n  a:\n    kind: oneharness\n",
                "    oneharness_config: ./oneharness.toml\n    dir: ./api\n",
            ),
            "requires graph schema version 3",
        ),
        (
            concat!(
                "version: 3\nname: g\nmembers:\n  a:\n    kind: oneharness\n",
                "    oneharness_config: ./oneharness.toml\n    dir: ''\n",
            ),
            "names no directory",
        ),
        // A graph's persona catalog is a directory too, and an empty one names
        // wherever the launching process happened to be rather than the catalog
        // whose personas the members were written against.
        (
            concat!(
                "version: 6\nname: g\npersonas: ''\nmembers:\n  a:\n    kind: oneharness\n",
                "    oneharness_config: ./oneharness.toml\n",
            ),
            "`personas` names no directory",
        ),
        // The key changes what a member's bare `persona: NAME` resolves to, so a
        // document declaring a schema that predates it is refused by the key's
        // name rather than run under a rule that schema never had.
        (
            concat!(
                "version: 5\nname: g\npersonas: ./personas\nmembers:\n  a:\n",
                "    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            ),
            "requires graph schema version 6",
        ),
        // Each field replaces what the graph supplies, so an empty one asks for
        // nothing rather than for the graph's — and an empty task reached the
        // harness as a `--prompt` with no value at all.
        (
            concat!(
                "version: 3\nname: g\nmembers:\n  a:\n    kind: oneharness\n",
                "    oneharness_config: ./oneharness.toml\n    task: '   '\n",
            ),
            "an empty one is no job",
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

/// A graph, or a ref inside one, may be an `https` URL — and a host that cannot
/// be reached is a refusal naming the URL, not a run that started against a
/// document nobody read. Cleartext is refused before a byte leaves.
///
/// The URL is under `.invalid`, which by RFC 6761 resolves nowhere, so this
/// journey drives the real fetch path without leaving the machine. The half
/// where a ref *is* fetched needs a real origin to answer, and lives in
/// `remote.rs` next to the TLS it rides on.
#[test]
fn an_unreachable_https_ref_and_a_cleartext_one_are_both_refused_by_url() {
    let workspace = Workspace::new();
    let unreachable = "https://oneagentgraph.invalid/graph.yaml";

    let graph = workspace.run(&["validate", unreachable]);
    graph.expect_code(2);
    assert!(graph.stderr.contains(unreachable), "{}", graph.stderr);
    assert!(graph.stderr.contains("cannot fetch"), "{}", graph.stderr);

    // And the same for a ref *inside* a graph this build can read, so the
    // refusal is the fetch rather than the graph.
    workspace.graph(
        &two_party_graph(&fake_harness(), NO_ENV)
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
    let json = workspace.run_task("fake:complete-now: render me");
    json.expect_code(0);

    let text = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "fake:complete-now: render me",
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
        "fake:complete-now: detached",
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
    let task_file = workspace.write("task.md", "fake:complete-now: detached from a file\n");
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
    let ran = workspace.run_task("fake:complete-now: recorded once");
    ran.expect_code(0);

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

    // llmlint: ignore-block[tests_mirror_real_usage] `history::events` is a
    // public library API the contract gives no verb of its own, so the library
    // call *is* the interface a consumer reaches for — the same reason the
    // liveness journeys read `oneagentgraph::scratch` directly. What makes this
    // realistic is the store: a real run persisted it a moment ago through the
    // CLI, and the answer is compared against what that run printed.
    let persisted = oneagentgraph::history::events(&workspace.state(), &run_id)
        .expect("the run's own stream reads back");
    assert_eq!(
        persisted, ran.stdout,
        "the persisted stream is not the one the run printed"
    );
    let absent = oneagentgraph::history::events(&workspace.state(), "no-such-run").unwrap_err();
    assert!(absent.to_string().contains("no-such-run"), "{absent}");
    // llmlint: ignore-end[tests_mirror_real_usage]
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
    workspace
        .run_task("fake:complete-now: one run")
        .expect_code(0);
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

/// Every smoke directory is a git worktree, without nesting a repository when
/// the caller supplied a directory already inside one.
#[test]
fn smoke_prepares_standalone_directories_and_preserves_existing_worktrees() {
    let standalone = Workspace::new();
    let plain = standalone.at("plain");
    std::fs::create_dir_all(&plain).expect("plain smoke dir");
    std::fs::write(plain.join("oneharness.toml"), CHAIN).expect("chain");
    let run = standalone.run_with(
        &["smoke", "--dir", &plain.display().to_string()],
        &[("ONEHARNESS_BIN_CLAUDE_CODE", &fake_harness())],
    );
    run.expect_code(0);
    assert!(
        plain.join(".git").is_dir(),
        "a standalone --dir was not initialized"
    );

    let existing = Workspace::new();
    let initialized = std::process::Command::new("git")
        .arg("init")
        .arg("--quiet")
        .arg(existing.path())
        .status()
        .expect("git runs");
    assert!(initialized.success(), "git init failed");
    let nested = existing.at("nested/smoke");
    let run = existing.run_with(
        &["smoke", "--dir", &nested.display().to_string()],
        &[("ONEHARNESS_BIN_CLAUDE_CODE", &fake_harness())],
    );
    run.expect_code(0);
    assert!(
        !nested.join(".git").exists(),
        "smoke nested a repository inside the caller's existing worktree"
    );
}

/// `sweep` reports what it would reclaim and removes nothing while it does, and
/// the floor keeps a run's own record until an operator asks past it.
///
/// The escape hatch this is: a host that filled up has a verb that says what is
/// there and what can go, and a `--dry-run` an operator can read before anything
/// is destroyed. So the ordering is the assertion — the same directory is
/// reported, kept, and only then taken.
#[test]
fn sweep_reports_before_it_reclaims_and_removes_nothing_in_a_dry_run() {
    let workspace = Workspace::new();
    workspace
        .run_task("fake:complete-now: leave a record behind")
        .expect_code(0);
    let id = run_id(&workspace.state()).expect("a run");
    let recorded = workspace.state().join(&id);

    // A temp root of this journey's own: the family is the host's `TMPDIR`, and
    // a sweep pointed at the real one would be judging scratch belonging to
    // whatever else is running on this machine.
    let temp = workspace.at("tmp");
    std::fs::create_dir_all(&temp).expect("mkdir");
    let temp = temp.display().to_string();
    let env = [("TMPDIR", temp.as_str())];

    // The floor first: a run that finished a moment ago is exactly what an
    // operator is about to read `history` for, so the default sweep keeps it and
    // says which flag takes it.
    let floored = workspace.run_with(&["sweep"], &env);
    floored.expect_code(0);
    assert!(
        floored.stdout.contains("--min-age-hours 0"),
        "{}",
        floored.stdout
    );
    assert!(recorded.is_dir(), "the default floor took a fresh run");

    let dry = workspace.run_with(&["sweep", "--dry-run", "--min-age-hours", "0"], &env);
    dry.expect_code(0);
    assert!(
        dry.stdout
            .contains(&format!("would reclaim {}", recorded.display())),
        "a dry run that never named what it would take: {}",
        dry.stdout
    );
    assert!(
        recorded.is_dir(),
        "a --dry-run sweep removed a run's scratch"
    );
    // And the run is still there to read, which is what "removed nothing" is
    // worth to whoever ran the dry run.
    workspace.run(&["history", "show", &id]).expect_code(0);

    let swept = workspace.run_with(&["sweep", "--min-age-hours", "0"], &env);
    swept.expect_code(0);
    assert!(
        swept
            .stdout
            .contains(&format!("reclaimed {}", recorded.display())),
        "{}",
        swept.stdout
    );
    assert!(
        !recorded.exists(),
        "a sweep left behind what it reported reclaiming"
    );
}

/// A `smoke`'s own throwaway directory is scratch the sweep can reclaim, and a
/// neighbour's directory in the same shared root is not.
///
/// Both halves matter and neither covers the other. `TMPDIR` is the host's, not
/// this crate's: a sweep that took everything there would delete whatever else
/// is running on the machine, and one that could take nothing there would leave
/// the family that actually leaks — a directory per `smoke`, kept on purpose for
/// its operator to read — growing forever. So the neighbour here is one the
/// proofs *would* clear, and the only thing standing between it and the sweep is
/// that it is not this crate's.
#[test]
fn sweep_reclaims_a_smoke_s_own_scratch_and_leaves_a_neighbour_s_alone() {
    let workspace = Workspace::new();
    let temp = workspace.at("tmp");
    std::fs::create_dir_all(&temp).expect("mkdir");
    let temp_env = temp.display().to_string();

    workspace
        .run_with(
            &["smoke"],
            &[
                ("TMPDIR", temp_env.as_str()),
                ("ONEHARNESS_BIN_CLAUDE_CODE", &fake_harness()),
                ("ONEHARNESS_HARNESSES", "claude-code"),
            ],
        )
        .expect_code(0);
    let left_behind: Vec<std::path::PathBuf> = std::fs::read_dir(&temp)
        .expect("tmp")
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert_eq!(
        left_behind.len(),
        1,
        "a smoke left {left_behind:?} rather than one directory"
    );

    // Everything the proofs ask for, and nothing this crate created: a lock
    // nobody holds, recording a number with a start token nobody holds either.
    let neighbour = temp.join("someone-elses-work");
    std::fs::create_dir_all(&neighbour).expect("mkdir");
    std::fs::write(
        neighbour.join(oneagentgraph::liveness::OWNER_LOCK_FILE),
        format!("{} 1\n", std::process::id()),
    )
    .expect("write");
    // A *file* wearing this crate's own prefix. Scratch is a directory, and a
    // sweep that took anything whose name matched would delete a neighbour's
    // file for looking like one.
    let namesake = temp.join("oneagentgraph-not-a-directory");
    std::fs::write(&namesake, "someone else's notes").expect("write");

    let swept = workspace.run_with(
        &["sweep", "--min-age-hours", "0"],
        &[("TMPDIR", temp_env.as_str())],
    );
    swept.expect_code(0);
    assert!(
        !left_behind[0].exists(),
        "a sweep left this crate's own throwaway scratch behind:\n{}",
        swept.stdout
    );
    assert!(
        neighbour.is_dir(),
        "a sweep took a directory in a shared root that this crate never created:\n{}",
        swept.stdout
    );
    assert!(
        namesake.is_file(),
        "a sweep took a file for a scratch directory because the name matched:\n{}",
        swept.stdout
    );
}

/// A family the sweep could not examine is named as one, so a zero is never read
/// as "there was nothing there".
///
/// This is the property the whole verb rests on: `reclaimed 0 B` from a sweep
/// that never managed to look at one of its two families is the answer that
/// sends an operator away from the disk that is still full.
#[test]
fn sweep_names_the_family_it_could_not_examine() {
    let workspace = Workspace::new();
    // A file where the temp root should be: the family exists, and cannot be
    // read.
    let blocked = workspace.write("not-a-directory", "").display().to_string();

    let run = workspace.run_with(&["sweep", "--min-age-hours", "0"], &[("TMPDIR", &blocked)]);
    run.expect_code(0);
    assert!(
        run.stdout.contains("could not examine family \"temp\""),
        "{}",
        run.stdout
    );
    // Both lists in the one line a reader takes the verdict from, so the zero
    // beside them cannot be mistaken for a clean host.
    assert!(
        run.stdout
            .contains("examined families: runs; unexamined families: temp ("),
        "a zero that does not say which families it covered: {}",
        run.stdout
    );
    assert!(run.stdout.contains("reclaimed 0 B"), "{}", run.stdout);

    // The other zero, and the reason both lists are always printed: a root that
    // is not there yet holds nothing, and that *is* knowable — so it is an
    // examined zero rather than the skip above. An operator reading the two runs
    // side by side can tell which one they got.
    let nowhere = workspace.at("nowhere").display().to_string();
    let empty = workspace.run_with(
        &["sweep", "--min-age-hours", "0"],
        &[
            ("TMPDIR", nowhere.as_str()),
            ("ONEAGENTGRAPH_STATE_DIR", nowhere.as_str()),
        ],
    );
    empty.expect_code(0);
    assert!(
        empty
            .stdout
            .contains("examined families: runs, temp; unexamined families: none"),
        "a root that does not exist yet was reported as one that could not be examined: {}",
        empty.stdout
    );
}

/// `health` forwards what oneharness knows about each identity — through
/// oneharness's own library, with no `oneharness` process anywhere — and says why
/// there is no answer when there is none.
#[test]
fn health_reads_oneharness_data_without_spawning_oneharness() {
    let workspace = Workspace::new();
    let run = workspace.run(&["health"]);
    run.expect_code(0);
    let report: serde_json::Value = serde_json::from_str(&run.stdout).expect("health answers JSON");
    // The document `oneharness usage --format json` prints, which is what this
    // verb promises to forward: a report of identities, not a bare value a caller
    // would read as an answer about its own.
    assert!(
        report["identities"].is_array(),
        "health answered something that is not oneharness's report: {run}",
        run = run.stdout
    );

    // The same answer with the `oneharness` binary pointed at something that is
    // not there. This is the proof the hop is gone: `run` and `interrupt` still
    // reach that binary, so a `health` that needed it would fail here — and it
    // used to, by design.
    let unspawnable = workspace.run_with(
        &["health"],
        &[(
            "ONEAGENTGRAPH_ONEHARNESS_BIN",
            "oneharness-that-is-not-installed",
        )],
    );
    unspawnable.expect_code(0);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&unspawnable.stdout)
            .expect("health answers JSON")["identities"]
            .as_array()
            .map(Vec::len),
        report["identities"].as_array().map(Vec::len),
        "health answered a different fleet without a oneharness binary to spawn"
    );

    // The one thing left that can refuse the sweep: configuration oneharness will
    // not load. It is refused with oneharness's own diagnostic — naming the file —
    // rather than reported as a host with no identities.
    let broken = workspace.write("oneharness.toml", "harnesses = [\n");
    let refused = workspace.run(&["health"]);
    refused.expect_code(2);
    assert!(
        refused.stderr.contains("this host's identities")
            && refused.stderr.contains(&broken.display().to_string()),
        "{}",
        refused.stderr
    );
    assert!(
        refused.stdout.is_empty(),
        "a refusal must not read as a report: {}",
        refused.stdout
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

/// A launch whose provider crashed having spent nothing is relaunched, and a
/// smoke that never got one going says how many starts reached that answer.
///
/// This is the accounting decision read end to end. A crashed provider's report
/// carries oneharness's own `usage` with every counter empty and no failure
/// classification, and `smoke` asks *oneharness's own* predicate whether that is
/// billed work — the same one its quota classifier and its fallback chain share.
/// Answering yes would refuse to retry a host that merely stumbled; answering no
/// on a report that says nothing at all would pay for the same question twice.
#[test]
fn a_smoke_whose_provider_spent_nothing_is_relaunched_and_says_how_often() {
    let workspace = Workspace::new();
    let dir = workspace.at("smoke");
    std::fs::create_dir_all(&dir).expect("smoke dir");
    std::fs::write(dir.join("oneharness.toml"), CHAIN).expect("chain");
    let attempts = workspace.at("harness-attempts");

    let run = workspace.run_with(
        &["smoke", "--dir", &dir.display().to_string()],
        &[
            ("ONEHARNESS_BIN_CLAUDE_CODE", &fake_harness()),
            ("FAKE_HARNESS_ATTEMPT_LOG", &attempts.display().to_string()),
            ("FAKE_HARNESS_CRASH", "1"),
        ],
    );
    run.expect_code(1);
    assert!(
        run.stderr.contains("after 3 attempts"),
        "a smoke that spent nothing did not report its attempts: {}",
        run.stderr
    );
    assert_eq!(
        std::fs::read_to_string(&attempts)
            .expect("the harness recorded its launches")
            .lines()
            .count(),
        3,
        "a launch that provably spent nothing was not relaunched"
    );
}

/// Accounting this build cannot read is **not** proof that a turn was free, so
/// the launch is not retried.
///
/// The pair with the journey above is the whole point, and the difference is one
/// launch versus three. A report whose `usage` is absent proves the provider
/// published nothing and is worth another try; a `usage` that is *there* and
/// unparsable proves nothing at all — least of all that nobody was billed — and
/// relaunching on it pays for the same question twice, which is the one thing
/// `smoke` must never do. An upstream `Usage` that gained a required field would
/// have walked straight into that, silently.
///
/// oneharness itself stands in here, because the subject *is* what this crate
/// does with a report it cannot fully parse — the same seam and the same reason
/// `health`'s journeys above use one. The launch count is read from the script's
/// own log, so the assertion is on what was actually spawned.
// A POSIX shell, for the reason the journey below it gives: a canned answer is
// how a report shape this crate must handle is expressed without a second
// compiled binary per case.
#[cfg(unix)]
#[test]
fn a_smoke_whose_accounting_cannot_be_read_is_not_relaunched() {
    let workspace = Workspace::new();
    let dir = workspace.at("smoke");
    std::fs::create_dir_all(&dir).expect("smoke dir");
    std::fs::write(dir.join("oneharness.toml"), CHAIN).expect("chain");
    let attempts = workspace.at("oneharness-attempts");

    // A report shaped exactly like a real one except for `usage`, whose token
    // counts are prose. Nothing here names a `failure_kind`, so the accounting is
    // the *only* thing standing between this and a relaunch.
    let answering = workspace.write(
        "unreadable-usage.sh",
        &format!(
            concat!(
                "#!/bin/sh\n",
                "echo launched >> {log}\n",
                "cat <<'JSON'\n",
                "{{\"results\": [{{\"harness\": \"claude-code\", \"status\": \"nonzero\",\n",
                "  \"exit_code\": 1, \"failure_kind\": null,\n",
                "  \"usage\": {{\"input_tokens\": \"lots\", \"output_tokens\": \"some\"}}}}]}}\n",
                "JSON\n",
                "exit 1\n",
            ),
            log = attempts.display()
        ),
    );
    executable(&answering);

    let run = workspace.run_with(
        &["smoke", "--dir", &dir.display().to_string()],
        &[(
            "ONEAGENTGRAPH_ONEHARNESS_BIN",
            &answering.display().to_string(),
        )],
    );
    run.expect_code(1);
    assert!(
        !run.stdout.contains("passed"),
        "a smoke whose accounting was unreadable was reported as a pass: {}",
        run.stdout
    );
    assert_eq!(
        std::fs::read_to_string(&attempts)
            .expect("the stand-in recorded its launches")
            .lines()
            .count(),
        1,
        "a launch whose accounting proved nothing was retried anyway — which is \
         how the same question gets paid for twice"
    );
}

/// Real `oneagentgraph run` dispatches, every one of them held live inside its
/// agent for as long as a journey needs.
///
/// The load is *generated* rather than asserted on: what it exists to do is put
/// the host under the same contention the failure happened under, so the smoke
/// running beside it is running the way it ran when it lied.
struct Load {
    /// Held, not read: dropping these removes the directories the members are
    /// working in, so they have to outlive the load.
    _workspaces: Vec<Workspace>,
    members: Vec<std::process::Child>,
    /// The file every held agent is waiting on.
    release: std::path::PathBuf,
}

impl Load {
    /// Start `count` dispatches and wait until every one is inside its agent.
    fn start(count: usize, release: &std::path::Path) -> Self {
        let workspaces: Vec<Workspace> = (0..count).map(|_| Workspace::new()).collect();
        let ready: Vec<std::path::PathBuf> = workspaces
            .iter()
            .map(|workspace| workspace.at("in-flight"))
            .collect();
        let members = workspaces
            .iter()
            .zip(&ready)
            .map(|(workspace, ready)| {
                workspace.spawn_with(
                    &[
                        "run",
                        "./graph.yaml",
                        "--task",
                        &format!(
                            "fake:complete-now: hold this dispatch live while the smoke runs \
                             fake:record-prompt={} fake:hold={}",
                            ready.display(),
                            release.display()
                        ),
                        "--dir",
                        &workspace.dir().display().to_string(),
                    ],
                    &[("ONEHARNESS_BIN_CLAUDE_CODE", &fake_harness())],
                )
            })
            .collect();
        // Started is not enough: the load has to be *inside* its agents, which
        // is where it holds the host, and the double records its prompt on the
        // way to the barrier.
        until(
            "the concurrent dispatches to all reach their agents",
            || ready.iter().all(|path| path.exists()),
        );
        Self {
            _workspaces: workspaces,
            members,
            release: release.to_path_buf(),
        }
    }

    /// How many dispatches are still running.
    fn alive(&mut self) -> usize {
        let mut alive = 0;
        for member in &mut self.members {
            if member
                .try_wait()
                .expect("a member's status is readable")
                .is_none()
            {
                alive += 1;
            }
        }
        alive
    }
}

impl Drop for Load {
    /// Release the held agents and wait for them, whichever way the journey
    /// went: a panicking assertion must not leave real dispatches behind.
    fn drop(&mut self) {
        let _ = std::fs::write(&self.release, "go");
        // Waited for before the workspaces they are working in are removed,
        // which is why this runs here rather than being left to field drop.
        for member in &mut self.members {
            let _ = member.wait();
        }
    }
}

/// A launch that failed under load is a busy host, not an outage: the smoke
/// starts the turn again and passes, saying how many starts it took.
///
/// Ported from `test_smoke_survives_a_harness_that_refuses_a_launch_under_a_live_load`.
/// The failure it carries cost a publication that had already passed its gate:
/// the smoke reported the selected harness "ran but did not succeed" while a full
/// e2e run was in flight spawning real subprocesses, and passed standalone
/// immediately before and after. The stated cause — a harness outage — was not
/// the real one.
#[test]
fn smoke_survives_a_launch_that_failed_under_a_live_load() {
    let workspace = Workspace::new();
    let dir = workspace.at("smoke");
    std::fs::create_dir_all(&dir).expect("smoke dir");
    std::fs::write(dir.join("oneharness.toml"), CHAIN).expect("chain");
    let attempts = workspace.at("harness-attempts");

    let mut load = Load::start(3, &workspace.at("release"));
    assert_eq!(load.alive(), 3, "the load never got going");

    let run = workspace.run_with(
        &["smoke", "--dir", &dir.display().to_string()],
        &[
            ("ONEHARNESS_BIN_CLAUDE_CODE", &fake_harness()),
            ("FAKE_HARNESS_ATTEMPT_LOG", &attempts.display().to_string()),
            ("FAKE_HARNESS_UNAVAILABLE_ATTEMPTS", "1"),
        ],
    );

    // Live for the whole smoke, not merely started before it.
    assert_eq!(
        load.alive(),
        3,
        "the concurrent dispatches did not outlive the smoke"
    );
    run.expect_code(0);
    assert!(
        run.stdout.contains("smoke: passed via claude-code"),
        "{}",
        run.stdout
    );
    // The retry is reported rather than smoothed over: the operator is the only
    // one who can act on a host that needed two launches to record one turn.
    assert!(run.stdout.contains("after 2 attempts"), "{}", run.stdout);
    assert_eq!(
        std::fs::read_to_string(&attempts)
            .expect("an attempt log")
            .lines()
            .count(),
        2,
        "the smoke did not launch exactly twice"
    );
}

/// Tolerating a transient failure must not tolerate a broken launch path: when
/// every start fails, the smoke fails and says how many it made.
///
/// Ported from `test_smoke_still_fails_when_every_launch_under_load_fails`. The
/// retry is the dangerous half of the previous journey — a smoke that retried
/// its way past a launch path that genuinely does not work would report a
/// publication as safe on a host that cannot run a turn at all.
#[test]
fn smoke_still_fails_when_every_launch_under_load_fails() {
    let workspace = Workspace::new();
    let dir = workspace.at("smoke");
    std::fs::create_dir_all(&dir).expect("smoke dir");
    std::fs::write(dir.join("oneharness.toml"), CHAIN).expect("chain");
    let attempts = workspace.at("harness-attempts");

    let mut load = Load::start(2, &workspace.at("release"));

    let run = workspace.run_with(
        &["smoke", "--dir", &dir.display().to_string()],
        &[
            ("ONEHARNESS_BIN_CLAUDE_CODE", &fake_harness()),
            ("FAKE_HARNESS_ATTEMPT_LOG", &attempts.display().to_string()),
            // More refusals than the smoke has attempts, so every one fails.
            ("FAKE_HARNESS_UNAVAILABLE_ATTEMPTS", "9"),
        ],
    );

    assert_eq!(
        load.alive(),
        2,
        "the concurrent dispatches did not outlive the smoke"
    );
    run.expect_code(1);
    assert!(run.stderr.contains("after 3 attempts"), "{}", run.stderr);
    assert!(
        run.stderr.contains("rerun `oneagentgraph smoke`"),
        "{}",
        run.stderr
    );
    assert!(
        !run.stdout.contains("passed"),
        "a launch path that never worked was reported as a pass: {}",
        run.stdout
    );
    // Bounded: a smoke that kept retrying would spend a host's whole capacity
    // on a question already answered.
    assert_eq!(
        std::fs::read_to_string(&attempts)
            .expect("an attempt log")
            .lines()
            .count(),
        3,
        "the smoke did not stop at its attempt bound"
    );
}

/// A turn that was spent and failed is not a proven launch path, even though the
/// report reads like one.
///
/// This is the `rate_limit` half of the rule the module exists for: a candidate
/// billed for work it did not complete is recorded by oneharness as the identity
/// that *ran*, with nothing in `fell_through` — so the report alone is
/// indistinguishable from a healthy launch. oneharness's exit status is the only
/// thing that separates them, and `smoke` reported "passed" until it read one.
#[test]
fn smoke_refuses_a_turn_that_was_spent_and_failed() {
    let workspace = Workspace::new();
    let dir = workspace.at("smoke");
    std::fs::create_dir_all(&dir).expect("smoke dir");
    std::fs::write(dir.join("oneharness.toml"), CHAIN).expect("chain");

    let run = workspace.run_with(
        &["smoke", "--dir", &dir.display().to_string()],
        &[
            ("ONEHARNESS_BIN_CLAUDE_CODE", &fake_harness()),
            ("FAKE_HARNESS_REFUSAL", "rate_limit"),
        ],
    );
    run.expect_code(1);
    assert!(
        run.stderr.contains("did not succeed"),
        "a spent, failed turn was not refused: {}",
        run.stderr
    );
    assert!(
        !run.stdout.contains("passed"),
        "a spent, failed turn was reported as a pass: {}",
        run.stdout
    );
}

/// A failed paid harness cannot keep the real oneharness launch waiting for
/// more caller input, and its diagnostic survives on the command's stderr.
#[test]
fn a_failing_smoke_closes_stdin_and_returns_an_actionable_error() {
    let workspace = Workspace::new();
    let dir = workspace.at("smoke");
    std::fs::create_dir_all(&dir).expect("smoke dir");
    std::fs::write(dir.join("oneharness.toml"), CHAIN).expect("chain");
    let mut child = workspace.spawn_with_open_stdin(
        &["smoke", "--dir", &dir.display().to_string()],
        &[
            ("ONEHARNESS_BIN_CLAUDE_CODE", &fake_harness()),
            ("FAKE_HARNESS_REFUSAL", "rate_limit"),
        ],
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().expect("smoke status") {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("a failed smoke remained blocked on its caller's open stdin");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(
        status.code(),
        Some(1),
        "a spent, failed turn fails the smoke"
    );
    let output = child.wait_with_output().expect("smoke output");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("did not succeed"), "{stderr}");
    assert!(stderr.contains("rate_limit"), "{stderr}");
}

/// A candidate that never ran the turn is the chain doing its job: `smoke` names
/// it on its own line, above the verdict, and still passes.
///
/// The `rate_limit` half of the same rule — a record carrying work the provider
/// already billed for, which a chain does **not** step past — is judged by
/// `smoke::judge` against a real report shape rather than here, because
/// oneharness stops the chain on one and so never produces a `fell_through`
/// entry a journey could construct.
// A POSIX shell stands in for a provider here, which is a platform
// capability like the others this suite gates on: the behaviour under test
// needs one *identity* to answer differently from another, and a script is
// how that is expressed without a second compiled binary per case.
#[cfg(unix)]
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
    // Joined rather than written with a `/`, because the qualified name is what
    // is under test and one of the three platforms qualifies it with a `\`.
    let nested = std::path::Path::new("nested").join("bad.yaml");
    assert!(
        run.stderr.contains(&nested.display().to_string()),
        "{}",
        run.stderr
    );
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

// llmlint: ignore-block[tests_mirror_real_usage] the assertion below reads a file
// the doubled harness wrote recording the prompt it was given, and that is the
// subject: whether the document this crate resolved is the one the agent actually
// ran on. Nothing a user reads carries it — a graph that silently fell back to a
// local file of the same name settles identically and records a digest just the
// same. This is the observation point ai-orchestrator's originals use, for the
// same reason; the recorder is the single sanctioned double, and the exit code,
// the stream, and the run record are all still asserted through the CLI.
/// A shipped persona is reachable by name, with nothing to resolve — a graph can
/// say `persona: engineer` and get one.
#[test]
fn a_shipped_persona_is_reachable_by_name() {
    let workspace = Workspace::new();
    let record = workspace.at("prompts.txt");
    workspace.graph(
        &two_party_graph(&fake_harness(), NO_ENV).replace("persona: engineer", "persona: reviewer"),
    );
    let run = workspace.run_task(&format!(
        "fake:complete-now: shipped fake:record-prompt={}",
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

// llmlint: ignore-end[tests_mirror_real_usage]

/// A cron member fires again on `trigger`, and `reset-timer` restarts a
/// resettable clock. `cancel` is what ends it.
#[test]
fn a_cron_member_fires_on_trigger_and_stops_on_cancel() {
    let workspace = Workspace::new();
    let release = workspace.at("cron-keeper-release");
    workspace.graph(&graph_with(
        concat!(
            "version: 2\nname: node-scope\n",
            "env: {}\n",
            "members:\n  reporter:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "    schedule: {every: 3600, resettable: true}\n",
            "  anchor:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "  keeper:\n    kind: onejudge\n    base_config: ./base.yaml\n",
            "    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n    deps: [anchor]\n",
        ),
        &[
            (FAKE_HARNESS_KEY, fake_harness()),
            (
                "members.keeper.task",
                format!("fake:complete-now fake:hold={}", release.display()),
            ),
        ],
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
                    "fake:complete-now: scheduled",
                    "--dir",
                    &dir.join("work").display().to_string(),
                ])
                .current_dir(&dir)
                .env("ONEAGENTGRAPH_STATE_DIR", &state)
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

    workspace.run(&["cancel", &id, "reporter"]).expect_code(0);
    std::fs::write(&release, "release").expect("release keeper");
    let output = handle.join().expect("the run thread");
    assert!(
        output.status.code().is_some(),
        "the cancelled run never exited"
    );

    let stream = std::fs::read_to_string(&events).expect("the stream");
    let fired = stream.matches("\"cron-fired\"").count();
    assert!(fired >= 1, "the trigger never fired the member: {stream}");
}

/// `cancel RUN MEMBER`, with no `--kill`, stops **that member alone** and leaves
/// the rest of the run running.
///
/// The pair with the whole-run cancels in `tests/e2e/liveness.rs` is the point,
/// and this is the half neither of those reaches. Both of those name `--kill`,
/// which reaps a process tree — an escalation. This is the *ask*: a member-scoped
/// stop signal the run picks up on its own, with nothing signalled and nothing
/// killed. A cancel that quietly stopped every member would pass every other
/// journey here and lose an operator the whole point of naming one.
#[test]
fn a_member_scoped_cancel_stops_that_member_and_leaves_the_run_running() {
    let workspace = Workspace::new();
    let release = workspace.at("member-cancel-keeper-release");
    workspace.graph(&graph_with(
        concat!(
            "version: 2\nname: node-scope\n",
            "env: {}\n",
            "members:\n",
            "  reporter:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "    schedule: {every: 3600, resettable: true}\n",
            "  auditor:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "    schedule: {every: 3600, resettable: true}\n",
            "  anchor:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "  keeper:\n    kind: onejudge\n    base_config: ./base.yaml\n",
            "    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n    deps: [anchor]\n",
        ),
        &[
            (FAKE_HARNESS_KEY, fake_harness()),
            (
                "members.keeper.task",
                format!("fake:complete-now fake:hold={}", release.display()),
            ),
        ],
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
                    "fake:complete-now: scheduled",
                    "--dir",
                    &dir.join("work").display().to_string(),
                ])
                .current_dir(&dir)
                .env("ONEAGENTGRAPH_STATE_DIR", &state)
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
    let fired_by = |member: &str| {
        let stream = std::fs::read_to_string(&events).unwrap_or_default();
        stream
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|event| {
                event["kind"] == serde_json::json!("cron-fired")
                    && crate::support::labels(event)
                        .get("member")
                        .map(String::as_str)
                        == Some(member)
            })
            .count()
    };
    // Both members have to have started before one of them can be told to stop:
    // a signal written before its loop exists would be read as a stop it never
    // saw, and the assertion below would pass on a member that never ran.
    until("both members to settle their first run", || {
        let stream = std::fs::read_to_string(&events).unwrap_or_default();
        ["reporter", "auditor"].iter().all(|member| {
            stream
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .any(|event| {
                    event["kind"] == serde_json::json!("member-settled")
                        && crate::support::labels(&event)
                            .get("member")
                            .map(String::as_str)
                            == Some(*member)
                })
        })
    });

    let cancelled = workspace.run(&["cancel", &id, "reporter"]);
    cancelled.expect_code(0);
    assert!(
        cancelled.stdout.contains("reporter"),
        "a member-scoped cancel did not name the member: {}",
        cancelled.stdout
    );
    // Nothing was reaped: this is the ask, not the kill.
    assert!(
        !cancelled.stdout.contains("signalled"),
        "a cancel with no --kill reaped a process tree: {}",
        cancelled.stdout
    );

    let stopped = fired_by("reporter");
    // Two full firings of the *other* member after the cancel. Both loops tick on
    // the same cadence in the same process, so by the second one reporter has had
    // many ticks in which it would have consumed the trigger below had its loop
    // still been running — which is what makes "it never fired again" an
    // assertion rather than a race won.
    for round in 1..=2 {
        let before = fired_by("auditor");
        workspace.run(&["trigger", &id, "reporter"]).expect_code(0);
        workspace.run(&["trigger", &id, "auditor"]).expect_code(0);
        until("the surviving member to fire", || {
            fired_by("auditor") > before
        });
        assert_eq!(
            fired_by("reporter"),
            stopped,
            "round {round}: the cancelled member fired again"
        );
    }

    // And the run is still the run: the whole-run cancel is what ends it.
    workspace.run(&["cancel", &id, "auditor"]).expect_code(0);
    std::fs::write(&release, "release").expect("release keeper");
    let output = handle.join().expect("the run thread");
    assert!(
        output.status.code().is_some(),
        "the cancelled run never exited"
    );
}

/// `trigger` and `reset-timer` name a member the run does not have rather than
/// leaving a signal nothing will ever read.
#[test]
fn a_signal_for_an_unknown_member_is_refused_by_name() {
    let workspace = Workspace::new();
    workspace
        .run_task("fake:complete-now: signalled")
        .expect_code(0);
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
    workspace.graph(&graph_with(
        concat!(
            "version: 2\nname: node-scope\n",
            "env: {}\n",
            "members:\n  reporter:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "    schedule: {every: 3600, resettable: true}\n",
        ),
        &[(FAKE_HARNESS_KEY, fake_harness())],
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
                    "fake:complete-now: in flight",
                    "--dir",
                    &dir.join("work").display().to_string(),
                ])
                .current_dir(&dir)
                .env("ONEAGENTGRAPH_STATE_DIR", &state)
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

/// `--detach` refuses a graph it cannot read, by name, without spawning anything.
///
/// `--detach` reports on a child it then cannot watch, so everything knowable has
/// to be known before it leaves: it checks the graph itself first, and a caller
/// given `{run_id, …}` and exit 0 has been told a run really started.
#[test]
fn detach_refuses_a_graph_it_cannot_read_before_it_launches_anything() {
    let workspace = Workspace::new();
    let run = workspace.run(&[
        "run",
        "./no-such-graph.yaml",
        "--task",
        "fake:complete-now: doomed",
        "--dir",
        &workspace.dir().display().to_string(),
        "--detach",
    ]);
    run.expect_code(2);
    assert!(
        run.stderr.contains("no-such-graph.yaml"),
        "the refusal did not name the graph it could not read: {}",
        run.stderr
    );
    assert!(
        run.stdout.trim().is_empty(),
        "a refusal printed a detach answer on stdout: {}",
        run.stdout
    );
    assert!(
        run_id(&workspace.state()).is_none(),
        "a refused --detach still left a run behind"
    );
}

/// A graph that parses but could never run is refused by `--detach` too, with the
/// contract's exit 2 and nothing on stdout.
///
/// This is the harder half: the child writes its run record *before* it builds
/// member invocations, so a graph that dies there leaves a record behind. Waiting
/// only for one to appear reported it as a started run — `{run_id, …}` and exit 0
/// for a config the contract gives exit 2, and a caller left watching a stream
/// that would never have a second line.
#[test]
fn detach_refuses_a_graph_that_could_never_run_rather_than_reporting_it_started() {
    let workspace = Workspace::new();
    // A model paired with a chain spanning two harness families: it parses, it
    // resolves, and it fails when the member's invocation is built.
    workspace.write(
        "oneharness.toml",
        "run_mode = \"fallback\"\nharnesses = [\"claude-code:alternate\", \"codex\"]\n",
    );
    workspace.graph(&two_party_graph(&fake_harness(), NO_ENV).replace(
        "    agent:\n      oneharness_config: ./oneharness.toml\n",
        "    agent:\n      oneharness_config: ./oneharness.toml\n      model: claude-opus-5\n",
    ));

    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "fake:complete-now: never gets here",
        "--dir",
        &workspace.dir().display().to_string(),
        "--detach",
    ]);
    run.expect_code(2);
    assert!(
        run.stderr.contains("one harness family"),
        "the refusal did not name what made the graph unrunnable: {}",
        run.stderr
    );
    assert!(
        run.stdout.trim().is_empty(),
        "a graph that could never run was reported as started: {}",
        run.stdout
    );
    // And nothing was launched: no run recorded itself at all.
    assert!(
        run_id(&workspace.state()).is_none(),
        "a refused --detach still left a run behind"
    );

    // The task is part of what makes an invocation buildable, and `--detach`
    // knows the real one — so a run with no task is the same refusal here as in
    // the foreground. Checking a stand-in task instead is what let this exit 0
    // with a `{run_id, …}` on stdout for a config `run` exits 2 for.
    let workspace = Workspace::new();
    let taskless = workspace.run(&[
        "run",
        "./graph.yaml",
        "--dir",
        &workspace.dir().display().to_string(),
        "--detach",
    ]);
    taskless.expect_code(2);
    assert!(
        taskless.stderr.contains("no task"),
        "the refusal did not name the missing task: {}",
        taskless.stderr
    );
    assert!(
        taskless.stdout.trim().is_empty(),
        "a taskless run was reported as started: {}",
        taskless.stdout
    );
    assert!(
        run_id(&workspace.state()).is_none(),
        "a taskless --detach still left a run behind"
    );

    // A `--set` that names nothing is the same class: `--detach` forwards every
    // override to the child, so the parent has to apply them to know whether the
    // run it is about to report could start.
    let bad_override = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "fake:complete-now: never gets here",
        "--dir",
        &workspace.dir().display().to_string(),
        "--set",
        "members.ghost.mode=read-only",
        "--detach",
    ]);
    bad_override.expect_code(2);
    assert!(
        bad_override.stderr.contains("no ghost"),
        "the refusal did not name the override that could not apply: {}",
        bad_override.stderr
    );
    assert!(
        bad_override.stdout.trim().is_empty(),
        "an override that names nothing was reported as a started run: {}",
        bad_override.stdout
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
    let release = workspace.at("reset-keeper-release");
    workspace.graph(&graph_with(
        concat!(
            "version: 2\nname: node-scope\n",
            "env: {}\n",
            "members:\n  reporter:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "    schedule: {every: 3600, resettable: false}\n",
            "  anchor:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "  keeper:\n    kind: onejudge\n    base_config: ./base.yaml\n",
            "    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n    deps: [anchor]\n",
        ),
        &[
            (FAKE_HARNESS_KEY, fake_harness()),
            (
                "members.keeper.task",
                format!("fake:complete-now fake:hold={}", release.display()),
            ),
        ],
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
                    "fake:complete-now: unresettable",
                    "--dir",
                    &dir.join("work").display().to_string(),
                ])
                .current_dir(&dir)
                .env("ONEAGENTGRAPH_STATE_DIR", &state)
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

    workspace.run(&["cancel", &id, "reporter"]).expect_code(0);
    std::fs::write(&release, "release").expect("release keeper");
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
    workspace
        .run_task("fake:complete-now: recorded")
        .expect_code(0);
    let id = run_id(&workspace.state()).expect("a run");

    let absent = workspace.at("no-such-binary").display().to_string();
    let without = [("ONEAGENTGRAPH_ONEHARNESS_BIN", absent.as_str())];
    for args in [
        vec!["validate", "./graph.yaml"],
        // `health` joined this list when its sweep stopped being a child
        // process: it is oneharness's own `usage` verb, called as a library.
        vec!["health"],
        vec!["history"],
        vec!["history", id.as_str()],
        // `persona validate` takes a path, per the contract; scaffolding one is
        // the other half of what the README says needs no CLI.
        vec!["persona", "new", "solo"],
        vec!["persona", "validate", "solo.yaml"],
        vec!["trigger", id.as_str(), "worker"],
        vec!["reset-timer", id.as_str(), "worker"],
        vec!["cancel", id.as_str()],
        // Reporting only: the reclaiming half is driven by its own journeys,
        // and a suite that removed scratch out of this one would be sweeping
        // the host it happens to run on.
        vec!["sweep", "--dry-run"],
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
#[cfg(unix)]
fn executable(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    #[cfg(not(unix))]
    let _ = path;
}
