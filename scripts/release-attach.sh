#!/usr/bin/env bash
# Attach a staged directory's archives and their checksums to a GitHub Release.
# release.yml's upload-darwin job runs this; scripts/release-rehearse.sh runs it
# with `gh` doubled, so what the rehearsal sees attached is what this sends.
set -euo pipefail
fail() {
  echo "release-attach: $1" >&2
  echo 'ACTION: run bash scripts/release-attach.sh <vX.Y.Z> <staged-artifact-directory>' >&2
  exit 2
}
[ "$#" -eq 2 ] || fail 'two arguments are required'
[ -d "$2" ] || fail "staged artifact directory does not exist: $2"
shopt -s nullglob
files=("$2"/*.tar.gz "$2"/*.sha256)
[ "${#files[@]}" -gt 0 ] || fail "no archive or checksum is staged in $2"
gh release upload "$1" "${files[@]}" --clobber
