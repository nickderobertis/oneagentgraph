#!/usr/bin/env bash
# The `oneagentgraph-pool-maintenance-seconds` measurement: one steady-state pass
# of the declared pool maintenance (`just pool-maintenance`), timed and reported
# by `cargo-prune-superseded` itself through the onebudgetspec SDK.
#
# Steady state is the pass a regular post-run maintenance faces — one ordinary
# run's worth of superseded builds, never a first pass over accumulated history —
# so the state is established before anything is timed:
#
#   (a) the declared maintenance runs once, untimed, to clear any backlog;
#   (b) this repository's test targets are built the way its gate builds them,
#       but without `--all-features`, which gives its own crate new metadata
#       hashes; every entry that build created is backdated past the 2-day window
#       to stand in for a run's builds that have since been superseded;
#   (c) the ordinary gate build runs, so each unit's newest build is its own;
#   (d) one declared pass is timed: it removes (b)'s builds.
#
# Only (d) reports the figure. (a)'s seconds go into the detail as separate
# evidence, and the run fails if (d) removed anything (b) did not create, since
# that would make the figure something other than the pass it claims to be.
#
# It really prunes this checkout and builds it twice: run it in your own
# worktree. Its listings stay in `.logs/pool-maintenance/`.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# `cargo test` writes the `test` profile into the `dev` profile's directory, and
# `just pool-maintenance` prunes the target dir that holds it (`.cargo/config.toml`).
PROFILE_DIR=target/debug
LOGS=.logs/pool-maintenance
# Older than any build the ordinary configuration can have, so none of (b)'s
# builds is ever a unit's newest and the pruner keeps the ordinary one instead.
SUPERSEDED_AT="2000-01-01T00:00:00Z"
export CARGO_TERM_QUIET=true

mkdir -p "$LOGS"

# Every build-named entry of the profile, one `<container>/<name>` per line: the
# names carry the unit and its metadata hash (or incremental session id), which
# is how the pruner groups them.
entries() {
  local container
  for container in deps .fingerprint build incremental; do
    [ -d "$PROFILE_DIR/$container" ] || continue
    find "$PROFILE_DIR/$container" -mindepth 1 -maxdepth 1 -printf "$container/%P\n"
  done | LC_ALL=C sort
}

# One `just pool-maintenance` pass, reporting through the result file it is given.
maintain() {
  ONEBUDGETSPEC_RESULT="$1" just pool-maintenance >&2
}

result_field() {
  node -e 'const r=JSON.parse(require("node:fs").readFileSync(process.argv[1],"utf8"));process.stdout.write(String(r[process.argv[2]]??""))' "$1" "$2"
}

# The gate's uninstrumented build of every test target, as `_uninstrumented` builds it.
gate_build() {
  cargo test --no-run --locked --lib --bins --tests "$@" >&2
}

echo "pool-maintenance: (a) clearing any backlog, untimed" >&2
: >"$LOGS/backlog.json"
maintain "$LOGS/backlog.json"
backlog_seconds="$(result_field "$LOGS/backlog.json" value)"

echo "pool-maintenance: (b) building the alternative configuration (no features)" >&2
# Its own crate's earlier `dev` builds go first, so this build creates every one of
# its units rather than finding one already fresh from an earlier measurement.
cargo clean --locked --quiet --profile dev --package oneagentgraph
entries >"$LOGS/before-alternative.txt"
gate_build
entries >"$LOGS/after-alternative.txt"
LC_ALL=C comm -13 "$LOGS/before-alternative.txt" "$LOGS/after-alternative.txt" >"$LOGS/alternative.txt"
[ -s "$LOGS/alternative.txt" ] || {
  echo "pool-maintenance: the alternative build created no entries in $PROFILE_DIR — nothing would be superseded" >&2
  exit 1
}
while IFS= read -r entry; do
  find "$PROFILE_DIR/$entry" -exec touch -h -d "$SUPERSEDED_AT" {} +
done <"$LOGS/alternative.txt"

echo "pool-maintenance: (c) the ordinary gate build" >&2
gate_build --all-features
entries >"$LOGS/before-timed.txt"
LC_ALL=C comm -23 "$LOGS/before-timed.txt" "$LOGS/alternative.txt" >"$LOGS/others.txt"

echo "pool-maintenance: (d) the timed pass" >&2
timed="${ONEBUDGETSPEC_RESULT:-$LOGS/timed.json}"
maintain "$timed"
entries >"$LOGS/after-timed.txt"
LC_ALL=C comm -23 "$LOGS/before-timed.txt" "$LOGS/after-timed.txt" >"$LOGS/removed.txt"
LC_ALL=C comm -23 "$LOGS/removed.txt" "$LOGS/alternative.txt" >"$LOGS/removed-elsewhere.txt"
LC_ALL=C comm -12 "$LOGS/others.txt" "$LOGS/after-timed.txt" >"$LOGS/others-after.txt"

echo "pool-maintenance: alternative-build entries: $(wc -l <"$LOGS/alternative.txt"); removed by the timed pass: $(wc -l <"$LOGS/removed.txt"); every other entry, the ordinary build's among them, before: $(wc -l <"$LOGS/others.txt"), after: $(wc -l <"$LOGS/others-after.txt") (listings in $LOGS/)" >&2
if [ -s "$LOGS/removed-elsewhere.txt" ]; then
  echo "pool-maintenance: the timed pass removed entries the alternative build did not create:" >&2
  cat "$LOGS/removed-elsewhere.txt" >&2
  exit 1
fi

# The pruner's own report stays the figure; the backlog pass is evidence beside it.
node -e '
const fs = require("node:fs");
const [file, backlog] = process.argv.slice(1);
const result = JSON.parse(fs.readFileSync(file, "utf8"));
result.detail += "; untimed backlog pass seconds (not counted): " + backlog;
fs.writeFileSync(file, JSON.stringify(result));
' "$timed" "$backlog_seconds"
echo "pool-maintenance: timed pass: $(result_field "$timed" value) s; $(result_field "$timed" detail)" >&2
