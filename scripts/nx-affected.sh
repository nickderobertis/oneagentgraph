#!/usr/bin/env bash
# Affected-only selection, keyed off an explicitly derived merge base.
#
# Two modes:
#   scripts/nx-affected.sh -t check        run a target over the affected projects
#   scripts/nx-affected.sh --affects NAME  print `true`/`false` for one project
#
# Both **fail closed**: when the merge base cannot be derived — a shallow clone,
# a missing base branch, a detached build — this runs everything and says so on
# stderr rather than reporting a scoped pass as a full one. Affected selection is
# a speed optimisation, and a speed optimisation that can silently skip a check
# is a correctness hole.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || {
  echo "nx-affected: cannot enter the repository root $ROOT" >&2
  echo "ACTION: run this from a checkout whose directories are readable" >&2
  exit 1
}

# The base branch as GitHub names it on a pull request, or the local default.
#
# `GITHUB_BASE_REF` is workflow-controlled rather than attacker-controlled, but it
# reaches `git fetch` as a refspec, so its shape is validated at the boundary
# instead of trusted: a branch name is what a branch name may look like.
#
# In CI its absence is meaningful rather than missing: a push build is *on* the
# base branch, so scoping against it would find nothing changed and skip every
# check. There is no base there, and no base means run everything.
base_branch() {
  local ref="${ONEAGENTGRAPH_NX_BASE_REF:-${GITHUB_BASE_REF:-}}"
  if [ -z "$ref" ]; then
    if [ -n "${CI:-}" ]; then
      echo "nx-affected: no base branch — this is not a pull-request build" >&2
      return 1
    fi
    printf 'main'
    return 0
  fi
  if ! printf '%s' "$ref" | grep -Eq '^[A-Za-z0-9][A-Za-z0-9._/-]*$'; then
    echo "nx-affected: '$ref' is not a usable branch name" >&2
    return 1
  fi
  printf '%s' "$ref"
}

# The merge base this branch forked from, or nothing when it cannot be derived.
resolve_base() {
  local branch
  branch="$(base_branch)" || return 1
  # A PR runner's checkout has the base branch only as a remote-tracking ref if
  # it was fetched; fetch it before asking for the merge base so detection does
  # not depend on how deep the checkout happened to be.
  if [ -n "${CI:-}" ]; then
    git fetch --no-tags --quiet origin \
      "+refs/heads/$branch:refs/remotes/origin/$branch" 2>/dev/null || true
  fi
  git merge-base "origin/$branch" HEAD 2>/dev/null || return 1
}

case "${1:-}" in
--affects)
  project="${2:-}"
  [ -n "$project" ] || {
    echo "nx-affected: --affects needs a project name" >&2
    exit 2
  }
  if ! base="$(resolve_base)"; then
    echo "nx-affected: no merge base — treating '$project' as affected" >&2
    printf 'true\n'
    exit 0
  fi
  # Read for Nx's answer, so the wrapper must not fold it into a summary line.
  if ! projects="$(ONEAGENTGRAPH_NX_SHOW_OUTPUT=1 bash scripts/nx.sh show projects --affected --base="$base" --head=HEAD --json)"; then
    echo "nx-affected: Nx could not list the affected projects — treating '$project' as affected" >&2
    printf 'true\n'
    exit 0
  fi
  # Matched as a parsed JSON array element rather than by grepping the text: a
  # project whose name is a substring of another's would otherwise answer for it.
  if printf '%s' "$projects" |
    node -e 'const fs=require("node:fs");process.exit(JSON.parse(fs.readFileSync(0,"utf8")).includes(process.argv[1])?0:1)' "$project"; then
    printf 'true\n'
  else
    printf 'false\n'
  fi
  ;;
*)
  [ "$#" -gt 0 ] || {
    echo "nx-affected: pass the Nx arguments to run, e.g. '-t check'" >&2
    exit 2
  }
  if ! base="$(resolve_base)"; then
    echo "nx-affected: no merge base — running every project instead of the affected ones" >&2
    exec bash scripts/nx.sh run-many "$@"
  fi
  exec bash scripts/nx.sh affected --base="$base" --head=HEAD "$@"
  ;;
esac
