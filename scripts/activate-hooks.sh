#!/usr/bin/env bash
# Point this clone's git hooks at the committed `.githooks` directory.
#
# Run by `just bootstrap`, because `core.hooksPath` is per-clone state that is
# never committed: without it, the screencomp visual guard in `.githooks/pre-push`
# is a file nothing ever executes, and the strict gate loses its local half.
# `screencomp doctor --env` is what reports the difference.
#
# Idempotent and quiet when already pointed here. Nothing is displaced: this
# repository ships no other hook and `.git/hooks` holds only git's own `.sample`
# files, which git never runs.
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "activate-hooks: not a git checkout, so there are no hooks to point at — skipping" >&2
  exit 0
fi

if [ "$(git config --get core.hooksPath || true)" = ".githooks" ]; then
  exit 0
fi

git config core.hooksPath .githooks
echo "activate-hooks: core.hooksPath -> .githooks (the screencomp pre-push guard is now active)" >&2
