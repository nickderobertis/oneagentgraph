//! Every integration-test target runs in exactly one tier.
//!
//! The suite is split into Nx projects, and the justfile's `contract-tests`,
//! `e2e-tests`, and `repo-tooling-tests` lists are what each tier's targets hand
//! to cargo. Cargo discovers a new `tests/*.rs` on its own, so a target missing
//! from every list would compile and then run nowhere — a test that silently
//! stopped gating. This holds the lists to what Cargo actually builds (asked of
//! `cargo metadata`, not restated), and each listed target to the directory its
//! Nx project owns, since ownership is how Nx decides which tier a change to the
//! file reaches.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// This checkout, whose committed files are what is read.
const REPO: &str = env!("CARGO_MANIFEST_DIR");

/// Each split tier's list in the justfile, and the directory its project owns.
const TIERS: &[(&str, &str, &str)] = &[
    ("contract", "contract-tests", "tests"),
    ("e2e", "e2e-tests", "tests/e2e"),
    ("repo-tooling", "repo-tooling-tests", "tests/repo-tooling"),
];

/// The package's integration-test targets, by name, with their source file.
fn test_targets() -> BTreeMap<String, String> {
    let output = Command::new(env!("CARGO"))
        .current_dir(REPO)
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--offline",
        ])
        .output()
        .expect("cargo metadata runs");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata prints JSON");
    let package = metadata["packages"]
        .as_array()
        .and_then(|packages| packages.iter().find(|p| p["name"] == "oneagentgraph"))
        .expect("the oneagentgraph package is in the metadata");
    package["targets"]
        .as_array()
        .expect("a package lists its targets")
        .iter()
        .filter(|target| {
            target["kind"]
                .as_array()
                .is_some_and(|k| k.iter().any(|k| k == "test"))
        })
        .map(|target| {
            let source = target["src_path"].as_str().expect("a target has a source");
            let relative = Path::new(source)
                .strip_prefix(REPO)
                .expect("a test target lives in this checkout")
                .to_string_lossy()
                .replace('\\', "/");
            (
                target["name"]
                    .as_str()
                    .expect("a target has a name")
                    .to_string(),
                relative,
            )
        })
        .collect()
}

/// One `name := "a b c"` list from the justfile.
fn justfile_list(justfile: &str, variable: &str) -> Vec<String> {
    let prefix = format!("{variable} := \"");
    let line = justfile
        .lines()
        .find(|line| line.starts_with(&prefix))
        .unwrap_or_else(|| panic!("the justfile no longer declares `{variable}`"));
    line[prefix.len()..]
        .trim_end_matches('"')
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

#[test]
fn every_test_target_runs_in_exactly_one_tier_inside_the_directory_its_project_owns() {
    let justfile =
        std::fs::read_to_string(Path::new(REPO).join("justfile")).expect("read the justfile");
    let targets = test_targets();
    assert!(
        targets.contains_key("e2e") && targets.contains_key("contract"),
        "cargo metadata reported no integration tests: {targets:?}"
    );

    let mut tier_of: BTreeMap<String, &str> = BTreeMap::new();
    for (tier, variable, root) in TIERS {
        for name in justfile_list(&justfile, variable) {
            if let Some(other) = tier_of.insert(name.clone(), tier) {
                panic!("`{name}` is listed in both the {other} and the {tier} tier");
            }
            let source = targets.get(&name).unwrap_or_else(|| {
                panic!("the {tier} tier lists `{name}`, which is no test target")
            });
            let directory = Path::new(source)
                .parent()
                .expect("a test source has a directory")
                .to_string_lossy()
                .replace('\\', "/");
            assert_eq!(
                &directory, root,
                "`{name}` ({source}) runs in the {tier} tier but sits outside {root}, the \
                 directory that tier's Nx project owns — a change to it would reach the wrong \
                 tier"
            );
        }
    }

    let unassigned: Vec<_> = targets
        .keys()
        .filter(|name| !tier_of.contains_key(*name))
        .collect();
    assert!(
        unassigned.is_empty(),
        "these test targets run in no tier — add each to one of the justfile's tier lists: \
         {unassigned:?}"
    );
}
