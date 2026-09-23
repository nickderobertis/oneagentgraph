// The visual-docs capture step, driven under the shell its container gives it.
//
// `.github/workflows/visual-docs.yml` hands screencomp's reusable workflow a
// `capture-command`, and that command runs as a step inside `container:
// rust:bookworm`. A container step sets no `shell:` there, so GitHub runs it with
// `sh -e {0}` — not the `bash -e {0}` a host-runner step gets — and on a Debian
// image that `sh` is dash. A bashism in that block therefore ends the step at the
// line that uses it, before a single shot exists: `set -euo pipefail` did exactly
// that (`set: Illegal option -o pipefail`, exit 2) on every run of the workflow
// from its adoption until this suite landed, `main` included.
//
// Nothing else catches that. The capture is deliberately outside `just check` and
// the `gate` job, its own workflow is not a required check, and the local pre-push
// guard runs the script under an interactive developer's bash — which is the one
// shell that cannot see the defect. So the block itself is driven here: extracted
// from the workflow verbatim, run under a strict POSIX `sh` with `-e` the way the
// runner runs it, with the tools it reaches for stubbed on its search path so the
// case is about the shell rather than about the network.
//
// The final line is `bash scripts/screenshots.sh`, and that is the portable seam:
// the capture itself keeps its own `set -euo pipefail` because bash is what runs
// it. These cases hold the seam in place — a bashism moved back up into the block
// fails here.

import { spawnSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const WORKFLOW = join(REPO_ROOT, ".github", "workflows", "visual-docs.yml");
const TOOLS_ENV = join(REPO_ROOT, "screenshots", "tools.env");

/// The shell a container step is given. `/bin/sh` is dash on the Linux runner
/// that runs this suite and in the `rust:bookworm` image the capture runs in, so
/// the same rejection happens here and there.
const SH = "/bin/sh";

/// Whether this host's `/bin/sh` is that strict POSIX shell. macOS's is bash in
/// POSIX mode, which accepts the very bashism the capture died on — it can still
/// run a POSIX command, so the cases below hold, but it cannot witness a refusal.
const SH_REFUSES_BASHISMS = spawnSync(SH, ["-c", "set -o pipefail"]).status !== 0;

const STRICT_SH_ONLY = SH_REFUSES_BASHISMS
  ? {}
  : { skip: `${SH} accepts bash-only syntax on this host, so it cannot witness a refusal` };

/// The `capture-command:` block, verbatim out of the workflow — the string the
/// runner writes to a file and hands the shell. Read rather than restated: a copy
/// here would be a second capture command, and the one that runs is that file's.
function captureCommand() {
  const source = readFileSync(WORKFLOW, "utf8");
  const header = "\n      capture-command: |\n";
  const start = source.indexOf(header);
  assert.notEqual(start, -1, "no `capture-command: |` block in visual-docs.yml");
  const body = [];
  for (const line of source.slice(start + header.length).split("\n")) {
    if (line !== "" && !line.startsWith("        ")) break;
    body.push(line.slice(8));
  }
  while (body.length > 0 && body.at(-1) === "") body.pop();
  assert.ok(body.length > 1, "the capture-command block is empty");
  return `${body.join("\n")}\n`;
}

/// A stub that records the call and exits with whatever the case asked for, so a
/// failing download is a case rather than a broken network.
function stub(name, statusVar) {
  return `#!/bin/sh
printf '%s %s\\n' "${name}" "$*" >>"$CAPTURE_CALLS"
exit "\${${statusVar}:-0}"
`;
}

const STUBS = {
  curl: "CURL_STATUS",
  tar: "TAR_STATUS",
  install: "INSTALL_STATUS",
  // The capture proper. Stubbed because what these cases are about is whether the
  // step reaches it at all; the capture's own behaviour is the workflow's job.
  bash: "BASH_STATUS",
};

/// Run the workflow's own capture-command the way the runner runs it — `sh -e
/// <file>`, from the checkout root, with `$SHOTS_OUT` set as the reusable workflow
/// exports it — and hand back what it did and which tools it called.
function capture(env = {}) {
  const dir = mkdtempSync(join(tmpdir(), "visual-docs-capture-"));
  try {
    const bin = join(dir, "bin");
    mkdirSync(bin);
    for (const [name, statusVar] of Object.entries(STUBS)) {
      const path = join(bin, name);
      writeFileSync(path, stub(name, statusVar));
      chmodSync(path, 0o755);
    }
    const script = join(dir, "capture-command.sh");
    writeFileSync(script, captureCommand());
    const calls = join(dir, "calls");
    writeFileSync(calls, "");

    const result = spawnSync(SH, ["-e", script], {
      cwd: REPO_ROOT,
      env: {
        PATH: `${bin}${delimiter}${process.env.PATH}`,
        HOME: process.env.HOME,
        CAPTURE_CALLS: calls,
        SHOTS_OUT: "shots/current/x86_64",
        ...env,
      },
      encoding: "utf8",
    });
    assert.equal(result.error, undefined, `the capture could not be spawned: ${result.error}`);
    return { ...result, calls: readFileSync(calls, "utf8") };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/// The stubs are POSIX executables on a search path, and the workflow's capture
/// runs on Linux. Nothing here has a Windows reading.
const POSIX_ONLY = process.platform === "win32" ? { skip: "stubs POSIX executables" } : {};

describe("the visual-docs capture command", () => {
  it("runs to its end under the `sh -e` a container step is given", POSIX_ONLY, () => {
    const result = capture();
    assert.equal(
      result.status,
      0,
      `the capture exited ${result.status} under ${SH}: ${result.stderr}`,
    );
    assert.equal(result.stderr, "", "a shell that had to complain about the command is the bug");
    // The step's whole point: it must reach the capture, under bash, which is the
    // shell scripts/screenshots.sh is written for.
    assert.match(
      result.calls,
      /^bash scripts\/screenshots\.sh$/m,
      "the capture script must be reached, and handed to bash",
    );
    // `. screenshots/tools.env` is what puts the pinned renderer in the URL. If the
    // sourcing had not worked, the version would be empty rather than wrong.
    const version = /^FREEZE_VERSION=(\S+)$/m.exec(readFileSync(TOOLS_ENV, "utf8"))?.[1];
    assert.ok(version, "screenshots/tools.env declares no FREEZE_VERSION");
    assert.ok(
      result.calls.includes(`/v${version}/freeze_${version}_Linux_x86_64.tar.gz`),
      `the download must name the pinned freeze ${version}: ${result.calls}`,
    );
  });

  it("stops at a failed download instead of capturing nothing and passing", POSIX_ONLY, () => {
    // What `-e` buys, and the reason dropping `pipefail` costs nothing here: with
    // no pipeline in the block, a failing command is the only way this can go
    // wrong, and it must not reach a capture that would then find no renderer.
    const result = capture({ CURL_STATUS: "22" });
    assert.notEqual(result.status, 0, "a failed download must fail the step");
    assert.doesNotMatch(
      result.calls,
      /^bash scripts\/screenshots\.sh$/m,
      "the capture must not run once its renderer could not be fetched",
    );
  });

  it("is run by a shell that refuses bash-only syntax", STRICT_SH_ONLY, () => {
    // The guard on the case above: it only means something if this shell would
    // have caught the original defect. `set -euo pipefail` is the line CI died on,
    // so drive that line rather than assert something about it.
    const result = spawnSync(SH, ["-e", "-c", "set -euo pipefail\necho reached"], {
      encoding: "utf8",
    });
    assert.notEqual(result.status, 0, "this shell accepted a bashism the container's refuses");
    assert.match(result.stderr, /pipefail/, "the refusal must be the one CI reported");
    assert.doesNotMatch(result.stdout, /reached/, "the step must end at that line");
  });
});
