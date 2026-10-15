//! What a crates.io consumer actually receives.
//!
//! `Cargo.toml`'s `include` list decides that, and the cost of getting it wrong
//! is paid after publishing rather than before: the shipped personas and the
//! contract document are pulled in by `include_str!` and by
//! `tests/contract.rs`, so a list that omits one produces a crate that does not
//! *compile* — discovered by the first consumer, on a version that cannot be
//! unpublished.
//!
//! `cargo package --list` is the ground truth for that list, so this asks it
//! rather than restating the globs.

use std::process::Command;

/// Every file the crate needs at build time is in the package.
///
/// Named individually rather than by glob: a glob here would pass on the same
/// mistake it is meant to catch, since the thing being checked *is* whether the
/// manifest's glob covers these files.
#[test]
fn the_package_carries_every_file_the_crate_is_built_from() {
    let listed = package_list();

    let mut needed: Vec<String> = oneagentgraph::persona::SHIPPED_PERSONAS
        .iter()
        .map(|(name, _)| format!("personas/{name}.yaml"))
        .collect();
    // `persona new` scaffolds from this one through `include_str!`, and
    // `tests/contract.rs` reads the contract the same way — both are build
    // inputs, not documentation that merely happens to sit nearby.
    needed.push("personas/_template.yaml".into());
    needed.push("docs/contract.md".into());
    // The persisted-record goldens, read the same way by `tests/record.rs`.
    // Every version this build understands, so dropping the older one — the thing
    // that proves an old record still reads — is caught here too.
    needed.push("tests/golden/record.v1.json".into());
    needed.push("tests/golden/record.v2.json".into());
    // The YAML `tests/yaml_corpus.rs` reads, and what it holds that YAML to.
    needed.push("tests/golden/graph.v10.yaml".into());
    needed.push("tests/yaml-corpus/expected.json".into());
    needed.push("tests/yaml-corpus/host/config/onejudge.base.yaml".into());
    // The release declaration `tests/release_declaration.rs` hands to the reader
    // that defines its schema, read the same way.
    needed.push("release-targets.toml".into());
    // The justfile, whose `oneharness-version` pin `tests/e2e/support.rs` reads
    // the same way to hold the CLI it drives to the release provisioning installs.
    needed.push("justfile".into());

    for path in &needed {
        assert!(
            listed.iter().any(|packaged| packaged == path),
            "`cargo package` would ship a crate without {path}, which it is built \
             from — fix the `include` list in Cargo.toml.\npackaged:\n{}",
            listed.join("\n")
        );
    }
}

/// The package does not carry the symlink registries reject.
///
/// `CLAUDE.md` is a symlink to `AGENTS.md`; crates.io refuses an archive
/// containing one, so this fails here rather than at `cargo publish`.
#[test]
fn the_package_carries_no_symlink_a_registry_would_reject() {
    let listed = package_list();
    assert!(
        !listed.iter().any(|packaged| packaged == "CLAUDE.md"),
        "the package includes the CLAUDE.md symlink, which a registry rejects"
    );
}

/// The package carries this crate's own files and nothing else.
///
/// `include` takes gitignore-style patterns, where one with no separator in it
/// matches at *any* depth — so a bare `README.md` and `LICENSE` swept in every
/// vendored copy under `node_modules`, shipping 154 files of somebody else's
/// package, and their licences, to crates.io. The presence check above cannot
/// see that: every file it names was there too.
#[test]
fn the_package_carries_nothing_from_outside_the_crate() {
    let listed = package_list();
    // `screenshots/fixture/` for its graphs, which `tests/yaml_corpus.rs` reads.
    let roots = [
        "src/",
        "tests/",
        "personas/",
        "docs/",
        "screenshots/fixture/",
    ];
    for packaged in &listed {
        let inside_a_root = roots.iter().any(|root| packaged.starts_with(root));
        // The manifest, the lockfile, and the two documents cargo adds itself.
        let at_the_top = !packaged.contains('/');
        assert!(
            inside_a_root || at_the_top,
            "`cargo package` would ship {packaged}, which is not this crate's to \
             ship — anchor the `include` pattern that swept it in.\npackaged:\n{}",
            listed.join("\n")
        );
    }
}

/// Everything `cargo package` would put in the archive.
fn package_list() -> Vec<String> {
    // `--allow-dirty` because this runs from a work tree, and `--list` neither
    // builds nor writes: it only answers what the manifest selects.
    let output = Command::new(env!("CARGO"))
        .args(["package", "--list", "--allow-dirty"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo package --list runs");
    assert!(
        output.status.success(),
        "cargo package --list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        // An archive entry is always `/`-separated, but `--list` prints it with
        // the host's separator — so on Windows every path here arrives with
        // backslashes and matches nothing this file names.
        .map(|line| line.replace('\\', "/"))
        .collect()
}

/// The edited release packager carries the same compiled binary through all
/// distribution forms. Linux executes only its native products; Darwin payloads
/// are inspected as artifact contracts, never executed on this host. The Darwin
/// targets come from the Mac release job itself, which builds Apple Silicon
/// alone: Intel macOS is refused by every packaging entry point.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn release_products_reuse_the_binary_and_the_native_wheel_installs_and_runs() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::var_os("ONEPIPELINE_NODE_SCRATCH_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let work = tempfile::tempdir_in(scratch).expect("packaging workspace");
    let version = env!("CARGO_PKG_VERSION");
    let native_target = "x86_64-unknown-linux-gnu";
    let native = work.path().join(native_target);
    let target_dir = repo.join("target/release-packaging");
    let built = Command::new("bash")
        .current_dir(repo)
        .env("CARGO_TARGET_DIR", &target_dir)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .args([
            "scripts/release-build.sh",
            native_target,
            &format!("v{version}"),
        ])
        .arg(&native)
        .args(["--profile", "dev"])
        .output()
        .expect("real production builder runs for the native target");
    assert!(
        built.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let native_binary = target_dir.join(native_target).join("debug/oneagentgraph");
    let binary = native_binary.to_str().expect("native binary path");
    let workflow = std::fs::read_to_string(repo.join(".github/workflows/release.yml"))
        .expect("release workflow reads");
    let declared = workflow
        .lines()
        .find_map(|line| line.trim().strip_prefix("DARWIN_TARGETS: '"))
        .and_then(|rest| rest.strip_suffix('\''))
        .expect("the Mac release job declares DARWIN_TARGETS");
    let darwin: Vec<String> =
        serde_json::from_str(declared).expect("DARWIN_TARGETS is a JSON list");
    assert_eq!(
        darwin,
        ["aarch64-apple-darwin"],
        "the Mac release job builds arm64 alone"
    );
    for target in std::iter::once(native_target).chain(darwin.iter().map(String::as_str)) {
        let out = work.path().join(target);
        if target != native_target {
            let result = Command::new("python3")
                .current_dir(repo)
                .args([
                    "scripts/release-package.py",
                    "--target",
                    target,
                    "--binary",
                    binary,
                    "--tag",
                    &format!("v{version}"),
                    "--out",
                ])
                .arg(&out)
                .output()
                .expect("release packager runs");
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let package = Command::new("node")
                .current_dir(repo)
                .args([
                    "scripts/npm-build.mjs",
                    "platform",
                    "--target",
                    target,
                    "--binary",
                    binary,
                    "--out",
                ])
                .arg(out.join("npm"))
                .output()
                .expect("npm package builder runs");
            assert!(
                package.status.success(),
                "{}",
                String::from_utf8_lossy(&package.stderr)
            );
            let dir = String::from_utf8(package.stdout).expect("npm package path");
            let packed = Command::new("npm")
                .current_dir(dir.trim())
                .args(["pack", "--pack-destination"])
                .arg(&out)
                .output()
                .expect("npm pack runs");
            assert!(
                packed.status.success(),
                "{}",
                String::from_utf8_lossy(&packed.stderr)
            );
        }
        let inspected = Command::new("python3")
            .current_dir(repo)
            .args([
                "-c",
                include_str!("support/release_payload.py"),
                binary,
                target,
                version,
            ])
            .arg(&out)
            .output()
            .expect("artifact inspector runs");
        assert!(
            inspected.status.success(),
            "{}",
            String::from_utf8_lossy(&inspected.stderr)
        );
        if target != native_target {
            // Exactly what the Mac job uploads for publication, and nothing Intel.
            let mut staged: Vec<String> = std::fs::read_dir(&out)
                .expect("staged artifacts list")
                .map(|entry| {
                    entry
                        .expect("staged entry")
                        .file_name()
                        .into_string()
                        .expect("utf-8 name")
                })
                .filter(|name| name != "npm")
                .collect();
            staged.sort();
            let wheel_version = version.replace('-', "_");
            assert_eq!(
                staged,
                [
                    "aarch64-apple-darwin.json".to_string(),
                    format!("oneagentgraph-cli-darwin-arm64-{version}.tgz"),
                    format!("oneagentgraph-v{version}-aarch64-apple-darwin.tar.gz"),
                    format!("oneagentgraph-v{version}-aarch64-apple-darwin.tar.gz.sha256"),
                    format!("oneagentgraph_cli-{wheel_version}-py3-none-macosx_11_0_arm64.whl"),
                ]
            );
        }
    }
    let result = Command::new("bash")
        .current_dir(repo)
        .args(["scripts/release-smoke.sh", native_target])
        .arg(&native)
        .arg(version)
        .output()
        .expect("native product installs run");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let rejected = Command::new("python3")
        .current_dir(repo)
        .args([
            "scripts/release-package.py",
            "--target",
            "aarch64-apple-darwin",
            "--binary",
            binary,
            "--tag",
            "v999.0.0",
            "--out",
        ])
        .arg(work.path().join("wrong-tag"))
        .output()
        .expect("invalid tag packager runs");
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("release tag must equal"));
    // Intel macOS is discontinued: each entry point refuses it before building
    // or writing anything, rather than producing an artifact nobody publishes.
    let intel = work.path().join("intel");
    for (program, args) in [
        (
            "bash",
            vec!["scripts/release-build.sh", "x86_64-apple-darwin", "v1.2.3"],
        ),
        (
            "python3",
            vec![
                "scripts/release-package.py",
                "--target",
                "x86_64-apple-darwin",
                "--binary",
                binary,
                "--tag",
                "v1.2.3",
                "--out",
            ],
        ),
    ] {
        let refused = Command::new(program)
            .current_dir(repo)
            .args(args)
            .arg(&intel)
            .output()
            .expect("Intel target request runs");
        assert_eq!(
            refused.status.code(),
            Some(2),
            "{program} accepted Intel macOS"
        );
        assert!(String::from_utf8_lossy(&refused.stderr).contains("x86_64-apple-darwin"));
        assert!(!intel.exists(), "{program} wrote output for Intel macOS");
    }
    let npm = Command::new("node")
        .current_dir(repo)
        .args([
            "scripts/npm-build.mjs",
            "platform",
            "--target",
            "x86_64-apple-darwin",
            "--binary",
            binary,
            "--out",
        ])
        .arg(&intel)
        .output()
        .expect("npm builder runs for Intel macOS");
    assert!(!npm.status.success(), "npm-build accepted Intel macOS");
    assert!(!intel.exists(), "npm-build wrote output for Intel macOS");
}
