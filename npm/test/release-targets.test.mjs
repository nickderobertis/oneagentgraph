// The drift gate for what this repository declares it releases.
//
// `release-targets.toml` names one registry-qualified identifier per
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
// `release-targets.toml` accounts for it; declare one this repository does not
// publish and it fails the other way.
//
// What that document may SAY is a separate question from whether it is true, and
// it is answered separately: `npm/test/release-declaration.test.mjs` holds it to
// the canonical schema. This reads it through that schema's own reader, so a
// target here is one whose identifier, short name and prose have already been
// validated, and everything below is about drift alone.
//
// A gate that only ever runs against a tree it passes proves nothing about what it
// would catch, so the last two cases run these same checks over a checkout of this
// repository's real release configuration whose declaration has been changed in one
// direction and then the other — a name published without being declared, and a
// name declared without being published. Nothing is stubbed: every input is the
// real file, and exactly one of them differs.
//
// A release also attaches per-target archives to the GitHub Release, and those
// are not targets: nothing *depends* on one — they are for manual download, and
// the three registries are the documented install surfaces — so no dependent can
// be waiting on one. Only the `publish-*` jobs reach a registry, which is why
// they are what this reads.
//
// The per-platform packages are covered rather than declared: nothing names one
// in order to depend on it — npm resolves it for the launcher, at the launcher's
// own exact version — so the launcher is the target and they are the identifiers
// it `covers`. `platform-matrix.test.mjs` holds the launcher's
// `optionalDependencies` to the release matrix; this file holds the declaration's
// `covers` to what a release actually publishes.

import {
  accessSync,
  constants,
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

import { parse, stringify } from "smol-toml";

import { FILE, readDeclaration } from "../../scripts/check-release-declaration.mjs";

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

/// The value of `name` in a TOML `[section]`, taking the first `name = "..."`
/// after the header and before the next section — so a dependency's name can
/// never be mistaken for the package's. The same hand parse `npm-build.mjs`
/// reads the version with.
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

/// Every file the gate reads, relative to a checkout root. A case that stands one
/// up copies exactly these, so a checkout it built and this repository differ in
/// nothing but the declaration.
const INPUT_FILES = [
  [".github", "workflows", "release.yml"],
  ["Cargo.toml"],
  ["pyproject.toml"],
  ["npm", "oneagentgraph-cli", "package.json"],
  ["scripts", "npm-build.mjs"],
  ["scripts", "release-probe.sh"],
  [FILE],
];

/// The release configuration of one checkout, read off disk. Taking a root rather
/// than closing over this repository's is what lets the last two cases run the
/// very checks the gate runs against a checkout that has drifted.
function inputs(root) {
  const read = (...parts) => readFileSync(join(root, ...parts), "utf8");
  return {
    root,
    workflow: read(".github", "workflows", "release.yml"),
    declaration: readDeclaration(join(root, FILE)),
    launcher: JSON.parse(read("npm", "oneagentgraph-cli", "package.json")),
    buildScript: read("scripts", "npm-build.mjs"),
    probe: read("scripts", "release-probe.sh"),
    cargoName: tomlName(read("Cargo.toml"), "package"),
    pypiName: tomlName(read("pyproject.toml"), "project"),
  };
}

/// The per-platform package names a release assembles, built the way
/// `npm-build.mjs` builds them: its own template, over its own TARGETS table.
function platformPackages(buildScript) {
  const template = buildScript.match(
    /const pkgName = `([a-z0-9-]+)-\$\{facts\.platform\}-\$\{facts\.arch\}`/,
  );
  assert.ok(template, "npm-build.mjs no longer names platform packages by that template");
  const facts = [...buildScript.matchAll(/platform: "([^"]+)", arch: "([^"]+)"/g)];
  assert.ok(facts.length > 0, "npm-build.mjs declares no targets");
  return facts.map(([, platform, arch]) => `${template[1]}-${platform}-${arch}`);
}

/// Every name a checkout publishes, per registry, derived from its release
/// configuration above.
function publishedNames(io, registry) {
  switch (registry) {
    case "crate":
      return [io.cargoName];
    case "pypi":
      return [io.pypiName];
    case "npm":
      return [io.launcher.name, ...platformPackages(io.buildScript)];
    default:
      return assert.fail(`no derivation for the ${registry} registry`);
  }
}

/// What one identifier is accounted for by, keyed the way a registry serves it.
function key(registry, name) {
  return `${registry}:${name}`;
}

/// Every registry a release publishes to has a target, and no target names a
/// registry no release reaches.
function declaresEveryPublishingRegistry(io) {
  const publishing = jobNames(io.workflow)
    .filter((job) => job.startsWith("publish-"))
    .map((job) => {
      const registry = PUBLISHERS[job];
      assert.ok(
        registry,
        `release.yml's \`${job}\` job publishes somewhere PUBLISHERS does not name — ` +
          "map it to a registry here and declare its target in release-targets.toml",
      );
      return registry;
    });
  assert.ok(publishing.length > 0, "release.yml publishes nothing");
  assert.deepEqual(
    [...new Set(io.declaration.targets.map((t) => t.registry))].sort(),
    [...new Set(publishing)].sort(),
    "release-targets.toml and release.yml disagree about which registries this repository publishes to",
  );
}

/// The first direction: everything a release publishes is either declared as a
/// target or covered by one, and never both and never twice.
function coversEveryPublishedName(io) {
  const accounted = new Map();
  for (const target of io.declaration.targets) {
    // The target's own artifact, then everything its release also ships. A
    // covered entry is a whole identifier, so it is keyed by the registry it
    // names rather than by the one its coverer is served from.
    const shipped = [
      key(target.registry, target.artifact),
      ...target.covers.map((covered) => key(covered.registry, covered.name)),
    ];
    for (const id of shipped) {
      assert.equal(
        accounted.get(id),
        undefined,
        `${id} is accounted for by both ${accounted.get(id)} and ${target.id}`,
      );
      accounted.set(id, target.id);
    }
  }
  for (const registry of new Set(io.declaration.targets.map((t) => t.registry))) {
    for (const name of publishedNames(io, registry)) {
      assert.ok(
        accounted.has(key(registry, name)),
        `a release publishes ${key(registry, name)}, which no declared target names or ` +
          "covers — add it to release-targets.toml, or cover it from the launcher that " +
          "resolves it",
      );
    }
  }
}

/// The other direction: nothing the declaration names — as a target or as
/// something a target covers — is absent from what a release publishes.
function declaresNothingUnpublished(io) {
  for (const target of io.declaration.targets) {
    const published = publishedNames(io, target.registry);
    assert.ok(
      published.includes(target.artifact),
      `${target.id} is declared, but a release publishes ${target.registry}:{${published.join(", ")}} — ` +
        "the declaration names an artifact this repository does not publish",
    );
    for (const covered of target.covers) {
      assert.ok(
        publishedNames(io, covered.registry).includes(covered.name),
        `${target.id} claims to cover ${covered.registry}:${covered.name}, which a release does not publish`,
      );
    }
  }
}

/// A checkout of this repository's release configuration whose declaration has
/// been changed by `mutate`, and nothing else has. The caller removes it.
function checkoutDeclaring(mutate) {
  const root = mkdtempSync(join(tmpdir(), "release-drift-"));
  for (const parts of INPUT_FILES) {
    mkdirSync(join(root, ...parts.slice(0, -1)), { recursive: true });
    copyFileSync(join(REPO_ROOT, ...parts), join(root, ...parts));
  }
  const document = parse(readFileSync(join(root, FILE), "utf8"));
  mutate(document);
  writeFileSync(join(root, FILE), stringify(document));
  return root;
}

/// Run `check` over a checkout `mutate` has drifted, and answer what it refused.
/// A check that passes one is itself the finding: the gate would have let this
/// repository ship a declaration nobody could act on.
function drift(mutate, check) {
  const root = checkoutDeclaring(mutate);
  try {
    check(inputs(root));
  } catch (refusal) {
    return refusal;
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
  return assert.fail("the drift gate accepted a checkout whose declaration had drifted");
}

describe("the declared release targets", () => {
  // Each target's `registry` and `artifact` are its identifier's two halves, split
  // by the schema's own reader; `name` is the short name a consumer waits on it by,
  // which is deliberately not derivable from either.
  const io = inputs(REPO_ROOT);

  it("declares a target for every registry a release publishes to", () => {
    declaresEveryPublishingRegistry(io);
  });

  it("covers every published name exactly once", () => {
    coversEveryPublishedName(io);
  });

  it("declares nothing this repository does not publish", () => {
    declaresNothingUnpublished(io);
  });

  it("names only registries the probe can answer for", () => {
    const answerable = probeRegistries(io.probe);
    for (const target of io.declaration.targets) {
      assert.ok(
        answerable.includes(target.registry),
        `${target.id} names a registry ${io.declaration.probe} cannot answer for (it knows ${answerable.join(", ")})`,
      );
    }
  });

  it("points at an executable probe", () => {
    accessSync(join(REPO_ROOT, io.declaration.probe), constants.X_OK);
  });

  it("agrees with the crate name release.yml publishes by", () => {
    // `publish-crate` carries a second copy of the crate name, in the index
    // lookup that makes the publish idempotent. A rename that missed it would
    // leave every release re-publishing rather than skipping.
    assert.ok(
      jobBody(io.workflow, "publish-crate").includes(`"name":"${io.cargoName}"`),
      `release.yml's publish-crate job does not look up '${io.cargoName}', the crate Cargo.toml publishes`,
    );
  });
});

describe("the drift gate itself", () => {
  it("fails on a name this repository publishes without declaring", () => {
    // The launcher stops covering one of the per-platform packages a release
    // still builds and publishes. Nothing would wait on that package — it is
    // covered, not a target — but the launcher would be shipping something the
    // declaration no longer accounts for.
    const dropped = drift((document) => document.target[2].covers.pop(), coversEveryPublishedName);
    assert.match(dropped.message, /no declared target names or covers/);
    assert.match(dropped.message, /npm:oneagentgraph-cli-win32-x64/);

    // And a whole registry going undeclared, which is the same failure one level up.
    const unpublished = drift(
      (document) => document.target.splice(1, 1),
      declaresEveryPublishingRegistry,
    );
    assert.match(unpublished.message, /disagree about which registries/);
  });

  it("fails on a name declared without this repository publishing it", () => {
    const renamed = drift((document) => {
      document.target[1].id = "pypi:oneagentgraph-command-line";
    }, declaresNothingUnpublished);
    assert.match(renamed.message, /names an artifact this repository does not publish/);

    const covered = drift((document) => {
      document.target[2].covers.push("npm:oneagentgraph-cli-solaris-sparc");
    }, declaresNothingUnpublished);
    assert.match(covered.message, /claims to cover npm:oneagentgraph-cli-solaris-sparc/);
  });
});
