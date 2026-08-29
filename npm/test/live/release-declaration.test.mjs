// The canonical reader's own verdict on this repository's declaration.
//
// `scripts/check-release-declaration.mjs` mirrors the canonical release-target
// schema, because the `onevcs` release carrying the real reader is not on
// crates.io yet. A mirror nobody compares to the thing it mirrors is a second
// opinion presented as the first one — so this is the comparison: the document
// this repository ships is handed to `onevcs release declaration`, which IS the
// schema, and its verdict is read.
//
// Outside `npm/test/*.test.mjs`, and so outside `just check`, for the reason the
// live probe tier is: it needs something a clean clone does not have. `just
// release-declaration-check` runs it, and it is the one such tier with no CI job —
// the reader it calls is unpublished, so nothing CI can install provides it.
//
// Nothing here is stubbed. A fake `onevcs` would put the fake under test, and a
// fake is precisely the thing that cannot tell you the canonical schema moved,
// which is the whole failure this tier exists to catch.

import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

import { parse, stringify } from "smol-toml";

import { FILE, readDeclaration } from "../../../scripts/check-release-declaration.mjs";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");

/// Ask the canonical reader about one document, as a direct subprocess.
function declaration(path) {
  const result = spawnSync("onevcs", ["release", "declaration", path], {
    cwd: REPO_ROOT,
    encoding: "utf8",
    timeout: 60_000,
  });
  assert.equal(
    result.error,
    undefined,
    `onevcs could not be spawned: ${result.error} — this tier needs the onevcs release ` +
      "carrying `release declaration`, which is the definition of this schema",
  );
  assert.doesNotMatch(
    result.stderr,
    /unrecognized subcommand/,
    "this onevcs predates `release declaration`, so there is nothing to check against yet — " +
      "install a newer one",
  );
  return result;
}

describe("the canonical release-target reader, against this repository's declaration", () => {
  it("accepts release-targets.toml, whole", () => {
    const result = declaration(FILE);
    assert.equal(result.status, 0, `the canonical reader refused this document:\n${result.stderr}`);
    // Every identifier this repository declares or covers, read back out of the
    // reader's own report: an accepted document that dropped a target on the way
    // through would pass a bare exit-code assertion.
    const declared = readDeclaration(join(REPO_ROOT, FILE));
    for (const target of declared.targets) {
      assert.match(result.stdout, new RegExp(target.id.replace(/[.]/g, "\\.")));
      assert.match(result.stdout, new RegExp(`\\b${target.name}\\b`));
      for (const covered of target.covers) {
        assert.match(result.stdout, new RegExp(covered.id.replace(/[.]/g, "\\.")));
      }
    }
  });

  it("refuses what this repository's own checker refuses", () => {
    // The direction that says the reader is deciding rather than merely present:
    // a document the mirror rejects has to be rejected by the definition too, or
    // the mirror is stricter than the schema and this repository is holding itself
    // to a rule nobody else has.
    const dir = mkdtempSync(join(tmpdir(), "release-declaration-live-"));
    try {
      const document = parse(readFileSync(join(REPO_ROOT, FILE), "utf8"));
      document.target[1].name = document.target[0].name;
      const path = join(dir, FILE);
      writeFileSync(path, stringify(document));
      const result = declaration(path);
      assert.notEqual(result.status, 0, "the canonical reader accepted two targets sharing a name");
      assert.match(result.stderr, /short name/);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
