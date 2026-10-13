#!/usr/bin/env bash
# One compiled binary per target feeds every distribution form. No publication.
set -euo pipefail
fail() {
  echo "release-darwin: $1" >&2
  echo 'ACTION: run bash scripts/release-darwin.sh <x86_64-apple-darwin|aarch64-apple-darwin> <vX.Y.Z> <absolute-output-directory>' >&2
  exit 2
}
[ "$#" -eq 3 ] || fail 'three arguments are required'
TARGET=$1
TAG=$2
OUT=$3
case "$TARGET" in x86_64-apple-darwin|aarch64-apple-darwin) ;; *) fail "unsupported Darwin target: $TARGET";; esac
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || fail 'tag must be a release version'
[[ "$OUT" == /* ]] || fail 'output directory must be absolute'
rustup target add "$TARGET"
RUSTFLAGS='-D warnings' cargo build --release --locked --target "$TARGET" --bin oneagentgraph
bin="target/$TARGET/release/oneagentgraph"
python3 scripts/release-package.py --target "$TARGET" --binary "$bin" --tag "$TAG" --out "$OUT"
pkgdir=$(node scripts/npm-build.mjs platform --target "$TARGET" --binary "$bin" --out "$OUT/packages")
(cd "$pkgdir" && npm pack --pack-destination "$OUT")
