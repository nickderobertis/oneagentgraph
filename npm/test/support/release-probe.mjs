// How every tier drives `scripts/release-probe.sh`: as the contract says a
// consumer drives it, and never any other way.
//
// Shared by the offline tier (`npm/test/release-probe.test.mjs`, the
// not-answered answer) and the live one (`npm/test/live/release-probe.test.mjs`,
// the two a registry has to serve). One spawn helper rather than two, so the two
// tiers cannot end up proving different things about the same script — and so
// the environment restriction below is asserted by every case in both.

import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const PROBE = join(REPO_ROOT, "scripts", "release-probe.sh");

/// Run the real probe with the environment the contract allows it, and nothing
/// else: a direct subprocess with no shell interposed, from the repository root,
/// carrying only a search path and a home directory. That the probe needs no
/// credential is not asserted anywhere — it is simply never given one.
///
/// On Windows the shebang is not honoured, so bash is named as the interpreter.
/// Still an argv, never a command line for a shell to re-parse.
export function probe(...args) {
  const windows = process.platform === "win32";
  const env = { PATH: process.env.PATH };
  if (windows) {
    // The home directory under the name this OS gives it, plus the one variable
    // curl on Windows cannot open its TLS backend without.
    env.USERPROFILE = process.env.USERPROFILE;
    env.SystemRoot = process.env.SystemRoot;
  } else {
    env.HOME = process.env.HOME;
  }
  const started = Date.now();
  const result = spawnSync(windows ? "bash" : PROBE, windows ? [PROBE, ...args] : args, {
    cwd: REPO_ROOT,
    env,
    encoding: "utf8",
    // Generously past the sixty seconds the contract allows, so a probe that
    // overran is reported as an overrun rather than as a killed process.
    timeout: 90_000,
  });
  assert.equal(result.error, undefined, `the probe could not be spawned: ${result.error}`);
  return { ...result, elapsedMs: Date.now() - started };
}

/// The third answer: a non-zero exit, nothing on stdout — a caller reads stdout
/// as the answer, so one byte there would be read as a version — and a reason
/// with a next action on stderr.
export function assertNotAnswered(result, because) {
  assert.notEqual(result.status, 0, `${because}: expected a non-zero exit, got 0`);
  assert.equal(result.stdout, "", `${because}: wrote to stdout while not answering`);
  assert.match(result.stderr, /ACTION:/, `${because}: gave no next action on stderr`);
}
