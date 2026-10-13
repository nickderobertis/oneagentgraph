#!/usr/bin/env bash
# Install staged products on the Apple Silicon release host, before publication.
# Linux drives this same installer in the native packaging integration journey.
set -euo pipefail
fail() {
  echo "release-smoke: $1" >&2
  echo 'ACTION: run bash scripts/release-smoke.sh <x86_64-apple-darwin|aarch64-apple-darwin|x86_64-unknown-linux-gnu> <absolute-staged-artifact-directory> <X.Y.Z>' >&2
  exit 2
}
[ "$#" -eq 3 ] || fail 'three arguments are required'
TARGET=$1
OUT=$2
VERSION=$3
[[ "$OUT" == /* && -d "$OUT" ]] || fail 'staged artifact directory must exist and be absolute'
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || fail 'version must have release-version syntax'
case "$TARGET" in
  x86_64-apple-darwin) cpu=x86_64; npm_cpu=x64; npm_os=darwin; wheel_platform=macosx_10_12_x86_64 ;;
  aarch64-apple-darwin) cpu=arm64; npm_cpu=arm64; npm_os=darwin; wheel_platform=macosx_11_0_arm64 ;;
  x86_64-unknown-linux-gnu) cpu=x86_64; npm_cpu=x64; npm_os=linux; wheel_platform=linux_x86_64 ;;
  *) fail "unsupported release target: $TARGET" ;;
esac
archive="$OUT/oneagentgraph-v$VERSION-$TARGET.tar.gz"
carrier="$OUT/oneagentgraph-cli-$npm_os-$npm_cpu-$VERSION.tgz"
[ -f "$archive" ] || fail "missing staged archive: $archive"
[ -f "$carrier" ] || fail "missing staged npm carrier: $carrier"
shopt -s nullglob
wheels=("$OUT"/oneagentgraph_cli-*-py3-none-"$wheel_platform".whl)
[ "${#wheels[@]}" -eq 1 ] && [ -f "${wheels[0]}" ] || fail "stage exactly one $wheel_platform CLI wheel"
architecture_run() {
  if [ "$npm_os" = darwin ]; then /usr/bin/arch "-$cpu" "$@"; else "$@"; fi
}
work=$(mktemp -d "${ONEPIPELINE_NODE_SCRATCH_DIR:-${TMPDIR:-/tmp}}/release-smoke.XXXXXXXX")
trap 'rm -rf "$work"' EXIT
tar -xzf "$archive" -C "$work"
PATH="$work:$PATH" architecture_run /bin/bash scripts/smoke-published.sh --expect-version "$VERSION" --label "staged archive $TARGET"
# These wheels carry executables, not Python extensions. Native pip selects
# the declared target platform and installs its scripts; Rosetta runs Intel's
# installed executable. No Intel Python distribution or host libraries needed.
python3 -m venv "$work/venv"
"$work/venv/bin/python" -m pip install --no-index --only-binary=:all: --platform "$wheel_platform" --target "$work/python" "${wheels[0]}"
PATH="$work/python/bin:$PATH" architecture_run /bin/bash scripts/smoke-published.sh --expect-version "$VERSION" --label "staged wheel $TARGET"
architecture_run npm install --prefix "$work/npm" --offline --os="$npm_os" --cpu="$npm_cpu" "$carrier"
# Pack and install the launcher the way a user installs the registry tarball.
launcher=$(node scripts/npm-build.mjs launcher --out "$work/launcher")
(cd "$launcher" && npm pack --pack-destination "$work")
architecture_run npm install --prefix "$work/npm" --offline --omit=optional --os="$npm_os" --cpu="$npm_cpu" "$work/oneagentgraph-cli-$VERSION.tgz"
PATH="$work/npm/node_modules/.bin:$PATH" architecture_run /bin/bash scripts/smoke-published.sh --expect-version "$VERSION" --label "staged npm $TARGET"
