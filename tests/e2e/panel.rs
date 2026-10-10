//! A two-party member judged by a **stack** of judges — a harness reviewer, an
//! `llmlint` run, and a repository's own command — against the compiled binary
//! and the real onejudge panel it links.
//!
//! What is read is what the member was actually launched with: the effective
//! onejudge config at the path its `member-started` event published, the argv
//! the llmlint judge was really run with, and each judge's own decision as it
//! reached the merged stream — never a value this crate composed and then
//! asserted against itself.

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

use std::path::{Path, PathBuf};

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

// llmlint: ignore-block[tests_mirror_real_usage] the helpers below read files
// only at paths the CLI itself published — `member-started` names the effective
// onejudge config a member was launched with, and that config names each harness
// judge's `judge_config` — so what is opened is what the stream told an operator
// to open: `JudgeLaunch::config` documents these as the launch evidence a person
// reads to see exactly what the member ran. Nothing here reconstructs the state
// directory's layout. The read is the assertion because the stream cannot hold
// what the file holds: a member launched as one provider or another, with a
// judge's config at one path or another, settles with a stream identical to a
// correct one. The argv log is the llmlint double's recording of what onejudge
// composed for it, at the same point and for the same reason the harness
// double's `record-*` files are read by the journeys in dispatch.rs. Every
// assertion about behaviour — the exit code, the events, each judge's decision,
// the worker's next instruction — is still made through the CLI.
/// The effective onejudge config the worker was launched with, at the path
/// its `member-started` event published: where it is, and what it says.
fn launched_config(run: &Run) -> (PathBuf, Value) {
    let started = run.of_kind("member-started");
    let path = PathBuf::from(
        started[0]["payload"]["config"]
            .as_str()
            .expect("member-started names the launched config"),
    );
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("cannot read {} ({err})", path.display()));
    (
        path,
        serde_norway::from_str(&text).expect("the effective config is YAML"),
    )
}

/// One file beside the launched config — in the scratch the CLI named — or
/// `None` when the run wrote no such file there.
fn beside(config: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(config.with_file_name(name)).ok()
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

/// One path as the host resolves it, so a path this test spelled and the same
/// path as the CLI handed it on compare equal when they name one file.
///
/// The two are spelled by different parties: `workspace.at(..)` is the
/// temporary directory as `tempfile` created it, while the llmlint `config` in
/// the launched provider block is that path made absolute against the
/// directory the binary ran in — which is how "absolute" is promised for a
/// graph named as `./graph.yaml`. A host whose temporary directory is a symlink
/// spells those differently and means the same file — macOS's `/var` →
/// `/private/var` is the case that fails a string comparison here while
/// nothing is wrong, and the sibling journeys in dispatch.rs compare the same
/// way for the same reason.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|err| panic!("{} cannot be resolved: {err}", path.display()))
}

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

    // What the member was launched with, read from where the stream said it is.
    let (launched, config) = launched_config(&run);
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
    assert_eq!(reviewer.parent(), launched.parent(), "{config}");
    // Stamped with the member's ownership evidence and **not** with its `mode`:
    // a judge's posture is its own config's, over onejudge's read-only default,
    // and a panel of judges each handed the agent's `bypass` is one onejudge
    // refuses outright.
    let stamped = std::fs::read_to_string(reviewer).expect("the reviewer's config");
    let document: toml_edit::DocumentMut = stamped.parse().expect("the reviewer's config");
    assert!(document.get("mode").is_none(), "{stamped}");
    assert!(
        document["env"]
            .get(oneagentgraph::scratch::SCRATCH_ENV)
            .is_some(),
        "{stamped}"
    );
    assert!(
        beside(&launched, "oneharness.judge.toml").is_none(),
        "the single-judge file must not be written for a stack"
    );

    assert_eq!(judges[1]["kind"], "llmlint");
    let llmlint_config = Path::new(judges[1]["config"].as_str().expect("a path"));
    assert!(llmlint_config.is_absolute(), "{config}");
    assert_eq!(
        canonical(llmlint_config),
        canonical(&workspace.at("llmlint.yml")),
        "{config}"
    );
    assert_eq!(judges[1]["diff_base"], "origin/main");
    assert_eq!(judges[1]["bin"], fake_llmlint());
    assert_eq!(judges[1]["args"], serde_json::json!(["--tag", "panel"]));
    assert!(
        judges[1].get("label").is_none(),
        "an absent label is left to onejudge: {config}"
    );
    assert!(
        beside(&launched, "llmlint.yml").is_none(),
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
    assert_eq!(
        canonical(Path::new(after("--cwd"))),
        canonical(&workspace.dir()),
        "{lint:?}"
    );
    assert_eq!(
        canonical(Path::new(after("-c"))),
        canonical(&workspace.at("llmlint.yml")),
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

/// A judge's reason longer than the payload bound reaches the stream cut at the
/// bound, marked `truncated`, and still reads back through the public
/// `JudgeDecided` type — the bus emitter's cut is a record this crate's own
/// stream carries, never one its own type refuses.
///
/// The long reason is the command judge's own words: it releases a worker that
/// reports a blocker nothing in the run can clear, and names that blocker in its
/// reason, so a worker reporting one past the bound is a judge answering past it.
#[test]
fn a_judges_reason_past_the_bound_is_cut_on_the_stream_and_reads_back_through_its_type() {
    let workspace = Workspace::new();
    workspace.graph(&stacked_graph(&workspace, &[]));
    let condition = "a credential this run was never given ".repeat(160);
    let answer = workspace.write(
        "report.txt",
        &format!("There is no next action for me to take.\n\nblocker: {condition}\n"),
    );
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        &format!(
            "fake:complete-now judged by a stack fake:answer-file={}",
            answer.display()
        ),
        "--dir",
        &workspace.dir().display().to_string(),
    ]);

    let checks: Vec<Value> = run
        .of_kind("judge-decided")
        .into_iter()
        .filter(|event| event["payload"]["judge"] == "checks")
        .collect();
    let decided = checks
        .first()
        .unwrap_or_else(|| panic!("the command judge decided nothing:\n{}", run.stdout));
    let full = format!(
        "terminal blocker reported: {}",
        condition.trim().trim_end_matches('.')
    );
    assert!(full.len() > oneagentgraph::event::MAX_PAYLOAD_TEXT_BYTES);

    let payload = decided["payload"].clone();
    assert_eq!(payload["truncated"], true, "{decided}");
    let reason = payload["reason"].as_str().expect("a reason");
    assert_eq!(
        reason.len(),
        oneagentgraph::event::MAX_PAYLOAD_TEXT_BYTES,
        "{decided}"
    );
    assert!(
        full.starts_with(reason),
        "the cut kept something other than the reason's head: {reason}"
    );

    let read: oneagentgraph::event::JudgeDecided =
        serde_json::from_value(payload).unwrap_or_else(|err| {
            panic!("the stream's own record did not read back ({err}): {decided}")
        });
    assert!(read.truncated);
    assert_eq!(read.judge, "checks");
    assert_eq!(read.kind, "command");
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

        let (launched, config) = launched_config(&run);
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
        assert_eq!(
            judge_config.parent(),
            launched.parent(),
            "{label}: {config}"
        );
        let stamped = std::fs::read_to_string(judge_config).expect("the judge's config");
        assert_eq!(
            stamped.contains("# the override's judge"),
            label == "overridden",
            "{label}: {stamped}"
        );
    }
}

/// A list entry that is none of the three shapes, a side whose config cannot
/// be read, and a command side whose argv no process could be handed are
/// refused by both verbs naming the entry — before any member is built.
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
        &stacked.replace(&fake_provider(), "' '"),
        &["judge entry 3", "a command judge needs a command to run"],
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

/// A version 10 graph whose worker is judged by a panel of two — a harness
/// reviewer over `./oneharness.judge.toml` and a command judge — with `extra`
/// written into the worker's own mapping, which is where a journey puts the
/// `judge_settings` it is about. `reviewer` is the reviewer's own mapping
/// tail, which is where its `settings` go.
fn settings_graph(extra: &str, reviewer: &str) -> String {
    graph_with(
        &format!(
            concat!(
                "version: 10\nname: node-scope\n",
                "env: {{}}\n",
                "members:\n  worker:\n    kind: onejudge\n",
                "    base_config: ./base.yaml\n",
                "    agent:\n      oneharness_config: ./oneharness.toml\n",
                "    judge:\n",
                "      - oneharness_config: ./oneharness.judge.toml\n        label: reviewer\n{}",
                "      - command: [the provider below]\n        label: checks\n",
                "    mode: bypass\n{}",
            ),
            reviewer, extra
        ),
        &[
            (FAKE_HARNESS_KEY.to_string(), fake_harness()),
            (
                "members.worker.judge.1.command.0".to_string(),
                fake_provider(),
            ),
        ],
    )
}

/// The member's death detail, which is where onejudge's own refusal reaches the
/// stream when the member's plan cannot be built.
fn death_detail(run: &Run) -> String {
    let died = run.of_kind("member-died");
    assert_eq!(died.len(), 1, "{}", run.stdout);
    died[0]["payload"]["detail"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// onejudge refuses a panel holding a judge that can write to the worktree,
/// and `judge_settings` is how a graph says it accepts that: the same member,
/// judged by a reviewer whose own config names the writable `default` mode
/// beside a command judge, dies on onejudge's own `allow_writable_judges`
/// refusal without it — and starts and completes with it, whether the graph
/// writes it or `--set` supplies it to a graph that wrote no `judge_settings`
/// at all. The setting reaches the composed provider block as the boolean it
/// is, and the real onejudge, not this crate, is what reads it.
#[test]
fn a_writable_judge_in_a_panel_runs_only_when_judge_settings_allow_it() {
    let workspace = Workspace::new();
    workspace.write(
        "oneharness.judge.toml",
        &format!("mode = \"default\"\n{}", crate::support::CHAIN),
    );
    let dir = workspace.dir().display().to_string();
    let run = |graph: &str, extra: &[&str]| {
        workspace.graph(graph);
        let mut argv = vec![
            "run",
            "./graph.yaml",
            "--task",
            "fake:complete-now: judged by a writable panel",
            "--dir",
            &dir,
        ];
        argv.extend(extra);
        workspace.run(&argv)
    };

    let refused = run(&settings_graph("", ""), &[]);
    refused.expect_code(1);
    let detail = death_detail(&refused);
    assert!(detail.contains("allow_writable_judges"), "{detail}");
    assert!(detail.contains("`default`"), "{detail}");
    assert!(detail.contains("reviewer"), "{detail}");
    assert!(
        refused.of_kind("turn-started").is_empty(),
        "no turn may be spent: {}",
        refused.stdout
    );

    for (spelling, graph, extra) in [
        (
            "in the graph",
            settings_graph("    judge_settings: {allow_writable_judges: true}\n", ""),
            vec![],
        ),
        (
            "through --set",
            settings_graph("", ""),
            vec![
                "--set",
                "members.worker.judge_settings.allow_writable_judges=true",
            ],
        ),
    ] {
        let allowed = run(&graph, &extra);
        allowed.expect_code(0);
        assert_eq!(
            allowed.of_kind("member-settled")[0]["payload"]["completed"],
            serde_json::json!(true),
            "{spelling}"
        );
        let (_, config) = launched_config(&allowed);
        assert_eq!(config["provider"]["kind"], "split", "{spelling}: {config}");
        assert_eq!(
            config["provider"]["allow_writable_judges"],
            serde_json::json!(true),
            "{spelling}: {config}"
        );
        assert_eq!(
            decisions(&allowed, 1)
                .iter()
                .map(|(judge, _, decision, _)| (judge.as_str(), decision.as_str()))
                .collect::<Vec<_>>(),
            vec![("reviewer", "done"), ("checks", "done")],
            "{spelling}"
        );
    }
}

/// A key in one judge's `settings` reaches onejudge exactly as written, into
/// that judge's entry and no other: a real per-judge setting this crate does
/// not type — the reviewer's `instructions`, appended to every prompt that
/// judge is handed — arrives in the prompt the reviewer's harness is given,
/// and a key onejudge has no such field for is refused by onejudge itself,
/// naming it. Both spellings: written in the graph, and supplied by `--set` to
/// a judge whose graph wrote no `settings` mapping for it.
#[test]
fn a_judges_own_settings_reach_onejudge_unchanged() {
    let workspace = Workspace::new();
    let dir = workspace.dir().display().to_string();
    let prompt = workspace.at("reviewer-prompt.log");
    let task = format!(
        "fake:complete-now: judge with instructions fake:record-supervisor-prompt={}",
        prompt.display()
    );
    let run = |graph: &str, extra: &[&str]| {
        workspace.graph(graph);
        let mut argv = vec!["run", "./graph.yaml", "--task", &task, "--dir", &dir];
        argv.extend(extra);
        workspace.run(&argv)
    };
    let instructed = "Read CHANGELOG-SENTINEL before you decide.";

    for (spelling, graph, extra) in [
        (
            "in the graph",
            settings_graph(
                "",
                &format!("        settings: {{instructions: {instructed}}}\n"),
            ),
            vec![],
        ),
        (
            "through --set",
            settings_graph("", ""),
            vec![
                "--set".to_string(),
                format!("members.worker.judge.0.settings.instructions={instructed}"),
            ],
        ),
    ] {
        let _ = std::fs::remove_file(&prompt);
        let extra: Vec<&str> = extra.iter().map(String::as_str).collect();
        let judged = run(&graph, &extra);
        judged.expect_code(0);
        let (_, config) = launched_config(&judged);
        let judges = &config["provider"]["judges"];
        assert_eq!(
            judges[0]["instructions"], instructed,
            "{spelling}: {config}"
        );
        assert!(
            judges[1].get("instructions").is_none(),
            "{spelling}: a judge's settings are its own: {config}"
        );
        let seen = std::fs::read_to_string(&prompt)
            .unwrap_or_else(|err| panic!("{spelling}: the reviewer was never prompted ({err})"));
        assert!(seen.contains(instructed), "{spelling}: {seen}");
    }

    for (spelling, graph, extra) in [
        (
            "in the graph",
            settings_graph("", "        settings: {not_a_onejudge_setting: 1}\n"),
            vec![],
        ),
        (
            "through --set",
            settings_graph("", ""),
            vec![
                "--set",
                "members.worker.judge.1.settings.not_a_onejudge_setting=1",
            ],
        ),
    ] {
        let refused = run(&graph, &extra);
        refused.expect_code(1);
        let detail = death_detail(&refused);
        assert!(
            detail.contains("not_a_onejudge_setting"),
            "{spelling}: {detail}"
        );
        assert!(
            refused.of_kind("turn-started").is_empty(),
            "{spelling}: no turn may be spent: {}",
            refused.stdout
        );
    }
}

/// A key this crate composes is refused by name at either level, by both
/// verbs, before any member is built — and a single harness judge's entry,
/// which is the provider block itself, refuses a provider key too.
#[test]
fn a_composed_key_in_either_settings_is_refused_by_both_verbs_naming_it() {
    let workspace = Workspace::new();
    for (graph, expected) in [
        (
            settings_graph("    judge_settings: {judges: []}\n", ""),
            "`judge_settings` names `judges`",
        ),
        (
            settings_graph("", "        settings: {judge_config: ./elsewhere.toml}\n"),
            "judge entry 1: `settings` names `judge_config`",
        ),
        (
            graph_with(
                concat!(
                    "version: 10\nname: node-scope\nenv: {}\n",
                    "members:\n  worker:\n    kind: onejudge\n",
                    "    base_config: ./base.yaml\n",
                    "    agent:\n      oneharness_config: ./oneharness.toml\n",
                    "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
                    "      settings: {stream: false}\n",
                    "    mode: bypass\n",
                ),
                &[(FAKE_HARNESS_KEY, fake_harness())],
            ),
            "judge entry 1: `settings` names `stream`",
        ),
    ] {
        workspace.graph(&graph);
        for args in [
            vec!["validate", "./graph.yaml"],
            vec!["run", "./graph.yaml", "--task", "fake:complete-now"],
        ] {
            let refused = workspace.run(&args);
            refused.expect_code(2);
            assert!(
                refused.stderr.contains(expected),
                "{}: expected {expected:?} in: {}",
                args.join(" "),
                refused.stderr
            );
            assert!(
                refused.stderr.contains("oneagentgraph's to compose"),
                "{}",
                refused.stderr
            );
            assert!(
                refused.stdout.trim().is_empty(),
                "a refusal must publish no event: {}",
                refused.stdout
            );
        }
    }
}

/// A version 10 graph whose worker is judged by exactly one harness judge, with
/// `extra` in the worker's own mapping and `judge` in that judge's — the shape
/// whose composed `kind: oneharness` block is both the provider and the judge's
/// entry.
fn single_judge_graph(extra: &str, judge: &str) -> String {
    graph_with(
        &format!(
            concat!(
                "version: 10\nname: node-scope\nenv: {{}}\n",
                "members:\n  worker:\n    kind: onejudge\n",
                "    base_config: ./base.yaml\n",
                "    agent:\n      oneharness_config: ./oneharness.toml\n",
                "    judge:\n      oneharness_config: ./oneharness.judge.toml\n{}",
                "    mode: bypass\n{}",
            ),
            judge, extra
        ),
        &[(FAKE_HARNESS_KEY, fake_harness())],
    )
}

/// One harness judge composes one `kind: oneharness` provider, and both
/// levels of settings land in it: the member's `instructions` reach the prompt
/// that judge's harness is handed, its own `events` sit beside them, and the
/// block is otherwise the one it always was. A setting onejudge refuses on
/// that shape — `allow_writable_judges`, which only a split can carry — is
/// passed through all the same and refused by onejudge, naming it: this crate
/// does not second-guess which shape a setting belongs on.
#[test]
fn a_single_harness_judge_carries_both_levels_of_settings_in_its_one_block() {
    let workspace = Workspace::new();
    let dir = workspace.dir().display().to_string();
    let prompt = workspace.at("judge-prompt.log");
    let task = format!(
        "fake:complete-now: judge alone fake:record-supervisor-prompt={}",
        prompt.display()
    );
    let instructed = "Read SINGLE-SENTINEL before you decide.";
    workspace.graph(&single_judge_graph(
        &format!("    judge_settings: {{instructions: {instructed}}}\n"),
        "      settings: {events: true}\n",
    ));
    let judged = workspace.run(&["run", "./graph.yaml", "--task", &task, "--dir", &dir]);
    judged.expect_code(0);
    let (_, config) = launched_config(&judged);
    let provider = &config["provider"];
    assert_eq!(provider["kind"], "oneharness", "{config}");
    assert_eq!(provider["instructions"], instructed, "{config}");
    assert_eq!(provider["events"], true, "{config}");
    assert_eq!(provider["stream"], true, "{config}");
    assert_eq!(provider["control"], true, "{config}");
    let seen = std::fs::read_to_string(&prompt)
        .unwrap_or_else(|err| panic!("the judge was never prompted ({err})"));
    assert!(seen.contains(instructed), "{seen}");

    workspace.graph(&single_judge_graph(
        "    judge_settings: {allow_writable_judges: true}\n",
        "",
    ));
    let refused = workspace.run(&["run", "./graph.yaml", "--task", &task, "--dir", &dir]);
    refused.expect_code(1);
    let detail = death_detail(&refused);
    assert!(detail.contains("allow_writable_judges"), "{detail}");
    assert!(
        refused.of_kind("turn-started").is_empty(),
        "no turn may be spent: {}",
        refused.stdout
    );
}

/// An llmlint judge's `settings` reach its entry as written too: a key onejudge
/// has no field for is carried into the llmlint judge's composed entry beside
/// the fields this crate composed for it, and onejudge refuses it by name before
/// the member spends a turn.
#[test]
fn an_llmlint_judges_settings_reach_onejudge_unchanged() {
    let workspace = Workspace::new();
    let mut stacked: serde_norway::Value =
        serde_norway::from_str(&stacked_graph(&workspace, &[])).expect("the stacked graph");
    stacked["version"] = 10.into();
    stacked["members"]["worker"]["judge"][1]["settings"] =
        serde_norway::from_str("{not_a_llmlint_setting: 1}").expect("a mapping");
    let stacked = serde_norway::to_string(&stacked).expect("the graph serializes");
    workspace.graph(&stacked);
    let run = workspace.run_task("fake:complete-now: never judged");
    run.expect_code(1);
    let (_, config) = launched_config(&run);
    let llmlint = &config["provider"]["judges"][1];
    assert_eq!(llmlint["kind"], "llmlint", "{config}");
    assert_eq!(llmlint["not_a_llmlint_setting"], 1, "{config}");
    assert_eq!(llmlint["diff_base"], "origin/main", "{config}");
    let detail = death_detail(&run);
    assert!(detail.contains("not_a_llmlint_setting"), "{detail}");
    assert!(
        run.of_kind("turn-started").is_empty(),
        "no turn may be spent: {}",
        run.stdout
    );
}

/// A document declaring a schema before version 10 that names either level of
/// settings anyway is refused by both verbs, naming the field and the version
/// that has it, rather than run with the setting silently dropped.
#[test]
fn settings_under_an_older_schema_are_refused_naming_the_version_that_has_them() {
    let workspace = Workspace::new();
    for graph in [
        settings_graph("    judge_settings: {allow_writable_judges: true}\n", ""),
        settings_graph("", "        settings: {instructions: look}\n"),
    ] {
        workspace.graph(&graph.replace("version: 10\n", "version: 9\n"));
        for args in [
            vec!["validate", "./graph.yaml"],
            vec!["run", "./graph.yaml", "--task", "fake:complete-now"],
        ] {
            let refused = workspace.run(&args);
            refused.expect_code(2);
            for text in [
                "`judge_settings`",
                "`settings`",
                "requires graph schema version 10",
            ] {
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
    }
}
