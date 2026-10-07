#!/usr/bin/env bash
# One digest of the working tree, for the `lint-llm-diff` target's Nx cache key.
#
# The judged tier answers once per tree, so its key has to move with any file in
# it: every file git would show — tracked or untracked, never ignored — by path
# and content. This is what the `{workspaceRoot}/**/*` file input used to give,
# and it is a runtime input instead for one reason: Nx also reads a target's
# `{workspaceRoot}` file inputs as "a change to this file affects this project",
# and a project whose input is every file is affected by every change. That
# would put `oneagentgraph-workspace` — and with it every Rust job CI skips on an
# npm-only change — in every affected set.
#
# Nx treats a runtime input that fails as no contribution and runs the task
# anyway, which for this key would mean "the same tree as every other failure"
# and a replayed verdict for a tree nobody judged. So a digest that cannot be
# taken is replaced by one that matches nothing, and the tier re-judges.
#
# Run it by hand to see the digest a run would key on. The judged tier's cache
# journeys in tests/repo-tooling/llmlint_cache.rs drive it end to end: an
# unchanged tree replays, a changed one re-judges.
set -uo pipefail

unmatched() {
  echo "workspace-digest: $1 — keying this run on a value that matches no recorded verdict, so the judged tier re-judges" >&2
  echo "ACTION: $2, then rerun 'just lint-llm-diff' to cache its verdict again" >&2
  printf 'undigested-%s-%s\n' "$(date +%s%N)" "$$"
  exit 0
}

root="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)" || unmatched "cannot locate the repository root" "run it from a checkout whose directories are readable"
cd "$root" || unmatched "cannot enter $root" "make $root readable and searchable by this user"

# Paths NUL-separated end to end, so no file name can be read as two.
listing="$(mktemp)" || unmatched "cannot open temporary storage" "free disk space under ${TMPDIR:-/tmp} (df -h)"
trap 'rm -f "$listing"' EXIT
git ls-files -z --cached --others --exclude-standard --deduplicate >"$listing" ||
  unmatched "git cannot list the tree" "run 'git status' in $root and fix what it reports"

# A tracked file deleted in the work tree is still in the index listing; its
# absence is the change, so it is hashed as a marker rather than as content.
digest="$(
  while IFS= read -r -d '' path; do
    if [ -L "$path" ]; then
      printf 'link %s %s\0' "$path" "$(readlink -- "$path")"
    elif [ -f "$path" ]; then
      printf 'file %s %s\0' "$path" "$(git hash-object --no-filters -- "$path")"
    else
      printf 'gone %s\0' "$path"
    fi
  done <"$listing" | sha256sum
)" || unmatched "cannot hash the tree" "run 'git status' in $root and make every file it lists readable"
printf '%s\n' "${digest%% *}"
