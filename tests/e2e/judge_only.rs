//! `run --judge-only`: one member's judge side alone, over an input, against
//! the bar a dispatch of that member would apply — and `--bar-fingerprint`, the
//! digest of that bar.
//!
//! Every journey drives the compiled binary. The member's panel is a harness
//! reviewer — the paid model doubled at oneharness's `ONEHARNESS_BIN_<ID>` seam
//! — and an llmlint judge whose `bin` is the llmlint double, judging the input
//! against fixture rules its config and a file that config includes declare.
//! What each judge was handed is read off the doubles' own recordings; what the
//! run decided is read off the exit code, the stream and stderr.

// llmlint: ignore-file[e2e_not_mocked] the declaration every journey file
// carries: the paid harness process is the one sanctioned double, replaced at
// oneharness's own `ONEHARNESS_BIN_<ID>` seam, and the `llmlint` the graph
// names is an *input* — a `bin` the graph supplies — on the terms
// tests/e2e/panel.rs states. Real oneagentgraph selects and prepares the
// member, the real onejudge engine runs its judge-only decision, and the real
// `oneharness` CLI routes the harness judge.

// llmlint: ignore-file[expensive_tests_stay_behind_their_own_edge] this crate
// has exactly one e2e target, and every journey file is a module of it, by the
// design AGENTS.md states. Each journey here is one judge-only decision of the
// doubled harness, and the hang journey waits out a two-second stall bound.

use serde_json::Value;

use crate::support::{
    as_env, bounds, fake_harness, fake_llmlint, graph_with, Run, Workspace, CHAIN, FAKE_HARNESS_KEY,
};

/// The material every passing journey judges.
const PLAN: &str = "The plan: migrate the store in three verified steps, each reversible.\n";

/// What the shipped `reviewer` role tells its judge — the persona text the
/// harness judge must receive when the member judges under its own persona.
const REVIEWER_BAR: &str = "You are a senior reviewer's editor.";

/// A persona file's own bar text, which the judge must receive in place of the
/// member's when `--persona` names that file.
const STRICT_BAR: &str = "You audit plans for an operator who has been burned before.";

/// The graph every journey but the adoption one runs: `planner`, judged by a
/// harness reviewer and an llmlint judge, and `auditor`, the same panel shape
/// under a different persona and a different harness routing — so a journey
/// can tell which member's routing a judgement went through. Each member's
/// task names the file its supervisor prompts are recorded in.
fn workspace() -> Workspace {
    let workspace = Workspace::new();
    let log = |name: &str| toml_edit::Value::from(workspace.at(name).display().to_string());
    workspace.write(
        "oneharness.toml",
        &format!(
            "{CHAIN}\n[env]\nFAKE_HARNESS_ATTEMPT_LOG = {}\n",
            log("agent.log")
        ),
    );
    workspace.write(
        "reviewer.judge.toml",
        &format!(
            "{CHAIN}\n[env]\nFAKE_HARNESS_ATTEMPT_LOG = {}\n",
            log("reviewer.log")
        ),
    );
    workspace.write(
        "auditor.judge.toml",
        &format!(
            "{CHAIN}\n[env]\nFAKE_HARNESS_ATTEMPT_LOG = {}\n",
            log("auditor.log")
        ),
    );
    workspace.write(
        "llmlint.yml",
        concat!(
            "plugins:\n",
            "  - ./rules/plan-rules.yml\n",
            "  - https://example.com/llmlint/shared.yml@1\n",
            "rules:\n  - id: no-todo\n    forbid: TODO\n",
        ),
    );
    workspace.write(
        "rules/plan-rules.yml",
        "rules:\n  - id: no-tbd\n    forbid: TBD\n",
    );
    workspace.write(
        "strict.yaml",
        &format!("name: strict\nuser:\n  persona: \"{STRICT_BAR}\"\n  done_when: \"every step is reversible\"\n"),
    );
    workspace.write("plan.md", PLAN);
    workspace.graph(&graph(&workspace));
    workspace
}

/// The graph document [`workspace`] writes.
fn graph(workspace: &Workspace) -> String {
    let prompts = |name: &str| workspace.at(name).display().to_string();
    graph_with(
        concat!(
            "version: 9\nname: review\n",
            "env:\n  FAKE_LLMLINT_VERDICT: rules\n",
            "members:\n",
            "  planner:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: reviewer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n",
            "      - oneharness_config: ./reviewer.judge.toml\n        label: reviewer\n",
            "      - kind: llmlint\n        config: ./llmlint.yml\n        bin: set below\n",
            "    mode: bypass\n",
            "  auditor:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n",
            "      - oneharness_config: ./auditor.judge.toml\n        label: auditor\n",
            "      - kind: llmlint\n        config: ./llmlint.yml\n        bin: set below\n",
            "    mode: bypass\n",
        ),
        &[
            (FAKE_HARNESS_KEY.to_string(), fake_harness()),
            (
                "env.FAKE_LLMLINT_ARGV_LOG".to_string(),
                workspace.at("llmlint-argv.log").display().to_string(),
            ),
            ("members.planner.judge.1.bin".to_string(), fake_llmlint()),
            ("members.auditor.judge.1.bin".to_string(), fake_llmlint()),
            (
                "members.planner.task".to_string(),
                format!(
                    "Review this plan. fake:record-supervisor-prompt={}",
                    prompts("planner-prompts.log")
                ),
            ),
            (
                "members.auditor.task".to_string(),
                format!(
                    "Audit this plan. fake:record-supervisor-prompt={}",
                    prompts("auditor-prompts.log")
                ),
            ),
        ],
    )
}

/// `run --judge-only` of `member` over `input`, plus `extra` flags.
fn judge(workspace: &Workspace, member: &str, input: &str, extra: &[&str]) -> Run {
    let mut args = vec![
        "run",
        "./graph.yaml",
        "--judge-only",
        "--member",
        member,
        "--input",
        input,
    ];
    args.extend_from_slice(extra);
    workspace.run(&args)
}

/// `run --judge-only --bar-fingerprint` of `member`, plus `extra` flags: the
/// digest it printed, having asserted it printed exactly one.
fn fingerprint(workspace: &Workspace, member: &str, extra: &[&str]) -> String {
    let mut args = vec![
        "run",
        "./graph.yaml",
        "--judge-only",
        "--member",
        member,
        "--bar-fingerprint",
    ];
    args.extend_from_slice(extra);
    let run = workspace.run(&args);
    run.expect_code(0);
    let digest = run.stdout.trim().to_string();
    assert!(
        digest.len() == 64 && digest.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
        "--bar-fingerprint printed {:?}",
        run.stdout
    );
    digest
}

/// The judged member's one `member-settled` payload.
fn settled(run: &Run) -> Value {
    let settled = run.of_kind("member-settled");
    assert_eq!(settled.len(), 1, "{:?}", run.kinds());
    settled[0]["payload"].clone()
}

// llmlint: ignore-block[tests_mirror_real_usage] the report is read at the path
// the run's own `member-settled` published (`report_path`), which is where the
// contract tells an operator the member's full onejudge report is; the doubles'
// recordings are read for the reason tests/e2e/panel.rs gives — the stream
// cannot hold what a judge was handed, and that is the assertion.
/// The member's onejudge report, read from where its settle said it is.
fn report(run: &Run) -> Value {
    let path = settled(run)["report_path"]
        .as_str()
        .expect("member-settled names its report")
        .to_string();
    serde_json::from_str(&std::fs::read_to_string(&path).expect("the report is stored"))
        .expect("the report is JSON")
}

/// How many times a doubled harness was launched under one routing, by the
/// attempt log that routing's config gives it.
fn launches(workspace: &Workspace, log: &str) -> usize {
    workspace.read(log).lines().count()
}

/// Every argv the llmlint double was run with.
fn llmlint_argvs(workspace: &Workspace) -> Vec<Vec<String>> {
    workspace
        .read("llmlint-argv.log")
        .lines()
        .map(|line| serde_json::from_str(line).expect("one argv per line"))
        .collect()
}
// llmlint: ignore-end[tests_mirror_real_usage]

/// The run streamed the judge side and nothing else: no agent turn opened, and
/// the agent's routing never launched a harness.
fn no_agent_side(workspace: &Workspace, run: &Run) {
    assert_eq!(
        launches(workspace, "agent.log"),
        0,
        "an agent-side harness ran"
    );
    for started in run.of_kind("turn-started") {
        assert_ne!(
            started["payload"]["role"], "assistant",
            "an agent turn opened: {started}"
        );
    }
}

/// Under the member's own persona — a built-in role — the judge is handed the
/// input and that role's bar, through the member's own routing alone; a passing
/// input exits 0 with the member's report on the stream, an input breaking a
/// rule a file the llmlint config includes declares exits 1 naming it, and a
/// harness judge that never answers exits 2.
#[test]
fn a_judge_only_run_under_the_members_own_role_reports_each_verdict_by_its_exit() {
    let workspace = workspace();

    let pass = judge(&workspace, "planner", "plan.md", &[]);
    pass.expect_code(0);
    no_agent_side(&workspace, &pass);
    assert_eq!(launches(&workspace, "reviewer.log"), 1);
    assert_eq!(
        launches(&workspace, "auditor.log"),
        0,
        "another member's routing ran"
    );
    let prompt = workspace.read("planner-prompts.log");
    assert!(
        prompt.contains(PLAN.trim()),
        "the judge never saw the input:\n{prompt}"
    );
    assert!(
        prompt.contains(REVIEWER_BAR),
        "the judge never saw the role's bar:\n{prompt}"
    );
    let payload = settled(&pass);
    assert_eq!(payload["completed"], Value::Bool(true), "{payload}");
    assert_eq!(
        pass.of_kind("member-settled")[0]["labels"]["persona"],
        "reviewer"
    );
    let passed = report(&pass);
    assert!(passed["completion_reason"].is_string(), "{passed}");
    assert_eq!(
        pass.of_kind("judge-decided")
            .iter()
            .map(|event| event["payload"]["decision"]
                .as_str()
                .unwrap_or_default()
                .to_string())
            .collect::<Vec<_>>(),
        ["done", "done"]
    );
    // The llmlint judge linted the input itself.
    let lint = llmlint_argvs(&workspace)
        .into_iter()
        .find(|argv| argv.first().map(String::as_str) == Some("lint"))
        .expect("the llmlint judge ran");
    assert!(
        lint.last().is_some_and(|file| file.ends_with("plan.md")),
        "{lint:?}"
    );
    // Only the judged member is recorded: the rest of the graph never started.
    let record = workspace.record();
    assert_eq!(
        record["members"]
            .as_object()
            .map(|members| members.keys().cloned().collect::<Vec<_>>()),
        Some(vec!["planner".to_string()]),
        "{record}"
    );

    workspace.write("tbd.md", "The plan: step one, then step two is TBD.\n");
    let fail = judge(&workspace, "planner", "tbd.md", &[]);
    fail.expect_code(1);
    no_agent_side(&workspace, &fail);
    assert!(fail.stderr.contains("no-tbd"), "{}", fail.stderr);
    assert!(fail.stderr.contains("failed its bar"), "{}", fail.stderr);
    let decided: Vec<(String, String)> = fail
        .of_kind("judge-decided")
        .iter()
        .map(|event| {
            let field = |name: &str| {
                event["payload"][name]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            };
            (field("judge"), field("decision"))
        })
        .collect();
    assert!(
        decided.contains(&("reviewer".into(), "done".into())),
        "{decided:?}"
    );
    assert!(
        decided
            .iter()
            .any(|(judge, decision)| judge != "reviewer" && decision != "done"),
        "{decided:?}"
    );
    assert!(report(&fail).to_string().contains("no-tbd"));

    workspace.write(
        "hang.md",
        "The plan, which the reviewer never answers: fake:hang\n",
    );
    let env = bounds("60", "2");
    let hang = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--judge-only",
            "--member",
            "planner",
            "--input",
            "hang.md",
        ],
        &as_env(&env),
    );
    hang.expect_code(2);
    no_agent_side(&workspace, &hang);
    assert!(hang.stderr.contains("could not judge"), "{}", hang.stderr);
    let died = hang.of_kind("member-died");
    assert_eq!(died.len(), 1, "{:?}", hang.kinds());
    assert_eq!(died[0]["payload"]["rule"], "activity", "{}", died[0]);
}

/// `--persona` naming a persona file replaces the member's bar with that file's,
/// resolved as a dispatch resolves one: the judge is handed the file's text and
/// not the role's, a pass exits 0, and a persona whose bar the doubled judge is
/// scripted to reject exits 1 with that judge's finding on stderr and in the
/// printed report.
#[test]
fn a_persona_file_named_on_the_command_line_is_the_bar_the_judge_applies() {
    let workspace = workspace();

    let pass = judge(
        &workspace,
        "planner",
        "plan.md",
        &["--persona", "./strict.yaml"],
    );
    pass.expect_code(0);
    no_agent_side(&workspace, &pass);
    let prompt = workspace.read("planner-prompts.log");
    assert!(prompt.contains(PLAN.trim()), "{prompt}");
    assert!(
        prompt.contains(STRICT_BAR),
        "the judge never saw the file's bar:\n{prompt}"
    );
    assert!(
        !prompt.contains(REVIEWER_BAR),
        "the member's own role leaked in:\n{prompt}"
    );
    assert_eq!(
        pass.of_kind("member-settled")[0]["labels"]["persona"],
        "strict"
    );

    // A bar the doubled reviewer is scripted to reject: its sentinel is in the
    // persona file alone, so the input that passed above now fails.
    workspace.write(
        "rejecting.yaml",
        "name: rejecting\nuser:\n  persona: \"Reject until verified. fake:should-fail\"\n",
    );
    let fail = judge(
        &workspace,
        "planner",
        "plan.md",
        &["--persona", "./rejecting.yaml"],
    );
    fail.expect_code(1);
    no_agent_side(&workspace, &fail);
    assert!(
        fail.stderr
            .contains("fake supervisor requires another turn"),
        "{}",
        fail.stderr
    );
    let rejected = report(&fail);
    assert!(
        rejected
            .to_string()
            .contains("fake supervisor requires another turn"),
        "{rejected}"
    );
    assert_eq!(settled(&fail)["completed"], Value::Bool(false));
}

/// A member's judgement runs under that member's own harness routing: judging
/// `auditor` reaches the harness its routing names and never `planner`'s — and
/// the input may arrive on stdin.
#[test]
fn each_member_is_judged_under_its_own_harness_routing() {
    let workspace = workspace();
    let mut child = workspace.spawn_with_open_stdin(
        &[
            "run",
            "./graph.yaml",
            "--judge-only",
            "--member",
            "auditor",
            "--input",
            "-",
        ],
        &[],
    );
    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().expect("stdin");
        stdin
            .write_all(PLAN.as_bytes())
            .expect("the input is written");
    }
    let output = child.wait_with_output().expect("the run finishes");
    let run = Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };
    run.expect_code(0);
    no_agent_side(&workspace, &run);
    assert_eq!(launches(&workspace, "auditor.log"), 1);
    assert_eq!(
        launches(&workspace, "reviewer.log"),
        0,
        "planner's routing ran"
    );
    let prompt = workspace.read("auditor-prompts.log");
    assert!(prompt.contains(PLAN.trim()), "{prompt}");
    assert!(workspace.read("planner-prompts.log").is_empty());

    // A member with no judge side is refused before anything is created.
    workspace.write(
        "graph.yaml",
        &graph_with(
            concat!(
                "version: 9\nname: review\nenv: {}\n",
                "members:\n  reporter:\n    kind: oneharness\n",
                "    oneharness_config: ./oneharness.toml\n",
            ),
            &[(FAKE_HARNESS_KEY, fake_harness())],
        ),
    );
    let refused = judge(&workspace, "reporter", "plan.md", &[]);
    refused.expect_code(2);
    assert!(
        refused.stderr.contains("no judge side"),
        "{}",
        refused.stderr
    );
    assert!(refused.stdout.is_empty(), "{}", refused.stdout);
}

/// `--bar-fingerprint` prints the digest a judge-only run's settle carries; it
/// moves with the member's judge configuration, its llmlint config, a rule in a
/// file that config includes, a plugin pin, and the resolved persona — and not
/// with the input, nor with anything another member of the graph changes. It
/// makes no harness call, starts no judge, and records no run.
#[test]
fn the_bar_fingerprint_moves_exactly_when_the_bar_does() {
    let workspace = workspace();
    let base = fingerprint(&workspace, "planner", &[]);
    assert_eq!(
        launches(&workspace, "reviewer.log"),
        0,
        "a fingerprint ran a harness"
    );
    let argvs = llmlint_argvs(&workspace);
    assert!(
        !argvs.is_empty() && argvs.iter().all(|argv| argv[0] == "fingerprint"),
        "a fingerprint started a judge: {argvs:?}"
    );
    assert_eq!(
        std::fs::read_dir(workspace.state()).expect("state").count(),
        0,
        "a fingerprint recorded a run"
    );
    assert_eq!(base, fingerprint(&workspace, "planner", &[]), "not stable");

    // The digest the run judged against is the one printed, whatever the input.
    for input in ["plan.md", "other.md"] {
        workspace.write(
            "other.md",
            "A different plan, judged against the same bar.\n",
        );
        let run = judge(&workspace, "planner", input, &[]);
        run.expect_code(0);
        assert_eq!(
            settled(&run)["bar_fingerprint"],
            Value::String(base.clone())
        );
    }

    // Nothing another member changes moves this member's bar.
    for set in [
        "members.auditor.persona=planner",
        "members.auditor.judge.0.label=lead",
        "members.auditor.judge.0.oneharness_config=./reviewer.judge.toml",
    ] {
        assert_eq!(
            fingerprint(&workspace, "planner", &["--set", set]),
            base,
            "{set} moved another member's bar"
        );
    }
    let auditor = workspace.read("auditor.judge.toml");
    workspace.write("auditor.judge.toml", &format!("{auditor}\n# rerouted\n"));
    assert_eq!(
        fingerprint(&workspace, "planner", &[]),
        base,
        "auditor's routing"
    );
    workspace.write("auditor.judge.toml", &auditor);

    // Everything the bar is made of does.
    let mut seen = vec![base.clone()];
    let mut moved = |what: &str, digest: String| {
        assert!(!seen.contains(&digest), "{what} did not move the bar");
        seen.push(digest);
    };
    moved(
        "the member's judge config",
        fingerprint(
            &workspace,
            "planner",
            &["--set", "members.planner.judge.0.label=lead"],
        ),
    );
    moved(
        "the member's persona",
        fingerprint(&workspace, "planner", &["--persona", "engineer"]),
    );
    moved(
        "a persona file",
        fingerprint(&workspace, "planner", &["--persona", "./strict.yaml"]),
    );
    let strict = workspace.read("strict.yaml");
    workspace.write(
        "strict.yaml",
        &strict.replace("reversible", "reversible and tested"),
    );
    moved(
        "an edited persona file",
        fingerprint(&workspace, "planner", &["--persona", "./strict.yaml"]),
    );
    restore_then(
        &workspace,
        "reviewer.judge.toml",
        "\n# rerouted\n",
        |digest| {
            moved("the member's judge routing", digest);
        },
    );
    restore_then(&workspace, "llmlint.yml", "\n# tightened\n", |digest| {
        moved("the llmlint config", digest);
    });
    restore_then(
        &workspace,
        "rules/plan-rules.yml",
        "  - id: no-fixme\n    forbid: FIXME\n",
        |digest| {
            moved("a rule in an included file", digest);
        },
    );
    let config = workspace.read("llmlint.yml");
    workspace.write(
        "llmlint.yml",
        &config.replace("shared.yml@1", "shared.yml@2"),
    );
    moved("a plugin pin", fingerprint(&workspace, "planner", &[]));
    workspace.write("llmlint.yml", &config);
    assert_eq!(fingerprint(&workspace, "planner", &[]), base, "restored");
}

/// Append `addition` to `file`, hand the fingerprint that results to `check`,
/// and put the file back.
fn restore_then(workspace: &Workspace, file: &str, addition: &str, check: impl FnOnce(String)) {
    let original = workspace.read(file);
    workspace.write(file, &format!("{original}{addition}"));
    check(fingerprint(workspace, "planner", &[]));
    workspace.write(file, &original);
}

/// Adoptable by configuration alone: a repository with its own graph, persona
/// catalog, onejudge base, judge routing and llmlint rules — laid out its own
/// way, and holding nothing of any other orchestrator's — runs the judge-only
/// journey over its own draft under its own catalog's persona, by name.
#[test]
fn a_repository_of_its_own_configures_a_judge_only_review_by_configuration_alone() {
    let workspace = Workspace::new();
    let repo = |path: &str| format!("acme/{path}");
    workspace.write(
        &repo("config/routing/agent.toml"),
        &format!(
            "{CHAIN}\n[env]\nFAKE_HARNESS_ATTEMPT_LOG = {}\n",
            toml_edit::Value::from(workspace.at("acme-agent.log").display().to_string())
        ),
    );
    workspace.write(&repo("config/routing/design-judge.toml"), CHAIN);
    workspace.write(
        &repo("config/onejudge.base.yaml"),
        "system_prompt: |\n  Acme house rules.\nuser:\n  done_when: \"the design is ready to build\"\n",
    );
    workspace.write(
        &repo("config/personas/design-judge.yaml"),
        "name: design-judge\nuser:\n  persona: \"You judge Acme design documents against the house template.\"\n",
    );
    workspace.write(
        &repo("config/lint/design.llmlint.yml"),
        "rules:\n  - id: no-open-questions\n    forbid: \"OPEN QUESTION\"\n",
    );
    workspace.write(
        &repo("drafts/design.md"),
        "# Design\n\nThe store migrates in three reversible steps.\n",
    );
    let prompts = workspace.at("acme-prompts.log");
    workspace.write(
        &repo("config/graphs/design.yaml"),
        &graph_with(
            concat!(
                "version: 9\nname: design-doc\n",
                "personas: ../personas\n",
                "env:\n  FAKE_LLMLINT_VERDICT: rules\n",
                "members:\n  writer:\n    kind: onejudge\n",
                "    base_config: ../onejudge.base.yaml\n",
                "    agent:\n      oneharness_config: ../routing/agent.toml\n",
                "    judge:\n",
                "      - oneharness_config: ../routing/design-judge.toml\n        label: design\n",
                "      - kind: llmlint\n        config: ../lint/design.llmlint.yml\n",
                "        bin: set below\n",
                "    mode: bypass\n",
            ),
            &[
                (FAKE_HARNESS_KEY.to_string(), fake_harness()),
                ("members.writer.judge.1.bin".to_string(), fake_llmlint()),
                (
                    "members.writer.task".to_string(),
                    format!(
                        "Write the design document. fake:record-supervisor-prompt={}",
                        prompts.display()
                    ),
                ),
            ],
        ),
    );
    let graph = repo("config/graphs/design.yaml");
    let draft = repo("drafts/design.md");
    let args = [
        "run",
        graph.as_str(),
        "--judge-only",
        "--member",
        "writer",
        "--persona",
        "design-judge",
        "--input",
        draft.as_str(),
    ];
    let run = workspace.run(&args);
    run.expect_code(0);
    assert_eq!(workspace.read("acme-agent.log").lines().count(), 0);
    let prompt = std::fs::read_to_string(&prompts).expect("the judge was prompted");
    assert!(prompt.contains("three reversible steps"), "{prompt}");
    assert!(prompt.contains("against the house template"), "{prompt}");
    let digest = settled(&run)["bar_fingerprint"].clone();
    let printed = workspace.run(&[
        "run",
        graph.as_str(),
        "--judge-only",
        "--member",
        "writer",
        "--persona",
        "design-judge",
        "--bar-fingerprint",
    ]);
    printed.expect_code(0);
    assert_eq!(Value::String(printed.stdout.trim().to_string()), digest);

    workspace.write(&draft, "# Design\n\nOPEN QUESTION: which store?\n");
    let fail = workspace.run(&args);
    fail.expect_code(1);
    assert!(fail.stderr.contains("no-open-questions"), "{}", fail.stderr);
}
