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

use std::path::PathBuf;

use serde_json::Value;

use crate::support::{
    as_env, bounds, fake_harness, fake_llmlint, fake_provider, graph_with, until, Run, Workspace,
    CHAIN, FAKE_HARNESS_KEY,
};

/// The material every passing journey judges.
const PLAN: &str = "The plan: migrate the store in three verified steps, each reversible.\n";

/// What the shipped `reviewer` role tells its judge — the persona text the
/// harness judge must receive when the member judges under its own persona.
const REVIEWER_BAR: &str = "You are a senior reviewer's editor.";

/// A persona file's own bar text, which the judge must receive in place of the
/// member's when `--persona` names that file.
const STRICT_BAR: &str = "You audit plans for an operator who has been burned before.";

/// The boolean eval the `evaluated` persona carries, which onejudge scores
/// against a judge-only transcript as it does a normal run's.
const STRICT_EVAL: &str = "the plan names a rollback for every step";

/// onejudge's own task for judge-only material that names none — what a member
/// with no `task` of its own is judged against when the run gives none either.
const DEFAULT_TASK: &str = "Produce the material under review.";

/// The graph every journey but the adoption one runs: `planner`, judged by a
/// harness reviewer and an llmlint judge, and `auditor`, the same panel shape
/// under a different persona and a different harness routing — so a journey
/// can tell which member's routing a judgement went through. Each of those
/// two members' tasks names the file its supervisor prompts are recorded in.
/// `solo` is judged by one bare harness side and carries no task of its own.
/// The planner's llmlint judge passes extra `args`: a `--label`, which shapes
/// a run, and a `--prompt-template`, a `--model`, `--rationales` and a second
/// `--config=`, which decide a verdict.
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
        "solo.judge.toml",
        &format!(
            "{CHAIN}\n[env]\nFAKE_HARNESS_ATTEMPT_LOG = {}\n",
            log("solo.log")
        ),
    );
    workspace.write("prompt.md", "Judge each rule strictly.\n");
    workspace.write("extra-llmlint.yml", "rules: []\n");
    workspace.write("short-llmlint.yml", "rules: []\n");
    // `scoped` works in a directory of its own, whose llmlint judge names no
    // config and so discovers the one there.
    workspace.write(
        "scoped/llmlint.yml",
        "rules:\n  - id: no-wip\n    forbid: WIP\n",
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
        &format!(
            "name: strict\nuser:\n  persona: \"{STRICT_BAR}\"\n  done_when: \"every step is reversible\"\n"
        ),
    );
    workspace.write(
        "evaluated.yaml",
        &format!(
            "name: evaluated\nuser:\n  persona: \"Check the rollbacks.\"\nevals:\n  - criterion: \"{STRICT_EVAL}\"\n    kind: boolean\n"
        ),
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
            "version: 11\nname: review\n",
            "env:\n  FAKE_LLMLINT_VERDICT: rules\n",
            "members:\n",
            "  planner:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: reviewer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n",
            "      - oneharness_config: ./reviewer.judge.toml\n        label: reviewer\n",
            "      - kind: llmlint\n        config: ./llmlint.yml\n        bin: set below\n",
            "        args: [--label, team=docs, --prompt-template, set below,\n",
            "               --model, judge-model, --rationales, set below,\n",
            "               --strictness, high, set below]\n",
            "    mode: bypass\n",
            "  auditor:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n",
            "      - oneharness_config: ./auditor.judge.toml\n        label: auditor\n",
            "      - kind: llmlint\n        config: ./llmlint.yml\n        bin: set below\n",
            "    mode: bypass\n",
            "  scoped:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    dir: ./scoped\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n",
            "      - oneharness_config: ./solo.judge.toml\n",
            "      - kind: llmlint\n        bin: set below\n",
            "    mode: bypass\n",
            "  solo:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./solo.judge.toml\n",
            "    mode: bypass\n",
        ),
        &[
            (FAKE_HARNESS_KEY.to_string(), fake_harness()),
            (
                "env.FAKE_LLMLINT_ARGV_LOG".to_string(),
                workspace.at("llmlint-argv.log").display().to_string(),
            ),
            ("members.planner.judge.1.bin".to_string(), fake_llmlint()),
            (
                "members.planner.judge.1.args.3".to_string(),
                workspace.at("prompt.md").display().to_string(),
            ),
            (
                "members.planner.judge.1.args.7".to_string(),
                format!("--config={}", workspace.at("extra-llmlint.yml").display()),
            ),
            ("members.auditor.judge.1.bin".to_string(), fake_llmlint()),
            ("members.scoped.judge.1.bin".to_string(), fake_llmlint()),
            (
                "members.planner.judge.1.args.10".to_string(),
                format!("-c{}", workspace.at("short-llmlint.yml").display()),
            ),
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

    // Every judge that did not pass is named, not only the first: the same
    // rejecting bar over material breaking a fixture rule fails both judges,
    // and the bar's own verdict besides.
    workspace.write("tbd.md", "The plan: step one, then step two is TBD.\n");
    let both = judge(
        &workspace,
        "planner",
        "tbd.md",
        &["--persona", "./rejecting.yaml"],
    );
    both.expect_code(1);
    let findings: Vec<&str> = both
        .stderr
        .lines()
        .filter(|line| line.starts_with("  - "))
        .collect();
    assert!(
        findings
            .iter()
            .any(|line| line.starts_with("  - reviewer (oneharness) continue")),
        "{}",
        both.stderr
    );
    assert!(
        findings
            .iter()
            .any(|line| line.starts_with("  - llmlint (llmlint) continue")),
        "{}",
        both.stderr
    );
    assert!(both.stderr.contains("no-tbd"), "{}", both.stderr);
    assert!(findings.len() >= 3, "{}", both.stderr);

    // A persona's own boolean eval is scored against the judge-only
    // transcript, and one that fails is a finding on stderr like any judge's.
    // On `solo`, whose one harness judge scores it: an llmlint judge would lint
    // the worktree for an eval, which the `rules` fixture does not.
    workspace.write(
        "risky.md",
        "The plan: drop the old table first. fake:should-fail\n",
    );
    let eval = judge(
        &workspace,
        "solo",
        "risky.md",
        &["--persona", "./evaluated.yaml"],
    );
    eval.expect_code(1);
    let finding = eval
        .stderr
        .lines()
        .find(|line| line.contains(&format!("{STRICT_EVAL}: ")))
        .unwrap_or_else(|| panic!("no finding for the eval: {}", eval.stderr));
    assert!(finding.contains("fake judge verdict"), "{finding}");
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

    // A request that cannot be judged is refused before anything starts: a
    // member the graph does not have, no input at all, an input that cannot be
    // read — each exit 2, saying which, with nothing streamed or launched.
    let launched = launches(&workspace, "reviewer.log") + launches(&workspace, "auditor.log");
    let recorded = std::fs::read_dir(workspace.state()).expect("state").count();
    for (args, says) in [
        (
            &["--member", "nobody", "--input", "plan.md"][..],
            "has no such member",
        ),
        (&["--member", "planner"][..], "needs --input"),
        (
            &["--member", "planner", "--input", "no-such-plan.md"][..],
            "cannot read --input",
        ),
    ] {
        let mut argv = vec!["run", "./graph.yaml", "--judge-only"];
        argv.extend_from_slice(args);
        let refused = workspace.run(&argv);
        refused.expect_code(2);
        assert!(
            refused.stderr.contains(says),
            "{args:?}: {}",
            refused.stderr
        );
        assert!(refused.stdout.is_empty(), "{args:?}: {}", refused.stdout);
    }
    assert_eq!(
        launches(&workspace, "reviewer.log") + launches(&workspace, "auditor.log"),
        launched,
        "a refused request launched a judge"
    );
    assert_eq!(
        std::fs::read_dir(workspace.state()).expect("state").count(),
        recorded,
        "a refused request recorded a run"
    );

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

    // Nor does anything of the member's own that is not its bar: the task it
    // is judged under, its turn cap, or its agent side's routing.
    let tasked = workspace.run(&[
        "run",
        "./graph.yaml",
        "--judge-only",
        "--member",
        "planner",
        "--input",
        "plan.md",
        "--task",
        "A different task over the same bar.",
    ]);
    tasked.expect_code(0);
    assert_eq!(
        settled(&tasked)["bar_fingerprint"],
        Value::String(base.clone())
    );
    assert_eq!(
        fingerprint(
            &workspace,
            "planner",
            &["--set", "members.planner.max_turns=9"]
        ),
        base,
        "the member's turn cap"
    );
    let agent = workspace.read("oneharness.toml");
    workspace.write(
        "oneharness.toml",
        &format!("{agent}\n# the agent rerouted\n"),
    );
    assert_eq!(
        fingerprint(&workspace, "planner", &[]),
        base,
        "agent routing"
    );
    workspace.write("oneharness.toml", &agent);

    // The llmlint judge was fingerprinted with the options of its `args` that
    // `fingerprint` lists, and not the ones it does not.
    let probe = llmlint_argvs(&workspace)
        .into_iter()
        .find(|argv| argv[0] == "fingerprint" && argv.iter().all(|arg| arg != "--help"))
        .expect("a fingerprint ran");
    for forwarded in [
        "--prompt-template",
        "--model",
        "judge-model",
        "--rationales",
    ] {
        assert!(
            probe.iter().any(|arg| arg == forwarded),
            "{forwarded}: {probe:?}"
        );
    }
    assert!(
        probe.iter().any(|arg| arg.starts_with("--config=")),
        "{probe:?}"
    );
    assert!(probe.iter().all(|arg| arg != "--label"), "{probe:?}");

    // Nor does how a document happens to order its keys: the same base with
    // its top-level keys, and the keys under `user`, written the other way
    // round is the same bar.
    let ordered = workspace.read("base.yaml");
    workspace.write(
        "base.yaml",
        concat!(
            "user:\n  max_turns: 4\n  done_when: \"the task is complete\"\n",
            "system_prompt: |\n  Standing bar: verify before you claim done.\n",
            "provider:\n  kind: oneharness\n",
        ),
    );
    assert_eq!(fingerprint(&workspace, "planner", &[]), base, "key order");
    workspace.write("base.yaml", &ordered);

    // Nor do the parts of the base config a judge never reads: its provider
    // block, session, task, skill, and its turn cap.
    restore_then(
        &workspace,
        "base.yaml",
        "session: elsewhere\ntask: ignored\nskill: ./skills/none\n",
        |digest| assert_eq!(digest, base, "the base's session, task or skill"),
    );
    let shared = workspace.read("base.yaml");
    workspace.write(
        "base.yaml",
        &shared
            .replace("max_turns: 4", "max_turns: 7")
            .replace("kind: oneharness", "kind: oneharness\n  bin: elsewhere"),
    );
    assert_eq!(
        fingerprint(&workspace, "planner", &[]),
        base,
        "the base's cap or provider"
    );
    workspace.write("base.yaml", &shared);

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
    moved(
        "the member's judge settings",
        fingerprint(
            &workspace,
            "planner",
            &[
                "--set",
                "members.planner.judge_settings.allow_writable_judges=true",
            ],
        ),
    );
    let graph = workspace.read("graph.yaml");
    assert!(graph.contains("judge-model"), "{graph}");
    workspace.graph(&graph.replace("judge-model", "other-model"));
    moved(
        "the llmlint judge's model",
        fingerprint(&workspace, "planner", &[]),
    );
    workspace.graph(&graph);
    restore_then(&workspace, "extra-llmlint.yml", "# stricter\n", |digest| {
        moved("a second llmlint config", digest);
    });
    restore_then(&workspace, "short-llmlint.yml", "# stricter\n", |digest| {
        moved("a config named by an attached -c", digest);
    });
    restore_then(&workspace, "prompt.md", "Cite the line.\n", |digest| {
        moved("the llmlint judge's prompt template", digest);
    });
    restore_then(
        &workspace,
        "base.yaml",
        "evals:\n  - criterion: tested\n",
        |digest| {
            moved("the base config's bar", digest);
        },
    );
    let config = workspace.read("llmlint.yml");
    workspace.write(
        "llmlint.yml",
        &config.replace("shared.yml@1", "shared.yml@2"),
    );
    moved("a plugin pin", fingerprint(&workspace, "planner", &[]));
    // A config the llmlint judge's own `fingerprint` refuses is a bar that
    // cannot be digested: exit 2, naming the judge entry, and no digest.
    let garbled = workspace.run(&[
        "run",
        "./graph.yaml",
        "--judge-only",
        "--member",
        "planner",
        "--bar-fingerprint",
        "--set",
        "env.FAKE_LLMLINT_FINGERPRINT=garbled",
    ]);
    garbled.expect_code(2);
    assert!(garbled.stdout.is_empty(), "{}", garbled.stdout);
    assert!(
        garbled.stderr.contains("not one SHA-256 digest"),
        "{}",
        garbled.stderr
    );
    // Which args reach `fingerprint` follows its help as listed: one a later
    // llmlint lists is forwarded, one it does not is not, and a help that
    // fails, lists nothing, or cannot be run at all is a bar not digested.
    let probes = || llmlint_argvs(&workspace).len();
    let before = probes();
    fingerprint(
        &workspace,
        "planner",
        &["--set", "env.FAKE_LLMLINT_FINGERPRINT=help-wider"],
    );
    let wider = llmlint_argvs(&workspace)
        .into_iter()
        .skip(before)
        .find(|argv| argv[0] == "fingerprint" && argv.iter().all(|arg| arg != "--help"))
        .expect("a fingerprint ran");
    assert!(
        wider
            .windows(2)
            .any(|pair| pair == ["--strictness", "high"]),
        "{wider:?}"
    );
    assert!(
        !probe.iter().any(|arg| arg == "--strictness"),
        "forwarded an option the help does not list: {probe:?}"
    );
    for (knob, says) in [
        ("help-fails", "fingerprint --help` failed"),
        ("help-blank", "listed no option"),
    ] {
        let refused = workspace.run(&[
            "run",
            "./graph.yaml",
            "--judge-only",
            "--member",
            "planner",
            "--bar-fingerprint",
            "--set",
            &format!("env.FAKE_LLMLINT_FINGERPRINT={knob}"),
        ]);
        refused.expect_code(2);
        assert!(refused.stdout.is_empty(), "{knob}: {}", refused.stdout);
        assert!(refused.stderr.contains(says), "{knob}: {}", refused.stderr);
    }
    let unspawnable = workspace.run(&[
        "run",
        "./graph.yaml",
        "--judge-only",
        "--member",
        "planner",
        "--bar-fingerprint",
        "--set",
        &format!(
            "members.planner.judge.1.bin={}",
            workspace.unreachable_harness()
        ),
    ]);
    unspawnable.expect_code(2);
    assert!(
        unspawnable.stderr.contains("could not run"),
        "{}",
        unspawnable.stderr
    );

    workspace.write("llmlint.yml", "plugins: ./not-a-list.yml\n");
    let refused = workspace.run(&[
        "run",
        "./graph.yaml",
        "--judge-only",
        "--member",
        "planner",
        "--bar-fingerprint",
    ]);
    refused.expect_code(2);
    assert!(refused.stdout.is_empty(), "{}", refused.stdout);
    assert!(
        refused.stderr.contains("judge entry 2") && refused.stderr.contains("must be a list"),
        "{}",
        refused.stderr
    );
    workspace.write("llmlint.yml", &config);
    assert_eq!(fingerprint(&workspace, "planner", &[]), base, "restored");
}

/// `--artifact` replaces the artifacts the bar names, and beats onejudge's own
/// `ONEJUDGE_ARTIFACTS`: the judge is pointed at the ones given, and at neither
/// the persona's nor the environment's.
#[test]
fn artifacts_named_on_the_command_line_replace_the_bars_own() {
    let workspace = workspace();
    workspace.write("persona-notes.md", "Notes the persona points at.\n");
    workspace.write("given-notes.md", "Notes the caller points at.\n");
    workspace.write(
        "noted.yaml",
        "name: noted\nuser:\n  persona: \"Read the notes.\"\n  artifacts: [persona-notes.md]\n",
    );
    let pointed = judge(
        &workspace,
        "planner",
        "plan.md",
        &["--persona", "./noted.yaml"],
    );
    pointed.expect_code(0);
    assert!(
        workspace
            .read("planner-prompts.log")
            .contains("persona-notes.md"),
        "the bar's own artifact never reached the judge"
    );
    std::fs::remove_file(workspace.at("planner-prompts.log")).expect("cleared");

    // onejudge's own environment variable replaces the bar's artifacts …
    workspace.write("env-notes.md", "Notes the environment points at.\n");
    let from_env = ["--set", "env.ONEJUDGE_ARTIFACTS=env-notes.md"];
    let mut args = vec!["--persona", "./noted.yaml"];
    args.extend_from_slice(&from_env);
    judge(&workspace, "planner", "plan.md", &args).expect_code(0);
    let prompt = workspace.read("planner-prompts.log");
    assert!(prompt.contains("env-notes.md"), "{prompt}");
    std::fs::remove_file(workspace.at("planner-prompts.log")).expect("cleared");

    // … and `--artifact` replaces both.
    args.extend_from_slice(&["--artifact", "given-notes.md"]);
    let given = judge(&workspace, "planner", "plan.md", &args);
    given.expect_code(0);
    let prompt = workspace.read("planner-prompts.log");
    assert!(prompt.contains("given-notes.md"), "{prompt}");
    assert!(!prompt.contains("persona-notes.md"), "{prompt}");
    assert!(!prompt.contains("env-notes.md"), "{prompt}");
}

/// A member that depends on another, runs on a schedule, carries scheduled
/// notes and is background is still judged once, now: none of that belongs to
/// one judgement of it, so its dependency never runs, its schedule's delay is
/// never waited out, and no note's clock fires into the judgement.
#[test]
fn a_scheduled_dependent_member_is_judged_once_and_at_once() {
    let workspace = workspace();
    let mut document: Value =
        oneagentgraph::yaml::from_str(&workspace.read("graph.yaml")).expect("the graph");
    let planner = &mut document["members"]["planner"];
    planner["deps"] = serde_json::json!(["auditor"]);
    planner["schedule"] = serde_json::json!({"every": 3600, "start_after": 3600});
    planner["background"] = Value::Bool(true);
    // A note whose clock would fire at once, carrying the double's rejection:
    // were it delivered, the judge would reject and a firing would be streamed.
    planner["scheduled_notes"] = serde_json::json!([{
        "id": "nudge",
        "every": 1,
        "start_after": 0,
        "addressee": "supervisor",
        "text": "Reject this. fake:should-fail",
    }]);
    workspace.graph(&oneagentgraph::yaml::to_string(&document).expect("serializes"));

    // The judge's decision is held open past the note's first firings, so a
    // clock that was started would be seen firing into it.
    let gate = workspace.at("decide");
    workspace.write(
        "held.md",
        &format!("{PLAN}fake:supervisor-hold={}\n", gate.display()),
    );
    let started = std::time::Instant::now();
    let child = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--judge-only",
            "--member",
            "planner",
            "--input",
            "held.md",
        ],
        &[],
    );
    let entered = workspace.at("decide.entered");
    until("the judge to start deciding", || entered.exists());
    std::thread::sleep(std::time::Duration::from_millis(2500));
    workspace.write("decide", "go");
    let output = child.wait_with_output().expect("the run finishes");
    let run = Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };
    run.expect_code(0);
    assert!(started.elapsed() < std::time::Duration::from_secs(120));
    assert_eq!(launches(&workspace, "reviewer.log"), 1);
    assert_eq!(launches(&workspace, "auditor.log"), 0, "a dependency ran");
    assert_eq!(run.of_kind("member-started").len(), 1, "{:?}", run.kinds());
    for kind in ["cron-fired", "note-admitted"] {
        assert!(
            run.of_kind(kind).is_empty(),
            "a scheduled note ran: {:?}",
            run.kinds()
        );
    }
}

/// A judge whose answer never parses as a decision reaches no judgement:
/// onejudge settles on its `settled_reason`, and that is exit 2 with the reason
/// on stderr — never a fail with nothing to act on.
#[test]
fn a_judge_whose_answer_never_parses_could_not_judge() {
    let workspace = workspace();
    workspace.write(
        "evasive.md",
        "The plan, which the judge will not decide on. fake:unparseable\n",
    );
    let run = judge(&workspace, "solo", "evasive.md", &[]);
    run.expect_code(2);
    assert!(run.stderr.contains("could not judge"), "{}", run.stderr);
    assert!(run.stderr.contains("did not parse"), "{}", run.stderr);
    let report = report(&run);
    assert!(report["settled_reason"].is_string(), "{report}");

    // A plan onejudge refuses to build starts no judge at all: the member is
    // unstartable, and that is exit 2 too.
    let before = launches(&workspace, "solo.log");
    let refused = judge(
        &workspace,
        "solo",
        "plan.md",
        &["--set", "env.ONEJUDGE_MAX_TURNS=lots"],
    );
    refused.expect_code(2);
    assert!(
        refused.stderr.contains("could not start"),
        "{}",
        refused.stderr
    );
    assert_eq!(launches(&workspace, "solo.log"), before, "a judge started");
}

/// The run's own flags mean what they mean for `run`: the task the material
/// answers (onejudge's default when neither the member nor the run names
/// one), the directory, the labels, the filter, and the text rendering.
#[test]
fn a_judge_only_run_takes_the_runs_own_flags() {
    let workspace = workspace();
    let prompts = workspace.at("solo-prompts.log");
    workspace.write(
        "recorded.md",
        &format!(
            "The plan, step by step. fake:record-supervisor-prompt={}\n",
            prompts.display()
        ),
    );

    let filtered = judge(
        &workspace,
        "solo",
        "recorded.md",
        &[
            "--label",
            "review=pre-launch",
            "--event-filter",
            r#"{"include":[{"kind":"member-settled"}]}"#,
        ],
    );
    filtered.expect_code(0);
    assert_eq!(filtered.kinds(), ["member-settled"], "{}", filtered.stdout);
    assert_eq!(
        filtered.of_kind("member-settled")[0]["labels"]["review"],
        "pre-launch"
    );
    let prompt = std::fs::read_to_string(&prompts).expect("the judge was prompted");
    assert!(prompt.contains(DEFAULT_TASK), "{prompt}");
    std::fs::remove_file(&prompts).expect("cleared");

    let dir = workspace.dir().display().to_string();
    let tasked = judge(
        &workspace,
        "solo",
        "recorded.md",
        &["--task", "Judge the migration plan.", "--dir", &dir],
    );
    tasked.expect_code(0);
    let prompt = std::fs::read_to_string(&prompts).expect("the judge was prompted");
    assert!(prompt.contains("Judge the migration plan."), "{prompt}");
    assert!(!prompt.contains(DEFAULT_TASK), "{prompt}");
    assert_eq!(
        tasked.of_kind("member-started")[0]["payload"]["worktree"],
        Value::String(dir)
    );

    let text = judge(&workspace, "solo", "plan.md", &["--output", "text"]);
    text.expect_code(0);
    assert!(!text.stdout.trim().is_empty());
    // Rendered, not NDJSON: the first line is no envelope, and the settle is
    // drawn by its kind.
    let first = text.stdout.lines().next().unwrap_or_default();
    assert!(serde_json::from_str::<Value>(first).is_err(), "{first}");
    assert!(text.stdout.contains("member-settled"), "{}", text.stdout);
}

/// An llmlint judge is fingerprinted from its member's own worktree, so a
/// member working in a directory of its own digests the config found there —
/// not the one beside the caller.
#[test]
fn an_llmlint_judge_is_fingerprinted_from_its_members_own_directory() {
    let workspace = workspace();
    let base = fingerprint(&workspace, "scoped", &[]);
    let probe = llmlint_argvs(&workspace)
        .into_iter()
        .find(|argv| argv[0] == "fingerprint" && argv.iter().all(|arg| arg != "--help"))
        .expect("a fingerprint ran");
    let cwd = probe
        .iter()
        .position(|arg| arg == "--cwd")
        .map(|at| PathBuf::from(&probe[at + 1]))
        .unwrap_or_else(|| panic!("no --cwd: {probe:?}"));
    assert_eq!(
        cwd.canonicalize().expect("the --cwd exists"),
        workspace
            .at("scoped")
            .canonicalize()
            .expect("scoped exists"),
        "{probe:?}"
    );
    let caller = workspace.read("llmlint.yml");
    workspace.write("llmlint.yml", &format!("{caller}# the caller's own\n"));
    assert_eq!(
        fingerprint(&workspace, "scoped", &[]),
        base,
        "the caller's directory's config"
    );
    workspace.write("llmlint.yml", &caller);
    let own = workspace.read("scoped/llmlint.yml");
    workspace.write("scoped/llmlint.yml", &format!("{own}# tightened\n"));
    assert_ne!(
        fingerprint(&workspace, "scoped", &[]),
        base,
        "the member's own directory's config did not move its bar"
    );
}

/// A command judge judges a judge-only input through the same driver as any
/// other judge, and its command line is part of the bar; and the settle of an
/// ordinary run of the same member carries no `bar_fingerprint` at all.
#[test]
fn a_command_judge_is_judged_only_too_and_an_ordinary_settle_names_no_bar() {
    let workspace = workspace();
    let record = workspace.at("provider.log");
    let mut document: Value =
        oneagentgraph::yaml::from_str(&workspace.read("graph.yaml")).expect("the graph");
    document["env"]["FAKE_PROVIDER_RECORD"] = Value::String(record.display().to_string());
    document["members"]["checked"] = serde_json::json!({
        "kind": "onejudge",
        "base_config": "./base.yaml",
        "agent": {"oneharness_config": "./oneharness.toml"},
        "judge": [{"command": [fake_provider()], "label": "checks"}],
        "mode": "bypass",
    });
    workspace.graph(&oneagentgraph::yaml::to_string(&document).expect("serializes"));

    let base = fingerprint(&workspace, "checked", &[]);
    let run = judge(&workspace, "checked", "plan.md", &[]);
    run.expect_code(0);
    no_agent_side(&workspace, &run);
    assert_eq!(
        settled(&run)["bar_fingerprint"],
        Value::String(base.clone())
    );
    let decided = run.of_kind("judge-decided");
    assert_eq!(decided.len(), 1, "{:?}", run.kinds());
    assert_eq!(decided[0]["payload"]["kind"], "command");
    assert_eq!(decided[0]["payload"]["judge"], "checks");
    let requests = std::fs::read_to_string(&record).expect("the command judge was asked");
    assert!(
        requests.contains("migrate the store in three verified steps"),
        "{requests}"
    );

    // The command line is the judge: another argument is another bar.
    document["members"]["checked"]["judge"][0]["command"] =
        serde_json::json!([fake_provider(), "--strict"]);
    workspace.graph(&oneagentgraph::yaml::to_string(&document).expect("serializes"));
    assert_ne!(
        fingerprint(&workspace, "checked", &[]),
        base,
        "the command line"
    );

    // An ordinary run of a member like it — agent and judge — settles with no
    // bar fingerprint: that field is a judge-only run's alone.
    let ordinary: Value = serde_json::json!({
        "version": 11,
        "name": "ordinary",
        "env": document["env"].clone(),
        "members": {"checked": {
            "kind": "onejudge",
            "base_config": "./base.yaml",
            "agent": {"oneharness_config": "./oneharness.toml"},
            "judge": [{"command": [fake_provider()], "label": "checks"}],
            "mode": "bypass",
        }},
    });
    workspace.write(
        "ordinary.yaml",
        &oneagentgraph::yaml::to_string(&ordinary).expect("serializes"),
    );
    let normal = workspace.run(&[
        "run",
        "./ordinary.yaml",
        "--task",
        "fake:complete-now: write the plan",
    ]);
    normal.expect_code(0);
    let payload = settled(&normal);
    assert_eq!(payload["completed"], Value::Bool(true), "{payload}");
    assert!(
        payload.get("bar_fingerprint").is_none(),
        "an ordinary settle named a bar: {payload}"
    );
}

/// One edit's fingerprint, taken in isolation: each edit is undone before the
/// next is made, so every digest it yields differs from the unedited bar by
/// that one edit alone.
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
