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

/// The release packaging journeys below share this driver: the production
/// scripts, run the way `release.yml` runs them, over one staging directory.
#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
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
/// exactly the native target's set carrying an x86_64 ELF, and installed and run
/// from every distribution form. Darwin's products cannot be linked on this host,
/// so its journey is the macOS test below; what Linux proves about Darwin is the
/// declaration (`DARWIN_TARGETS` is arm64 alone) and the refusals that keep a
/// wrong-architecture or Intel artifact out of a staged set.
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

/// The Mac release job's own journey, on the Apple Silicon host it runs on:
/// `release-build.sh` for every `DARWIN_TARGETS` entry, the staged set verified
/// to be exactly the arm64 one carrying an arm64 Mach-O, each publication route
/// replayed from `release.yml` with only its transport doubled, and every
/// distribution form installed and run natively.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn darwin_release_products_are_the_arm64_set_and_run_natively() {
    use release::*;
    let work = workspace();
    let workflow = workflow();
    let targets = darwin_targets(&workflow);
    assert_eq!(
        targets,
        ["aarch64-apple-darwin"],
        "the Mac release job builds arm64 alone"
    );
    let target = "aarch64-apple-darwin";
    let job = work.path().join("job");
    let darwin = job.join("darwin");
    succeeded(&build(target, &darwin), "release-build");
    let expected = expected_set(target, "macosx_11_0_arm64", "darwin-arm64");
    assert_eq!(verified_set(&verify(&[target], &darwin)), expected);

    // The payload's own architecture, read by the host's toolchain rather than
    // by the verifier under test.
    let unpacked = work.path().join("unpacked");
    std::fs::create_dir_all(&unpacked).expect("unpack directory");
    succeeded(
        &Command::new("tar")
            .arg("-xzf")
            .arg(darwin.join(format!("oneagentgraph-v{VERSION}-{target}.tar.gz")))
            .arg("-C")
            .arg(&unpacked)
            .output()
            .expect("tar runs"),
        "unpack",
    );
    let archs = Command::new("lipo")
        .arg("-archs")
        .arg(unpacked.join("oneagentgraph"))
        .output()
        .expect("lipo runs");
    succeeded(&archs, "lipo");
    assert_eq!(String::from_utf8_lossy(&archs.stdout).trim(), "arm64");

    // What each upload-artifact step of the Mac job carries out of it.
    let steps = workflow["jobs"]["darwin"]["steps"]
        .as_array()
        .expect("darwin steps");
    let mut uploaded = std::collections::BTreeMap::new();
    for step in steps.iter().filter(|step| {
        step["uses"]
            .as_str()
            .is_some_and(|uses| uses.starts_with("actions/upload-artifact@"))
    }) {
        let mut files = Vec::new();
        for pattern in step["with"]["path"]
            .as_str()
            .expect("artifact path")
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            let suffix = pattern
                .strip_prefix("darwin/*")
                .expect("artifact paths are darwin/*<suffix> globs");
            for entry in std::fs::read_dir(&darwin).expect("staged directory lists") {
                let entry = entry.expect("staged entry");
                let name = entry.file_name().into_string().expect("utf-8 name");
                if entry.file_type().expect("entry type").is_file() && name.ends_with(suffix) {
                    files.push(name);
                }
            }
        }
        files.sort();
        uploaded.insert(
            step["with"]["name"]
                .as_str()
                .expect("artifact name")
                .to_string(),
            files,
        );
    }
    let archive = format!("oneagentgraph-v{VERSION}-{target}.tar.gz");
    let wheel = format!("oneagentgraph_cli-{VERSION}-py3-none-macosx_11_0_arm64.whl");
    let carrier = format!("oneagentgraph-cli-darwin-arm64-{VERSION}.tgz");
    assert_eq!(
        uploaded["wheels-darwin"],
        [wheel],
        "PyPI receives exactly the arm64 wheel"
    );
    assert_eq!(uploaded["npm-darwin"], std::slice::from_ref(&carrier));
    assert_eq!(
        uploaded["archives-darwin"],
        [
            format!("{target}.json"),
            archive.clone(),
            format!("{archive}.sha256")
        ],
        "the forge receives the arm64 archive, checksum and receipt"
    );

    // Doubles for the forge and the registry: they record what would leave and
    // hand everything else to the real npm.
    let bin = work.path().join("doubles");
    std::fs::create_dir_all(&bin).expect("doubles directory");
    let real_npm = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v npm"])
            .output()
            .expect("npm resolves")
            .stdout,
    )
    .expect("npm path");
    let record = work.path().join("published");
    std::fs::write(
        bin.join("gh"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$RECORD\"\n",
    )
    .expect("gh double");
    std::fs::write(
        bin.join("npm"),
        format!("#!/bin/sh\ncase \"$1\" in\n  view) echo 'npm error code E404' >&2; exit 1 ;;\n  publish) printf 'npm publish %s\\n' \"$2\" >> \"$RECORD\"; exit 0 ;;\n  *) exec {} \"$@\" ;;\nesac\n", real_npm.trim()),
    )
    .expect("npm double");
    for double in ["gh", "npm"] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join(double), std::fs::Permissions::from_mode(0o755))
            .expect("double is executable");
    }
    let run_step = |job_name: &str, step_name: &str, workspace: &std::path::Path| {
        let script = workflow["jobs"][job_name]["steps"]
            .as_array()
            .expect("job steps")
            .iter()
            .find(|step| step["name"].as_str() == Some(step_name))
            .and_then(|step| step["run"].as_str())
            .unwrap_or_else(|| panic!("{job_name} has a {step_name:?} step"))
            .to_string();
        let output = Command::new("bash")
            .current_dir(if job_name == "upload-darwin" {
                workspace
            } else {
                repo()
            })
            .args(["--noprofile", "--norc", "-eo", "pipefail", "-c", &script])
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH")),
            )
            .env("RECORD", &record)
            .env("GITHUB_REF_NAME", tag())
            .env("GITHUB_WORKSPACE", workspace)
            .env("GH_TOKEN", "doubled")
            .env("NODE_AUTH_TOKEN", "doubled")
            .output()
            .expect("workflow step runs");
        succeeded(&output, step_name);
    };
    // upload-darwin downloads archives-darwin into `darwin/` and attaches it.
    let forge = work.path().join("forge");
    std::fs::create_dir_all(forge.join("darwin")).expect("forge workspace");
    for name in &uploaded["archives-darwin"] {
        std::fs::copy(darwin.join(name), forge.join("darwin").join(name))
            .expect("artifact downloads");
    }
    run_step(
        "upload-darwin",
        "Attach prevalidated Darwin archives",
        &forge,
    );
    // publish-npm merges every npm-* artifact into `npm-artifacts/`; this is the Mac job's share.
    let registry = work.path().join("registry");
    std::fs::create_dir_all(registry.join("npm-artifacts")).expect("registry workspace");
    std::fs::copy(
        darwin.join(&carrier),
        registry.join("npm-artifacts").join(&carrier),
    )
    .expect("artifact downloads");
    run_step(
        "publish-npm",
        "Publish the platform packages, then the launcher",
        &registry,
    );
    let published = std::fs::read_to_string(&record).expect("doubles recorded");
    let lines: Vec<&str> = published.lines().collect();
    assert_eq!(
        lines[..6],
        [
            "release",
            "upload",
            &tag(),
            &format!("darwin/{archive}"),
            &format!("darwin/{archive}.sha256"),
            "--clobber"
        ],
        "the forge receives exactly the arm64 archive and checksum"
    );
    assert_eq!(
        lines[6],
        format!(
            "npm publish {}",
            registry.join("npm-artifacts").join(&carrier).display()
        )
    );
    assert!(
        lines[7].starts_with("npm publish ") && lines[7].ends_with("oneagentgraph-cli"),
        "then the launcher: {}",
        lines[7]
    );
    assert_eq!(lines.len(), 8, "nothing else publishes: {published}");

    // Native execution, as the Mac job runs it before any upload.
    let smoke = Command::new("bash")
        .current_dir(repo())
        .args(["scripts/release-smoke.sh", target])
        .arg(&darwin)
        .arg(VERSION)
        .output()
        .expect("native product installs run");
    succeeded(&smoke, "release-smoke");

    // Mach-O cputype 0x01000007 is x86_64.
    defects_are_refused(
        target,
        &darwin,
        work.path(),
        |macho| macho[4..8].copy_from_slice(&0x0100_0007u32.to_le_bytes()),
        "mach-o/x86_64",
    );
    intel_is_refused(&built_binary(target), work.path());
}
