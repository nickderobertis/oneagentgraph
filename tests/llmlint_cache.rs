//! The judged tier answers once per tree, base, and judge configuration.
//!
//! `llmlint` is an LLM judge: two runs over one unchanged diff have named
//! different rules, and one `just gate` invocation has produced two opposite
//! verdicts on a single tree. `just lint-llm-diff` therefore routes through the
//! cached Nx `oneagentgraph:lint-llm-diff` target, which replays a clean run's own
//! report instead of rolling the dice again. These journeys drive that recipe —
//! the real `justfile`, the real `scripts/nx.sh`, real Nx, the real target
//! definition, the real `scripts/llmlint-fingerprint.sh` and
//! `scripts/llmlint-judge.sh`, and a real git checkout — in a throwaway copy of
//! this repository.
//!
//! Counting judge runs is what proves a report was replayed rather than re-rolled,
//! and the claim under test — that one tree yields the same answer twice — is one
//! a non-deterministic judge cannot demonstrate about itself. So the judge is the
//! one thing here that is not real.
//!
//! Unix-only for the reason the published smoke is: the recipe, the wrapper, and
//! the two scripts under test are bash, and the Windows CI leg reaches them
//! through a shell this test cannot assume.
//!
//! llmlint: ignore-file[e2e_not_mocked] The `llmlint` binary is this tier's paid
//! model boundary, stood in for exactly as `tests/e2e/support.rs` stands in for the
//! paid harness process — and it is also the one thing these journeys cannot use
//! for real, because the property being proven is that an unchanged tree answers
//! twice the same, which a judge that rolls the dice cannot show. Every other
//! boundary is real: `just`, `scripts/nx.sh`, Nx's own cache, git, and both
//! scripts this change adds.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// This checkout, which every journey copies rather than mutates.
const REPO: &str = env!("CARGO_MANIFEST_DIR");
/// What the stand-in judge prints when it finds nothing.
///
/// It reaches the judge through the environment rather than being spliced into
/// its script: a payload pasted into a shell body is parsed as shell, and a real
/// judge's report is full of the backticks, `$` and `$(` that would then run.
/// This same fixture shape in a sibling repository pasted a report line
/// containing `` `llmlint history <id>` `` into a double-quoted `echo`, which
/// re-entered the stand-in on `PATH` and forked the host to its process limit. A
/// parameter expansion of a value the shell never parsed cannot do that, whatever
/// the value says.
///
/// So both payloads carry the backticks a real judge's report is full of, and the
/// command inside them is one no host has — anyone who re-splices these into the
/// script gets an assertion failure on a verdict that came back short, rather than
/// a fork bomb.
const PASS_VERDICT: &str = "fake-judge: 4 passed, 0 failed — full report: `llmlint-history 0ff1ce`";
/// What it prints when it does, on the run that also exits non-zero.
const FAIL_FINDING: &str = "fake-judge finding: robust_shell in scripts/llmlint-judge.sh — \
     full report: `llmlint-history 0ff1ce`";
/// The recipe's provenance line for a run that paid the judge.
const JUDGED: &str = "judged this diff against base";
/// The recipe's provenance line for a run that replayed one that already had.
const REPLAYED: &str = "replayed the recorded verdict for base";

/// A judge that counts its runs, and resolves a configuration for real off disk.
///
/// `config` renders the two things the fingerprint has to notice — the resolved
/// oneharness binary, which a caller's environment can otherwise smuggle into the
/// key, and every plugin's rule source, which lives outside the tree Nx hashes.
const FAKE_LLMLINT: &str = r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then
  echo "llmlint ${FAKE_LLMLINT_VERSION:-0.0.0-e2e}"
  exit 0
fi
if [[ ${1:-} == "config" ]]; then
  echo "oneharness.bin=${LLMLINT_ONEHARNESS_BIN:-<resolved beside llmlint>}"
  cat llmlint.yml
  sed -n 's/^  - "\(.*\)"$/\1/p' llmlint.yml | while read -r plugin; do cat "$plugin"; done
  exit 0
fi
printf '%s\n' "$*" >>"$FAKE_LLMLINT_LOG"
if [[ ${FAKE_LLMLINT_EXIT:-0} != 0 ]]; then
  printf '%s\n' "$FAKE_LLMLINT_FINDING"
  exit "$FAKE_LLMLINT_EXIT"
fi
printf '%s\n' "$FAKE_LLMLINT_VERDICT"
"#;

/// An llmlint that answers only `--version`, for the journeys that put one in
/// front of the pinned install on `PATH`. Reaching it for anything else is the
/// failure those journeys are looking for, so it says so and fails.
const AMBIENT_LLMLINT: &str = r#"#!/usr/bin/env bash
set -uo pipefail
[[ ${1:-} == "--version" ]] || { echo "the ambient llmlint was reached for '${1:-}'" >&2; exit 2; }
echo "llmlint 1.0.0-ambient"
"#;

/// What one `just lint-llm-diff` invocation produced.
struct Verdict {
    ok: bool,
    /// The tier's product on a green run: the judge's report.
    stdout: String,
    /// Its diagnostics, and its report when the run failed.
    stderr: String,
    /// Both, for an assertion that does not care which stream carried it.
    report: String,
}

impl Verdict {
    fn new(output: &std::process::Output) -> Self {
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Self {
            ok: output.status.success(),
            report: format!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"),
            stdout,
            stderr,
        }
    }

    fn assert_green(&self, what: &str) -> &Self {
        assert!(self.ok, "{what} should have passed:\n{}", self.report);
        self
    }

    fn assert_says(&self, expected: &str) -> &Self {
        assert!(
            self.report.contains(expected),
            "the run never said `{expected}`:\n{}",
            self.report
        );
        self
    }
}

/// A throwaway copy of this repository, wired to count judge runs.
struct Workspace {
    /// Held for its drop: everything below lives inside it.
    _dir: tempfile::TempDir,
    root: PathBuf,
    /// The rule source outside the checkout, which only the fingerprint can see.
    plugin: PathBuf,
    /// Where the pinned judge is installed, so a journey can replace it.
    pinned_bin: PathBuf,
    judge_log: PathBuf,
    env: BTreeMap<String, String>,
}

impl Workspace {
    /// Copy exactly what git would commit from here, so the copy hashes the way
    /// the original does: Nx skips ignored state, and bringing `target/` or `.nx/`
    /// along would add files the original never hashed.
    ///
    /// `node_modules` is the one exception — ignored state Nx itself needs, far too
    /// large to copy, so it is a link out to this checkout's own install. That is
    /// why `just bootstrap` has to have run before these journeys do.
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a directory for the throwaway checkout");
        let root = dir.path().join("checkout");
        let listing = git(
            Path::new(REPO),
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
        );
        for relative in listing.split('\0').filter(|entry| !entry.is_empty()) {
            let target = root.join(relative);
            std::fs::create_dir_all(target.parent().expect("a tracked file has a parent"))
                .expect("create the copy's directory");
            // Not `copy`: `CLAUDE.md` is a symlink, and following it would write a
            // second real file where the original has a link.
            let source = Path::new(REPO).join(relative);
            match std::fs::read_link(&source) {
                Ok(points_at) => std::os::unix::fs::symlink(points_at, &target),
                Err(_) => std::fs::copy(&source, &target).map(|_| ()),
            }
            .unwrap_or_else(|err| panic!("copy {relative} into the throwaway checkout: {err}"));
        }
        let install = Path::new(REPO).join("node_modules");
        // Checked rather than merely linked: a dangling link would send the copy's
        // `scripts/nx.sh` into `npm ci` *through* it, writing this checkout's
        // install from inside a temporary directory — a several-minute detour that
        // reads as a hung test rather than as missing provisioning.
        assert!(
            install.is_dir(),
            "these journeys drive real Nx and take this checkout's own install: run `just \
             bootstrap` first"
        );
        std::os::unix::fs::symlink(install, root.join("node_modules"))
            .expect("link the copy at this checkout's own Nx install");

        // The judge goes where `scripts/setup-llmlint.sh` installs it, under a home
        // directory of this journey's own, because that is the directory the tier
        // pins and the thing several journeys need to be able to replace.
        let home = dir.path().join("home");
        let pinned_bin = home.join(".local/bin");
        let judge_log = dir.path().join("judge-runs.log");
        write_executable(&pinned_bin.join("llmlint"), FAKE_LLMLINT);
        std::fs::write(&judge_log, "").expect("open the judge-run log");

        // A plugin outside the tree: no file input can see it, so only the judge
        // configuration fingerprint can notice when its rules change.
        let plugin = dir.path().join("external-plugin.yml");
        std::fs::write(
            &plugin,
            "version: 1\nrules:\n  - name: plugin_rule\n    description: The change documents \
             every new operator entry point.\n",
        )
        .expect("write the external plugin");
        std::fs::write(
            root.join("llmlint.yml"),
            format!(
                "files:\n  exclude:\n    - \"**/.git/**\"\nplugins:\n  - \"{}\"\n",
                plugin.display()
            ),
        )
        .expect("point the copy's llmlint config at the external plugin");

        let env = BTreeMap::from([
            ("HOME".to_string(), home.display().to_string()),
            // Nx roots its own cache here, and so would anything else that caches
            // per user; neither may reach the developer's real one.
            (
                "XDG_CACHE_HOME".to_string(),
                dir.path().join("cache").display().to_string(),
            ),
            (
                "FAKE_LLMLINT_LOG".to_string(),
                judge_log.display().to_string(),
            ),
            // The judge's two payloads, handed to it as data rather than pasted
            // into its script — see [`PASS_VERDICT`].
            ("FAKE_LLMLINT_VERDICT".to_string(), PASS_VERDICT.to_string()),
            ("FAKE_LLMLINT_FINDING".to_string(), FAIL_FINDING.to_string()),
            // Nx renders its cache reporting with ANSI escapes under some parents,
            // and `cargo llvm-cov nextest` is one of them: that pushed an escape in
            // front of the anchor the recipe matches on and reported every replay as
            // a fresh judgement, on a suite that was green without it. Forced here so
            // the journeys always read the harder of the two renderings.
            ("FORCE_COLOR".to_string(), "1".to_string()),
            // The recipe's own `command -v llmlint` guard resolves through `PATH`,
            // exactly as it does for a contributor whose session ran
            // `scripts/setup-llmlint.sh`.
            (
                "PATH".to_string(),
                format!(
                    "{}:{}",
                    pinned_bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            ),
        ]);

        let workspace = Self {
            _dir: dir,
            root,
            plugin,
            pinned_bin,
            judge_log,
            env,
        };
        git(&workspace.root, &["init", "-q"]);
        workspace.commit("the checkout under test");
        workspace
    }

    /// Run the recipe an operator and CI already invoke, with the shape they use.
    fn lint(&self, base: &str, nx_args: &[&str], overrides: &[(&str, &str)]) -> Verdict {
        let mut command = Command::new("just");
        command
            .current_dir(&self.root)
            .arg("lint-llm-diff")
            .arg(base)
            .args(nx_args);
        // A developer's own environment must not reach these journeys: an exported
        // cache skip or judge-binary override is exactly what several of them set
        // deliberately, one at a time.
        for name in [
            "NX_SKIP_NX_CACHE",
            "NX_DISABLE_NX_CACHE",
            "LLMLINT_ONEHARNESS_BIN",
        ] {
            command.env_remove(name);
        }
        command.envs(&self.env);
        command.envs(overrides.iter().copied());
        let output = command
            .output()
            .expect("`just` runs the recipe — install it and retry");
        Verdict::new(&output)
    }

    /// Run the fingerprint the way an operator diagnosing a cache miss would.
    fn fingerprint(&self, overrides: &[(&str, &str)]) -> Verdict {
        let mut command = Command::new("bash");
        command
            .current_dir(&self.root)
            .arg("scripts/llmlint-fingerprint.sh");
        command.env_remove("LLMLINT_ONEHARNESS_BIN");
        command.envs(&self.env);
        command.envs(overrides.iter().copied());
        let output = command.output().expect("bash runs the fingerprint");
        Verdict::new(&output)
    }

    /// What the cached target wrote, from the Nx output file it speaks through.
    fn judge_report(&self) -> String {
        std::fs::read_to_string(self.root.join(".logs/llmlint-diff.report"))
            .unwrap_or_else(|err| format!("<no report at .logs/llmlint-diff.report: {err}>"))
    }

    /// Remove the target's output, so the next run has to restore it from the cache.
    fn discard_the_judge_report(&self) {
        std::fs::remove_file(self.root.join(".logs/llmlint-diff.report"))
            .expect("the judged run wrote a report to discard");
    }

    /// How many times the judge has actually been paid for.
    fn judge_runs(&self) -> usize {
        std::fs::read_to_string(&self.judge_log)
            .expect("read the judge-run log")
            .lines()
            .count()
    }

    /// Put an llmlint that answers only `--version` in front of the pinned one.
    fn ambient_llmlint(&self, name: &str) -> (String, String) {
        let directory = self
            .root
            .parent()
            .expect("the checkout has a parent")
            .join(name);
        write_executable(&directory.join("llmlint"), AMBIENT_LLMLINT);
        (
            "PATH".to_string(),
            format!("{}:{}", directory.display(), self.env["PATH"]),
        )
    }

    fn commit(&self, message: &str) -> String {
        git(&self.root, &["add", "-A"]);
        git(
            &self.root,
            &["commit", "-q", "--allow-empty", "-m", message],
        );
        self.head()
    }

    fn head(&self) -> String {
        git(&self.root, &["rev-parse", "HEAD"]).trim().to_string()
    }

    fn touch_a_tracked_file(&self) {
        let readme = self.root.join("README.md");
        let existing = std::fs::read_to_string(&readme).expect("read the copy's README");
        std::fs::write(&readme, existing + "\n<!-- judged again -->\n").expect("change the tree");
    }

    fn change_the_plugin_rules(&self) {
        let existing = std::fs::read_to_string(&self.plugin).expect("read the external plugin");
        std::fs::write(
            &self.plugin,
            existing.replace("entry point", "entry point twice"),
        )
        .expect("change the external plugin");
    }
}

fn write_executable(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("an executable has a parent"))
        .expect("create the directory holding an executable");
    std::fs::write(path, body).expect("write an executable");
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .expect("make it executable");
}

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args([
            "-c",
            "user.name=journey",
            "-c",
            "user.email=journey@invalid",
        ])
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("git {args:?}: {err}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git output is UTF-8")
}

/// The whole point: one tree, one base, one answer.
#[test]
fn an_unchanged_tree_and_base_replays_the_first_verdict_instead_of_re_judging() {
    let workspace = Workspace::new();
    let base = workspace.head();

    let first = workspace.lint(&base, &[], &[]);
    // Deleted so the second run cannot pass by reading what the first left behind:
    // the report has to come back out of the cache, which is the whole claim.
    workspace.discard_the_judge_report();
    let second = workspace.lint(&base, &[], &[]);

    first
        .assert_green("the first run")
        .assert_says(PASS_VERDICT);
    // The verdict is what this command is for, so it lands on stdout — both times.
    // Everything else it says, provenance included, is a diagnostic on stderr.
    for run in [&first, &second] {
        assert!(
            run.stdout.contains(PASS_VERDICT) && !run.stderr.contains(PASS_VERDICT),
            "the verdict did not come back on stdout alone:\n{}",
            run.report
        );
    }
    // A replayed run has to say everything a fresh one did: the report *is* the
    // record, so a summary reconstructed from somewhere else would be a second
    // source of truth about a verdict nobody re-rolled.
    second
        .assert_green("the replayed run")
        .assert_says(PASS_VERDICT);
    assert_eq!(
        workspace.judge_runs(),
        1,
        "the judge was rolled twice over one tree"
    );
    // "Green" is a claim about one base commit, so the provenance names it: a
    // worker's gate and the rebuild that publishes its work resolving different
    // bases are answering different questions.
    first.assert_says(&format!("{JUDGED} {base}"));
    second.assert_says(&format!("{REPLAYED} {base}"));
}

#[test]
fn a_changed_tree_is_judged_again() {
    let workspace = Workspace::new();
    let base = workspace.head();
    workspace
        .lint(&base, &[], &[])
        .assert_green("the first run");

    workspace.touch_a_tracked_file();
    let second = workspace.lint(&base, &[], &[]);

    second
        .assert_green("the run after the tree changed")
        .assert_says(JUDGED);
    assert_eq!(workspace.judge_runs(), 2);
}

/// An identical tree judged against a different comparison is a different
/// question, and a base that advanced under a branch is the ordinary way to get
/// one.
#[test]
fn an_advanced_base_is_judged_again_and_then_cached_against_that_base() {
    let workspace = Workspace::new();
    let original = workspace.head();
    workspace
        .lint(&original, &[], &[])
        .assert_green("the first run");

    let advanced = workspace.commit("advance the base");
    assert_ne!(advanced, original);
    let moved = workspace.lint(&advanced, &[], &[]);
    let repeated = workspace.lint(&advanced, &[], &[]);

    moved
        .assert_green("the run against the advanced base")
        .assert_says(JUDGED);
    repeated.assert_green("the repeat").assert_says(REPLAYED);
    assert_eq!(workspace.judge_runs(), 2);
}

/// The rules live outside the tree Nx hashes, so the fingerprint is the only thing
/// that can notice they moved.
#[test]
fn a_changed_plugin_rule_source_is_judged_again() {
    let workspace = Workspace::new();
    let base = workspace.head();
    workspace
        .lint(&base, &[], &[])
        .assert_green("the first run");

    workspace.change_the_plugin_rules();
    let second = workspace.lint(&base, &[], &[]);

    second
        .assert_green("the run after the rules changed")
        .assert_says(JUDGED);
    assert_eq!(workspace.judge_runs(), 2);
}

/// Nor does the installed judge record itself anywhere in the tree.
#[test]
fn a_changed_llmlint_version_is_judged_again() {
    let workspace = Workspace::new();
    let base = workspace.head();
    workspace
        .lint(&base, &[], &[("FAKE_LLMLINT_VERSION", "0.3.25")])
        .assert_green("the first run");

    let second = workspace.lint(&base, &[], &[("FAKE_LLMLINT_VERSION", "0.4.0")]);

    second
        .assert_green("the run under a newer judge")
        .assert_says(JUDGED);
    assert_eq!(workspace.judge_runs(), 2);
}

/// The tier pins its own judge binary, so an inherited override is not an input.
///
/// `llmlint config` renders the resolved oneharness path, and a sibling checkout's
/// wrapper exported into the environment reached this repository that way — which
/// would key one judged diff differently for every caller.
#[test]
fn a_callers_judge_binary_override_does_not_change_the_key() {
    let workspace = Workspace::new();
    let base = workspace.head();
    workspace
        .lint(
            &base,
            &[],
            &[("LLMLINT_ONEHARNESS_BIN", "/caller/one/oneharness")],
        )
        .assert_green("the first run");

    let second = workspace.lint(
        &base,
        &[],
        &[("LLMLINT_ONEHARNESS_BIN", "/caller/two/oneharness")],
    );

    second
        .assert_green("the run under a second caller")
        .assert_says(REPLAYED);
    assert_eq!(workspace.judge_runs(), 1);
}

/// A cache hit alone would not prove the fingerprint survived a foreign llmlint.
///
/// Nx scores a runtime input that exits non-zero as *no contribution* rather than
/// as an error, so a fingerprint the caller's environment can break does not fail
/// the tier — it silently shrinks the key to the tree and the base, and both runs
/// then share that degraded key. So this changes the judge configuration under the
/// foreign llmlint and requires a real re-judge, and reads the fingerprint directly
/// to see that it still resolves.
#[test]
fn an_ambient_llmlint_cannot_drop_the_judge_configuration_out_of_the_key() {
    let workspace = Workspace::new();
    let base = workspace.head();
    let ambient = workspace.ambient_llmlint("ambient-judge");
    let on_path = [(ambient.0.as_str(), ambient.1.as_str())];

    let first = workspace.lint(&base, &[], &on_path);
    let printed = workspace.fingerprint(&on_path);
    workspace.change_the_plugin_rules();
    let second = workspace.lint(&base, &[], &on_path);

    first.assert_green("the first run under an ambient llmlint");
    printed.assert_green("the fingerprint under an ambient llmlint");
    second
        .assert_green("the run after the rules changed under an ambient llmlint")
        .assert_says(JUDGED);
    assert_eq!(workspace.judge_runs(), 2);
}

/// A fingerprint that cannot be produced is named, not swallowed.
///
/// It is what stops the judge configuration dropping out of the key silently, and
/// an operator asking "why did this miss?" runs it directly — so it has to say
/// which half of the toolchain is broken and what to do about it.
#[test]
fn a_fingerprint_that_cannot_be_produced_names_the_broken_half() {
    for (body, expected) in [
        (
            "set -uo pipefail\n[[ ${1:-} == \"--version\" ]] && exit 1\nexit 0\n",
            "'llmlint --version' failed",
        ),
        (
            "set -uo pipefail\n[[ ${1:-} == \"config\" ]] && exit 1\necho 'llmlint 0.0.0-e2e'\n",
            "'llmlint config' failed",
        ),
    ] {
        let workspace = Workspace::new();
        write_executable(
            &workspace.pinned_bin.join("llmlint"),
            &format!("#!/usr/bin/env bash\n{body}"),
        );

        let printed = workspace.fingerprint(&[]);

        assert!(
            !printed.ok,
            "a broken judge toolchain fingerprinted anyway:\n{}",
            printed.report
        );
        printed.assert_says(expected);
        printed.assert_says("retry");
    }
}

/// Only a successful run is replayed, because Nx caches successful tasks only.
///
/// That is the deliberate trade: a red costs a fresh roll every time, rather than
/// a failing verdict becoming a stored answer no documented lever displaces.
#[test]
fn findings_and_a_toolchain_that_never_reached_a_verdict_both_fail_and_re_judge() {
    for exit in ["1", "2"] {
        let workspace = Workspace::new();
        let base = workspace.head();

        let first = workspace.lint(&base, &[], &[("FAKE_LLMLINT_EXIT", exit)]);
        let second = workspace.lint(&base, &[], &[("FAKE_LLMLINT_EXIT", exit)]);

        for run in [&first, &second] {
            assert!(
                !run.ok,
                "a judge that exited {exit} passed the tier:\n{}",
                run.report
            );
            // Whatever the run managed to say still reaches the operator who has to
            // clear it; what it must never do is become a stored answer. A failed
            // run's report is a diagnostic, so it comes back on stderr rather than
            // as this command's output.
            run.assert_says(FAIL_FINDING).assert_says(JUDGED);
            assert!(
                run.stderr.contains(FAIL_FINDING) && run.stdout.is_empty(),
                "a failing run put diagnostics on stdout:\n{}",
                run.report
            );
        }
        assert_eq!(
            workspace.judge_runs(),
            2,
            "a failing run at exit {exit} was replayed"
        );
    }
}

/// The path a worker actually walks: judge, clear the finding, judge again, settle.
#[test]
fn a_cleared_finding_caches_the_green_that_replaced_it() {
    let workspace = Workspace::new();
    let base = workspace.head();

    let red = workspace.lint(&base, &[], &[("FAKE_LLMLINT_EXIT", "1")]);
    workspace.touch_a_tracked_file();
    let green = workspace.lint(&base, &[], &[]);
    let settled = workspace.lint(&base, &[], &[]);

    assert!(
        !red.ok,
        "the finding did not fail the tier:\n{}",
        red.report
    );
    green
        .assert_green("the run that cleared it")
        .assert_says(JUDGED);
    settled
        .assert_green("the settled run")
        .assert_says(REPLAYED);
    assert_eq!(workspace.judge_runs(), 2);
}

/// The one supported re-judge lever is per-invocation, and it works under an
/// ambient global skip — which is itself reported and ignored, because honouring
/// it would re-roll a non-deterministic judge from every unrelated command.
#[test]
fn the_re_judge_lever_is_per_invocation_and_a_global_cache_skip_is_ignored() {
    let workspace = Workspace::new();
    let base = workspace.head();
    workspace
        .lint(&base, &[], &[])
        .assert_green("the first run");

    let under_skip = workspace.lint(&base, &[], &[("NX_SKIP_NX_CACHE", "true")]);
    let under_disable = workspace.lint(&base, &[], &[("NX_DISABLE_NX_CACHE", "true")]);
    let forced = workspace.lint(&base, &["--skip-nx-cache"], &[("NX_SKIP_NX_CACHE", "true")]);

    for ignored in [&under_skip, &under_disable] {
        ignored
            .assert_green("the run under an ambient global cache skip")
            .assert_says("ignoring the ambient global Nx cache skip")
            .assert_says(&format!("just lint-llm-diff {base} --skip-nx-cache"))
            .assert_says(REPLAYED);
    }
    forced
        .assert_green("the forced re-judge")
        .assert_says(JUDGED);
    assert_eq!(
        workspace.judge_runs(),
        2,
        "only the forced run should have re-rolled"
    );
}

/// A base ref that does not resolve is refused before the judge is paid.
#[test]
fn an_unresolvable_base_is_refused_before_the_judge_is_paid() {
    let workspace = Workspace::new();

    let refused = workspace.lint("no-such-ref", &[], &[]);

    assert!(
        !refused.ok,
        "an unresolvable base was judged anyway:\n{}",
        refused.report
    );
    refused.assert_says("does not resolve to a commit");
    assert_eq!(workspace.judge_runs(), 0);
}

/// The recipe resolves the base; the target refuses anything else.
///
/// These states are reachable only by driving the cached target directly, which is
/// the misuse this guard names — and keying and judging on the same resolved commit
/// is what stops a verdict computed against one base being replayed for another.
#[test]
fn the_cached_target_refuses_a_base_it_cannot_judge() {
    let workspace = Workspace::new();
    for (base_sha, expected) in [
        ("", "must be a resolved commit id"),
        ("origin/main", "must be a resolved commit id"),
        (&"0".repeat(40), "missing from this checkout"),
    ] {
        let output = Command::new("bash")
            .current_dir(&workspace.root)
            .arg("scripts/nx.sh")
            .args(["run", "oneagentgraph:lint-llm-diff"])
            .env_remove("LLMLINT_ONEHARNESS_BIN")
            .envs(&workspace.env)
            .env("LLMLINT_DIFF_BASE_SHA", base_sha)
            .output()
            .expect("bash runs the Nx wrapper");

        // Read the way the recipe surfaces it: the target speaks through its
        // declared output file, so a refusal reaches an operator whether or not Nx
        // drained the task's pipe before resolving it.
        let report = format!(
            "--- report ---\n{}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            workspace.judge_report(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            !output.status.success(),
            "base `{base_sha}` was judged anyway:\n{report}"
        );
        assert!(
            report.contains(expected),
            "base `{base_sha}` was not named:\n{report}"
        );
    }
    assert_eq!(workspace.judge_runs(), 0);
}
