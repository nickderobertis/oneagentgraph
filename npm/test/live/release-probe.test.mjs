// The release probe's two registry-served answers, against the real registries.
//
// Outside `npm/test/*.test.mjs`, and so outside `just check`, on purpose: the
// deterministic gate stays offline and credential-free, exactly as `cargo deny`
// does under `just deps-check`. `just release-probe-check` runs this, and CI
// gives it a job of its own for the same reason it gives `deps-check` one.
//
// Nothing here is stubbed. A fake registry would put the fake under test — and
// the fake is precisely the thing that cannot tell you that crates.io renamed a
// field, which is the failure mode that would turn a released artifact into an
// empty answer and launch a consumer early.
//
// Two answers, on all three registries this repository publishes to:
//
//   * a target it has released           -> exit 0, the served version
//   * a name that registry never served  -> exit 0, nothing at all
//
// The third answer is `npm/test/release-probe.test.mjs`, which needs no network.

import { dirname, join, resolve } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

import { FILE, readDeclaration } from "../support/declaration.mjs";
import { probe } from "../support/release-probe.mjs";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");

/// This repository's own targets, read from the declaration rather than listed
/// here: a target added there is probed here without being transcribed.
const DECLARED = readDeclaration(join(REPO_ROOT, FILE)).targets;

/// A name no registry has ever served, per registry. Deliberately derived from
/// each declared identifier so it stays in this repository's own namespace: a
/// bare made-up word is one publish away from being somebody's real package,
/// and that publish would turn this from a passing test into a silent one.
function neverPublished(id) {
  return `${id}-no-such-release`;
}

/// The bound the release-target contract sets on an answer.
const BOUND_MS = 60_000;

describe("the release probe, against the public registries", () => {
  for (const target of DECLARED) {
    const [registry] = target.id.split(":");

    it(`reports the version ${registry} serves for ${target.id}`, () => {
      const result = probe(target.id);
      assert.equal(
        result.status,
        0,
        `${target.id} was not answered: ${result.stderr}\n` +
          "if this is a network failure it is not evidence that nothing is published",
      );
      assert.match(
        result.stdout,
        /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?\n$/,
        `${target.id} answered '${result.stdout}', which is not one version on one line`,
      );
      assert.ok(
        result.elapsedMs < BOUND_MS,
        `${target.id} took ${result.elapsedMs}ms, past the ${BOUND_MS}ms a consumer allows it`,
      );
    });

    it(`answers nothing at all for a ${registry} name that was never released`, () => {
      const id = neverPublished(target.id);
      const result = probe(id);
      assert.equal(result.status, 0, `${id} was not answered: ${result.stderr}`);
      assert.equal(
        result.stdout,
        "",
        `${id} has never been published, so the only correct answer is an empty one`,
      );
      assert.ok(
        result.elapsedMs < BOUND_MS,
        `${id} took ${result.elapsedMs}ms, past the ${BOUND_MS}ms a consumer allows it`,
      );
    });
  }
});
