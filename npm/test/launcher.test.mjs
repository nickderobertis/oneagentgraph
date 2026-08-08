// The npm distribution's real journeys: assemble the packages the release
// publishes, install them the way a user does, and run what npm put on PATH.
//
// Nothing here is stubbed. `scripts/npm-build.mjs` assembles the real packages
// around the real compiled binary, `npm install` resolves them, and the launcher
// resolves the platform package and execs the binary. The one thing this cannot
// do locally is publish to the registry; `.github/workflows/release.yml` and
// `published-smoke.yml` cover that against the real npm.

import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { after, before, describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");

/// The Rust target triple for the host this test runs on — the one platform
/// package that can carry a binary this machine can execute.
function hostTarget() {
  const key = `${process.platform}-${process.arch}`;
  const targets = {
    "linux-x64": "x86_64-unknown-linux-gnu",
    "linux-arm64": "aarch64-unknown-linux-gnu",
    "darwin-x64": "x86_64-apple-darwin",
    "darwin-arm64": "aarch64-apple-darwin",
    "win32-x64": "x86_64-pc-windows-msvc",
  };
  const target = targets[key];
  assert.ok(target, `no prebuilt npm package exists for ${key}`);
  return target;
}

function run(command, args, options = {}) {
  return execFileSync(command, args, {
    cwd: REPO_ROOT,
    encoding: "utf8",
    ...options,
  });
}

/// Pack a package directory into the tarball the registry would serve.
///
/// Installing the directory instead would symlink it, and node resolves a
/// symlinked package's dependencies from its *realpath* — so the launcher would
/// look for its platform package beside the build output rather than in the
/// project's node_modules, and never find it. A tarball is both the real shape
/// and the one `release.yml` publishes.
function pack(dir, into) {
  const packed = JSON.parse(
    run("npm", ["pack", "--json", "--pack-destination", into, dir], {
      stdio: ["ignore", "pipe", "ignore"],
    }),
  );
  assert.equal(packed.length, 1, "npm pack must produce exactly one tarball");
  return join(into, packed[0].filename);
}

/// Install the given packed packages into a throwaway project.
/// `--omit=optional` keeps npm from reaching the registry for the launcher's
/// per-platform pins, which exist only once a release has published them: the
/// platform package under test is passed explicitly instead when it is wanted.
function installInto(project, packages) {
  run("npm", [
    "install",
    "--prefix",
    project,
    "--no-audit",
    "--no-fund",
    "--omit=optional",
    ...packages,
  ]);
}

/// Invoke the installed launcher, returning its exit code, stdout, and stderr.
function launch(project, args) {
  const bin = join(project, "node_modules", ".bin", "oneagentgraph");
  try {
    const stdout = run(bin, args, { cwd: project, stdio: ["ignore", "pipe", "pipe"] });
    return { code: 0, stdout, stderr: "" };
  } catch (error) {
    return {
      code: error.status,
      stdout: error.stdout ?? "",
      stderr: error.stderr ?? "",
    };
  }
}

describe("the npm distribution", () => {
  let work;
  let dist;
  let launcherDir;
  let platformDir;
  let launcherTgz;
  let platformTgz;
  let version;

  before(() => {
    version = JSON.parse(
      run("cargo", ["metadata", "--no-deps", "--format-version", "1", "--locked"]),
    ).packages.find((pkg) => pkg.name === "oneagentgraph").version;

    // The real binary the package will carry. Debug rather than release: this
    // proves the packaging, and a release build would cost the gate minutes.
    run("cargo", ["build", "--locked", "--quiet"]);

    work = mkdtempSync(join(tmpdir(), "oneagentgraph-npm-"));
    dist = join(work, "dist");
    platformDir = run("node", [
      "scripts/npm-build.mjs",
      "platform",
      "--target",
      hostTarget(),
      "--binary",
      join("target", "debug", "oneagentgraph"),
      "--out",
      dist,
    ]).trim();
    launcherDir = run("node", ["scripts/npm-build.mjs", "launcher", "--out", dist]).trim();

    const tarballs = join(work, "tgz");
    mkdirSync(tarballs);
    platformTgz = pack(platformDir, tarballs);
    launcherTgz = pack(launcherDir, tarballs);
  });

  after(() => {
    if (work) rmSync(work, { recursive: true, force: true });
  });

  it("stamps the crate's version into the launcher and its platform pins", () => {
    const manifest = JSON.parse(readFileSync(join(launcherDir, "package.json"), "utf8"));
    assert.equal(manifest.version, version, "the launcher takes its version from Cargo.toml");
    for (const [name, pin] of Object.entries(manifest.optionalDependencies)) {
      assert.equal(pin, version, `${name} must be pinned to the release version`);
    }
    const platform = JSON.parse(readFileSync(join(platformDir, "package.json"), "utf8"));
    assert.equal(platform.version, version);
    assert.deepEqual(platform.os, [process.platform]);
    assert.deepEqual(platform.cpu, [process.arch]);
  });

  it("puts a working `oneagentgraph` on PATH", () => {
    const project = mkdtempSync(join(work, "app-"));
    installInto(project, [launcherTgz, platformTgz]);

    const reported = launch(project, ["--version"]);
    assert.equal(reported.code, 0, reported.stderr);
    assert.match(reported.stdout, new RegExp(version.replace(/\./g, "\\.")));
  });

  it("propagates the binary's exit code rather than its own", () => {
    const project = mkdtempSync(join(work, "app-"));
    installInto(project, [launcherTgz, platformTgz]);

    // The contract assigns exit 2 to an invalid config, and a caller branches on
    // it; a launcher that collapsed every failure to 1 would hide the contract's
    // own codes. A graph that is not there is the cheapest way to reach one.
    const refused = launch(project, ["run", "no-such-graph.yaml"]);
    assert.equal(refused.code, 2, refused.stderr);
    assert.match(refused.stderr, /no-such-graph\.yaml/);
    assert.equal(refused.stdout, "", "a refusal must not read as an event stream");
  });

  it("says what to do when the platform package is missing", () => {
    const project = mkdtempSync(join(work, "app-"));
    installInto(project, [launcherTgz]);

    const failed = launch(project, ["--version"]);
    assert.equal(failed.code, 1);
    assert.match(failed.stderr, /platform package oneagentgraph-cli-/);
    assert.match(failed.stderr, /Reinstall with optional/);
    assert.match(failed.stderr, /pip install oneagentgraph-cli/);
  });
});
