// The published smoke's reporter, driven end to end against a stubbed `gh`.
//
// `scripts/report-workflow-failure.sh` is the only thing that makes a published
// smoke failure visible: that workflow runs when a release completes, so it has
// no pull request to turn red and nobody watching its checks list. It runs
// exactly when something is already broken — the one time it matters is the one
// time nobody is watching it work.
//
// It is also the rare path CI cannot rehearse: a real run files real issues into
// this repository. So `gh` is stubbed, the script runs as a subprocess, and the
// assertions read the argv it actually invoked — both branches, because "open an
// issue" and "comment on the one that is already open" are different behavior
// and the second is what keeps three broken releases to one thread.

import { spawnSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const REPORTER = join(REPO_ROOT, "scripts", "report-workflow-failure.sh");
const WORKFLOW = join(REPO_ROOT, ".github", "workflows", "published-smoke.yml");

/// The title every case files under, so a case can plant an issue that matches
/// it exactly and one that only looks like it.
const TITLE = "Published smoke is failing";
const RUN_URL = "https://example.invalid/run/1";

/// A `gh` that records every call it was given and answers `issue list` from a
/// file, so a case picks which branch the reporter should take. `GH_ERROR` makes
/// it fail the way the real one does: a message on stderr and a non-zero exit.
const STUB = `#!/usr/bin/env bash
printf '%s\\n' "$*" >>"$GH_CALLS"
if [ "\${1:-}" = "issue" ] && [ "\${2:-}" = "list" ] && [ -z "\${GH_FAIL_LIST:-}" ]; then
  cat "$GH_EXISTING"
  exit 0
fi
if [ -n "\${GH_ERROR:-}" ]; then
  printf '%s\\n' "$GH_ERROR" >&2
  exit 1
fi
echo "https://example.invalid/issues/7"
`;

/// Run the real reporter with a stubbed `gh` first on its search path, and hand
/// back what it did: its own output, and the calls the stub recorded.
///
/// `existing` is the `number<TAB>title` listing `gh issue list` should print.
/// Everything else is the environment the workflow's `report` job supplies.
function report({ existing = "", env = {} } = {}) {
  const dir = mkdtempSync(join(tmpdir(), "report-workflow-failure-"));
  try {
    const bin = join(dir, "bin");
    mkdirSync(bin);
    const stub = join(bin, "gh");
    writeFileSync(stub, STUB);
    chmodSync(stub, 0o755);

    const listing = join(dir, "existing");
    const calls = join(dir, "calls");
    writeFileSync(listing, existing);
    writeFileSync(calls, "");

    const result = spawnSync("bash", [REPORTER], {
      cwd: REPO_ROOT,
      env: {
        PATH: `${bin}${delimiter}${process.env.PATH}`,
        HOME: process.env.HOME,
        GH_CALLS: calls,
        GH_EXISTING: listing,
        REPO: "owner/repo",
        TITLE,
        BODY: "the smoke failed",
        RUN_URL,
        ...env,
      },
      encoding: "utf8",
    });
    assert.equal(result.error, undefined, `the reporter could not be spawned: ${result.error}`);
    return { ...result, calls: readFileSync(calls, "utf8") };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/// The stub is a shell script on a search path, which is what the GitHub runner
/// this job declares gives it. Nothing about the reporter is platform-specific,
/// and the gate that runs the workflow is Linux.
const POSIX_ONLY = process.platform === "win32" ? { skip: "stubs `gh` as a POSIX executable" } : {};

/// A green run: whatever it did, it must not have died doing it.
function assertFiled(result) {
  assert.equal(result.status, 0, `the reporter exited ${result.status}: ${result.stderr}`);
}

describe("the published smoke's reporter", () => {
  it("opens an issue when there is no open one", POSIX_ONLY, () => {
    const result = report();
    assertFiled(result);
    assert.match(result.calls, /^issue create /m, "expected an 'issue create' call");
    assert.doesNotMatch(
      result.calls,
      /^issue comment /m,
      "must not comment when there is nothing to comment on",
    );
    // Without the run URL the issue names a failure nobody can open.
    assert.ok(result.calls.includes(RUN_URL), "the run URL must reach the issue body");
  });

  it("comments on the open issue rather than opening a second one", POSIX_ONLY, () => {
    // Three broken releases must be one thread. Opening an issue per failure is
    // the shape that gets muted, which is the same as reporting nothing.
    const result = report({ existing: `41\t${TITLE}` });
    assertFiled(result);
    assert.match(result.calls, /^issue comment 41 /m, "expected a comment on #41");
    assert.doesNotMatch(result.calls, /^issue create /m, "must not open a second issue");
  });

  it(
    "does not comment a smoke failure onto an issue that merely resembles this one",
    POSIX_ONLY,
    () => {
      // `--search … in:title` is fuzzy, so it answers with near misses. Commenting
      // onto someone else's issue is worse than opening a second one.
      const result = report({ existing: `41\t${TITLE} (macOS)` });
      assertFiled(result);
      assert.match(result.calls, /^issue create /m, "a near miss must go to the create branch");
      assert.doesNotMatch(result.calls, /^issue comment /m, "must not comment on another issue");
    },
  );

  it("refuses an issue id that is not a number instead of addressing it", POSIX_ONLY, () => {
    // Drift in `gh issue list`. A comment addressed at whatever that names is a
    // request to something nobody chose.
    const result = report({ existing: `not-a-number\t${TITLE}` });
    assert.notEqual(result.status, 0, "a non-numeric id must be refused");
    assert.doesNotMatch(result.calls, /^issue comment /m, "must not comment on it");
  });

  it("refuses a missing input rather than filing an empty issue", POSIX_ONLY, () => {
    const result = report({ env: { TITLE: "" } });
    assert.notEqual(result.status, 0, "an empty title must be refused, not filed");
    assert.equal(result.calls, "", "a refused run must not call gh at all");
    assert.match(result.stderr, /TITLE/, "must name the variable that was missing");
    assert.match(result.stderr, /ACTION:/, "must say what to do about it");
  });

  it("never swallows the failure it was reporting when gh itself fails", POSIX_ONLY, () => {
    // The path this script exists to survive being on. It runs only when
    // something is already broken, so a `gh` failure here takes a real finding
    // down with it unless it says what it was doing, what gh said, what to do,
    // and where the finding still is.
    for (const [said, expected] of [
      ["gh: To get started with GitHub CLI, please run: gh auth login", /GH_TOKEN/],
      ["HTTP 403: Resource not accessible by integration", /issues: write/],
      ["HTTP 422: Validation Failed", /TITLE/],
      ["something nobody predicted", /ACTION:/],
    ]) {
      const result = report({ env: { GH_ERROR: said, GH_FAIL_LIST: "1" } });
      assert.notEqual(result.status, 0, `a failing gh must not read as a filed issue: ${said}`);
      assert.ok(result.stderr.includes(said), `must repeat what gh said: ${said}`);
      assert.match(result.stderr, expected, `must give the next action for: ${said}`);
      assert.ok(result.stderr.includes(RUN_URL), "the reported failure must stay findable");
    }
    // The two write branches fail their own way, and neither may be silent.
    const creating = report({ env: { GH_ERROR: "HTTP 500: Server Error" } });
    assert.match(creating.stderr, /opening an issue/, "the create branch must name itself");
    const commenting = report({
      existing: `41\t${TITLE}`,
      env: { GH_ERROR: "HTTP 500: Server Error" },
    });
    assert.match(commenting.stderr, /commenting on #41/, "the comment branch must name itself");
  });
});

describe("the published smoke's trigger", () => {
  const workflow = readFileSync(WORKFLOW, "utf8");

  it("asks the registries when a release completes, and on no schedule", () => {
    // A weekly sweep learns a moved dist-tag up to seven days late; a release
    // completing is the moment the answer can have just become wrong.
    assert.match(workflow, /^\s{2}workflow_run:\n\s+workflows: \["Release"\]/m);
    assert.doesNotMatch(workflow, /^\s*schedule:/m, "there must be no cron schedule left");
    assert.doesNotMatch(workflow, /cron:/, "there must be no cron expression left");
    // The entry point after a registry incident, and the input it takes.
    assert.match(workflow, /^\s{2}workflow_dispatch:/m);
    assert.match(workflow, /^\s{6}version:/m);
  });

  it("runs the reporter from a checkout of this repository", () => {
    // The reporter is a file in this tree, so a `report` job without a checkout
    // is a job that reports nothing — which is the failure mode this replaces.
    const reportJob = workflow.slice(workflow.indexOf("\n  report:"));
    assert.ok(reportJob.includes("actions/checkout"), "the report job must check this repo out");
    assert.ok(
      reportJob.includes("bash scripts/report-workflow-failure.sh"),
      "the report job must run the reporter this suite drives",
    );
    assert.ok(reportJob.includes("issues: write"), "the report job must be able to write issues");
    assert.ok(
      reportJob.includes(`TITLE: ${TITLE}`),
      "the workflow files under the title tested here",
    );
  });
});
