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
# What a published artifact is held to here is what it can prove *alone*: it
# reports its version, prints the documented command list, refuses a graph it
# cannot read with the contract's exit 2, and never reports a graph it did not
# run as settled. Running one is deliberately out of scope — that needs the
# `onejudge` and `oneharness` CLIs and a paid harness, which this script exists
# to stay free of. The e2e suite drives those for real; this proves the artifact
# that ships is the one the suite tested.
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

# One scratch file for a probe's stderr, so a failure report can carry the
# binary's own diagnostic rather than only the assertion that tripped.
probe_stderr="$(mktemp)"
trap 'rm -f "$probe_stderr"' EXIT

command -v oneagentgraph >/dev/null 2>&1 || fail "no 'oneagentgraph' on PATH" \
  "install it first — 'pip install oneagentgraph-cli' or 'npm install -g oneagentgraph-cli'"

# Windows ships the same bytes with CRLF once anything touches them, so strip CR
# rather than let a line ending decide the verdict.
#
# Each probe carries its own `|| fail`: under `set -e` an install that cannot run
# at all would otherwise end the script on the binary's exit status, with no
# cause and no next action — the report a broken artifact most needs.
reported="$(oneagentgraph --version 2>"$probe_stderr" | tr -d '\r')" || fail \
  "'--version' failed: $(cat "$probe_stderr")" \
  "the installed binary cannot run at all — reinstall it, and check the platform package matches this machine"
if [ -n "$expect_version" ] && [ "$reported" != "oneagentgraph $expect_version" ]; then
  fail "reports '$reported', not 'oneagentgraph $expect_version'" \
    "the install resolved a different version than the one just published"
fi

help="$(oneagentgraph --help 2>"$probe_stderr" | tr -d '\r')" || fail \
  "'--help' failed: $(cat "$probe_stderr")" \
  "the installed binary runs but cannot print its own surface — reinstall this version and re-run"
for command in run validate trigger reset-timer cancel history health smoke persona; do
  case "$help" in
    *"$command"*) ;;
    *) fail "'--help' does not list the '$command' command" \
         "the installed binary does not carry the documented command surface" ;;
  esac
done

# A graph that is not there is the contract's exit 2, and nothing on stdout: a
# caller reads a line on stdout as an event, so a refusal must not produce one.
code=0
why="$(oneagentgraph run no-such-graph.yaml 2>"$probe_stderr")" || code=$?
out="$why"
why="$(cat "$probe_stderr")"
if [ "$code" -ne 2 ]; then
  fail "'run' on a missing graph exited $code, not 2: $why" \
    "reinstall this version and re-run; if it still does, the published artifact is not the revision CI gated — re-cut the release"
fi
if [ -n "$out" ]; then
  fail "'run' wrote to stdout while refusing a missing graph" \
    "reinstall this version and re-run; if it still does, revert the change that made a refusal write to stdout"
fi

# `validate` is the one verb that needs nothing else installed, so it is what
# proves the artifact can actually read a graph rather than only parse argv.
work="$(mktemp -d)"
trap 'rm -rf "$work" "$probe_stderr"' EXIT
printf 'version: 1\nname: smoke\nmembers:\n  a:\n    kind: oneharness\n    oneharness_config: ./h.toml\n' > "$work/graph.yaml"
printf 'run_mode = "fallback"\nharnesses = ["claude-code"]\n' > "$work/h.toml"
if ! why="$(oneagentgraph validate "$work/graph.yaml" 2>&1 >/dev/null)"; then
  fail "'validate' refused a graph it should read: $why" \
    "fix what that refusal names, or reinstall — an install that cannot read a graph is truncated or from the wrong revision"
fi

code=0
why="$(oneagentgraph validate "$work/nowhere.yaml" 2>&1 >/dev/null)" || code=$?
if [ "$code" -ne 2 ]; then
  fail "'validate' on a missing graph exited $code, not 2: $why" \
    "reinstall this version and re-run 'oneagentgraph validate <a missing path>'; a code other than 2 means this artifact predates the contract's exit codes"
fi

echo "$label: surface smoke test passed${expect_version:+ for $expect_version}"
