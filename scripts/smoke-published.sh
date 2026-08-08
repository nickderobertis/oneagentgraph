#!/usr/bin/env bash
# Smoke-test an `oneagentgraph` that is already on PATH, and name the install
# that broke when it does not behave.
#
# One script, one set of assertions. `release.yml`'s verify jobs and
# `published-smoke.yml` run this over a binary they installed from PyPI or npm;
# CI's `install` job runs the identical file over the binary this repo just
# compiled. That is what stops a workflow's idea of "it works" from drifting from
# what actually ships — assertions inlined in a workflow keep passing after the
# surface around them changes.
#
# Deliberately toolchain-free: bash and the installed binary. The scheduled sweep
# runs this every week on every OS, for both registries, and anything it had to
# install first would be a second thing that can rot.
#
# While the crate is interface-only, the promise a published artifact makes is
# its *surface*: it reports its version, prints the documented command list, and
# refuses to pretend it ran a graph. Extend the assertions here as behavior
# lands — this file is what a release proves.
set -euo pipefail

expect_version=""
label="installed oneagentgraph"

fail() {
  echo "::error::$label: $1" >&2
  echo "ACTION: $2" >&2
  exit 1
}

# Every option takes a value, so a missing one is an argument error rather than
# a silently empty setting.
need_value() {
  if [ "$#" -lt 2 ]; then
    echo "$1 needs a value" >&2
    echo "ACTION: pass it as '$1 <value>'" >&2
    exit 2
  fi
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --expect-version) need_value "$@"; expect_version="$2"; shift 2 ;;
    # What installed the binary, so a red matrix leg names the platform and the
    # registry rather than only the assertion that failed.
    --label) need_value "$@"; label="$2"; shift 2 ;;
    *)
      echo "unknown option $1" >&2
      echo "ACTION: run 'smoke-published.sh [--expect-version V] [--label TEXT]'" >&2
      exit 2
      ;;
  esac
done

command -v oneagentgraph >/dev/null 2>&1 || fail "no 'oneagentgraph' on PATH" \
  "install it first — 'pip install oneagentgraph-cli' or 'npm install -g oneagentgraph-cli'"

# Windows ships the same bytes with CRLF once anything touches them, so strip CR
# rather than let a line ending decide the verdict.
reported="$(oneagentgraph --version | tr -d '\r')"
if [ -n "$expect_version" ] && [ "$reported" != "oneagentgraph $expect_version" ]; then
  fail "reports '$reported', not 'oneagentgraph $expect_version'" \
    "the install resolved a different version than the one just published"
fi

help="$(oneagentgraph --help | tr -d '\r')"
for command in run validate trigger reset-timer cancel history health smoke persona; do
  case "$help" in
    *"$command"*) ;;
    *) fail "'--help' does not list the '$command' command" \
         "the installed binary does not carry the documented command surface" ;;
  esac
done

# The refusal is part of the shipped contract while the crate is interface-only:
# a build that silently succeeded here would report an unimplemented run as a
# graph that settled.
code=0
oneagentgraph run graph.yaml >/dev/null 2>&1 || code=$?
if [ "$code" -eq 0 ]; then
  fail "'run' exited 0 without running anything" \
    "an interface-only build must refuse; a caller reads exit 0 as a settled graph"
fi

echo "$label: surface smoke test passed${expect_version:+ for $expect_version}"
