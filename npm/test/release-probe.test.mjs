// The release probe's third answer: NOT ANSWERED.
//
// `scripts/release-probe.sh` answers a registry-qualified identifier in exactly
// three ways — a version on stdout, nothing on stdout, or a non-zero exit with
// its reason on stderr. The first two need a public registry to answer, so they
// are proven against the real registries by `npm/test/live/release-probe.test.mjs`
// under `just release-probe-check`, which sits outside `just check` for the
// reason `just deps-check` does: the deterministic gate stays offline.
//
// What is left is the answer that must never be confused with the second one,
// and every route to it is reachable with no network at all. A consumer holds
// indefinitely on "not answered" and must never read it as evidence that nothing
// is published, so each case below asserts all three of: a non-zero exit, an
// empty stdout, and a reason with a next action on stderr.

import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";

import { assertNotAnswered, probe, probeWithPath } from "./support/release-probe.mjs";

/// Where a tool actually is, so a search path can be built that has some of them
/// and not others.
function which(tool) {
  return execFileSync("bash", ["-c", `command -v ${tool}`], { encoding: "utf8" }).trim();
}

describe("the release probe's not-answered answer", () => {
  it("refuses anything but exactly one identifier", () => {
    assertNotAnswered(probe(), "no argument at all");
    assertNotAnswered(probe("crate:oneagentgraph", "npm:oneagentgraph-cli"), "two identifiers");
  });

  it("refuses an unqualified name rather than guessing a registry", () => {
    // The whole point of the qualification: one name is published to two
    // registries on two cadences, so an unqualified name is two artifacts and
    // answering for either would be answering the wrong question.
    const result = probe("oneagentgraph-cli");
    assertNotAnswered(result, "an unqualified name");
    assert.match(result.stderr, /not registry-qualified/);
  });

  it("refuses a registry it cannot answer for", () => {
    const result = probe("apt:oneagentgraph");
    assertNotAnswered(result, "an unknown registry");
    assert.match(result.stderr, /unknown registry/);
  });

  it("refuses a name it cannot build a URL for", () => {
    for (const identifier of ["npm:", "npm:@scope/pkg", "npm:one agent graph", "pypi:../etc"]) {
      assertNotAnswered(probe(identifier), `the name in '${identifier}'`);
    }
  });

  it("refuses to answer at all when it has no transport", (t) => {
    if (process.platform === "win32") {
      // The search path this builds is a POSIX one, and the gate that runs this
      // suite is Linux. Nothing about the branch is platform-specific.
      t.skip("builds a POSIX search path");
      return;
    }
    // A real machine with no curl on it, assembled rather than simulated: a
    // search path carrying its interpreter and the one tool the probe reaches
    // for before the transport check, and nothing else. Reaching a registry is
    // the only thing this probe can do, so a machine that cannot must say so —
    // an empty answer here would report every artifact in the world as
    // unreleased.
    const bin = mkdtempSync(join(tmpdir(), "release-probe-no-curl-"));
    try {
      for (const tool of ["bash", "grep"]) symlinkSync(which(tool), join(bin, tool));
      const result = probeWithPath(bin, "crate:oneagentgraph");
      assertNotAnswered(result, "a machine with no curl");
      assert.match(result.stderr, /curl/);
    } finally {
      rmSync(bin, { recursive: true, force: true });
    }
  });

  it("never answers an unrecognised identifier the way it answers an unreleased one", () => {
    // The failure this whole contract exists to prevent, stated as one
    // assertion. An unrecognised identifier must not exit 0 with empty output,
    // because that is precisely the answer that means "this registry has never
    // served it" — and a consumer reading the one as the other launches early,
    // against a fix that is not in force.
    const unrecognised = probe("npm:@scope/pkg");
    assert.notEqual(
      unrecognised.status,
      0,
      "an unrecognised identifier exited 0, which a caller reads as an answer about a real artifact",
    );
  });
});
