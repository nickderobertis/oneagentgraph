// The drift gate for what this repository declares it releases.
//
// `release-targets.json` names one registry-qualified identifier per
// **consumable** artifact — one a dependent names in order to depend on it — so
// an orchestrator that landed a fix here can hold until the artifact carrying it
// is downloadable. A declaration that has gone stale is worse than none: a
// consumer waits on the target it was told about while the artifact it actually
// consumes ships unwatched. That is the failure this file exists to make loud.
//
// So nothing here is transcribed. The published set is derived from the release
// configuration itself:
//
//   * which registries publish at all — the `publish-*` jobs in
//     `.github/workflows/release.yml`,
//   * the crate name — `Cargo.toml`'s `[package]`,
//   * the PyPI project name — `pyproject.toml`'s `[project]`,
//   * the npm launcher name — `npm/oneagentgraph-cli/package.json`, and the
//     per-platform package names — `scripts/npm-build.mjs`'s TARGETS, through
//     the same template that script names them by.
//
// Add an artifact anywhere in that configuration and this fails until
// `release-targets.json` accounts for it; declare one this repository does not
// publish and it fails the other way.
//
// A release also attaches per-target archives to the GitHub Release, and those
// are not targets: nothing *depends* on one — they are for manual download, and
// the three registries are the documented install surfaces — so no dependent can
// be waiting on one. Only the `publish-*` jobs reach a registry, which is why
// they are what this reads.
//
// The per-platform packages are covered rather than declared: nothing names one
// in order to depend on it — npm resolves it for the launcher, at the launcher's
// own exact version — so the launcher is the target and its
// `optionalDependencies` are what it covers. `platform-matrix.test.mjs` is what
// holds that field to the release matrix; this file holds it to the declaration.

import { accessSync, constants, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");

// The registry each `publish-*` job pushes to. This is the one place a registry
// is named by hand, and it is a mapping rather than an inventory: a `publish-`
// job with no entry here fails the first test below, because a repository that
// started publishing somewhere new must say so before anything can wait on it.
const PUBLISHERS = {
  "publish-crate": "crate",
  "publish-pypi": "pypi",
  "publish-npm": "npm",
};

function read(...parts) {
  return readFileSync(join(REPO_ROOT, ...parts), "utf8");
}

/// The value of `name` in a TOML `[section]`, taking the first `name = "..."`
/// after the header and before the next section — so a dependency's name can
/// never be mistaken for the package's. The same hand parse `npm-build.mjs`
/// reads the version with, for the same reason: no TOML dependency.
function tomlName(toml, section) {
  const start = toml.indexOf(`[${section}]`);
  assert.notEqual(start, -1, `no [${section}] section`);
  const rest = toml.slice(start);
  const end = rest.indexOf("\n[", 1);
  const body = end === -1 ? rest : rest.slice(0, end);
  const found = body.match(/^\s*name\s*=\s*"([^"]+)"/m);
  assert.ok(found, `no name in [${section}]`);
  return found[1];
}

/// Every top-level job in a workflow, in declaration order.
function jobNames(workflow) {
  const jobs = workflow.slice(workflow.indexOf("\njobs:\n"));
  assert.ok(jobs, "the workflow declares no jobs");
  return [...jobs.matchAll(/^ {2}([a-z][a-z0-9-]*):$/gm)].map((m) => m[1]);
}

/// One job's body: from its own header to the next line at job indentation.
function jobBody(workflow, job) {
  const start = workflow.indexOf(`\n  ${job}:\n`);
  assert.notEqual(start, -1, `no \`${job}\` job in release.yml`);
  const rest = workflow.slice(start + 1);
  const next = rest.slice(1).search(/\n {2}[a-z][a-z0-9-]*:\n/);
  return next === -1 ? rest : rest.slice(0, next + 1);
}

/// The registries `scripts/release-probe.sh` recognises, read out of the `case`
/// that dispatches on them — so a target declared for a registry the probe
/// cannot answer for is caught here rather than by a consumer that waits forever.
function probeRegistries(probe) {
  const start = probe.indexOf('case "$registry" in');
  assert.notEqual(start, -1, "release-probe.sh no longer dispatches on the registry");
  const body = probe.slice(start, probe.indexOf("\nesac", start));
  return [...body.matchAll(/^ {2}([a-z]+)\)\s+url=/gm)].map((m) => m[1]);
}

const workflow = read(".github", "workflows", "release.yml");
const declaration = JSON.parse(read("release-targets.json"));
const launcher = JSON.parse(read("npm", "oneagentgraph-cli", "package.json"));
const buildScript = read("scripts", "npm-build.mjs");

const cargoName = tomlName(read("Cargo.toml"), "package");
const pypiName = tomlName(read("pyproject.toml"), "project");

/// The per-platform package names a release assembles, built the way
/// `npm-build.mjs` builds them: its own template, over its own TARGETS table.
function platformPackages() {
  const template = buildScript.match(
    /const pkgName = `([a-z0-9-]+)-\$\{facts\.platform\}-\$\{facts\.arch\}`/,
  );
  assert.ok(template, "npm-build.mjs no longer names platform packages by that template");
  const facts = [...buildScript.matchAll(/platform: "([^"]+)", arch: "([^"]+)"/g)];
  assert.ok(facts.length > 0, "npm-build.mjs declares no targets");
  return facts.map(([, platform, arch]) => `${template[1]}-${platform}-${arch}`);
}

/// Every name this repository publishes, per registry, derived from the release
/// configuration above.
function publishedNames(registry) {
  switch (registry) {
    case "crate":
      return [cargoName];
    case "pypi":
      return [pypiName];
    case "npm":
      return [launcher.name, ...platformPackages()];
    default:
      return assert.fail(`no derivation for the ${registry} registry`);
  }
}

/// The names a declared target covers without declaring: the launcher's
/// `optionalDependencies`, read out of the manifest the declaration names.
function coveredBy(target) {
  if (!target.covers) return [];
  const manifest = JSON.parse(read(...target.covers.manifest.split("/")));
  const covered = manifest[target.covers.field];
  assert.ok(covered, `${target.covers.manifest} has no ${target.covers.field}`);
  return Object.keys(covered);
}

describe("the declared release targets", () => {
  const targets = declaration.targets;
  const parsed = targets.map((target) => {
    const split = target.id.indexOf(":");
    assert.notEqual(split, -1, `'${target.id}' is not registry-qualified`);
    return { ...target, registry: target.id.slice(0, split), name: target.id.slice(split + 1) };
  });

  it("declares a target for every registry a release publishes to", () => {
    const publishing = jobNames(workflow)
      .filter((job) => job.startsWith("publish-"))
      .map((job) => {
        const registry = PUBLISHERS[job];
        assert.ok(
          registry,
          `release.yml's \`${job}\` job publishes somewhere PUBLISHERS does not name — ` +
            "map it to a registry here and declare its target in release-targets.json",
        );
        return registry;
      });
    assert.ok(publishing.length > 0, "release.yml publishes nothing");
    assert.deepEqual(
      [...new Set(parsed.map((t) => t.registry))].sort(),
      [...new Set(publishing)].sort(),
      "release-targets.json and release.yml disagree about which registries this repository publishes to",
    );
  });

  it("covers every published name exactly once", () => {
    for (const registry of new Set(parsed.map((t) => t.registry))) {
      const onRegistry = parsed.filter((t) => t.registry === registry);
      const accounted = new Map();
      for (const target of onRegistry) {
        for (const name of [target.name, ...coveredBy(target)]) {
          assert.equal(
            accounted.get(name),
            undefined,
            `${registry}:${name} is accounted for by both ${accounted.get(name)} and ${target.id}`,
          );
          accounted.set(name, target.id);
        }
      }
      for (const name of publishedNames(registry)) {
        assert.ok(
          accounted.has(name),
          `a release publishes ${registry}:${name}, which no declared target names or covers — ` +
            "add it to release-targets.json, or cover it from the launcher that resolves it",
        );
      }
    }
  });

  it("declares nothing this repository does not publish", () => {
    for (const target of parsed) {
      const published = publishedNames(target.registry);
      assert.ok(
        published.includes(target.name),
        `${target.id} is declared, but a release publishes ${target.registry}:{${published.join(", ")}} — ` +
          "the declaration names an artifact this repository does not publish",
      );
      for (const covered of coveredBy(target)) {
        assert.ok(
          published.includes(covered),
          `${target.id} claims to cover ${target.registry}:${covered}, which a release does not publish`,
        );
      }
    }
  });

  it("names only registries the probe can answer for", () => {
    const answerable = probeRegistries(read("scripts", "release-probe.sh"));
    for (const target of parsed) {
      assert.ok(
        answerable.includes(target.registry),
        `${target.id} names a registry ${declaration.probe} cannot answer for (it knows ${answerable.join(", ")})`,
      );
    }
  });

  it("points at an executable probe", () => {
    accessSync(join(REPO_ROOT, declaration.probe), constants.X_OK);
  });

  it("agrees with the crate name release.yml publishes by", () => {
    // `publish-crate` carries a second copy of the crate name, in the index
    // lookup that makes the publish idempotent. A rename that missed it would
    // leave every release re-publishing rather than skipping.
    assert.ok(
      jobBody(workflow, "publish-crate").includes(`"name":"${cargoName}"`),
      `release.yml's publish-crate job does not look up '${cargoName}', the crate Cargo.toml publishes`,
    );
  });
});
