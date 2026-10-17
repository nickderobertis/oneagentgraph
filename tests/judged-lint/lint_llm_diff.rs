//! `just lint-llm-diff` judges once per tree, base, and judge, and never replays
//! a failure.
//!
//! These journeys drive the recipe in a throwaway copy of this repository through
//! the real `justfile`, `scripts/nx.sh`, Nx and its cache, git, and the llmlint
//! that `just setup-llmlint` installed. Only the paid model call is replaced: a
//! stand-in oneharness, named by `LLMLINT_ONEHARNESS_BIN`, answers each judge call
//! with a verdict and records it, and hands every other subcommand (`--version`,
//! `config`) to the real oneharness installed beside llmlint, so `llmlint
//! fingerprint` digests the real harness configuration.
//!
//! The judge configuration — the rules and the harness configuration — sits in
//! the directory above the copy, where llmlint and oneharness both discover it.
//! Nx cannot see it there, so a change to it reaches the cache key only through
//! `llmlint fingerprint`: the journeys that change it prove that input rather than
//! the tree's.
//!
//! Unix-only: the recipe and the stand-in are bash.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "../repo-tooling/checkout.rs"]
mod checkout;
use checkout::git;

/// The rules every journey judges with, written above the copy.
const RULES: &str = "files:\n  exclude:\n    - \"**/.git/**\"\nrules:\n  - name: \
                     comments_explain_why\n    description: Every comment says why the code \
                     is the way it is.\n  - name: readme_stays_plain\n    description: The \
                     README describes the tool in plain words.\n";

/// The file each journey's diff changes. One of its own, because the repository's
/// files carry `llmlint: ignore` directives naming rules [`RULES`] does not have.
const JUDGED: &str = "JUDGED.md";

/// What the stand-in records for one judge call over [`RULES`].
const JUDGED_RULES: &str = "comments_explain_why,readme_stays_plain";

/// Nx's own line for a task it replayed instead of running.
const NX_REPLAYED: &str = "Nx read the output from the cache instead of running the command";

/// The paid model call, stood in for. `run` reads the rule names from the schema
/// llmlint hands it, records them, and replies as `STANDIN_REPLY` says: `pass`
/// (every rule holds), `fail` (the first rule is violated in JUDGED.md), `none`
/// (no rule answered), or `some` (only the first rule answered).
const STAND_IN: &str = r#"#!/usr/bin/env bash
set -euo pipefail
[ "${1:-}" = run ] || exec "$STANDIN_REAL_ONEHARNESS" "$@"
exec node -e '
const fs = require("fs");
const args = process.argv.slice(1);
const schema = JSON.parse(fs.readFileSync(args[args.indexOf("--schema") + 1], "utf8"));
const rules = Object.keys(schema.properties || {}).sort();
fs.appendFileSync(process.env.STANDIN_JUDGE_LOG, rules.join(",") + "\n");
const reply = process.env.STANDIN_REPLY || "pass";
const answered = reply === "none" ? [] : reply === "some" ? rules.slice(0, 1) : rules;
const structured = {};
for (const rule of answered) {
  structured[rule] = reply === "fail" && rule === rules[0]
    ? { holds: false, rationale: "stand-in finding",
        violations: [{ file: "JUDGED.md", line: 1, message: "stand-in finding" }] }
    : { holds: true, rationale: "stand-in pass" };
}
const result = { harness: "claude-code", status: "ok", exit_code: 0, structured,
  schema_valid: true, schema_attempts: 1, schema_error: null, error: null };
process.stdout.write(JSON.stringify({ schema_version: "0.1",
  oneharness_version: "stand-in", results: [result] }) + "\n");
' -- "$@"
"#;

struct Run {
    ok: bool,
    stdout: String,
    /// Both streams, colour stripped, for assertions that do not care which.
    output: String,
}

impl Run {
    fn new(output: &std::process::Output) -> Self {
        let stdout = strip_ansi(&String::from_utf8_lossy(&output.stdout));
        let stderr = strip_ansi(&String::from_utf8_lossy(&output.stderr));
        Self {
            ok: output.status.success(),
            output: format!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"),
            stdout,
        }
    }

    /// llmlint's own `N rules: …` summary line.
    fn summary(&self) -> &str {
        self.stdout
            .lines()
            .map(str::trim)
            .find(|line| {
                line.split_once(" rules: ")
                    .is_some_and(|(count, _)| count.chars().all(|c| c.is_ascii_digit()))
            })
            .unwrap_or_else(|| panic!("the run printed no llmlint summary:\n{}", self.output))
    }

    fn says(&self, expected: &str) -> bool {
        self.output.contains(expected)
    }

    fn assert_says(&self, expected: &str) -> &Self {
        assert!(
            self.says(expected),
            "the run never said `{expected}`:\n{}",
            self.output
        );
        self
    }
}

fn strip_ansi(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    plain
}

/// The installed llmlint, where `scripts/setup-llmlint.sh` puts it, else on `PATH`.
fn installed_llmlint() -> PathBuf {
    let home = std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/bin"));
    let path = std::env::var_os("PATH").unwrap_or_default();
    home.into_iter()
        .chain(std::env::split_paths(&path))
        .map(|dir| dir.join("llmlint"))
        .find(|candidate| candidate.is_file())
        .expect("these journeys run the installed llmlint: run `just setup-llmlint` first")
}

/// A throwaway copy of this repository, judged through the stand-in.
struct Workspace {
    /// Held for its drop: everything below lives inside it.
    _dir: tempfile::TempDir,
    root: PathBuf,
    rules: PathBuf,
    harness_config: PathBuf,
    judge_log: PathBuf,
    env: Vec<(String, String)>,
}

impl Workspace {
    /// The copy at a base commit, then one commit adding [`JUDGED`] on top, so the
    /// diff against that base has a file to judge.
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a directory for the throwaway checkout");
        let root = dir.path().join("checkout");
        checkout::copy_into(&root);

        let rules = dir.path().join("llmlint.yml");
        std::fs::write(&rules, RULES).expect("write the rules above the copy");
        std::fs::remove_file(root.join("llmlint.yml")).expect("remove the copy's own rules");
        let harness_config = dir.path().join("oneharness.toml");
        std::fs::rename(root.join("oneharness.toml"), &harness_config)
            .expect("move the copy's harness configuration above it");

        let llmlint = installed_llmlint();
        let real_oneharness = std::fs::canonicalize(&llmlint)
            .expect("resolve the installed llmlint")
            .with_file_name("oneharness");
        assert!(
            real_oneharness.is_file(),
            "no oneharness beside the installed llmlint at {}: run `just setup-llmlint`",
            real_oneharness.display()
        );
        let stand_in = dir.path().join("stand-in/oneharness");
        write_executable(&stand_in, STAND_IN);
        let judge_log = dir.path().join("judge-calls.log");
        std::fs::write(&judge_log, "").expect("open the judge-call log");

        let home = dir.path().join("home");
        let path = format!(
            "{}:{}",
            llmlint.parent().expect("llmlint has a directory").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let env = [
            ("PATH", path),
            ("HOME", home.display().to_string()),
            ("XDG_CACHE_HOME", home.join(".cache").display().to_string()),
            (
                "XDG_CONFIG_HOME",
                home.join(".config").display().to_string(),
            ),
            ("XDG_STATE_HOME", home.join(".state").display().to_string()),
            ("XDG_DATA_HOME", home.join(".data").display().to_string()),
            ("LLMLINT_ONEHARNESS_BIN", stand_in.display().to_string()),
            (
                "STANDIN_REAL_ONEHARNESS",
                real_oneharness.display().to_string(),
            ),
            ("STANDIN_JUDGE_LOG", judge_log.display().to_string()),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_string(), value))
        .collect();

        let workspace = Self {
            _dir: dir,
            root,
            rules,
            harness_config,
            judge_log,
            env,
        };
        git(&workspace.root, &["init", "-q"]);
        workspace.commit("the base");
        workspace.change_judged_file();
        workspace.commit("the change under judgement");
        workspace
    }

    /// A command in the copy with only this journey's environment deciding the
    /// judge, the cache, and how Nx renders.
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.current_dir(&self.root);
        for (name, _) in std::env::vars() {
            if ["NX_", "ONEHARNESS_", "LLMLINT_"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
                || ["CI", "GITHUB_ACTIONS", "CLAUDECODE", "CODEX_THREAD_ID"].contains(&&*name)
            {
                command.env_remove(name);
            }
        }
        command.envs(self.env.iter().map(|(name, value)| (name, value)));
        command
    }

    /// `just lint-llm-diff <base> [nx_args]`, as CI and the gate call it.
    fn lint(&self, base: &str, nx_args: &[&str], env: &[(&str, &str)]) -> Run {
        let output = self
            .command("just")
            .args(["lint-llm-diff", base])
            .args(nx_args)
            .envs(env.iter().copied())
            .output()
            .expect("`just` runs the recipe — install it and retry");
        Run::new(&output)
    }

    /// The real `llmlint fingerprint`, under the environment the target runs in.
    fn fingerprint(&self) -> String {
        let run = Run::new(
            &self
                .command("llmlint")
                .arg("fingerprint")
                .output()
                .expect("run llmlint fingerprint"),
        );
        assert!(run.ok, "llmlint fingerprint failed:\n{}", run.output);
        run.stdout.trim().to_string()
    }

    /// The rule names of every judge call the stand-in received, in order.
    fn judge_calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.judge_log)
            .expect("read the judge-call log")
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn commit(&self, message: &str) -> String {
        git(&self.root, &["add", "-A"]);
        git(
            &self.root,
            &["commit", "-q", "--allow-empty", "-m", message],
        );
        git(&self.root, &["rev-parse", "HEAD"]).trim().to_string()
    }

    fn base(&self) -> String {
        git(&self.root, &["rev-parse", "HEAD~1"]).trim().to_string()
    }

    fn change_judged_file(&self) {
        let path = self.root.join(JUDGED);
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        std::fs::write(&path, existing + "One more plain sentence.\n")
            .expect("change the judged file");
    }

    fn replace_in(path: &Path, from: &str, to: &str) {
        let existing = std::fs::read_to_string(path).expect("read a judge configuration file");
        assert!(
            existing.contains(from),
            "{} has no `{from}`",
            path.display()
        );
        std::fs::write(path, existing.replace(from, to)).expect("change it");
    }

    /// Judge once and expect a pass the stand-in recorded.
    fn judged_pass(&self, base: &str, calls_before: usize) -> Run {
        let run = self.lint(base, &[], &[]);
        assert!(run.ok, "the judged run should have passed:\n{}", run.output);
        assert_eq!(
            self.judge_calls().len(),
            calls_before + 1,
            "the run made no judge call — it was replayed or never judged:\n{}",
            run.output
        );
        assert!(
            !run.says(NX_REPLAYED),
            "a judged run claims a replay:\n{}",
            run.output
        );
        run
    }

    /// Run again and expect Nx to replay the earlier pass with no judge call.
    fn replayed_pass(&self, base: &str, judged: &Run) -> Run {
        let calls = self.judge_calls().len();
        let run = self.lint(base, &[], &[]);
        assert!(
            run.ok,
            "the replayed run should have passed:\n{}",
            run.output
        );
        assert_eq!(
            self.judge_calls().len(),
            calls,
            "the run paid the judge again instead of replaying:\n{}",
            run.output
        );
        run.assert_says(NX_REPLAYED);
        assert_eq!(
            run.summary(),
            judged.summary(),
            "the replay said something else"
        );
        run
    }
}

fn write_executable(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("an executable has a directory"))
        .expect("create the directory holding an executable");
    std::fs::write(path, body).expect("write an executable");
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .expect("make it executable");
}

#[test]
fn a_judged_pass_replays_for_an_unchanged_tree_base_and_judge() {
    let workspace = Workspace::new();
    let base = workspace.base();

    let judged = workspace.judged_pass(&base, 0);
    judged.assert_says(&format!("lint-llm-diff: base {base}"));
    assert!(
        judged.summary().contains("2 passed, 0 failed"),
        "{}",
        judged.output
    );
    assert_eq!(workspace.judge_calls(), [JUDGED_RULES]);

    workspace
        .replayed_pass(&base, &judged)
        .assert_says(&format!("lint-llm-diff: base {base}"));
}

#[test]
fn a_new_base_is_judged_again_and_each_base_replays_its_own_pass() {
    let workspace = Workspace::new();
    let first_base = workspace.base();
    workspace.change_judged_file();
    workspace.commit("a second change");
    let second_base = workspace.base();

    let first = workspace.judged_pass(&first_base, 0);
    let second = workspace.judged_pass(&second_base, 1);
    second.assert_says(&format!("lint-llm-diff: base {second_base}"));

    workspace.replayed_pass(&first_base, &first);
    workspace.replayed_pass(&second_base, &second);
}

#[test]
fn a_changed_rule_is_judged_again() {
    let workspace = Workspace::new();
    let base = workspace.base();
    workspace.judged_pass(&base, 0);

    let before = workspace.fingerprint();
    Workspace::replace_in(&workspace.rules, "in plain words", "in short, plain words");
    assert_ne!(
        workspace.fingerprint(),
        before,
        "the changed rule left the digest as it was"
    );

    let judged = workspace.judged_pass(&base, 1);
    workspace.replayed_pass(&base, &judged);
}

#[test]
fn a_changed_harness_model_is_judged_again() {
    let workspace = Workspace::new();
    let base = workspace.base();
    workspace.judged_pass(&base, 0);

    let before = workspace.fingerprint();
    Workspace::replace_in(
        &workspace.harness_config,
        "model = \"claude-opus-4-8\"",
        "model = \"claude-sonnet-4-8\"",
    );
    assert_ne!(
        workspace.fingerprint(),
        before,
        "the changed model left the digest as it was"
    );

    let judged = workspace.judged_pass(&base, 1);
    workspace.replayed_pass(&base, &judged);
}

#[test]
fn a_changed_tracked_file_is_judged_again() {
    let workspace = Workspace::new();
    let base = workspace.base();
    workspace.judged_pass(&base, 0);

    let before = workspace.fingerprint();
    workspace.change_judged_file();
    assert_eq!(
        workspace.fingerprint(),
        before,
        "a tree change moved the judge digest"
    );

    let judged = workspace.judged_pass(&base, 1);
    workspace.replayed_pass(&base, &judged);
}

#[test]
fn a_failing_verdict_reaches_a_piped_caller_and_is_judged_again() {
    let workspace = Workspace::new();
    let base = workspace.base();
    let ci = [("CI", "true"), ("CLAUDECODE", "1")];

    let failed = workspace.lint(&base, &[], &[ci[0], ci[1], ("STANDIN_REPLY", "fail")]);
    assert!(!failed.ok, "a failing verdict passed:\n{}", failed.output);
    failed
        .assert_says("FAIL comments_explain_why")
        .assert_says("JUDGED.md:1: stand-in finding");
    assert_eq!(workspace.judge_calls().len(), 1);

    let again = workspace.lint(&base, &[], &ci);
    assert!(
        again.ok,
        "the re-judged run should have passed:\n{}",
        again.output
    );
    assert_eq!(
        workspace.judge_calls().len(),
        2,
        "the failure was replayed instead of judged again:\n{}",
        again.output
    );
}

/// A reply that leaves `unanswered` without a verdict fails naming each of them
/// and only them, and is not cached.
fn assert_an_incomplete_reply_fails_and_is_judged_again(reply: &str, unanswered: &[&str]) {
    let workspace = Workspace::new();
    let base = workspace.base();

    let refused = workspace.lint(&base, &[], &[("STANDIN_REPLY", reply)]);
    assert!(
        !refused.ok,
        "an incomplete reply passed:\n{}",
        refused.output
    );
    for rule in JUDGED_RULES.split(',') {
        let named = refused.says(&format!(
            "rule {rule}: the judge returned no verdict for it"
        ));
        assert_eq!(
            named,
            unanswered.contains(&rule),
            "`{rule}` named wrongly as unanswered:\n{}",
            refused.output
        );
    }
    assert_eq!(workspace.judge_calls().len(), 1);

    workspace.judged_pass(&base, 1);
}

#[test]
fn a_reply_answering_no_rule_fails_naming_each_and_is_judged_again() {
    assert_an_incomplete_reply_fails_and_is_judged_again(
        "none",
        &["comments_explain_why", "readme_stays_plain"],
    );
}

#[test]
fn a_reply_answering_some_rules_fails_naming_the_rest_and_is_judged_again() {
    assert_an_incomplete_reply_fails_and_is_judged_again("some", &["readme_stays_plain"]);
}

#[test]
fn an_unresolvable_base_is_refused_before_any_judge_call() {
    let workspace = Workspace::new();

    let refused = workspace.lint("no-such-base", &[], &[]);
    assert!(
        !refused.ok,
        "an unresolvable base was judged:\n{}",
        refused.output
    );
    let refusal: Vec<&str> = refused
        .output
        .lines()
        .filter(|line| line.contains("no-such-base"))
        .collect();
    assert_eq!(
        refusal,
        ["lint-llm-diff: 'no-such-base' does not resolve to a commit"],
        "the refusal is not one line naming the base, with no advice:\n{}",
        refused.output
    );
    assert!(
        !refused.says("nx run"),
        "Nx ran for a refused base:\n{}",
        refused.output
    );
    assert!(workspace.judge_calls().is_empty());
}

#[test]
fn an_explicit_or_ambient_cache_skip_judges_again() {
    let workspace = Workspace::new();
    let base = workspace.base();
    workspace.judged_pass(&base, 0);

    for (nx_args, env, what) in [
        (
            &["--skip-nx-cache"][..],
            &[][..],
            "`--skip-nx-cache` after the base",
        ),
        (
            &[][..],
            &[("NX_SKIP_NX_CACHE", "true")][..],
            "an exported NX_SKIP_NX_CACHE",
        ),
    ] {
        let calls = workspace.judge_calls().len();
        let run = workspace.lint(&base, nx_args, env);
        assert!(
            run.ok,
            "the run under {what} should have passed:\n{}",
            run.output
        );
        assert_eq!(
            workspace.judge_calls().len(),
            calls + 1,
            "{what} replayed the cached pass instead of judging again:\n{}",
            run.output
        );
    }
}
