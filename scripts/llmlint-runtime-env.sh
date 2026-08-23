#!/usr/bin/env bash
# One source for the environment this repository's llmlint tier judges under.
#
# Sourced by both ends of the cached tier — scripts/llmlint-judge.sh, which judges,
# and scripts/llmlint-fingerprint.sh, which keys the cache on the judge
# configuration. That sharing is the point: `llmlint config` renders the resolved
# oneharness binary into its output, so a fingerprint that read the caller's value
# instead would hash one judged diff to a different key per caller, and the
# non-deterministic judge would re-roll every round.
#
# `LLMLINT_ONEHARNESS_BIN` is cleared rather than set: `scripts/setup-llmlint.sh`
# installs llmlint and oneharness into one `uv tool` environment, where llmlint
# finds `oneharness` beside its own executable. This repository therefore pins no
# oneharness path, and an inherited one names a binary the judge here would never
# have used — a sibling checkout's wrapper reached this tier that way.
#
# The install directory that same script writes to is prepended to `PATH`, so both
# ends resolve the llmlint this repository provisions rather than whichever one a
# caller happens to have in front of it. Prepending rather than replacing: the rest
# of `PATH` still has to carry git, node, and the harness llmlint spawns.
#
# llmlint: ignore-file[boundary_inputs_validated] Nothing here crosses a trust
# boundary — the function takes no input. It reads `HOME` to name the directory
# `scripts/setup-llmlint.sh` installs into, and deliberately keeps the inherited
# `PATH` rather than narrowing it: llmlint lives outside the checkout, so a narrower
# `PATH` would leave the judge and the fingerprint resolving different binaries,
# which is the split key this helper exists to prevent. The function is these two
# lines, so this is file-scoped only because there is no smaller scope to name.
set -euo pipefail

llmlint_runtime_env() {
  unset LLMLINT_ONEHARNESS_BIN
  export PATH="${HOME:-}/.local/bin:$PATH"
}
