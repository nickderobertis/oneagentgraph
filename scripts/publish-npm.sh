#!/usr/bin/env bash
# Publish npm package directories/tarballs idempotently.
#
# A release job can be re-run — after a flaky sibling job, or to finish a partial
# publish — and an npm version is immutable, so a second `npm publish` of a
# version already live would red-fail a release that actually succeeded. This
# asks the registry first: `npm pack --dry-run` validates each manifest and
# yields its canonical name@version, and only a registry 404 permits publication.
# Auth, network, and server errors fail closed rather than being mistaken for
# "not published yet".
set -euo pipefail

fail() {
  printf 'publish-npm: %s\n' "$1" >&2
  exit 1
}

[ "$#" -gt 0 ] || fail "pass at least one package directory or tarball"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

published=""
skipped=""

for package in "$@"; do
  if ! metadata="$(npm pack --dry-run --json "$package" 2>"$work/pack-error")"; then
    cat "$work/pack-error" >&2
    fail "cannot read package metadata from '$package'; rebuild the npm artifact with scripts/npm-build.mjs"
  fi
  # The single-quoted program is JavaScript; its template expression is not shell.
  # shellcheck disable=SC2016
  if ! identity="$(printf '%s' "$metadata" | node -e '
    let input = "";
    process.stdin.on("data", chunk => input += chunk).on("end", () => {
      const items = JSON.parse(input);
      if (!Array.isArray(items) || items.length !== 1 ||
          typeof items[0]?.name !== "string" || typeof items[0]?.version !== "string") {
        throw new Error("npm pack did not return one name/version");
      }
      process.stdout.write(`${items[0].name}@${items[0].version}`);
    });
  ' 2>"$work/metadata-error")"; then
    cat "$work/metadata-error" >&2
    fail "npm returned invalid metadata for '$package'; rebuild the npm artifact with scripts/npm-build.mjs"
  fi

  if npm view "$identity" version >/dev/null 2>"$work/view-error"; then
    skipped="$skipped $identity"
  elif grep -Eq 'E404|404 Not Found' "$work/view-error"; then
    if ! npm publish "$package" --access public >"$work/publish-output" 2>&1; then
      cat "$work/publish-output" >&2
      fail "npm could not publish '$identity'; fix the reported authentication or package error, then re-run the release"
    fi
    published="$published $identity"
  else
    cat "$work/view-error" >&2
    fail "cannot query '$identity'; re-run the release when the npm registry is reachable"
  fi
done

# One line for the whole run, whatever it was handed: what a release log needs is
# which versions this push added and which were already live, not a running
# commentary. `# none` keeps the line readable when a re-run publishes nothing.
printf 'publish-npm: published%s; already on npm%s\n' \
  "${published:- none}" "${skipped:- none}"
