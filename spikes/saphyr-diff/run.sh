#!/usr/bin/env bash
# The one command for the serde-yaml-replacement differential spike:
#
#   spikes/saphyr-diff/run.sh [--orchestrator DIR] [--skip-e2e]
#
# 1. runs the e2e suite with SAPHYR_DIFF_CAPTURE set, so every file a journey
#    writes (graphs, bases, personas, filters) is copied out for the harness;
# 2. runs the harness over that capture plus the repository's and the host's
#    real YAML, printing every serde_norway / serde-saphyr difference;
# 3. prints the dependency facts: crate versions, whether either new crate
#    pulls a libyaml-family crate, and `cargo deny` against this repo's deny.toml.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
orchestrator=/home/nick/projects/ai-orchestrator
skip_e2e=
while [ $# -gt 0 ]; do
  case "$1" in
    --orchestrator) orchestrator="$2"; shift 2 ;;
    --skip-e2e) skip_e2e=1; shift ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
done
work="${ONEPIPELINE_NODE_SCRATCH_DIR:-${TMPDIR:-/tmp}}/saphyr-diff.$$"
capture="$work/e2e-capture"
mkdir -p "$capture"
export CARGO_TARGET_DIR="$repo/target/saphyr-diff"

if [ -z "$skip_e2e" ]; then
  echo "# capturing the e2e suite's YAML into $capture" >&2
  # The suite's own verdict is not this spike's: a failing journey still wrote
  # its files, so the capture is kept either way and the count reported.
  (cd "$repo" && SAPHYR_DIFF_CAPTURE="$capture" CARGO_TARGET_DIR="$repo/target" \
    cargo nextest run --locked --all-features --test e2e --no-fail-fast >"$work/e2e.log" 2>&1) \
    && echo "# e2e suite: passed" >&2 \
    || echo "# e2e suite: FAILED (see $work/e2e.log); capture kept" >&2
fi

args=(--orchestrator "$orchestrator")
[ -z "$skip_e2e" ] && args+=(--e2e-capture "$capture")
cargo run --quiet --release --manifest-path "$here/Cargo.toml" -- "${args[@]}"

echo
echo "=============================================================================="
echo "## DEPENDENCIES"
echo
cd "$here"
cargo tree --quiet -e normal -i serde-saphyr --depth 0
cargo tree --quiet -e normal -i saphyr --depth 0
cargo tree --quiet -e normal -i serde_norway --depth 0
echo
echo "what serde-saphyr and saphyr bring (normal edges):"
cargo tree --quiet -e normal -p serde-saphyr --prefix none | sort -u
cargo tree --quiet -e normal -p saphyr --prefix none | sort -u
echo
for banned in serde_yaml serde_yaml_ng serde_norway serde_yml unsafe-libyaml unsafe-libyaml-norway libyml; do
  for root in serde-saphyr saphyr; do
    if cargo tree --quiet -e normal,build -p "$root" --prefix none | grep -q "^$banned v"; then
      echo "BANNED-FAMILY: $root pulls $banned"
    else
      echo "ok: $root does not pull $banned"
    fi
  done
done
echo
echo "who still pulls a libyaml-family crate in this tree (normal edges):"
for banned in serde_yaml_ng serde_norway unsafe-libyaml unsafe-libyaml-norway; do
  cargo tree --quiet -e normal -i "$banned" 2>/dev/null | sed 's/^/  /' || true
done
echo
echo "cargo deny (this repository's deny.toml) over the harness's tree:"
cargo deny --manifest-path "$here/Cargo.toml" check --config "$repo/deny.toml" advisories bans licenses sources 2>&1 | tail -40 || true
