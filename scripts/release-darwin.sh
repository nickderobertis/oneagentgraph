#!/usr/bin/env bash
# One compiled binary per target feeds every distribution form. No publication.
set -euo pipefail
TARGET=${1:?Rust target required}
TAG=${2:?release tag required}
OUT=${3:?artifact directory required}
case "$TARGET" in x86_64-apple-darwin|aarch64-apple-darwin) ;; *) echo "unsupported Darwin target: $TARGET" >&2; exit 1;; esac
rustup target add "$TARGET"
RUSTFLAGS='-D warnings' cargo build --release --locked --target "$TARGET" --bin oneagentgraph
bin="target/$TARGET/release/oneagentgraph"
python3 scripts/release-package.py --target "$TARGET" --binary "$bin" --tag "$TAG" --out "$OUT"
pkgdir=$(node scripts/npm-build.mjs platform --target "$TARGET" --binary "$bin" --out "$OUT/packages")
(cd "$pkgdir" && npm pack --pack-destination "$OUT")
