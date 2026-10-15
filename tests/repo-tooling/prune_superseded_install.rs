//! `just bootstrap` leaves the pinned pruner as the one `PATH` resolves.
//!
//! onevcs runs `just pool-maintenance` in idle slots, and that recipe runs
//! whichever `cargo-prune-superseded` `PATH` resolves. These journeys drive the
//! real `_ensure-prune-superseded` recipe with the real `just` and bash. The
//! stand-ins are a `cargo` that records each install and writes a pruner
//! reporting the version it was asked for, plus a stale pruner, so the recipe's
//! decisions are observed without compiling the crate.
//!
//! Unix-only: the stand-ins are `#!` scripts, which Windows does not run by name.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// This checkout, whose real justfile is driven.
const REPO: &str = env!("CARGO_MANIFEST_DIR");

/// The release the justfile pins, read from the justfile itself.
fn pinned_version() -> String {
    let justfile =
        std::fs::read_to_string(Path::new(REPO).join("justfile")).expect("read justfile");
    justfile
        .lines()
        .find_map(|line| line.strip_prefix("prune-superseded-version := \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("the justfile pins prune-superseded-version")
        .to_string()
}

fn write_script(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).expect("create the script's directory");
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write a stand-in script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("make a stand-in executable");
}

/// A pruner that reports `version`.
fn pruner(dir: &Path, version: &str) -> PathBuf {
    let path = dir.join("cargo-prune-superseded");
    write_script(&path, &format!("echo 'cargo-prune-superseded {version}'"));
    path
}

/// The directory holding the `just` this test runs, found on the test's `PATH`.
fn just_dir() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .find(|dir| dir.join("just").is_file())
        .expect("`just` is on PATH")
}

struct Host {
    dir: tempfile::TempDir,
}

impl Host {
    /// A host whose `cargo` installs a pruner at the version it is asked for
    /// into `$CARGO_HOME/bin`, logging each install.
    fn new() -> Self {
        let host = Self {
            dir: tempfile::tempdir().expect("a scratch host"),
        };
        // Keep the real just executable without admitting its sibling tools into
        // the controlled PATH (CI installs the real pruner beside just).
        std::fs::copy(just_dir().join("just"), host.path("just"))
            .expect("copy the real just executable");
        write_script(
            &host.path("tools/cargo"),
            r#"mkdir -p "$CARGO_HOME/bin"
echo "$*" >> "$CARGO_HOME/installs.log"
version=""; previous=""
for arg in "$@"; do [ "$previous" = "--version" ] && version="$arg"; previous="$arg"; done
printf '#!/bin/sh\necho "cargo-prune-superseded %s"\n' "$version" > "$CARGO_HOME/bin/cargo-prune-superseded"
chmod +x "$CARGO_HOME/bin/cargo-prune-superseded""#,
        );
        host
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    fn cargo_bin(&self) -> PathBuf {
        self.path("cargohome/bin")
    }

    fn installs(&self) -> String {
        std::fs::read_to_string(self.path("cargohome/installs.log")).unwrap_or_default()
    }

    /// Run the real recipe with `PATH` set to `dirs`, then the stand-in tools,
    /// and the system directories. The real just runs by absolute path.
    fn ensure(&self, dirs: &[PathBuf]) -> Output {
        let mut path: Vec<PathBuf> = dirs.to_vec();
        path.extend([self.path("tools"), "/usr/bin".into(), "/bin".into()]);
        Command::new(self.path("just"))
            .arg("--justfile")
            .arg(Path::new(REPO).join("justfile"))
            .arg("--working-directory")
            .arg(REPO)
            .arg("_ensure-prune-superseded")
            .env("PATH", std::env::join_paths(path).unwrap())
            .env("CARGO_HOME", self.path("cargohome"))
            .stdin(Stdio::null())
            .output()
            .expect("run `just _ensure-prune-superseded`")
    }
}

fn stderr(run: &Output) -> String {
    String::from_utf8_lossy(&run.stderr).into_owned()
}

#[test]
fn the_pinned_pruner_on_path_is_kept_without_an_install() {
    let host = Host::new();
    let pinned = host.path("pinned");
    pruner(&pinned, &pinned_version());

    let run = host.ensure(&[pinned]);
    assert!(
        run.status.success(),
        "the pinned pruner was refused: {}",
        stderr(&run)
    );
    assert!(
        stderr(&run).is_empty(),
        "a pinned pruner is not quiet: {}",
        stderr(&run)
    );
    assert_eq!(host.installs(), "", "the pinned pruner was reinstalled");
}

#[test]
fn a_stale_pruner_shadowing_the_install_fails_until_path_resolves_the_pin() {
    let host = Host::new();
    let stale = host.path("stale");
    let stale_pruner = pruner(&stale, "0.0.1");
    // The pinned release already sits in the cargo bin directory, behind the
    // stale one: what maintenance would run is the stale one.
    pruner(&host.cargo_bin(), &pinned_version());

    let run = host.ensure(&[stale.clone(), host.cargo_bin()]);
    let message = stderr(&run);
    assert!(
        !run.status.success(),
        "a shadowed install was accepted: {message}"
    );
    assert!(
        message.contains(&stale_pruner.display().to_string())
            && message.contains("cargo-prune-superseded 0.0.1")
            && message.contains("earlier on PATH"),
        "the refusal does not name the shadowing binary and the fix: {message}"
    );

    // Recovered: the cargo bin directory first, so `PATH` resolves the pin.
    let run = host.ensure(&[host.cargo_bin(), stale]);
    assert!(
        run.status.success(),
        "the recovered PATH was refused: {}",
        stderr(&run)
    );
    let installs = host.installs();
    let run = host.ensure(&[host.cargo_bin()]);
    assert!(
        run.status.success(),
        "a second run was refused: {}",
        stderr(&run)
    );
    assert_eq!(
        host.installs(),
        installs,
        "a second run reinstalled the pin"
    );
}

#[test]
fn an_install_off_path_names_the_directory_to_add() {
    let host = Host::new();

    let run = host.ensure(&[]);
    let message = stderr(&run);
    assert!(
        !run.status.success(),
        "a pruner missing from PATH was accepted: {message}"
    );
    assert!(
        host.installs().contains(&format!(
            "cargo-prune-superseded --version {}",
            pinned_version()
        )),
        "the pinned release was not installed: {:?}",
        host.installs()
    );
    assert!(
        message.contains("no cargo-prune-superseded is on PATH")
            && message.contains(&host.cargo_bin().display().to_string()),
        "the refusal does not name the directory to add: {message}"
    );
}
