#!/usr/bin/env node
// Launcher for the `oneagentgraph` command installed from the
// `oneagentgraph-cli` npm package.
//
// Like the PyPI wheels (maturin `bindings = "bin"`, see pyproject.toml), the npm
// distribution carries the *prebuilt* Rust binary — no Rust toolchain, no
// compile, no download at install time. The platform-specific binary ships
// inside a per-platform package (`oneagentgraph-cli-<platform>-<arch>`) declared
// in this package's `optionalDependencies`; npm installs only the one whose
// `os`/`cpu` match the host, and this shim resolves it and execs it with the
// caller's argv.
//
// This file is committed source; the version and the optionalDependency versions
// are stamped from Cargo.toml at publish time by scripts/npm-build.mjs, which
// also generates the per-platform packages from the release binaries.

const path = require("node:path");
const { spawnSync } = require("node:child_process");

// process.platform-process.arch -> the platform package that carries the binary.
// The keys mirror the Rust target matrix in .github/workflows/release.yml, the
// TARGETS table in scripts/npm-build.mjs, and the optionalDependencies in
// package.json; keep the four in lockstep.
const PACKAGES = {
  "linux-x64": "oneagentgraph-cli-linux-x64",
  "linux-arm64": "oneagentgraph-cli-linux-arm64",
  "darwin-x64": "oneagentgraph-cli-darwin-x64",
  "darwin-arm64": "oneagentgraph-cli-darwin-arm64",
  "win32-x64": "oneagentgraph-cli-win32-x64",
};

// Every failure here is a failed install, so say what to do about it rather than
// only what went wrong.
const OTHER_INSTALLS =
  "Install another way instead: 'pip install oneagentgraph-cli', or " +
  "'cargo install oneagentgraph --locked'.";

function fail(message) {
  process.stderr.write(`oneagentgraph: ${message}\n`);
  process.exit(1);
}

function binaryPath() {
  const key = `${process.platform}-${process.arch}`;
  const pkg = PACKAGES[key];
  // llmlint: ignore-block[changed_behavior_has_e2e] reaching this branch means running
  // where no prebuilt package exists, and a test could only get there by lying to node
  // about process.platform — which would prove the lie. The sibling branches are driven
  // for real by npm/test/launcher.test.mjs, and npm's own os/cpu fields keep a user from
  // installing a platform package this map does not name.
  if (!pkg) {
    fail(
      `unsupported platform ${key}. Prebuilt binaries exist for: ` +
        `${Object.keys(PACKAGES).join(", ")}. ${OTHER_INSTALLS}`,
    );
  }
  // llmlint: ignore-end[changed_behavior_has_e2e]

  const binName = process.platform === "win32" ? "oneagentgraph.exe" : "oneagentgraph";
  try {
    // Resolve the platform package's manifest, then locate the binary beside it.
    // Resolving package.json (rather than the binary file directly) is portable
    // across Node resolution modes and does not require an `exports` entry for a
    // non-JS asset.
    const manifest = require.resolve(`${pkg}/package.json`);
    return path.join(path.dirname(manifest), "bin", binName);
  } catch (_err) {
    return fail(
      `the platform package ${pkg} is not installed. This usually means npm ` +
        "skipped optional dependencies (e.g. --no-optional / --omit=optional) " +
        "or the install was for a different platform. Reinstall with optional " +
        `dependencies enabled. ${OTHER_INSTALLS}`,
    );
  }
}

const result = spawnSync(binaryPath(), process.argv.slice(2), { stdio: "inherit" });

if (result.error) {
  fail(`failed to launch the oneagentgraph binary: ${result.error.message}`);
}

// Re-raise a terminating signal so callers observe the true cause; otherwise
// propagate the child's exit code verbatim — the contract assigns meaning to 1
// and 2, and a caller reads them.
if (result.signal) {
  process.kill(process.pid, result.signal);
}
process.exit(result.status === null ? 1 : result.status);
