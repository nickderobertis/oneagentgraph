#!/usr/bin/env bash
# One compiled binary per target feeds every distribution form. No publication.
# The Linux target/profile is the native integration journey for this same driver.
# `--binary` packages an already-linked binary instead of compiling one: how a
# Linux host drives this driver for Darwin, whose binary it cannot link, with a
# Mach-O it can. The packager still refuses one of another format or CPU.
set -euo pipefail
fail() {
  echo "release-build: $1" >&2
  echo 'ACTION: run bash scripts/release-build.sh <aarch64-apple-darwin|x86_64-unknown-linux-gnu> <vX.Y.Z> <absolute-output-directory> [--profile dev | --binary <absolute-path>]' >&2
  exit 2
}
[ "$#" -eq 3 ] || [ "$#" -eq 5 ] || fail 'three arguments are required (optionally followed by --profile dev or --binary <absolute-path>)'
TARGET=$1
TAG=$2
OUT=$3
case "$TARGET" in
  aarch64-apple-darwin|x86_64-unknown-linux-gnu) ;;
  # Discontinued: refused by name so a stale caller cannot build it by mistake.
  x86_64-apple-darwin) fail "Intel macOS is no longer a release target: $TARGET";;
  *) fail "unsupported release target: $TARGET";;
esac
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || fail 'tag must be a release version'
[[ "$OUT" == /* ]] || fail 'output directory must be absolute'
PROFILE=release
BIN_PROFILE=release
PREBUILT=
if [ "$#" -eq 5 ]; then
  case "$4" in
    --profile) [ "$5" = dev ] || fail 'the optional native-journey profile is --profile dev'
      PROFILE=dev
      BIN_PROFILE=debug ;;
    --binary) [[ "$5" == /* && -f "$5" ]] || fail "--binary must name an existing absolute file: $5"
      PREBUILT=$5 ;;
    *) fail "unknown option: $4" ;;
  esac
fi
if [ -n "$PREBUILT" ]; then
  bin=$PREBUILT
else
  rustup target add "$TARGET"
  RUSTFLAGS='-D warnings' cargo build --profile "$PROFILE" --locked --target "$TARGET" --bin oneagentgraph
  bin="${CARGO_TARGET_DIR:-target}/$TARGET/$BIN_PROFILE/oneagentgraph"
fi
python3 scripts/release-package.py --target "$TARGET" --binary "$bin" --tag "$TAG" --out "$OUT"
pkgdir=$(node scripts/npm-build.mjs platform --target "$TARGET" --binary "$bin" --out "$OUT/packages")
(cd "$pkgdir" && npm pack --pack-destination "$OUT")
