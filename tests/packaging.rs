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

/// The release packaging journey below drives the production scripts the way
/// `release.yml` runs them, over one staging directory.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod release {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    pub const VERSION: &str = env!("CARGO_PKG_VERSION");

    pub fn repo() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    pub fn tag() -> String {
        format!("v{VERSION}")
    }

    pub fn workspace() -> tempfile::TempDir {
        let scratch = std::env::var_os("ONEPIPELINE_NODE_SCRATCH_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        tempfile::tempdir_in(scratch).expect("packaging workspace")
    }

    pub fn succeeded(output: &Output, what: &str) {
        assert!(
            output.status.success(),
            "{what} failed\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Refused at the boundary (exit 2) with `reason` in its diagnostic.
    pub fn refused(output: &Output, what: &str, reason: &str) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{what} was not refused: {stderr}"
        );
        assert!(
            stderr.contains(reason),
            "{what} did not say {reason:?}: {stderr}"
        );
    }

    /// `scripts/release-build.sh`, as the Mac job calls it, at the dev profile.
    pub fn build(target: &str, out: &Path) -> Output {
        Command::new("bash")
            .current_dir(repo())
            .env("CARGO_TARGET_DIR", repo().join("target/release-packaging"))
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .args(["scripts/release-build.sh", target, &tag()])
            .arg(out)
            .args(["--profile", "dev"])
            .output()
            .expect("release builder runs")
    }

    pub fn built_binary(target: &str) -> PathBuf {
        repo()
            .join("target/release-packaging")
            .join(target)
            .join("debug/oneagentgraph")
    }

    /// `scripts/release-verify.py` over a staged directory.
    pub fn verify(targets: &[&str], dir: &Path) -> Output {
        let mut command = Command::new("python3");
        command.current_dir(repo()).arg("scripts/release-verify.py");
        for target in targets {
            command.args(["--target", target]);
        }
        command
            .args(["--tag", &tag(), "--dir"])
            .arg(dir)
            .output()
            .expect("release verifier runs")
    }

    /// Exactly the files a target stages for publication.
    pub fn expected_set(target: &str, wheel: &str, npm: &str) -> Vec<String> {
        let archive = format!("oneagentgraph-v{VERSION}-{target}.tar.gz");
        let mut names = vec![
            format!("{target}.json"),
            format!("{archive}.sha256"),
            archive,
            format!("oneagentgraph_cli-{VERSION}-py3-none-{wheel}.whl"),
            format!("oneagentgraph-cli-{npm}-{VERSION}.tgz"),
        ];
        names.sort();
        names
    }

    pub fn verified_set(output: &Output) -> Vec<String> {
        succeeded(output, "release-verify");
        serde_json::from_slice(&output.stdout).expect("release-verify prints the verified set")
    }

    /// A copy of a staged directory's files, to mutate without touching the original.
    pub fn copy_staged(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("copy directory");
        for entry in std::fs::read_dir(from).expect("staged directory lists") {
            let entry = entry.expect("staged entry");
            if entry.file_type().expect("staged entry type").is_file() {
                std::fs::copy(entry.path(), to.join(entry.file_name()))
                    .expect("staged file copies");
            }
        }
    }

    /// Rewrite `archive` in `dir` with its binary's header changed by `patch`,
    /// re-checksummed so the only defect left is the payload's architecture.
    pub fn tamper_archive(dir: &Path, archive: &str, patch: impl Fn(&mut [u8])) {
        use sha2::Digest;
        let unpacked = dir.join("unpacked");
        std::fs::create_dir_all(&unpacked).expect("unpack directory");
        succeeded(
            &Command::new("tar")
                .arg("-xzf")
                .arg(dir.join(archive))
                .arg("-C")
                .arg(&unpacked)
                .output()
                .expect("tar runs"),
            "unpack",
        );
        let binary = unpacked.join("oneagentgraph");
        let mut bytes = std::fs::read(&binary).expect("payload reads");
        patch(&mut bytes);
        std::fs::write(&binary, bytes).expect("payload writes");
        succeeded(
            &Command::new("tar")
                .arg("-czf")
                .arg(dir.join(archive))
                .arg("-C")
                .arg(&unpacked)
                .args(["oneagentgraph", "README.md", "LICENSE", "CHANGELOG.md"])
                .output()
                .expect("tar runs"),
            "repack",
        );
        std::fs::remove_dir_all(&unpacked).expect("unpack directory removes");
        let digest = sha2::Sha256::digest(std::fs::read(dir.join(archive)).expect("archive reads"));
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        std::fs::write(
            dir.join(format!("{archive}.sha256")),
            format!("{hex}  {archive}\n"),
        )
        .expect("checksum writes");
    }

    /// `scripts/release-rehearse.sh`, as the Mac release job runs it.
    pub fn rehearse(target: &str, dir: &Path) -> Output {
        Command::new("bash")
            .current_dir(repo())
            .args(["scripts/release-rehearse.sh", target])
            .arg(dir)
            .arg(tag())
            .output()
            .expect("release rehearsal runs")
    }

    /// Rewrite the staged wheel's `METADATA` to declare `version`, leaving its
    /// name, payload and every other file as the packager wrote them.
    pub fn restamp_wheel(dir: &Path, wheel: &str, version: &str) {
        const RESTAMP: &str = r#"
import re, sys, zipfile
from pathlib import Path
path, version = Path(sys.argv[1]), sys.argv[2]
with zipfile.ZipFile(path) as z:
    entries = [(info, z.read(info)) for info in z.infolist()]
with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED) as z:
    for info, data in entries:
        if info.filename.endswith('.dist-info/METADATA'):
            data = re.sub(rb'^Version: .*$', b'Version: ' + version.encode(), data, flags=re.M)
        z.writestr(info, data)
"#;
        succeeded(
            &Command::new("python3")
                .args(["-I", "-c", RESTAMP])
                .arg(dir.join(wheel))
                .arg(version)
                .output()
                .expect("python runs"),
            "restamp wheel",
        );
    }

    /// Rewrite one member of a staged wheel (zip) or npm carrier (tar.gz):
    /// `contents` replaces its bytes, and `mode` its permission bits.
    pub fn rewrite_member(path: &Path, member: &str, contents: Option<&[u8]>, mode: Option<u32>) {
        const REWRITE: &str = r#"
import io, sys, tarfile, zipfile
from pathlib import Path
path, member, contents, mode = Path(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4]
new = bytes.fromhex(contents) if contents != '-' else None
if zipfile.is_zipfile(path):
    with zipfile.ZipFile(path) as z:
        entries = [(info, z.read(info)) for info in z.infolist()]
    with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED) as z:
        for info, data in entries:
            if info.filename == member:
                data = new if new is not None else data
                if mode != '-':
                    info.external_attr = int(mode, 8) << 16
            z.writestr(info, data)
else:
    with tarfile.open(path) as tar:
        entries = [(info, tar.extractfile(info).read()) for info in tar.getmembers()]
    with tarfile.open(path, 'w:gz') as tar:
        for info, data in entries:
            if info.name == member:
                data = new if new is not None else data
                info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
"#;
        let hex = contents.map_or("-".to_string(), |bytes| {
            bytes.iter().map(|byte| format!("{byte:02x}")).collect()
        });
        succeeded(
            &Command::new("python3")
                .args(["-I", "-c", REWRITE])
                .arg(path)
                .args([
                    member,
                    &hex,
                    &mode.map_or("-".to_string(), |m| format!("{m:o}")),
                ])
                .output()
                .expect("python runs"),
            "rewrite member",
        );
    }

    /// Every way a staged set can differ from what its packager wrote is refused
    /// at the verifier's boundary (exit 2) with an ACTION, never a traceback, and
    /// so never reaches an upload.
    pub fn malformed_sets_are_refused(
        target: &str,
        staged: &Path,
        work: &Path,
        wheel: &str,
        carrier: &str,
    ) {
        let archive = format!("oneagentgraph-v{VERSION}-{target}.tar.gz");
        let receipt = format!("{target}.json");
        let dist_info = format!("oneagentgraph_cli-{VERSION}.dist-info");
        let recorded: serde_json::Value =
            serde_json::from_slice(&std::fs::read(staged.join(&receipt)).expect("receipt reads"))
                .expect("receipt parses");
        let with_receipt = |field: &str, value: &str| {
            let mut changed = recorded.clone();
            changed[field] = serde_json::Value::String(value.to_string());
            changed.to_string()
        };
        type Mutation<'a> = Box<dyn Fn(&Path) + 'a>;
        let write = |name: String, text: String| -> Mutation<'_> {
            Box::new(move |dir: &Path| {
                std::fs::write(dir.join(&name), &text).expect("staged file writes")
            })
        };
        let cases: Vec<(&str, Mutation<'_>, String)> = vec![
            (
                "no receipt",
                Box::new(|dir: &Path| {
                    std::fs::remove_file(dir.join(&receipt)).expect("receipt removes")
                }),
                format!("missing receipt {receipt}"),
            ),
            (
                "receipt not JSON",
                write(receipt.clone(), "{".into()),
                format!("a staged {target} artifact is malformed"),
            ),
            (
                "receipt schema",
                write(receipt.clone(), "{}".into()),
                "does not have the packager receipt schema".into(),
            ),
            (
                "receipt for another tag",
                write(receipt.clone(), with_receipt("tag", "v0.0.1")),
                format!("records {target} v0.0.1"),
            ),
            (
                "receipt names another wheel",
                write(
                    receipt.clone(),
                    with_receipt("wheel", "oneagentgraph_cli-9.9.9-py3-none-any.whl"),
                ),
                format!("not the {target} archive"),
            ),
            (
                "receipt records another binary",
                write(
                    receipt.clone(),
                    with_receipt("binarySha256", &"0".repeat(64)),
                ),
                format!("{archive} carries a different binary"),
            ),
            (
                "no npm carrier",
                Box::new(|dir: &Path| {
                    std::fs::remove_file(dir.join(carrier)).expect("carrier removes")
                }),
                format!("missing staged {target} artifacts: {carrier}"),
            ),
            (
                "checksum mismatch",
                write(
                    format!("{archive}.sha256"),
                    format!("{}  {archive}\n", "0".repeat(64)),
                ),
                format!("{archive}.sha256 does not match"),
            ),
            (
                "archive without its documents",
                Box::new(|dir: &Path| {
                    tamper_archive(dir, &archive, |_| {});
                    let unpacked = dir.join("only-binary");
                    std::fs::create_dir_all(&unpacked).expect("unpack directory");
                    succeeded(
                        &Command::new("tar")
                            .arg("-xzf")
                            .arg(dir.join(&archive))
                            .arg("-C")
                            .arg(&unpacked)
                            .output()
                            .expect("tar runs"),
                        "unpack",
                    );
                    succeeded(
                        &Command::new("tar")
                            .arg("-czf")
                            .arg(dir.join(&archive))
                            .arg("-C")
                            .arg(&unpacked)
                            .arg("oneagentgraph")
                            .output()
                            .expect("tar runs"),
                        "repack",
                    );
                    std::fs::remove_dir_all(&unpacked).expect("unpack directory removes");
                    use sha2::Digest;
                    let digest = sha2::Sha256::digest(
                        std::fs::read(dir.join(&archive)).expect("archive reads"),
                    );
                    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
                    std::fs::write(
                        dir.join(format!("{archive}.sha256")),
                        format!("{hex}  {archive}\n"),
                    )
                    .expect("checksum writes");
                }),
                "not the binary and its three documents".into(),
            ),
            (
                "corrupt wheel",
                write(wheel.to_string(), "not a zip".into()),
                format!("a staged {target} artifact is malformed"),
            ),
            (
                "wheel script not executable",
                Box::new(|dir: &Path| {
                    rewrite_member(
                        &dir.join(wheel),
                        &format!("oneagentgraph_cli-{VERSION}.data/scripts/oneagentgraph"),
                        None,
                        Some(0o644),
                    )
                }),
                "script is not executable".into(),
            ),
            (
                "wheel METADATA for another package",
                Box::new(|dir: &Path| {
                    rewrite_member(
                        &dir.join(wheel),
                        &format!("{dist_info}/METADATA"),
                        Some(b"Name: someone-else\n"),
                        None,
                    )
                }),
                "metadata is not oneagentgraph-cli".into(),
            ),
            (
                "corrupt npm carrier",
                write(carrier.to_string(), "not a tarball".into()),
                format!("a staged {target} artifact is malformed"),
            ),
            (
                "npm manifest not JSON",
                Box::new(|dir: &Path| {
                    rewrite_member(
                        &dir.join(carrier),
                        "package/package.json",
                        Some(b"[1"),
                        None,
                    )
                }),
                format!("a staged {target} artifact is malformed"),
            ),
            (
                "npm manifest for another package",
                Box::new(|dir: &Path| {
                    rewrite_member(
                        &dir.join(carrier),
                        "package/package.json",
                        Some(br#"{"name":"someone-else"}"#),
                        None,
                    )
                }),
                format!("{carrier} manifest is not the {target} carrier"),
            ),
            (
                "npm carrier with another binary",
                Box::new(|dir: &Path| {
                    rewrite_member(
                        &dir.join(carrier),
                        "package/bin/oneagentgraph",
                        Some(b"#!/bin/sh\n"),
                        None,
                    )
                }),
                format!("{carrier} payload is built for unknown/?"),
            ),
        ];
        for (index, (case, mutate, reason)) in cases.iter().enumerate() {
            let dir = work.join(format!("malformed-{index}"));
            copy_staged(staged, &dir);
            mutate(&dir);
            let output = verify(&[target], &dir);
            refused(&output, &format!("release-verify with {case}"), reason);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("ACTION: rebuild the staged directory"),
                "{case}: {stderr}"
            );
            assert!(!stderr.contains("Traceback"), "{case}: {stderr}");
        }
    }

    /// `.github/workflows/release.yml`, as data.
    pub fn workflow() -> serde_json::Value {
        let text = std::fs::read_to_string(repo().join(".github/workflows/release.yml"))
            .expect("release workflow reads");
        serde_saphyr::from_str(&text).expect("release workflow parses")
    }

    /// The Mac release job's targets.
    pub fn darwin_targets(workflow: &serde_json::Value) -> Vec<String> {
        let declared = workflow["jobs"]["darwin"]["env"]["DARWIN_TARGETS"]
            .as_str()
            .expect("the Mac release job declares DARWIN_TARGETS");
        serde_json::from_str(declared).expect("DARWIN_TARGETS is a JSON list")
    }

    /// Every packaging entry point refuses Intel macOS before writing anything.
    pub fn intel_is_refused(binary: &Path, work: &Path) {
        let intel = work.join("intel");
        refused(
            &build("x86_64-apple-darwin", &intel),
            "release-build",
            "Intel macOS is no longer a release target",
        );
        let package = Command::new("python3")
            .current_dir(repo())
            .args([
                "scripts/release-package.py",
                "--target",
                "x86_64-apple-darwin",
                "--binary",
            ])
            .arg(binary)
            .args(["--tag", &tag(), "--out"])
            .arg(&intel)
            .output()
            .expect("packager runs");
        refused(
            &package,
            "release-package",
            "invalid choice: 'x86_64-apple-darwin'",
        );
        std::fs::create_dir_all(work.join("staged-intel")).expect("verify directory");
        refused(
            &verify(&["x86_64-apple-darwin"], &work.join("staged-intel")),
            "release-verify",
            "invalid choice: 'x86_64-apple-darwin'",
        );
        let npm = Command::new("node")
            .current_dir(repo())
            .args([
                "scripts/npm-build.mjs",
                "platform",
                "--target",
                "x86_64-apple-darwin",
                "--binary",
            ])
            .arg(binary)
            .arg("--out")
            .arg(&intel)
            .output()
            .expect("npm builder runs");
        assert!(!npm.status.success(), "npm-build accepted Intel macOS");
        assert!(!intel.exists(), "an Intel request wrote output");
    }

    /// A verified staged directory stops verifying once it holds an Intel
    /// artifact or a payload of the wrong architecture.
    pub fn defects_are_refused(
        target: &str,
        staged: &Path,
        work: &Path,
        wrong_cpu: impl Fn(&mut [u8]),
        wrong: &str,
    ) {
        let extra = work.join("extra-intel");
        copy_staged(staged, &extra);
        let intel = format!("oneagentgraph-v{VERSION}-x86_64-apple-darwin.tar.gz");
        std::fs::copy(
            extra.join(format!("oneagentgraph-v{VERSION}-{target}.tar.gz")),
            extra.join(&intel),
        )
        .expect("Intel archive stages");
        refused(
            &verify(&[target], &extra),
            "release-verify with an Intel archive",
            &format!("would publish: {intel}"),
        );

        let tampered = work.join("wrong-architecture");
        copy_staged(staged, &tampered);
        tamper_archive(
            &tampered,
            &format!("oneagentgraph-v{VERSION}-{target}.tar.gz"),
            &wrong_cpu,
        );
        refused(
            &verify(&[target], &tampered),
            "release-verify with a wrong-architecture payload",
            &format!("built for {wrong}"),
        );

        let binary = work.join("wrong-architecture-binary");
        let mut bytes = std::fs::read(built_binary(target)).expect("built binary reads");
        wrong_cpu(&mut bytes);
        std::fs::write(&binary, bytes).expect("wrong binary writes");
        let package = Command::new("python3")
            .current_dir(repo())
            .args(["scripts/release-package.py", "--target", target, "--binary"])
            .arg(&binary)
            .args(["--tag", &tag(), "--out"])
            .arg(work.join("wrong-package"))
            .output()
            .expect("packager runs");
        refused(
            &package,
            "release-package with a wrong-architecture binary",
            &format!("built for {wrong}"),
        );
        assert!(
            !work.join("wrong-package").exists(),
            "the packager wrote a wrong-architecture artifact"
        );
    }
}

/// The Linux release products: built by the release builder, verified to be
/// exactly the native target's set carrying an x86_64 ELF, rehearsed through the
/// same publication script the Mac release job runs, and installed and run from
/// every distribution form. Darwin's products cannot be linked on this host: the
/// Mac release job runs `release-rehearse.sh` over them natively before upload.
/// What Linux proves about Darwin is the declaration (`DARWIN_TARGETS` is arm64
/// alone) and the refusals that keep a wrong-architecture or Intel artifact out.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn release_products_reuse_the_binary_and_the_native_wheel_installs_and_runs() {
    use release::*;
    let work = workspace();
    let target = "x86_64-unknown-linux-gnu";
    let staged = work.path().join(target);
    succeeded(&build(target, &staged), "release-build");
    assert_eq!(
        verified_set(&verify(&[target], &staged)),
        expected_set(target, "linux_x86_64", "linux-x64")
    );
    let rehearsed = rehearse(target, &staged);
    succeeded(&rehearsed, "release-rehearse");
    assert!(
        String::from_utf8_lossy(&rehearsed.stdout).contains(
            "attach and npm publish would send exactly its archive, checksum and carrier; Intel and elf/aarch64 artifacts refused"
        ),
        "{}",
        String::from_utf8_lossy(&rehearsed.stdout)
    );
    let smoke = Command::new("bash")
        .current_dir(repo())
        .args(["scripts/release-smoke.sh", target])
        .arg(&staged)
        .arg(VERSION)
        .output()
        .expect("native product installs run");
    succeeded(&smoke, "release-smoke");

    assert_eq!(
        darwin_targets(&workflow()),
        ["aarch64-apple-darwin"],
        "the Mac release job builds arm64 alone"
    );
    // This host's ELF is not a Darwin payload, and the packager says so rather
    // than wrapping it in a Darwin wheel.
    let mislabelled = Command::new("python3")
        .current_dir(repo())
        .args([
            "scripts/release-package.py",
            "--target",
            "aarch64-apple-darwin",
            "--binary",
        ])
        .arg(built_binary(target))
        .args(["--tag", &tag(), "--out"])
        .arg(work.path().join("mislabelled"))
        .output()
        .expect("packager runs");
    refused(
        &mislabelled,
        "release-package",
        "built for elf/x86_64, not mach-o/arm64",
    );
    // ELF e_machine 183 is aarch64.
    defects_are_refused(
        target,
        &staged,
        work.path(),
        |elf| elf[18..20].copy_from_slice(&183u16.to_le_bytes()),
        "elf/aarch64",
    );
    intel_is_refused(&built_binary(target), work.path());
    malformed_sets_are_refused(
        target,
        &staged,
        work.path(),
        &format!("oneagentgraph_cli-{VERSION}-py3-none-linux_x86_64.whl"),
        &format!("oneagentgraph-cli-linux-x64-{VERSION}.tgz"),
    );
    // The rehearsal stops on a staged Intel artifact rather than publishing it.
    let extra = work.path().join("rehearse-extra");
    copy_staged(&staged, &extra);
    std::fs::copy(
        extra.join(format!("oneagentgraph-v{VERSION}-{target}.tar.gz")),
        extra.join(format!(
            "oneagentgraph-v{VERSION}-x86_64-apple-darwin.tar.gz"
        )),
    )
    .expect("Intel archive stages");
    refused(
        &rehearse(target, &extra),
        "release-rehearse with an Intel archive",
        "would publish: oneagentgraph-v",
    );
    // A wheel whose metadata declares another version is not this release's.
    let restamped = work.path().join("wheel-version");
    copy_staged(&staged, &restamped);
    let wheel = format!("oneagentgraph_cli-{VERSION}-py3-none-linux_x86_64.whl");
    restamp_wheel(&restamped, &wheel, "9.9.9");
    refused(
        &verify(&[target], &restamped),
        "release-verify with a mismatched wheel version",
        &format!("METADATA declares Version 9.9.9, not the release version {VERSION}"),
    );

    let wrong_tag = Command::new("python3")
        .current_dir(repo())
        .args(["scripts/release-package.py", "--target", target, "--binary"])
        .arg(built_binary(target))
        .args(["--tag", "v999.0.0", "--out"])
        .arg(work.path().join("wrong-tag"))
        .output()
        .expect("invalid tag packager runs");
    refused(&wrong_tag, "release-package", "release tag must equal");
}
