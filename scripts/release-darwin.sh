#!/usr/bin/env bash
# One compiled binary per target feeds every distribution form. No publication.
# The Linux target/profile is the native integration journey for this same driver.
set -euo pipefail
fail() {
  echo "release-darwin: $1" >&2
  echo 'ACTION: run bash scripts/release-darwin.sh <x86_64-apple-darwin|aarch64-apple-darwin> <vX.Y.Z> <absolute-output-directory>' >&2
  exit 2
}
[ "$#" -eq 3 ] || [ "$#" -eq 5 ] || fail 'three arguments are required (or append --profile dev for a native packaging journey)'
TARGET=$1
TAG=$2
OUT=$3
case "$TARGET" in x86_64-apple-darwin|aarch64-apple-darwin|x86_64-unknown-linux-gnu) ;; *) fail "unsupported Darwin target: $TARGET";; esac
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || fail 'tag must be a release version'
[[ "$OUT" == /* ]] || fail 'output directory must be absolute'
PROFILE=release
BIN_PROFILE=release
if [ "$#" -eq 5 ]; then
  [ "$4" = --profile ] && [ "$5" = dev ] || fail 'the optional native-journey profile is --profile dev'
  PROFILE=dev
  BIN_PROFILE=debug
fi
rustup target add "$TARGET"
RUSTFLAGS='-D warnings' cargo build --profile "$PROFILE" --locked --target "$TARGET" --bin oneagentgraph
bin="${CARGO_TARGET_DIR:-target}/$TARGET/$BIN_PROFILE/oneagentgraph"
python3 scripts/release-package.py --target "$TARGET" --binary "$bin" --tag "$TAG" --out "$OUT"
pkgdir=$(node scripts/npm-build.mjs platform --target "$TARGET" --binary "$bin" --out "$OUT/packages")
(cd "$pkgdir" && npm pack --pack-destination "$OUT")
