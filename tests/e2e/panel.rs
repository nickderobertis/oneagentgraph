//! A two-party member judged by a **stack** of judges — a harness reviewer, an
//! `llmlint` run, and a repository's own command — against the compiled binary
//! and the real onejudge panel it links.
//!
//! What is read is what the member was actually launched with: the effective
//! onejudge config in its scratch, the argv the llmlint judge was really run
//! with, and each judge's own decision as it reached the merged stream — never
//! a value this crate composed and then asserted against itself.

// llmlint: ignore-file[e2e_not_mocked] the same declaration its sibling journey
// files carry, and for the same reason — see tests/e2e/support.rs: the paid
// harness process is the one sanctioned double, replaced at oneharness's own
// `ONEHARNESS_BIN_<ID>` seam. The command judge and the `llmlint` these graphs
// name are *inputs* — an argv and a `bin` the graph supplies — on the terms the
// `pre_turn` view double states; real oneagentgraph composes the list, the real
// onejudge engine builds the panel from it and runs every judge of it.

// llmlint: ignore-file[expensive_tests_stay_behind_their_own_edge] this crate has
// exactly one e2e target — `[[test]] name = "e2e"` in Cargo.toml — and every
// journey file is a module of it, by the design AGENTS.md states: a journey runs
// inside `just check` rather than behind `#[ignore]`. Each journey here is one
// short conversation of the doubled harness, and the file finishes in seconds.

use std::path::Path;

use serde_json::Value;

use crate::support::{
    fake_harness, fake_llmlint, fake_provider, graph_with, Run, Workspace, FAKE_HARNESS_KEY,
};

/// The graph every stacked journey runs: one worker judged by a harness
/// reviewer, an llmlint run over `./llmlint.yml` against `origin/main` with two
/// extra `args`, and a command judge, in that order, with `extra_env` in the
/// graph's own `env:`
/// block — which is how the llmlint double is steered, because onejudge's
/// llmlint judge inherits the environment the graph exported.
fn stacked_graph(workspace: &Workspace, extra_env: &[(&str, String)]) -> String {
    workspace.write("llmlint.yml", "rules: []\n");
    let mut values = vec![
        (FAKE_HARNESS_KEY.to_string(), fake_harness()),
        ("members.worker.judge.1.bin".to_string(), fake_llmlint()),
        (
            "members.worker.judge.2.command.0".to_string(),
            fake_provider(),
        ),
    ];
    values.extend(
        extra_env
            .iter()
            .map(|(key, value)| (format!("env.{key}"), value.clone())),
    );
    graph_with(
        concat!(
            "version: 9\nname: node-scope\n",
            "env: {}\n",
            "members:\n  worker:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n",
            "      - oneharness_config: ./oneharness.judge.toml\n        label: reviewer\n",
            "      - kind: llmlint\n        config: ./llmlint.yml\n",
            "        diff_base: origin/main\n        bin: the double below\n",
            "        args: [--tag, panel]\n",
            "      - command: [the provider below]\n        label: checks\n",
            "    mode: bypass\n",
        ),
        &values,
    )
}

// llmlint: ignore-block[tests_mirror_real_usage] the three helpers below are the
// observation points these journeys assert composition through, and each is the
// point *because* it is the subject. `onejudge.yaml` and the `judge-<label>.toml`
// beside it are the member's launch evidence — `JudgeLaunch::config` documents
// them as the files an operator reads to see exactly what the member ran, and
// `Workspace::member_file` exists for that read — and the stream cannot hold what
// they hold: a member launched as one provider or another, with a judge's config
// at one path or another, settles with a stream identical to a correct one. The
// argv log is the llmlint double's recording of what onejudge composed for it, at
// the same point and for the same reason the harness double's `record-*` files
// are read by the journeys in dispatch.rs. Every assertion about behaviour — the
// exit code, the events, each judge's decision, the worker's next instruction —
// is still made through the CLI.
/// The effective onejudge config the worker was launched with, parsed.
fn launched_config(workspace: &Workspace) -> Value {
    let text = workspace.member_file("worker", "onejudge.yaml");
    serde_norway::from_str(&text).expect("the effective config is YAML")
}

/// One file the run wrote into the worker's scratch beside that config, or
/// `None` when the run wrote no such file.
fn scratch_file(workspace: &Workspace, name: &str) -> Option<String> {
    let run_id = workspace.record()["run_id"]
        .as_str()
        .expect("the record names its run")
        .to_string();
    std::fs::read_to_string(
        workspace
            .state()
            .join(run_id)
            .join("members")
            .join("worker")
            .join(name),
    )
    .ok()
}

/// Every argv the llmlint double was run with, one per line of its log.
fn llmlint_invocations(log: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("one argv per line"))
        .collect()
}
// llmlint: ignore-end[tests_mirror_real_usage]

/// Every `judge-decided` the worker published for `turn`, in stream order, as
/// `(judge, kind, decision, reason)`.
fn decisions(run: &Run, turn: u64) -> Vec<(String, String, String, String)> {
    run.of_kind("judge-decided")
        .iter()
        .filter(|event| event["labels"]["member"] == "worker" && event["payload"]["turn"] == turn)
        .map(|event| {
            let field = |name: &str| {
                event["payload"][name]
                    .as_str()
                    .unwrap_or_else(|| panic!("`{name}` is a string: {event}"))
                    .to_string()
            };
            (
                field("judge"),
                field("kind"),
                field("decision"),
                field("reason"),
            )
        })
        .collect()
}

/// A stacked panel is launched as onejudge's `split` provider with one judge
/// per side in list order — the reviewer's resolved config in its own scratch
/// file, the llmlint side's `config` absolute and never copied, the command
/// side as written, labels through verbatim — the llmlint judge really runs
/// over the worker's tree with the config, base and extra `args` the graph
/// named, and every judge's decision reaches the stream in the panel's order.
#[test]
fn a_stacked_panel_is_launched_as_split_and_every_judge_decides_on_the_stream() {
    let workspace = Workspace::new();
    let log = workspace.at("llmlint-argv.log");
    workspace.graph(&stacked_graph(
        &workspace,
        &[("FAKE_LLMLINT_ARGV_LOG", log.display().to_string())],
    ));

    let run = workspace.run_task("fake:complete-now: judged by a stack");
    run.expect_code(0);
    assert_eq!(
        run.of_kind("member-settled")[0]["payload"]["completed"],
        serde_json::json!(true)
    );

    // What the member was launched with, read back from its scratch.
    let config = launched_config(&workspace);
    let provider = &config["provider"];
    assert_eq!(provider["kind"], "split", "{config}");
    assert_eq!(provider["skill"]["kind"], "oneharness", "{config}");
    assert!(
        provider.get("judge").is_none(),
        "a stack is `judges:`, never `judge:`: {config}"
    );
    let judges = provider["judges"].as_array().expect("a list of judges");
    assert_eq!(judges.len(), 3, "{config}");

    assert_eq!(judges[0]["kind"], "oneharness");
    assert_eq!(judges[0]["label"], "reviewer");
    let reviewer = Path::new(judges[0]["judge_config"].as_str().expect("a path"));
    assert_eq!(
        reviewer.file_name().and_then(|name| name.to_str()),
        Some("judge-reviewer.toml"),
        "{config}"
    );
    let stamped = scratch_file(&workspace, "judge-reviewer.toml").expect("the reviewer's config");
    assert!(stamped.contains("mode = \"bypass\""), "{stamped}");
    assert!(
        scratch_file(&workspace, "oneharness.judge.toml").is_none(),
        "the single-judge file must not be written for a stack"
    );

    assert_eq!(judges[1]["kind"], "llmlint");
    let llmlint_config = Path::new(judges[1]["config"].as_str().expect("a path"));
    assert!(llmlint_config.is_absolute(), "{config}");
    assert_eq!(llmlint_config, workspace.at("llmlint.yml"), "{config}");
    assert_eq!(judges[1]["diff_base"], "origin/main");
    assert_eq!(judges[1]["bin"], fake_llmlint());
    assert_eq!(judges[1]["args"], serde_json::json!(["--tag", "panel"]));
    assert!(
        judges[1].get("label").is_none(),
        "an absent label is left to onejudge: {config}"
    );
    assert!(
        scratch_file(&workspace, "llmlint.yml").is_none(),
        "an llmlint config is never copied into the scratch"
    );

    assert_eq!(judges[2]["kind"], "command");
    assert_eq!(judges[2]["command"][0], fake_provider());
    assert_eq!(judges[2]["label"], "checks");

    // The llmlint judge really ran, over the worker's tree, with the config and
    // base the graph named — and was probed first, as onejudge documents.
    let invocations = llmlint_invocations(&log);
    assert_eq!(
        invocations.first().map(Vec::as_slice),
        Some(&["--version".to_string()][..])
    );
    let lint = invocations
        .iter()
        .find(|argv| argv.first().map(String::as_str) == Some("lint"))
        .unwrap_or_else(|| panic!("no lint run: {invocations:?}"));
    let after = |flag: &str| {
        lint.iter()
            .position(|arg| arg == flag)
            .map(|at| lint[at + 1].as_str())
            .unwrap_or_else(|| panic!("no {flag} in {lint:?}"))
    };
    assert_eq!(Path::new(after("--cwd")), workspace.dir(), "{lint:?}");
    assert_eq!(
        Path::new(after("-c")),
        workspace.at("llmlint.yml"),
        "{lint:?}"
    );
    assert!(lint.iter().any(|arg| arg == "--diff"), "{lint:?}");
    assert_eq!(after("--diff-base"), "origin/main", "{lint:?}");
    // The graph's own `args` reach the real invocation, after everything
    // onejudge composes, so a repository's extra llmlint flags are honoured.
    assert!(
        lint.ends_with(&["--tag".to_string(), "panel".to_string()]),
        "{lint:?}"
    );

    // Every judge's own decision, in the panel's order, on the first turn.
    let decided = decisions(&run, 1);
    assert_eq!(
        decided
            .iter()
            .map(|(judge, kind, decision, _)| (judge.as_str(), kind.as_str(), decision.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("reviewer", "oneharness", "done"),
            ("llmlint", "llmlint", "done"),
            ("checks", "command", "done"),
        ],
        "{decided:?}"
    );
    assert_eq!(decided[1].3, "0 violations, 3 rules hold");
    assert!(decided.iter().all(|(_, _, _, reason)| !reason.is_empty()));
}

/// One judge sending the worker back is enough: when only the llmlint judge
/// fails the turn, its decision says so on the stream — named, with its
/// summary as the reason — the worker's next turn opens on llmlint's own
/// findings under that judge's header, and once it passes the member settles
/// complete. This is the stack's whole purpose, proven on the real panel.
#[test]
fn a_judge_that_sends_the_worker_back_is_named_and_its_findings_open_the_next_turn() {
    let workspace = Workspace::new();
    workspace.graph(&stacked_graph(
        &workspace,
        &[
            ("FAKE_LLMLINT_VERDICT", "fail-once".to_string()),
            (
                "FAKE_LLMLINT_MARKER",
                workspace.at("llmlint-failed-once").display().to_string(),
            ),
        ],
    ));

    let run = workspace.run_task("fake:complete-now: judged by a stack");
    run.expect_code(0);
    assert_eq!(
        run.of_kind("member-settled")[0]["payload"]["completed"],
        serde_json::json!(true)
    );

    let first = decisions(&run, 1);
    assert_eq!(
        first
            .iter()
            .map(|(judge, _, decision, _)| (judge.as_str(), decision.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("reviewer", "done"),
            ("llmlint", "continue"),
            ("checks", "done")
        ],
        "{first:?}"
    );
    assert_eq!(first[1].3, "1 violation, 2 rules hold");

    // The worker's second turn opens on llmlint's own report, attributed.
    let second_turn = run
        .of_kind("turn-started")
        .into_iter()
        .find(|event| event["payload"]["turn"] == 2 && event["payload"]["role"] == "assistant")
        .expect("the worker took a second turn");
    let instruction = second_turn["payload"]["instruction"]
        .as_str()
        .expect("an instruction");
    assert!(
        instruction.contains("## Judge `llmlint` (llmlint)"),
        "{instruction}"
    );
    assert!(
        instruction.contains("fake finding: the double was told to fail this run"),
        "{instruction}"
    );
    assert!(
        !instruction.contains("## Judge `reviewer`"),
        "a judge that completed contributes nothing: {instruction}"
    );

    let second = decisions(&run, 2);
    assert!(
        second.iter().all(|(_, _, decision, _)| decision == "done"),
        "{second:?}"
    );
}

/// A single harness judge — the spelling every graph on this host uses, and
/// the `judge.oneharness_config=…` override laid over it — is still launched
/// as the `kind: oneharness` provider carrying both sides, with the judge's
/// config at the file it was always at, and publishes no `judge-decided` at
/// all: a bare provider is not a panel, and nothing is synthesized for it.
#[test]
fn a_single_harness_judge_is_still_launched_as_the_provider_it_always_was() {
    let workspace = Workspace::new();
    // A second judge config the override names, distinguishable from the
    // graph's own by a comment the stamp keeps byte for byte.
    workspace.write(
        "alt.judge.toml",
        &format!("# the override's judge\n{}", crate::support::CHAIN),
    );
    let dir = workspace.dir().display().to_string();
    for (label, args) in [
        ("as written", vec![]),
        (
            "overridden",
            vec![
                "--set",
                "members.worker.judge.oneharness_config=./alt.judge.toml",
            ],
        ),
    ] {
        let mut argv = vec![
            "run",
            "./graph.yaml",
            "--task",
            "fake:complete-now",
            "--dir",
            &dir,
        ];
        argv.extend(args);
        let run = workspace.run(&argv);
        run.expect_code(0);
        assert!(
            run.of_kind("judge-decided").is_empty(),
            "{label}: a bare provider decides nothing on the stream"
        );

        let config = launched_config(&workspace);
        let provider = &config["provider"];
        assert_eq!(provider["kind"], "oneharness", "{label}: {config}");
        assert_eq!(provider["stream"], true, "{label}: {config}");
        assert_eq!(provider["control"], true, "{label}: {config}");
        assert!(provider.get("judges").is_none(), "{label}: {config}");
        let judge_config = Path::new(provider["judge_config"].as_str().expect("a path"));
        assert_eq!(
            judge_config.file_name().and_then(|name| name.to_str()),
            Some("oneharness.judge.toml"),
            "{label}: {config}"
        );
        let stamped =
            scratch_file(&workspace, "oneharness.judge.toml").expect("the judge's config");
        assert_eq!(
            stamped.contains("# the override's judge"),
            label == "overridden",
            "{label}: {stamped}"
        );
    }
}

/// A list entry that is none of the three shapes, and a side whose config
/// cannot be read, are refused by both verbs naming the entry — before any
/// member is built.
#[test]
fn a_malformed_judge_entry_is_refused_by_both_verbs_naming_it() {
    let workspace = Workspace::new();
    let refuses = |document: &str, expected: &[&str]| {
        workspace.graph(document);
        for args in [
            vec!["validate", "./graph.yaml"],
            vec!["run", "./graph.yaml", "--task", "fake:complete-now"],
        ] {
            let refused = workspace.run(&args);
            refused.expect_code(2);
            for text in expected {
                assert!(
                    refused.stderr.contains(text),
                    "{}: expected {text:?} in: {}",
                    args.join(" "),
                    refused.stderr
                );
            }
            assert!(
                refused.stdout.trim().is_empty(),
                "a refusal must publish no event: {}",
                refused.stdout
            );
        }
    };
    let stacked = stacked_graph(&workspace, &[]);
    refuses(
        &stacked
            .replace("label: checks", "flag: checks")
            .replace("command:", "commnad:"),
        &[
            "judge entry 3",
            "{commnad, flag} is none of the three judge shapes",
        ],
    );
    refuses(
        &stacked.replace("config: ./llmlint.yml", "config: ./nowhere.yml"),
        &["judge entry 2", "nowhere.yml", "cannot be read"],
    );
    refuses(
        &stacked.replace(
            "oneharness_config: ./oneharness.judge.toml",
            "oneharness_config: ./nowhere.toml",
        ),
        &["judge entry 1", "nowhere.toml", "cannot read"],
    );
}

/// An llmlint `bin` that is not there is refused by onejudge's own probe when
/// the member's plan is built, so the member dies without a turn rather than
/// judging as a silent pass: a death after launch, not a refusal before it.
#[test]
fn an_llmlint_bin_nothing_answers_kills_the_member_before_its_first_turn() {
    let workspace = Workspace::new();
    let stacked = stacked_graph(&workspace, &[]);
    let missing = workspace.unreachable_harness();
    workspace.graph(&stacked.replace(&fake_llmlint(), &missing));
    let run = workspace.run_task("fake:complete-now: never judged");
    run.expect_code(1);
    let died = run.of_kind("member-died");
    assert_eq!(died.len(), 1, "{}", run.stdout);
    let detail = died[0]["payload"]["detail"].as_str().unwrap_or_default();
    assert!(detail.contains("no-such-harness-binary"), "{}", died[0]);
    assert!(
        run.of_kind("turn-started").is_empty(),
        "no turn may be spent: {}",
        run.stdout
    );
}
