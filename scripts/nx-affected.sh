#!/usr/bin/env bash
# Affected-only selection, keyed off an explicitly derived base.
#
# Two modes:
#   scripts/nx-affected.sh -t check        run a target over the affected projects
#   scripts/nx-affected.sh --affects NAME  print `true`/`false` for NAME's toolchain
#
# The base is the commit a push replaced when the caller names one
# (`ONEAGENTGRAPH_NX_BASE_SHA`, which wins), and otherwise the merge base with
# the base branch (`ONEAGENTGRAPH_NX_BASE_REF`, then GitHub's `GITHUB_BASE_REF`).
#
# Both **fail closed**: when the base cannot be derived — a shallow clone, a
# missing base branch, a pushed-over commit that is not in this checkout, a build
# that names no base at all — this runs everything and says so on stderr rather
# than reporting a scoped pass as a full one. Affected selection is a speed
# optimisation, and a speed optimisation that can silently skip a check is a
# correctness hole.
#
# llmlint: ignore-file[tool_output_is_signal] the fallback notices below are the whole
# point of failing closed: a run that silently widened its scope, or answered `true` for
# every project, looks identical to one that scoped correctly, and the next reader has no
# way to tell why the gate took ten minutes or passed without checking anything. They go
# to stderr, so the `--affects` answer on stdout stays parseable.
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
# instead of trusted: a branch name is what a branch name may look like — a
# conservative character set, and then git's own ref-name rules, which the set
# alone does not hold (`main..evil` and `main.lock` pass it).
#
# In CI its absence is meaningful rather than missing: a push build is *on* the
# base branch, so scoping against it would find nothing changed and skip every
# check. A push build names its base as a commit instead; one that names neither
# has no base, and no base means run everything.
base_branch() {
  local ref="${ONEAGENTGRAPH_NX_BASE_REF:-${GITHUB_BASE_REF:-}}"
  if [ -z "$ref" ]; then
    if [ -n "${CI:-}" ]; then
      echo "nx-affected: no base — not a pull-request build, and ONEAGENTGRAPH_NX_BASE_SHA names no commit" >&2
      return 1
    fi
    printf 'main'
    return 0
  fi
  if ! printf '%s' "$ref" | grep -Eq '^[A-Za-z0-9][A-Za-z0-9._/-]*$' ||
    ! git check-ref-format --branch "$ref" >/dev/null 2>&1; then
    echo "nx-affected: '$ref' is not a usable branch name" >&2
    return 1
  fi
  printf '%s' "$ref"
}

# The commit to compare HEAD against, or nothing when it cannot be derived.
#
# A push to the base branch has no fork to derive a merge base from, so the
# workflow names the commit the push replaced (GitHub's `github.event.before`).
# It is checked for shape and then for presence: a first push names the all-zero
# commit, and a force-push can name one this checkout never fetched — both fail
# closed, naming the variable, rather than falling back to a branch that would
# scope the run against something the caller did not ask for.
resolve_base() {
  local branch sha="${ONEAGENTGRAPH_NX_BASE_SHA:-}"
  if [ -n "$sha" ]; then
    if printf '%s' "$sha" | grep -Eq '^[0-9a-f]{7,64}$' && git cat-file -e "$sha^{commit}" 2>/dev/null; then
      printf '%s' "$sha"
      return 0
    fi
    echo "nx-affected: ONEAGENTGRAPH_NX_BASE_SHA '$sha' is not a commit in this checkout" >&2
    return 1
  fi
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
# llmlint: ignore[names_match_behavior] `--affects NAME` is the flag CI's `changes` job and this repository's acceptance contract call by that name, and NAME is the project whose jobs are asked about; the header states that the answer covers NAME's toolchain, which is the question those jobs need answered.
--affects)
  project="${2:-}"
  [ -n "$project" ] || {
    echo "nx-affected: --affects needs a project name" >&2
    exit 2
  }
  if ! base="$(resolve_base)"; then
    echo "nx-affected: no base — treating '$project' as affected" >&2
    printf 'true\n'
    exit 0
  fi
  # The question CI asks is whether to run the jobs that build with NAME's
  # toolchain — the Rust matrices for `oneagentgraph` — and those are owed to a
  # change that reaches any project of that language: the crate, a test tier
  # split out of it, or the repo-level project that owns the root configuration.
  # So the answer is NAME, or any affected project carrying NAME's `lang:` tag.
  # Read for Nx's answer, so the wrapper must not fold it into a summary line.
  if ! language="$(ONEAGENTGRAPH_NX_SHOW_OUTPUT=1 bash scripts/nx.sh show project "$project" --json |
    node -e 'const fs=require("node:fs");const tag=(JSON.parse(fs.readFileSync(0,"utf8")).tags||[]).find((t)=>t.startsWith("lang:"));process.stdout.write(tag??"")')"; then
    echo "nx-affected: Nx could not describe '$project' — treating it as affected" >&2
    printf 'true\n'
    exit 0
  fi
  if ! projects="$(ONEAGENTGRAPH_NX_SHOW_OUTPUT=1 bash scripts/nx.sh show projects --affected --base="$base" --head=HEAD --json)" ||
    ! family="$(ONEAGENTGRAPH_NX_SHOW_OUTPUT=1 bash scripts/nx.sh show projects --projects "tag:${language:-none}" --json)"; then
    echo "nx-affected: Nx could not list the affected projects — treating '$project' as affected" >&2
    printf 'true\n'
    exit 0
  fi
  # Matched as parsed JSON array elements rather than by grepping the text: a
  # project whose name is a substring of another's would otherwise answer for it.
  # Exit 1 is the one "not affected" answer; a list that is not a JSON array of
  # names exits 2 and fails closed, like every other answer Nx could not give.
  match=0
  node -e 'let affected,family;try{[affected,family]=[JSON.parse(process.argv[1]),JSON.parse(process.argv[2])];if(!Array.isArray(affected)||!Array.isArray(family))throw new Error("not an array")}catch{process.exit(2)}const name=process.argv[3];process.exit(affected.some((p)=>p===name||family.includes(p))?0:1)' \
    "$projects" "$family" "$project" || match=$?
  case "$match" in
  0) printf 'true\n' ;;
  1) printf 'false\n' ;;
  *)
    echo "nx-affected: Nx's project lists were not JSON arrays of names — treating '$project' as affected" >&2
    printf 'true\n'
    ;;
  esac
  ;;
*)
  [ "$#" -gt 0 ] || {
    echo "nx-affected: pass the Nx arguments to run, e.g. '-t check'" >&2
    exit 2
  }
  if ! base="$(resolve_base)"; then
    echo "nx-affected: no base — running every project instead of the affected ones" >&2
    exec bash scripts/nx.sh run-many "$@"
  fi
  exec bash scripts/nx.sh affected --base="$base" --head=HEAD "$@"
  ;;
esac
