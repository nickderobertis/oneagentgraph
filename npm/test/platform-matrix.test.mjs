// The drift gate for the one platform matrix this repository restates four
// times.
//
// The set of Rust targets a release builds has to agree, exactly, across:
//
//   1. `scripts/npm-build.mjs`'s TARGETS (triple -> platform/arch/exe),
//   2. `npm/oneagentgraph-cli/bin/oneagentgraph.js`'s PACKAGES (the launcher's
//      platform-to-package resolution),
//   3. `npm/oneagentgraph-cli/package.json`'s optionalDependencies (what npm
//      installs), and
//   4. the `upload`, `build-wheels`, and `build-npm` matrices in
//      `.github/workflows/release.yml` (what actually gets built).
//
// None can be generated from another — a workflow matrix is YAML a workflow
// engine reads, npm resolves optionalDependencies before any code runs, and the
// launcher must resolve with no build step. So the sets are reconciled here
// instead: add a platform in one place and this fails until it is added in all
// four. Drift here does not break a build; it 404s an install, on the one
// platform nobody tested.

import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");

function read(...parts) {
  return readFileSync(join(REPO_ROOT, ...parts), "utf8");
}

function objectLiteral(source, name) {
  const start = source.indexOf(`const ${name} = {`);
  assert.notEqual(start, -1, `no \`const ${name} = {\` in the source`);
  const end = source.indexOf("\n};", start);
  assert.notEqual(end, -1, `\`${name}\` is not terminated by a \`};\` line`);
  return source.slice(start, end);
}

function literalKeys(source, name) {
  return [...objectLiteral(source, name).matchAll(/^\s{2}"([^"]+)":/gm)].map((m) => m[1]);
}

function workflowTargets(workflow, job) {
  const start = workflow.indexOf(`\n  ${job}:\n`);
  assert.notEqual(start, -1, `no \`${job}\` job in release.yml`);
  // Jobs are indented two spaces, so the next line at that indent ends this one.
  const rest = workflow.slice(start + 1);
  const nextJob = rest.slice(1).search(/\n {2}[a-z][a-z0-9-]*:\n/);
  const body = nextJob === -1 ? rest : rest.slice(0, nextJob + 1);
  const targets = [...body.matchAll(/^\s*- target: (\S+)$/gm)].map((m) => m[1]);
  assert.ok(targets.length > 0, `the \`${job}\` job builds no targets`);
  return targets;
}

describe("the platform matrix", () => {
  const buildScript = read("scripts", "npm-build.mjs");
  const launcher = read("npm", "oneagentgraph-cli", "bin", "oneagentgraph.js");
  const manifest = JSON.parse(read("npm", "oneagentgraph-cli", "package.json"));
  const workflow = read(".github", "workflows", "release.yml");

  const triples = literalKeys(buildScript, "TARGETS");
  // `{ platform: "linux", arch: "x64", ... }` for each triple, in the same order.
  const facts = [
    ...objectLiteral(buildScript, "TARGETS").matchAll(/platform: "([^"]+)", arch: "([^"]+)"/g),
  ].map(([, platform, arch]) => ({ platform, arch }));

  it("names the same triples in every release matrix", () => {
    assert.ok(triples.length >= 1, "npm-build.mjs declares no targets");
    for (const job of ["upload", "build-wheels", "build-npm"]) {
      assert.deepEqual(
        workflowTargets(workflow, job).sort(),
        [...triples].sort(),
        `release.yml's \`${job}\` matrix and npm-build.mjs's TARGETS disagree`,
      );
    }
  });

  it("resolves every built target from the launcher", () => {
    assert.equal(facts.length, triples.length, "every target needs platform/arch facts");
    assert.deepEqual(
      literalKeys(launcher, "PACKAGES").sort(),
      facts.map(({ platform, arch }) => `${platform}-${arch}`).sort(),
      "the launcher's PACKAGES keys and npm-build.mjs's TARGETS disagree",
    );
  });

  it("installs exactly the packages the launcher resolves", () => {
    const built = facts.map(({ platform, arch }) => `oneagentgraph-cli-${platform}-${arch}`).sort();
    assert.deepEqual(
      Object.keys(manifest.optionalDependencies).sort(),
      built,
      "the launcher's optionalDependencies and the packages a release builds disagree",
    );
    const resolved = [...objectLiteral(launcher, "PACKAGES").matchAll(/: "([^"]+)"/g)]
      .map((m) => m[1])
      .sort();
    assert.deepEqual(
      resolved,
      built,
      "the launcher resolves package names a release does not build",
    );
  });
});
