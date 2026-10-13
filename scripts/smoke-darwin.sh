#!/usr/bin/env bash
# Install staged products on the Apple Silicon release host, before publication.
set -euo pipefail
TARGET=${1:?target required}
OUT=${2:?absolute artifact directory required}
VERSION=${3:?version required}
case "$TARGET" in
  x86_64-apple-darwin) cpu=x86_64; npm_cpu=x64; wheel_platform=macosx_10_12_x86_64 ;;
  aarch64-apple-darwin) cpu=arm64; npm_cpu=arm64; wheel_platform=macosx_11_0_arm64 ;;
  *) echo "unsupported Darwin target: $TARGET" >&2; exit 1 ;;
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
# Drive the real committed launcher with its staged platform carrier.
launcher=$(node scripts/npm-build.mjs launcher --out "$work/launcher")
mkdir -p "$launcher/node_modules"
cp -R "$work/npm/node_modules/oneagentgraph-cli-darwin-$npm_cpu" "$launcher/node_modules/"
PATH="$launcher/bin:$PATH" /usr/bin/arch "-$cpu" node "$launcher/bin/oneagentgraph.js" --version
# The launcher command is an npm bin symlink, as on a normal install.
mkdir -p "$work/bin"
ln -s "$launcher/bin/oneagentgraph.js" "$work/bin/oneagentgraph"
PATH="$work/bin:$PATH" /usr/bin/arch "-$cpu" /bin/bash scripts/smoke-published.sh --expect-version "$VERSION" --label "Darwin npm $TARGET"
