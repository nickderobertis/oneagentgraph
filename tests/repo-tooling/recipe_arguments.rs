//! A tier argument reaches the justfile's recipes as data, never as shell.
//!
//! The per-tier recipes (`_tier-sources`, `_tier-selectors`, `_tier-lint`,
//! `_tier-test`, …) once spliced `{{tier}}` into their shell line ahead of the
//! `case` that validates it, so a tier named `x$(cmd)` ran `cmd` before anything
//! could refuse it. They now read it as `"$1"` under `positional-arguments`.
//! These journeys drive the real `justfile` with the real `just`: every recipe
//! that takes a tier refuses a hostile one without running it and without
//! falling through to cargo, and the spliced form the recipes had before still
//! runs it — which is what proves the payload is live rather than inert.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// This checkout, whose real justfile is driven.
const REPO: &str = env!("CARGO_MANIFEST_DIR");

/// Every recipe that takes a tier as its first argument.
const TIER_RECIPES: &[&str] = &[
    "_tier-sources",
    "_tier-selectors",
    "_tier-fmt-check",
    "_tier-format",
    "_tier-lint",
    "_tier-test",
    "_tier-test-quick",
    "_instrumented",
];

/// A tier name that, spliced into a shell line, creates `sentinel`.
///
/// The path is written with forward slashes: the recipes run under bash, which
/// on Windows reads `D:\a\_temp\…` as escapes and would `touch` some other file,
/// so the spliced form would seem not to run it and the argv form's refusal
/// would prove nothing. Git Bash resolves `D:/a/_temp/…` to the sentinel itself.
fn hostile_tier(sentinel: &Path) -> String {
    format!(
        "x$(touch {})",
        sentinel.display().to_string().replace('\\', "/")
    )
}

/// Run `recipe tier` from `justfile`, in this checkout, with no stdin to read.
fn just(justfile: &Path, recipe: &str, tier: &str) -> Output {
    Command::new("just")
        .arg("--justfile")
        .arg(justfile)
        .arg("--working-directory")
        .arg(REPO)
        .args([recipe, tier])
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|err| panic!("run `just {recipe}` (is `just` installed?): {err}"))
}

/// `just`'s own report that a recipe exited non-zero. Its wording is not
/// pinned: CI installs the latest `just`, which writes `error: recipe `x`
/// failed …` where 1.42 wrote `error: Recipe `x` failed …`.
fn is_just_recipe_failure(line: &str) -> bool {
    line.to_ascii_lowercase().starts_with("error: recipe `") && line.contains("` failed ")
}

fn sentinel_in(dir: &tempfile::TempDir, recipe: &str) -> PathBuf {
    dir.path().join(format!("ran-{recipe}"))
}

#[test]
fn every_tier_recipe_refuses_a_hostile_tier_without_running_it_or_cargo() {
    let justfile = Path::new(REPO).join("justfile");
    let scratch = tempfile::tempdir().expect("a scratch directory");
    for recipe in TIER_RECIPES {
        let sentinel = sentinel_in(&scratch, recipe);
        let run = just(&justfile, recipe, &hostile_tier(&sentinel));
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            !sentinel.exists(),
            "`just {recipe}` ran its tier argument as shell: {stderr}"
        );
        assert_eq!(
            run.status.code(),
            Some(2),
            "`just {recipe}` should refuse an unknown tier with exit 2\nstdout: {stdout}\nstderr: {stderr}"
        );
        // Nothing but the refusal and just's own trace of it: a recipe that went
        // on to cargo or rustfmt with no selectors would print here, and would
        // have run every test target (this one included) or read stdin.
        assert!(stdout.is_empty(), "`just {recipe}` printed {stdout}");
        assert!(
            stderr
                .lines()
                .all(|line| line.contains("tier named") || is_just_recipe_failure(line)),
            "`just {recipe}` did more than refuse the tier: {stderr}"
        );
        assert!(
            stderr.contains("tier named 'x$(touch "),
            "`just {recipe}` should name the refused tier verbatim: {stderr}"
        );
    }
}

#[test]
fn the_spliced_form_the_recipes_had_ran_the_tier_and_the_argv_form_does_not() {
    let real =
        std::fs::read_to_string(Path::new(REPO).join("justfile")).expect("read the justfile");
    // `_tier-selectors` exactly as it read before the tier became an argument:
    // the same recipe with `{{tier}}` where it now has `$1`.
    let start = real
        .find("\n_tier-selectors tier:\n")
        .expect("the justfile defines `_tier-selectors`");
    let end = start + real[start + 1..].find("\n\n").expect("the recipe ends") + 1;
    let recipe = &real[start..end];
    assert_eq!(recipe.matches("\"$1\"").count(), 1, "{recipe}");
    assert_eq!(recipe.matches("'$1'").count(), 1, "{recipe}");
    let spliced = recipe
        .replace("\"$1\"", "\"{{tier}}\"")
        .replace("'$1'", "'{{tier}}'");
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let earlier = scratch.path().join("justfile");
    std::fs::write(
        &earlier,
        format!("{}{spliced}{}", &real[..start], &real[end..]),
    )
    .expect("write the spliced justfile");

    let before = scratch.path().join("ran-spliced");
    let run = just(&earlier, "_tier-selectors", &hostile_tier(&before));
    assert!(
        before.exists(),
        "the spliced recipe should have run the tier's `touch` (exit {:?}): {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );

    let after = scratch.path().join("ran-argv");
    let run = just(
        &Path::new(REPO).join("justfile"),
        "_tier-selectors",
        &hostile_tier(&after),
    );
    assert!(
        !after.exists(),
        "the finished recipe ran the tier's `touch`: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.status.code(), Some(2));

    // A real tier still resolves through the same recipe.
    let run = just(
        &Path::new(REPO).join("justfile"),
        "_tier-selectors",
        "repo-tooling",
    );
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("--test recipe_arguments"),
        "the repo-tooling tier lists this journey: {}",
        String::from_utf8_lossy(&run.stdout)
    );
}
