#!/usr/bin/env bash
# Rehearse one staged target's publication before anything leaves the runner.
#
#   1. scripts/release-verify.py: exactly the target's artifacts, each payload of
#      the target's own format and CPU, the wheel at the release version.
#   2. On macOS, the host's `lipo` reads the archived binary's architecture too.
#   3. scripts/release-attach.sh and scripts/publish-npm.sh run for real with only
#      their transport doubled (`gh`, and `npm view`/`npm publish`), and must send
#      exactly the archive, its checksum and the one npm carrier.
#   4. An Intel artifact, a wrong-CPU payload, and the Intel target are refused.
#
# release.yml's Darwin job runs this natively before uploading; tests/packaging.rs
# runs it over the Linux set inside `just check`. It never publishes.
set -euo pipefail
fail() {
  echo "release-rehearse: $1" >&2
  echo 'ACTION: run bash scripts/release-rehearse.sh <aarch64-apple-darwin|x86_64-unknown-linux-gnu> <absolute-staged-artifact-directory> <vX.Y.Z>' >&2
  exit 2
}
[ "$#" -eq 3 ] || fail 'three arguments are required'
TARGET=$1
DIR=$2
TAG=$3
[[ "$DIR" == /* && -d "$DIR" ]] || fail 'staged artifact directory must exist and be absolute'
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || fail 'tag must be a release version'
# The patch turns the binary into the other CPU of its own format.
case "$TARGET" in
  aarch64-apple-darwin) cpu=arm64; wrong=mach-o/x86_64 ;;
  x86_64-unknown-linux-gnu) cpu=x86_64; wrong=elf/aarch64 ;;
  x86_64-apple-darwin) fail "Intel macOS is no longer a release target: $TARGET" ;;
  *) fail "unsupported release target: $TARGET" ;;
esac
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
work=$(mktemp -d "${ONEPIPELINE_NODE_SCRATCH_DIR:-${TMPDIR:-/tmp}}/release-rehearse.XXXXXXXX")
trap 'rm -rf "$work"' EXIT
archive="oneagentgraph-$TAG-$TARGET.tar.gz"
intel="oneagentgraph-$TAG-x86_64-apple-darwin.tar.gz"
verify() { python3 "$root/scripts/release-verify.py" --target "$1" --tag "$TAG" --dir "$2"; }
# Run a command that must be refused at its boundary (exit 2) saying `reason`.
must_refuse() {
  local reason=$1 status=0
  shift
  "$@" >/dev/null 2>"$work/refusal" || status=$?
  [ "$status" -eq 2 ] && grep -qF -- "$reason" "$work/refusal" ||
    fail "$* was not refused with '$reason' (exit $status): $(cat "$work/refusal")"
}
copy_staged() { mkdir "$1" && find "$DIR" -maxdepth 1 -type f -exec cp {} "$1/" \; ; }

verified=$(verify "$TARGET" "$DIR")

mkdir "$work/unpacked"
tar -xzf "$DIR/$archive" -C "$work/unpacked"
if [ "$(uname -s)" = Darwin ]; then
  archs=$(lipo -archs "$work/unpacked/oneagentgraph")
  [ "$archs" = "$cpu" ] || fail "lipo reads '$archs' from $archive, not $cpu"
fi

mkdir "$work/bin"
real_npm=$(command -v npm) || fail 'npm is required to rehearse the registry upload'
printf '#!/bin/sh\nprintf "%%s\\n" "$@" >> "$RECORD"\n' >"$work/bin/gh"
cat >"$work/bin/npm" <<DOUBLE
#!/bin/sh
case "\$1" in
  view) echo 'npm error code E404' >&2; exit 1 ;;
  publish) printf 'npm publish %s\n' "\$2" >> "\$RECORD"; exit 0 ;;
  *) exec "$real_npm" "\$@" ;;
esac
DOUBLE
chmod +x "$work/bin/gh" "$work/bin/npm"
carrier=$(find "$DIR" -maxdepth 1 -type f -name '*.tgz')
RECORD="$work/record" PATH="$work/bin:$PATH" bash "$root/scripts/release-attach.sh" "$TAG" "$DIR" >/dev/null
RECORD="$work/record" PATH="$work/bin:$PATH" bash "$root/scripts/publish-npm.sh" "$carrier" >/dev/null
sent=$(cat "$work/record")
expected=$(printf '%s\n' release upload "$TAG" "$DIR/$archive" "$DIR/$archive.sha256" --clobber "npm publish $carrier")
[ "$sent" = "$expected" ] || fail "publication would send: $(printf '%s' "$sent" | tr '\n' ' ')"

copy_staged "$work/extra"
cp "$work/extra/$archive" "$work/extra/$intel"
must_refuse "would publish: $intel" verify "$TARGET" "$work/extra"
copy_staged "$work/wrong"
python3 -I - "$work/wrong" "$archive" "$work/wrong-binary" <<'PATCH'
import hashlib, io, struct, sys, tarfile
from pathlib import Path
staged, archive, binary_out = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3])
with tarfile.open(staged / archive) as tar:
    members = {member.name: (member, tar.extractfile(member).read()) for member in tar.getmembers()}
member, data = members['oneagentgraph']
data = bytearray(data)
if data[:4] == b'\xcf\xfa\xed\xfe':
    struct.pack_into('<I', data, 4, 0x01000007)
else:
    struct.pack_into('<H', data, 18, 183)
members['oneagentgraph'] = (member, bytes(data))
binary_out.write_bytes(data)
with tarfile.open(staged / archive, 'w:gz') as tar:
    for member, content in members.values():
        tar.addfile(member, io.BytesIO(content))
(staged / f'{archive}.sha256').write_text(f'{hashlib.sha256((staged / archive).read_bytes()).hexdigest()}  {archive}\n')
PATCH
must_refuse "built for $wrong" verify "$TARGET" "$work/wrong"
must_refuse "built for $wrong" python3 "$root/scripts/release-package.py" --target "$TARGET" --binary "$work/wrong-binary" --tag "$TAG" --out "$work/wrong-package"
[ ! -e "$work/wrong-package" ] || fail 'the packager wrote a wrong-architecture artifact'
must_refuse 'Intel macOS is no longer a release target' bash "$root/scripts/release-build.sh" x86_64-apple-darwin "$TAG" "$work/intel"
must_refuse "invalid choice: 'x86_64-apple-darwin'" verify x86_64-apple-darwin "$DIR"

echo "release-rehearse: $TARGET staged $verified; attach and npm publish would send exactly its archive, checksum and carrier; Intel and $wrong artifacts refused"
