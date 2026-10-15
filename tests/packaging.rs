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

    /// One change to one member of a staged zip or tar.gz.
    pub enum Edit<'a> {
        Replace(&'a [u8]),
        Append(&'a [u8]),
        Delete,
        /// A new member, beside the existing ones.
        Add(&'a [u8]),
        /// Permission bits, on a zip member.
        Chmod(u32),
        /// The member becomes a directory entry, on a tar member.
        Directory,
        /// The member becomes a symbolic link to `README.md`, on a tar member.
        Symlink,
    }

    /// Apply `edit` to `member` of a staged wheel (zip), archive or npm carrier
    /// (tar.gz), leaving every other member as the packager wrote it.
    pub fn rewrite_member(path: &Path, member: &str, edit: Edit<'_>) {
        const REWRITE: &str = r#"
import io, sys, tarfile, zipfile
from pathlib import Path
path, member, op, arg = Path(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4]
data_arg = bytes.fromhex(arg) if op in ('replace', 'append', 'add') else None
def edited(data):
    return data_arg if op == 'replace' else data + data_arg if op == 'append' else data
if zipfile.is_zipfile(path):
    with zipfile.ZipFile(path) as z:
        entries = [(info, z.read(info)) for info in z.infolist()]
    with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED) as z:
        for info, data in entries:
            if info.filename == member:
                if op == 'delete':
                    continue
                if op == 'chmod':
                    info.external_attr = int(arg, 8) << 16
                data = edited(data)
            z.writestr(info, data)
        if op == 'add':
            z.writestr(zipfile.ZipInfo(member), data_arg)
else:
    with tarfile.open(path) as tar:
        entries = [(info, tar.extractfile(info).read() if info.isfile() else b'') for info in tar.getmembers()]
    with tarfile.open(path, 'w:gz') as tar:
        for info, data in entries:
            if info.name == member:
                if op == 'delete':
                    continue
                if op == 'directory':
                    info.type, data = tarfile.DIRTYPE, b''
                if op == 'symlink':
                    info.type, info.linkname, data = tarfile.SYMTYPE, 'README.md', b''
                data = edited(data)
                info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
        if op == 'add':
            info = tarfile.TarInfo(member)
            info.size = len(data_arg)
            tar.addfile(info, io.BytesIO(data_arg))
"#;
        let hex =
            |bytes: &[u8]| -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() };
        let (op, arg) = match edit {
            Edit::Replace(bytes) => ("replace", hex(bytes)),
            Edit::Append(bytes) => ("append", hex(bytes)),
            Edit::Delete => ("delete", String::new()),
            Edit::Add(bytes) => ("add", hex(bytes)),
            Edit::Chmod(mode) => ("chmod", format!("{mode:o}")),
            Edit::Directory => ("directory", String::new()),
            Edit::Symlink => ("symlink", String::new()),
        };
        succeeded(
            &Command::new("python3")
                .args(["-I", "-c", REWRITE])
                .arg(path)
                .args([member, op, &arg])
                .output()
                .expect("python runs"),
            "rewrite member",
        );
    }

    /// Re-checksum a staged archive, so the only defect left is the one a case put in it.
    pub fn rechecksum(dir: &Path, archive: &str) {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(std::fs::read(dir.join(archive)).expect("archive reads"));
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        std::fs::write(
            dir.join(format!("{archive}.sha256")),
            format!("{hex}  {archive}\n"),
        )
        .expect("checksum writes");
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
        let script = format!("oneagentgraph_cli-{VERSION}.data/scripts/oneagentgraph");
        let recorded: serde_json::Value =
            serde_json::from_slice(&std::fs::read(staged.join(&receipt)).expect("receipt reads"))
                .expect("receipt parses");
        let with_receipt = |field: &str, value: &str| {
            let mut changed = recorded.clone();
            changed[field] = serde_json::Value::String(value.to_string());
            changed.to_string()
        };
        let malformed = format!("a staged {target} artifact is malformed");
        type Mutation<'a> = Box<dyn Fn(&Path) + 'a>;
        let write = |name: String, text: String| -> Mutation<'_> {
            Box::new(move |dir: &Path| {
                std::fs::write(dir.join(&name), &text).expect("staged file writes")
            })
        };
        let in_archive = |member: &'static str, edit: fn() -> Edit<'static>| -> Mutation<'_> {
            let archive = archive.clone();
            Box::new(move |dir: &Path| {
                rewrite_member(&dir.join(&archive), member, edit());
                rechecksum(dir, &archive);
            })
        };
        let in_member =
            |file: String, member: String, edit: fn() -> Edit<'static>| -> Mutation<'_> {
                Box::new(move |dir: &Path| rewrite_member(&dir.join(&file), &member, edit()))
            };
        let other_version = "9.9.9";
        let other_wheel = wheel.replace(VERSION, other_version);
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
                malformed.clone(),
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
                format!("not {archive} and {wheel}"),
            ),
            // Receipt, file name and METADATA agree with each other, and maturin
            // still says this checkout's wheel is another version.
            (
                "a wheel and receipt agreeing on another version",
                Box::new(|dir: &Path| {
                    std::fs::rename(dir.join(wheel), dir.join(&other_wheel))
                        .expect("wheel renames");
                    restamp_wheel(dir, &other_wheel, other_version);
                    std::fs::write(dir.join(&receipt), with_receipt("wheel", &other_wheel))
                        .expect("receipt writes");
                }),
                format!("not {archive} and {wheel}"),
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
                "archive without its README",
                in_archive("README.md", || Edit::Delete),
                "not the binary and its three documents".into(),
            ),
            (
                "archive binary not a regular file",
                in_archive("oneagentgraph", || Edit::Directory),
                format!("{archive} member oneagentgraph is not a regular file"),
            ),
            (
                "archive binary a link to another member",
                in_archive("oneagentgraph", || Edit::Symlink),
                format!("{archive} member oneagentgraph is not a regular file"),
            ),
            (
                "corrupt wheel",
                write(wheel.to_string(), "not a zip".into()),
                malformed.clone(),
            ),
            (
                "wheel with a second script",
                in_member(
                    wheel.to_string(),
                    format!("oneagentgraph_cli-{VERSION}.data/scripts/other"),
                    || Edit::Add(b"#!/bin/sh\n"),
                ),
                format!("{wheel} must carry exactly one oneagentgraph script"),
            ),
            (
                "wheel without its script",
                in_member(wheel.to_string(), script.clone(), || Edit::Delete),
                format!("{wheel} must carry exactly one oneagentgraph script"),
            ),
            (
                "wheel payload from another build",
                in_member(wheel.to_string(), script.clone(), || Edit::Append(b"\0")),
                format!("{wheel} carries a different binary"),
            ),
            (
                "wheel script not executable",
                in_member(wheel.to_string(), script.clone(), || Edit::Chmod(0o644)),
                "script is not executable".into(),
            ),
            (
                "wheel METADATA for another package",
                in_member(wheel.to_string(), format!("{dist_info}/METADATA"), || {
                    Edit::Replace(b"Name: someone-else\n")
                }),
                "metadata is not oneagentgraph-cli".into(),
            ),
            (
                "wheel tagged for another platform",
                in_member(wheel.to_string(), format!("{dist_info}/WHEEL"), || {
                    Edit::Replace(b"Wheel-Version: 1.0\nTag: py3-none-any\n")
                }),
                "metadata is not oneagentgraph-cli tagged py3-none-linux_x86_64".into(),
            ),
            (
                "wheel without METADATA",
                in_member(wheel.to_string(), format!("{dist_info}/METADATA"), || {
                    Edit::Delete
                }),
                format!("{wheel} has no {dist_info}/METADATA"),
            ),
            (
                "corrupt npm carrier",
                write(carrier.to_string(), "not a tarball".into()),
                malformed.clone(),
            ),
            (
                "npm manifest not JSON",
                in_member(carrier.to_string(), "package/package.json".into(), || {
                    Edit::Replace(b"[1")
                }),
                malformed.clone(),
            ),
            (
                "npm manifest not an object",
                in_member(carrier.to_string(), "package/package.json".into(), || {
                    Edit::Replace(b"[]")
                }),
                format!("{carrier} manifest is not the {target} carrier"),
            ),
            (
                "npm manifest for another package",
                in_member(carrier.to_string(), "package/package.json".into(), || {
                    Edit::Replace(br#"{"name":"someone-else"}"#)
                }),
                format!("{carrier} manifest is not the {target} carrier"),
            ),
            (
                "npm carrier without its binary",
                in_member(
                    carrier.to_string(),
                    "package/bin/oneagentgraph".into(),
                    || Edit::Delete,
                ),
                format!("{carrier} has no package/bin/oneagentgraph"),
            ),
            (
                "npm carrier with a script for a binary",
                in_member(
                    carrier.to_string(),
                    "package/bin/oneagentgraph".into(),
                    || Edit::Replace(b"#!/bin/sh\n"),
                ),
                format!("{carrier} payload is built for unknown/?"),
            ),
            (
                "npm payload from another build",
                in_member(
                    carrier.to_string(),
                    "package/bin/oneagentgraph".into(),
                    || Edit::Append(b"\0"),
                ),
                format!("{carrier} carries a different binary"),
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

    /// `scripts/release-rehearse.sh` from a copy of this checkout whose
    /// `scripts/` differ by `change`, with `doubles` (name, sh body) first on PATH.
    pub fn rehearse_altered(
        target: &str,
        dir: &Path,
        work: &Path,
        change: impl Fn(&Path),
        doubles: &[(&str, &str)],
    ) -> Output {
        let root = work.join("altered-checkout");
        std::fs::create_dir_all(root.join("scripts")).expect("altered checkout");
        for entry in std::fs::read_dir(repo()).expect("checkout lists") {
            let entry = entry.expect("checkout entry");
            if entry.file_name() != "scripts" {
                std::os::unix::fs::symlink(entry.path(), root.join(entry.file_name()))
                    .expect("checkout links");
            }
        }
        copy_staged(&repo().join("scripts"), &root.join("scripts"));
        change(&root.join("scripts"));
        let bin = work.join("altered-bin");
        std::fs::create_dir_all(&bin).expect("doubles directory");
        for (name, body) in doubles {
            let path = bin.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("double writes");
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("double is executable");
        }
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").expect("PATH is set")
        );
        let output = Command::new("bash")
            .current_dir(repo())
            .arg(root.join("scripts/release-rehearse.sh"))
            .arg(target)
            .arg(dir)
            .arg(tag())
            .env("PATH", path)
            .output()
            .expect("release rehearsal runs");
        std::fs::remove_dir_all(&root).expect("altered checkout removes");
        std::fs::remove_dir_all(&bin).expect("doubles directory removes");
        output
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
    // The host's own architecture reading (lipo, on macOS) is a refusal of its
    // own; here `uname` and `lipo` are doubled to say Darwin and another CPU.
    refused(
        &rehearse_altered(
            target,
            &staged,
            work.path(),
            |_| {},
            &[("uname", "echo Darwin"), ("lipo", "echo arm64")],
        ),
        "release-rehearse with lipo reading another architecture",
        &format!("lipo reads 'arm64' from oneagentgraph-v{VERSION}-{target}.tar.gz, not x86_64"),
    );
    // An attach script that would send more than the archive and checksum is
    // caught by the rehearsal before any upload.
    refused(
        &rehearse_altered(
            target,
            &staged,
            work.path(),
            |scripts| {
                let attach = scripts.join("release-attach.sh");
                let text = std::fs::read_to_string(&attach).expect("attach reads");
                std::fs::write(&attach, text.replace("--clobber", "--clobber --draft"))
                    .expect("attach writes");
            },
            &[],
        ),
        "release-rehearse with an attach that sends more",
        "publication would send",
    );
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
