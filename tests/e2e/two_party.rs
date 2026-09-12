//! A two-party member's own job — its worktree and its pace — against the
//! compiled binary and real onejudge turns.
//!
//! From graph schema version 9 a `kind: onejudge` member carries the `dir` and
//! `schedule` a `kind: oneharness` member has carried since version 3. The
//! journeys here are what those two fields *do* on a conversation: `dir` is the
//! directory the agent side's harness is started in and the one its skills are
//! discovered from, and `schedule` paces one conversation's turns rather than
//! starting a conversation per firing. Everything asserted is read off the run's
//! own stream or off what the harness double recorded about where it ran —
//! never off a launch value.

// llmlint: ignore-file[e2e_not_mocked] the same declaration its sibling journey
// files carry, and for the same reason — see tests/e2e/support.rs: the paid
// harness process is the one sanctioned double, replaced at oneharness's own
// `ONEHARNESS_BIN_<ID>` seam. Real oneagentgraph resolves the member's directory,
// the real onejudge engine it links opens the conversation there, and real
// oneharness starts the double in it.

use std::path::{Path, PathBuf};

use crate::support::{
    fake_harness, graph_with, Run, Workspace, FAKE_HARNESS_KEY, UNREACHABLE_CHAIN,
    UNREACHABLE_HARNESS_KEY,
};

/// Where Claude Code discovers a project skill from its working directory —
/// the layout `fake:project-skill` reads, so a skill planted here under one
/// directory is distinguishable from the same name planted under another.
fn plant_skill(root: &Path, name: &str, body: &str) {
    let dir = root.join(".claude").join("skills").join(name);
    std::fs::create_dir_all(&dir).expect("the skill directory");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: a skill\n---\n{body}\n"),
    )
    .expect("the skill");
}

/// The default two-party graph under the current schema, with `dir` on the
/// worker when `dir` names one.
fn graph(workspace: &Workspace, dir: Option<&str>) -> String {
    let mut values = vec![(FAKE_HARNESS_KEY.to_string(), fake_harness())];
    // A second identity the decoy config below names, made unreachable on every
    // host — so a member that *did* pick the decoy up dies rather than spending a
    // real turn where Codex happens to be installed.
    values.push((
        UNREACHABLE_HARNESS_KEY.to_string(),
        workspace.unreachable_harness(),
    ));
    if let Some(dir) = dir {
        values.push(("members.worker.dir".to_string(), dir.to_string()));
    }
    graph_with(
        concat!(
            "version: 9\nname: node-scope\n",
            "env: {}\n",
            "members:\n  worker:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n",
        ),
        &values,
    )
}

/// Every directory a harness recorded through `fake:record-cwd`, canonical so a
/// host whose temporary directory is a symlink compares equal to what the graph
/// named.
fn recorded_dirs(path: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|err| {
            panic!(
                "no harness recorded a directory in {}: {err}",
                path.display()
            )
        })
        .lines()
        .map(|line| canonical(Path::new(line.trim())))
        .collect()
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|err| panic!("{} cannot be resolved: {err}", path.display()))
}

/// The words the agent side spoke on the stream, one string per turn.
fn agent_messages(run: &Run) -> Vec<String> {
    run.of_kind("turn-message")
        .into_iter()
        .filter(|event| event["payload"]["role"] == "assistant")
        .filter_map(|event| event["payload"]["text"].as_str().map(str::to_string))
        .collect()
}

/// A member's stamped agent config with the one per-run value in it — the
/// scratch stamp, which names the run that wrote it — replaced by a placeholder,
/// so two runs' configs can be compared for everything that is not the run.
fn without_run_stamp(config: &str) -> String {
    config
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("ONEAGENTGRAPH_SCRATCH_DIR") {
                "ONEAGENTGRAPH_SCRATCH_DIR = <this run>".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Drive the default two-party graph with the worker's `dir` set to
/// `declared`, which resolves to `resolved`, and assert all three consequences
/// the contract gives that field.
///
/// The three, each read off the real conversation: the agent side's harness was
/// started in `resolved` (the double records where it was started); the
/// conversation's skills are discovered from `resolved` rather than from the
/// run's `--dir` (a skill of one name is planted under each, with different
/// bodies, and the transcript shows which one the agent side read); and the
/// agent side still runs under its stamped config whatever `dir` says — proven
/// the sharp way, with a decoy `oneharness.toml` under `resolved` naming an
/// unreachable chain, which a side discovering its config from its directory
/// would have died on.
fn worker_works_in(workspace: &Workspace, declared: &str, resolved: &Path) -> Run {
    std::fs::create_dir_all(resolved).expect("the member's own directory");
    plant_skill(&workspace.dir(), "scope", "the skill under the run's --dir");
    plant_skill(resolved, "scope", "the skill under the member's own dir");
    std::fs::write(resolved.join("oneharness.toml"), UNREACHABLE_CHAIN).expect("a decoy config");
    let cwd = workspace.at("worker.cwd");
    workspace.graph(&graph(workspace, Some(declared)));

    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        &format!(
            "fake:project-skill=scope report what your project skill says. fake:record-cwd={}",
            cwd.display()
        ),
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(0);

    // Where the harnesses were started: the member's own directory, and never
    // the run's — every side, because onejudge gives the judge the same worktree
    // as evidence context.
    let dirs = recorded_dirs(&cwd);
    assert!(
        dirs.contains(&canonical(resolved)),
        "the agent side's harness was not started in the member's own directory: {dirs:?}"
    );
    assert!(
        !dirs.contains(&canonical(&workspace.dir())),
        "a side of a member with its own `dir` was started in the run's --dir: {dirs:?}"
    );

    // Which directory's skill reached the conversation, off the transcript.
    let spoken = agent_messages(&run);
    assert!(
        spoken
            .iter()
            .any(|text| text.contains("the skill under the member's own dir")),
        "the skill under the member's own `dir` never reached the conversation: {spoken:?}"
    );
    assert!(
        !spoken
            .iter()
            .any(|text| text.contains("the skill under the run's --dir")),
        "the skill under the run's --dir reached a conversation told to work elsewhere: \
         {spoken:?}"
    );

    // And the member settled on the doubled chain, which is the stamped config's
    // — a side that had discovered the decoy under `dir` would have died on an
    // unreachable harness instead.
    assert_eq!(workspace.record()["members"]["worker"], "settled");
    assert!(
        run.of_kind("member-died").is_empty(),
        "the agent side died, which is what discovering the decoy config under `dir` looks \
         like: {}",
        run.stdout
    );
    run
}

/// The stamped agent config a member runs under is the same one whether or not
/// it names a `dir`, everything but the run's own scratch stamp byte-identical.
fn assert_same_stamped_config(with_dir: &Workspace, without_dir: &Workspace) {
    let pinned = without_run_stamp(&with_dir.member_file("worker", "oneharness.toml"));
    let baseline = without_run_stamp(&without_dir.member_file("worker", "oneharness.toml"));
    assert_eq!(
        pinned, baseline,
        "a member with a `dir` loads a different agent config from one without"
    );
    assert!(
        pinned.contains("claude-code"),
        "the stamped config is not the graph's own chain: {pinned}"
    );
}

/// The baseline every `dir` journey compares its stamped config against: the
/// same graph, the same task, and no `dir`.
fn baseline_without_dir() -> Workspace {
    let workspace = Workspace::new();
    workspace.graph(&graph(&workspace, None));
    workspace
        .run(&[
            "run",
            "./graph.yaml",
            "--task",
            "fake:complete-now report in.",
            "--dir",
            &workspace.dir().display().to_string(),
        ])
        .expect_code(0);
    workspace
}

/// A relative `dir` on a two-party member is the conversation's worktree, one
/// level inside the run's `--dir`: the agent side's harness starts there, the
/// conversation's skills come from there, and the agent side still runs under
/// its stamped config.
#[test]
fn a_two_party_members_relative_dir_is_the_conversations_worktree() {
    let workspace = Workspace::new();
    let resolved = workspace.dir().join("api");
    worker_works_in(&workspace, "./api", &resolved);
    assert_same_stamped_config(&workspace, &baseline_without_dir());
}

/// An absolute `dir` on a two-party member is used as written — a worktree that
/// is not below the run's `--dir` at all — with the same three consequences.
#[test]
fn a_two_party_members_absolute_dir_is_the_conversations_worktree() {
    let workspace = Workspace::new();
    let elsewhere = tempfile::tempdir().expect("a worktree elsewhere");
    let declared = elsewhere.path().display().to_string();
    worker_works_in(&workspace, &declared, elsewhere.path());
    assert_same_stamped_config(&workspace, &baseline_without_dir());
}

/// An empty `dir` on a two-party member is refused by `validate` and by `run`,
/// before any member is built, exactly as a single-sided member's is.
#[test]
fn a_two_party_members_empty_dir_is_refused_by_both_verbs() {
    let workspace = Workspace::new();
    workspace.graph(&graph(&workspace, Some("")));
    for args in [
        vec!["validate", "./graph.yaml"],
        vec!["run", "./graph.yaml", "--task", "fake:complete-now"],
    ] {
        let refused = workspace.run(&args);
        refused.expect_code(2);
        assert!(
            refused.stderr.contains("names no directory") && refused.stderr.contains("worker"),
            "{}: {}",
            args.join(" "),
            refused.stderr
        );
        assert!(
            refused.stdout.trim().is_empty(),
            "a refusal must publish no event: {}",
            refused.stdout
        );
    }
}
