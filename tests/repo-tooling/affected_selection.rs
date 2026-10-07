//! Which projects a change reaches, as the gate and CI's `changes` job ask it.
//!
//! The test suite is split across Nx projects so that a change pays only for the
//! tiers that read what it touched. That split is only as good as the selection
//! that runs it, so these journeys drive the real `scripts/nx-affected.sh`, the
//! real `scripts/nx.sh`, real Nx, and every real `project.json` over temporary
//! commits in a throwaway copy of this repository, and read the task graph Nx
//! would run (`--graph=stdout`, which plans without executing) and the
//! `--affects oneagentgraph` answer CI's `changes` job publishes.
//!
//! Selection is half of it; the other half is the cache. The last journeys warm
//! a split tier's real `test` target through `scripts/nx.sh` and show that a
//! change to one of its declared inputs reruns it while a change outside them
//! replays it. The test body is a stand-in there, so cargo is never the subject;
//! the `inputs` deciding are the project's own, untouched.
//!
//! Unix-only for the reason the judged tier's cache journeys are: the scripts
//! under test are bash, and the Windows CI leg reaches them through a shell this
//! test cannot assume.
#![cfg(unix)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

mod checkout;
use checkout::git;

/// The test targets of the tiers that read the crate, and the one that does not.
const UNIT: &str = "oneagentgraph:test";
const COVERAGE: &str = "oneagentgraph:coverage";
const CONTRACT: &str = "oneagentgraph-contract:test";
const E2E: &str = "oneagentgraph-e2e:test";
const REPO_TOOLING: &str = "oneagentgraph-repo-tooling:test";

/// A commit nothing in any checkout can name: GitHub's `before` on a first push.
const NO_COMMIT: &str = "0000000000000000000000000000000000000000";

/// What the script printed for one invocation.
struct Run {
    stdout: String,
    stderr: String,
}

/// A throwaway copy of this repository whose `origin/main` is its first commit.
struct Repo {
    /// Held for its drop: everything below lives inside it.
    _dir: tempfile::TempDir,
    root: PathBuf,
    cache: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a directory for the throwaway checkout");
        let root = dir.path().join("checkout");
        checkout::copy_into(&root);
        let repo = Self {
            cache: dir.path().join("cache"),
            _dir: dir,
            root,
        };
        git(&repo.root, &["init", "-q"]);
        let base = repo.commit("the checkout under test");
        // What a pull-request runner's fetch leaves behind: the base branch as a
        // remote-tracking ref, which the merge-base path resolves against.
        git(
            &repo.root,
            &["update-ref", "refs/remotes/origin/main", &base],
        );
        repo
    }

    /// Append a comment line to one tracked file and commit it.
    fn change(&self, relative: &str) -> String {
        let path = self.root.join(relative);
        let existing = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {relative} in the copy: {err}"));
        let marker = match Path::new(relative).extension().and_then(|e| e.to_str()) {
            Some("rs" | "js" | "mjs") => "// changed by a selection journey\n",
            _ => "# changed by a selection journey\n",
        };
        std::fs::write(&path, existing + marker)
            .unwrap_or_else(|err| panic!("change {relative} in the copy: {err}"));
        self.commit(&format!("change {relative}"))
    }

    fn commit(&self, message: &str) -> String {
        git(&self.root, &["add", "-A"]);
        git(
            &self.root,
            &["commit", "-q", "--allow-empty", "-m", message],
        );
        git(&self.root, &["rev-parse", "HEAD"]).trim().to_string()
    }

    /// Run the script with exactly the environment a journey names: nothing from
    /// the developer's shell or a CI runner may decide which base it picks.
    fn script(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let mut command = Command::new("bash");
        command
            .current_dir(&self.root)
            .arg("scripts/nx-affected.sh")
            .args(args);
        for name in [
            "CI",
            "GITHUB_BASE_REF",
            "GITHUB_EVENT_NAME",
            "ONEAGENTGRAPH_NX_BASE_REF",
            "ONEAGENTGRAPH_NX_BASE_SHA",
            "NX_SKIP_NX_CACHE",
            "NX_DISABLE_NX_CACHE",
        ] {
            command.env_remove(name);
        }
        command
            .env("XDG_CACHE_HOME", &self.cache)
            .env("ONEAGENTGRAPH_NX_SHOW_OUTPUT", "1")
            .envs(env.iter().copied());
        let output = command.output().expect("bash runs scripts/nx-affected.sh");
        let run = Run {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        };
        assert!(
            output.status.success(),
            "nx-affected.sh {args:?} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            run.stdout,
            run.stderr
        );
        run
    }

    /// CI's `changes` answer for the Rust jobs, and what the script said about it.
    fn reaches_a_rust_project(&self, env: &[(&str, &str)]) -> (bool, String) {
        let run = self.script(&["--affects", "oneagentgraph"], env);
        let answer = match run.stdout.trim() {
            "true" => true,
            "false" => false,
            other => panic!("`--affects` answered {other:?}:\n{}", run.stderr),
        };
        (answer, run.stderr)
    }

    /// Every task `check` would run, read from Nx's plan rather than by running it.
    fn gate_tasks(&self, env: &[(&str, &str)]) -> (BTreeSet<String>, String) {
        let run = self.script(&["-t", "check", "--graph=stdout"], env);
        let plan: serde_json::Value = serde_json::from_str(&run.stdout).unwrap_or_else(|err| {
            panic!(
                "Nx's task graph is not JSON ({err}):\n{}\n{}",
                run.stdout, run.stderr
            )
        });
        let tasks = plan["tasks"]["tasks"]
            .as_object()
            .unwrap_or_else(|| panic!("Nx's task graph lists no tasks:\n{}", run.stdout))
            .keys()
            .cloned()
            .collect();
        (tasks, run.stderr)
    }
}

/// The base a local run scopes against when the caller names one by branch.
const AGAINST_MAIN: &[(&str, &str)] = &[("ONEAGENTGRAPH_NX_BASE_REF", "main")];

fn assert_selects(tasks: &BTreeSet<String>, wanted: &[&str], unwanted: &[&str], why: &str) {
    for task in wanted {
        assert!(
            tasks.contains(*task),
            "{why}: {task} was not selected; the plan was {tasks:?}"
        );
    }
    for task in unwanted {
        assert!(
            !tasks.contains(*task),
            "{why}: {task} was selected; the plan was {tasks:?}"
        );
    }
}

#[test]
fn a_script_only_the_repo_tooling_journeys_read_selects_that_tier_alone() {
    let repo = Repo::new();
    repo.change("scripts/llmlint-judge.sh");

    let (tasks, _) = repo.gate_tasks(AGAINST_MAIN);
    assert_selects(
        &tasks,
        &[REPO_TOOLING],
        &[UNIT, COVERAGE, CONTRACT, E2E],
        "a change to a script only the repo-tooling journeys read",
    );
}

#[test]
fn a_crate_source_change_selects_the_crate_and_every_tier_that_reads_it() {
    let repo = Repo::new();
    repo.change("src/lib.rs");

    let (tasks, _) = repo.gate_tasks(AGAINST_MAIN);
    assert_selects(
        &tasks,
        &[UNIT, COVERAGE, CONTRACT, E2E],
        &[REPO_TOOLING],
        "a change to the crate's source",
    );
}

#[test]
fn a_test_tier_change_reruns_that_tier_and_the_floor_it_feeds() {
    let repo = Repo::new();
    repo.change("tests/contract.rs");

    let (tasks, _) = repo.gate_tasks(AGAINST_MAIN);
    // The merged floor needs every instrumented tier's profiles, so `coverage`
    // pulls the other two in; what the split buys is that nothing outside the
    // crate's instrumented tiers runs.
    assert_selects(
        &tasks,
        &[CONTRACT, COVERAGE, UNIT, E2E],
        &[REPO_TOOLING, "oneagentgraph-npm:test"],
        "a change to a contract-tier test",
    );
}

#[test]
fn a_change_confined_to_one_split_tier_still_runs_the_rust_jobs() {
    for file in [
        "tests/e2e/verbs.rs",
        "tests/contract.rs",
        "tests/repo-tooling/checkout.rs",
    ] {
        let repo = Repo::new();
        repo.change(file);
        let (answer, stderr) = repo.reaches_a_rust_project(AGAINST_MAIN);
        assert!(
            answer,
            "a change confined to {file} skipped the Rust jobs:\n{stderr}"
        );
    }
}

#[test]
fn a_change_reaching_no_rust_project_skips_the_rust_jobs() {
    let repo = Repo::new();
    repo.change("npm/oneagentgraph-cli/bin/oneagentgraph.js");

    let (answer, stderr) = repo.reaches_a_rust_project(AGAINST_MAIN);
    assert!(!answer, "an npm-only change ran the Rust jobs:\n{stderr}");
}

#[test]
fn a_change_to_root_configuration_still_runs_the_rust_jobs() {
    // `deny.toml` is read by no test, only by the supply-chain check the `deny`
    // job runs; the repo-level project that owns it is what keeps it in scope.
    let repo = Repo::new();
    repo.change("deny.toml");

    let (answer, stderr) = repo.reaches_a_rust_project(AGAINST_MAIN);
    assert!(
        answer,
        "a deny.toml-only change skipped the Rust jobs:\n{stderr}"
    );
}

#[test]
fn a_named_base_commit_takes_precedence_over_the_base_branch() {
    let repo = Repo::new();
    let crate_change = repo.change("src/lib.rs");
    repo.change("npm/oneagentgraph-cli/bin/oneagentgraph.js");

    // Against the branch, the crate change is in range.
    let (by_branch, stderr) = repo.reaches_a_rust_project(AGAINST_MAIN);
    assert!(
        by_branch,
        "the crate change was out of range of main:\n{stderr}"
    );

    // Against the commit after it, only the npm change is — and the branch,
    // still set, is not what the script used.
    let (by_commit, stderr) = repo.reaches_a_rust_project(&[
        ("ONEAGENTGRAPH_NX_BASE_REF", "main"),
        ("ONEAGENTGRAPH_NX_BASE_SHA", &crate_change),
    ]);
    assert!(
        !by_commit,
        "the named commit did not take precedence over the branch:\n{stderr}"
    );
}

#[test]
fn an_unresolvable_base_commit_fails_closed_and_names_the_variable() {
    let repo = Repo::new();
    repo.change("npm/oneagentgraph-cli/bin/oneagentgraph.js");

    for sha in [NO_COMMIT, "not-a-commit", "0123456789abcdef"] {
        let env = [
            ("ONEAGENTGRAPH_NX_BASE_REF", "main"),
            ("ONEAGENTGRAPH_NX_BASE_SHA", sha),
        ];
        let (answer, stderr) = repo.reaches_a_rust_project(&env);
        assert!(
            answer,
            "base {sha:?} scoped the answer instead of failing closed:\n{stderr}"
        );
        assert!(
            stderr.contains("ONEAGENTGRAPH_NX_BASE_SHA") && stderr.contains(sha),
            "base {sha:?} failed closed without naming the variable:\n{stderr}"
        );

        let (tasks, stderr) = repo.gate_tasks(&env);
        assert_selects(
            &tasks,
            &[
                UNIT,
                COVERAGE,
                CONTRACT,
                E2E,
                REPO_TOOLING,
                "oneagentgraph-npm:test",
            ],
            &[],
            &format!("base {sha:?} must run every project"),
        );
        assert!(
            stderr.contains("running every project"),
            "base {sha:?} widened the run without saying so:\n{stderr}"
        );
    }
}

#[test]
fn a_base_branch_git_would_refuse_fails_closed_and_names_it() {
    let repo = Repo::new();
    repo.change("npm/oneagentgraph-cli/bin/oneagentgraph.js");

    // Each passes the character set and fails git's ref-name rules, so only the
    // second check stands between it and the fetch refspec.
    for branch in ["main..evil", "main.lock", "main/"] {
        let env = [("ONEAGENTGRAPH_NX_BASE_REF", branch)];
        let (answer, stderr) = repo.reaches_a_rust_project(&env);
        assert!(
            answer,
            "base branch {branch:?} scoped the answer instead of failing closed:\n{stderr}"
        );
        assert!(
            stderr.contains(&format!("'{branch}' is not a usable branch name")),
            "base branch {branch:?} failed closed without naming it:\n{stderr}"
        );

        let (tasks, stderr) = repo.gate_tasks(&env);
        assert_selects(
            &tasks,
            &[
                UNIT,
                COVERAGE,
                CONTRACT,
                E2E,
                REPO_TOOLING,
                "oneagentgraph-npm:test",
            ],
            &[],
            &format!("base branch {branch:?} must run every project"),
        );
        assert!(
            stderr.contains("running every project"),
            "base branch {branch:?} widened the run without saying so:\n{stderr}"
        );
    }
}

#[test]
fn with_neither_base_a_local_run_scopes_against_main() {
    let repo = Repo::new();
    repo.change("scripts/llmlint-judge.sh");

    let (tasks, stderr) = repo.gate_tasks(&[]);
    assert_selects(
        &tasks,
        &[REPO_TOOLING],
        &[UNIT, CONTRACT, E2E],
        "a local run with no base named",
    );
    assert!(
        !stderr.contains("running every project"),
        "a local run with no base named failed closed:\n{stderr}"
    );
}

#[test]
fn a_push_build_scopes_to_the_commit_it_replaced_and_fails_closed_without_one() {
    let repo = Repo::new();
    let before = git(&repo.root, &["rev-parse", "HEAD"]).trim().to_string();
    repo.change("scripts/llmlint-judge.sh");

    // What ci.yml passes on a push to main: no base branch, and the commit the
    // push replaced.
    let after_before = [
        ("CI", "true"),
        ("GITHUB_EVENT_NAME", "push"),
        ("ONEAGENTGRAPH_NX_BASE_SHA", before.as_str()),
    ];
    let (tasks, stderr) = repo.gate_tasks(&after_before);
    assert_selects(
        &tasks,
        &[REPO_TOOLING],
        &[UNIT, COVERAGE, CONTRACT, E2E],
        "a push scoped to the commit it replaced",
    );
    assert!(
        !stderr.contains("running every project"),
        "a push with a usable base failed closed:\n{stderr}"
    );
    let (answer, stderr) = repo.reaches_a_rust_project(&after_before);
    assert!(answer, "the repo-tooling tier is a Rust project:\n{stderr}");

    // A first push names no commit at all, and a build that passes no base has
    // none to derive: both run everything.
    let first_push = [
        ("CI", "true"),
        ("GITHUB_EVENT_NAME", "push"),
        ("ONEAGENTGRAPH_NX_BASE_SHA", NO_COMMIT),
    ];
    let no_base = [("CI", "true"), ("GITHUB_EVENT_NAME", "push")];
    for (what, env) in [
        ("a first push", &first_push[..]),
        ("a push that names no base", &no_base[..]),
    ] {
        let (tasks, stderr) = repo.gate_tasks(env);
        assert_selects(
            &tasks,
            &[UNIT, COVERAGE, CONTRACT, E2E, REPO_TOOLING],
            &[],
            &format!("{what} must run every project"),
        );
        assert!(
            stderr.contains("running every project"),
            "{what} widened the run without saying so:\n{stderr}"
        );
    }
}

/// What one `nx run` of a stand-in test target reported, and whether it ran.
struct TargetRun {
    executed: bool,
    output: String,
}

impl Repo {
    /// Swap one tier's `test` command for a stand-in that records each execution
    /// outside the workspace, leaving the target's `inputs` and `outputs` exactly
    /// as the real `project.json` declares them: those, not the stand-in, are what
    /// Nx keys the cache on. The stand-in buys a test target that costs nothing to
    /// run, so the journey reads Nx's caching decision rather than cargo's.
    fn stand_in_test_body(&self, project_json: &str, runs: &Path) {
        let path = self.root.join(project_json);
        let mut project: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("read {project_json} in the copy: {err}")),
        )
        .unwrap_or_else(|err| panic!("{project_json} is not plain JSON: {err}"));
        let test = &mut project["targets"]["test"];
        assert!(
            test["inputs"].as_array().is_some_and(|i| !i.is_empty()),
            "{project_json}'s test target declares no inputs: {test}"
        );
        test["command"] = format!("echo ran >> '{}'", runs.display()).into();
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&project).expect("serialize"),
        )
        .unwrap_or_else(|err| panic!("write {project_json} in the copy: {err}"));
        self.commit(&format!("stand in for {project_json}'s test body"));
    }

    /// Run one target through the real `scripts/nx.sh`, as a gate recipe does,
    /// and read off whether the stand-in body actually executed.
    fn run_target(&self, task: &str, runs: &Path) -> TargetRun {
        let before = execution_count(runs);
        let mut command = Command::new("bash");
        command.current_dir(&self.root).arg("scripts/nx.sh").args([
            "run",
            task,
            "--outputStyle=static",
        ]);
        for name in ["CI", "NX_SKIP_NX_CACHE", "NX_DISABLE_NX_CACHE"] {
            command.env_remove(name);
        }
        command
            .env("XDG_CACHE_HOME", &self.cache)
            .env("ONEAGENTGRAPH_NX_SHOW_OUTPUT", "1");
        let output = command.output().expect("bash runs scripts/nx.sh");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "nx run {task} failed:\n{text}");
        let executed = match execution_count(runs) - before {
            0 => false,
            1 => true,
            more => panic!("nx run {task} executed its body {more} times:\n{text}"),
        };
        // Nx's own account has to agree with what the stand-in recorded, or the
        // journey would be reading a body that ran for some other reason.
        let says_cached = text.contains("existing outputs match the cache")
            || text.contains("[local cache]")
            || text.contains("read the output from the cache");
        assert_eq!(
            says_cached, !executed,
            "Nx's report disagrees with whether {task} ran (executed: {executed}):\n{text}"
        );
        TargetRun {
            executed,
            output: text,
        }
    }
}

fn execution_count(runs: &Path) -> usize {
    std::fs::read_to_string(runs).map_or(0, |log| log.lines().count())
}

/// Warm one tier's `test` cache, then show that a change to a file it declares
/// as an input reruns it while a change to a file outside its inputs replays it.
fn assert_test_target_keys_on_its_inputs(
    project_json: &str,
    task: &str,
    declared_input: &str,
    outside_inputs: &str,
) {
    let repo = Repo::new();
    let runs = repo.cache.with_file_name("runs.log");
    repo.stand_in_test_body(project_json, &runs);

    let cold = repo.run_target(task, &runs);
    assert!(
        cold.executed,
        "{task} replayed on a cold cache:\n{}",
        cold.output
    );
    let warm = repo.run_target(task, &runs);
    assert!(
        !warm.executed,
        "{task} reran with nothing changed:\n{}",
        warm.output
    );

    repo.change(outside_inputs);
    let unrelated = repo.run_target(task, &runs);
    assert!(
        !unrelated.executed,
        "{task} reran after a change to {outside_inputs}, which it does not read:\n{}",
        unrelated.output
    );

    repo.change(declared_input);
    let related = repo.run_target(task, &runs);
    assert!(
        related.executed,
        "{task} replayed a stale result after a change to its input {declared_input}:\n{}",
        related.output
    );
}

#[test]
fn the_repo_tooling_test_cache_reruns_on_a_script_and_replays_over_crate_source() {
    assert_test_target_keys_on_its_inputs(
        "tests/repo-tooling/project.json",
        REPO_TOOLING,
        "scripts/llmlint-judge.sh",
        "src/lib.rs",
    );
}

#[test]
fn the_e2e_test_cache_reruns_on_a_journey_and_replays_over_a_tooling_script() {
    assert_test_target_keys_on_its_inputs(
        "tests/e2e/project.json",
        E2E,
        "tests/e2e/verbs.rs",
        "scripts/llmlint-judge.sh",
    );
}
