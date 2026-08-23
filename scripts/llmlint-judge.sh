#!/usr/bin/env bash
# Body of the cached Nx `oneagentgraph:lint-llm-diff` target: judge the branch diff
# against one resolved base commit. Run it through `just lint-llm-diff <base>`,
# which resolves the base ref to the commit this reads and keys the cache on.
#
# Nothing here records or replays a verdict. llmlint runs and its exit status is
# this task's exit status, so Nx caches a clean run and restores what it wrote,
# while a run with findings and a run that never reached a verdict both stay
# uncached and re-judge.
#
# Everything this task says goes to `.logs/llmlint-diff.report`, which the target
# declares as its Nx output — deliberately, rather than to the terminal Nx would
# forward. Nx's `run-commands` resolves a task on the child's `exit` event, which
# fires before a pipe it wrote to just beforehand has drained; a report finished
# milliseconds before exit is therefore lost from both the live stream and the
# cached record, intermittently. A declared output file has neither problem: Nx
# stores it on a clean run and restores it on a hit, byte for byte. It is also the
# thing to `tail -f` while a two-minute judge is still thinking, since the recipe
# prints it once the task is done.
#
# The base arrives as `LLMLINT_DIFF_BASE_SHA` rather than an argument because Nx
# hashes declared environment variables but not target arguments: keying and
# judging on the same value is what stops a clean verdict computed against one base
# from being replayed for another.
#
# llmlint: ignore-file[changed_behavior_has_e2e] Every journey this script has — a
# judged and a replayed clean run, findings and a broken toolchain re-judging, each
# invalidation case, and a refused base — runs end to end in
# tests/llmlint_cache.rs. What remains are host-failure guards on the checkout
# layout; simulating a broken filesystem is the guard's job, not a journey's.
set -euo pipefail

root="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)" || {
  echo "lint-llm-diff: could not locate the repository from this script; reinstall the checkout and retry" >&2
  exit 1
}
# shellcheck source=scripts/llmlint-runtime-env.sh
. "$root/scripts/llmlint-runtime-env.sh" || {
  echo "lint-llm-diff: could not load the pinned runtime environment; restore scripts/llmlint-runtime-env.sh and retry" >&2
  exit 1
}
mkdir -p "$root/.logs" && chmod 700 "$root/.logs" || {
  echo "lint-llm-diff: could not open '$root/.logs' for the judge report; repair its permissions and retry" >&2
  exit 1
}
report="$root/.logs/llmlint-diff.report"
: >"$report" || {
  echo "lint-llm-diff: could not write '$report'; free disk space and retry" >&2
  exit 1
}
# From here on this task speaks only through the report, so everything it says —
# a refusal below as much as the judge's findings — is carried by the file Nx
# caches rather than by a stream Nx may resolve the task ahead of.
exec >>"$report" 2>&1

base_sha="${LLMLINT_DIFF_BASE_SHA:-}"
[[ "$base_sha" =~ ^[0-9a-f]{40,64}$ ]] || {
  echo "lint-llm-diff: LLMLINT_DIFF_BASE_SHA must be a resolved commit id; run 'just lint-llm-diff <base>' instead of this target directly"
  exit 1
}
git -C "$root" rev-parse --verify --quiet "${base_sha}^{commit}" >/dev/null || {
  echo "lint-llm-diff: base commit '$base_sha' is missing from this checkout; fetch it and retry"
  exit 1
}

llmlint_runtime_env
cd "$root" || {
  echo "lint-llm-diff: could not enter '$root'; repair its permissions and retry"
  exit 1
}
# The judge's own report and exit status are the task's: Nx caches a task only when
# it succeeds, which is exactly the record-keeping this tier delegates to it.
# llmlint: ignore[tool_output_is_signal] The judge's per-rule report is this tier's product, and the recipe prints this file in place of a verdict record — a wrapper line around it would bury the finding an operator has to clear.
exec llmlint --diff --diff-base "$base_sha"
