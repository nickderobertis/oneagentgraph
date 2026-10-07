//! A throwaway copy of this repository, for the journeys that drive the real
//! justfile, `scripts/`, Nx, and git without touching the checkout under test.

use std::path::Path;
use std::process::Command;

/// This checkout, which every journey copies rather than mutates.
pub const REPO: &str = env!("CARGO_MANIFEST_DIR");

/// Copy exactly what git would commit from here into `root`, so the copy hashes
/// the way the original does: Nx skips ignored state, and bringing `target/` or
/// `.nx/` along would add files the original never hashed.
///
/// `node_modules` is the one exception — ignored state Nx itself needs, far too
/// large to copy, so it is a link out to this checkout's own install. That is why
/// `just bootstrap` has to have run before these journeys do.
pub fn copy_into(root: &Path) {
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
    // `scripts/nx.sh` into `npm ci` *through* it, writing this checkout's install
    // from inside a temporary directory — a several-minute detour that reads as a
    // hung test rather than as missing provisioning.
    assert!(
        install.is_dir(),
        "these journeys drive real Nx and take this checkout's own install: run `just \
         bootstrap` first"
    );
    std::os::unix::fs::symlink(install, root.join("node_modules"))
        .expect("link the copy at this checkout's own Nx install");
}

/// Run git in `directory` as a fixed journey identity, failing on any error.
pub fn git(directory: &Path, args: &[&str]) -> String {
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
