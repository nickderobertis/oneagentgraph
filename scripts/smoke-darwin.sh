#!/usr/bin/env bash
# Install staged products on the Apple Silicon release host, before publication.
set -euo pipefail
fail() {
  echo "smoke-darwin: $1" >&2
  echo 'ACTION: run bash scripts/smoke-darwin.sh <x86_64-apple-darwin|aarch64-apple-darwin> <absolute-staged-artifact-directory> <X.Y.Z>' >&2
  exit 2
}
[ "$#" -eq 3 ] || fail 'three arguments are required'
TARGET=$1
OUT=$2
VERSION=$3
[[ "$OUT" == /* && -d "$OUT" ]] || fail 'staged artifact directory must exist and be absolute'
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || fail 'version must have release-version syntax'
case "$TARGET" in
  x86_64-apple-darwin) cpu=x86_64; npm_cpu=x64; wheel_platform=macosx_10_12_x86_64 ;;
  aarch64-apple-darwin) cpu=arm64; npm_cpu=arm64; wheel_platform=macosx_11_0_arm64 ;;
  *) fail "unsupported Darwin target: $TARGET" ;;
esac
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
tar -xzf "$OUT/oneagentgraph-v$VERSION-$TARGET.tar.gz" -C "$work"
PATH="$work:$PATH" /usr/bin/arch "-$cpu" /bin/bash scripts/smoke-published.sh --expect-version "$VERSION" --label "Darwin archive $TARGET"
# These wheels carry executables, not Python extensions. Native pip selects
# the declared target platform and installs its scripts; Rosetta runs Intel's
# installed executable. No Intel Python distribution or host libraries needed.
python3 -m venv "$work/venv"
"$work/venv/bin/python" -m pip install --no-index --only-binary=:all: --platform "$wheel_platform" --target "$work/python" "$OUT"/*"_$cpu.whl"
PATH="$work/python/bin:$PATH" /usr/bin/arch "-$cpu" /bin/bash scripts/smoke-published.sh --expect-version "$VERSION" --label "Darwin wheel $TARGET"
/usr/bin/arch "-$cpu" node --version
/usr/bin/arch "-$cpu" npm install --prefix "$work/npm" --offline --os=darwin --cpu="$npm_cpu" "$OUT/oneagentgraph-cli-darwin-$npm_cpu-$VERSION.tgz"
# Pack and install the launcher the way a user installs the registry tarball.
launcher=$(node scripts/npm-build.mjs launcher --out "$work/launcher")
(cd "$launcher" && npm pack --pack-destination "$work")
/usr/bin/arch "-$cpu" npm install --prefix "$work/npm" --offline --omit=optional --os=darwin --cpu="$npm_cpu" "$work/oneagentgraph-cli-$VERSION.tgz"
PATH="$work/npm/node_modules/.bin:$PATH" /usr/bin/arch "-$cpu" /bin/bash scripts/smoke-published.sh --expect-version "$VERSION" --label "Darwin npm $TARGET"
