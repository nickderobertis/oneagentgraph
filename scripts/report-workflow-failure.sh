#!/usr/bin/env bash
# Announce a failed workflow run as a GitHub issue.
#
# `published-smoke.yml` runs when a release finishes, not on a pull request, so
# it has nothing to turn red and nobody waiting on its checks list. A failure
# that announces itself nowhere is one nobody reads — which is the same as not
# asking the registries at all, and asking them is the whole reason that
# workflow exists. This gives its failures somewhere to be seen.
#
# One open issue, commented on each further failure, so a registry that is
# broken across three releases is one thread rather than three issues. A file
# rather than inline workflow YAML because the create-vs-comment branch is real
# behavior: npm/test/report-workflow-failure.test.mjs drives both halves against
# a stubbed `gh`, inside `just check`.
#
# Every failure below says what broke AND what to do about it, for the same
# reason the script exists: this runs only when something is already wrong, so a
# reporter that dies quietly takes the finding down with it.
#
# Reads (all required except RUN_URL):
#   REPO      owner/name to file against
#   TITLE     the issue title, which is also how an existing one is found
#   BODY      the issue/comment body
#   RUN_URL   appended to the body when set
# `gh` must be authenticated (GH_TOKEN in CI).
set -euo pipefail

for required in REPO TITLE BODY; do
  if [ -z "${!required:-}" ]; then
    echo "report-workflow-failure: \$$required is empty or unset, so there is nothing to file" >&2
    echo "  ACTION: the caller supplies all three. In CI that is the \`report\` job of the workflow that failed — give its step \`env: $required: …\`. Run it by hand with REPO=owner/name TITLE=… BODY=… bash scripts/report-workflow-failure.sh" >&2
    exit 2
  fi
done

# One place every `gh` failure is answered, because the causes that can
# plausibly happen here need different answers and the exit status tells them
# apart in none of them — only what `gh` wrote does. What it wrote is printed
# either way, so a cause nobody predicted is still diagnosable.
#   $1 what was being attempted, $2 gh's exit status, $3 what gh wrote
# shellcheck disable=SC2153  # TITLE is an input, not a typo for the `title` the listing loop reads
gh_failed() {
  local what="$1" status="$2" said="$3"
  echo "report-workflow-failure: $what failed (gh exited $status)" >&2
  if [ -n "$said" ]; then
    printf '%s\n' "$said" | sed 's/^/    gh: /' >&2
  else
    echo "    gh: (said nothing)" >&2
  fi
  case "$said" in
    *"gh auth login"* | *"authentication"* | *"HTTP 401"* | *"Bad credentials"*)
      echo "  ACTION: this run has no usable credential. In CI, pass \`env: GH_TOKEN: \${{ github.token }}\` to the step; locally, run \`gh auth login\`." >&2
      ;;
    *"HTTP 403"* | *"Resource not accessible"* | *"not authorized"*)
      echo "  ACTION: the credential works but may not write issues on $REPO. Give the job \`permissions: issues: write\` (it gets no more than the workflow declares), and check that issues are enabled on the repository." >&2
      ;;
    *"HTTP 404"*)
      echo "  ACTION: \`$REPO\` did not resolve — check \$REPO for a typo, and that the token can see a private repository." >&2
      ;;
    *"HTTP 422"* | *"Validation Failed"* | *"Invalid search query"*)
      echo "  ACTION: GitHub rejected the request itself rather than the caller — \$TITLE is the likeliest cause, since it is interpolated into a search query. Reproduce with: gh issue list --repo $REPO --state open --search '$TITLE in:title'" >&2
      ;;
    *)
      echo "  ACTION: reproduce the command above with \`gh --repo $REPO\` and read its error. The three causes worth ruling out first are the credential (\`gh auth status\`), the job's \`issues: write\` permission, and \$TITLE." >&2
      ;;
  esac
  echo "  The failure being reported is NOT lost: it is the red run at ${RUN_URL:-<no RUN_URL was passed>}." >&2
  exit 1
}

# shellcheck disable=SC2153  # BODY is an input, not a typo for the local below
body="$BODY"
[ -n "${RUN_URL:-}" ] && body="$body"$'\n\n'"Run: $RUN_URL"

said="$(mktemp)"
trap 'rm -f "$said"' EXIT

# `--search "<title> in:title"` rather than a label: a label has to exist first,
# and a workflow that has to create one before it can report a failure has one
# more way to fail while reporting a failure. The search is a fuzzy one, so the
# exact title is matched below rather than trusted from it.
status=0
listed="$(gh issue list --repo "$REPO" --state open --search "$TITLE in:title" \
  --json number,title --jq '.[] | "\(.number)\t\(.title)"' 2>"$said")" || status=$?
[ "$status" -eq 0 ] || gh_failed "looking for an open issue titled \"$TITLE\"" "$status" "$(cat "$said")"

# The title is compared here rather than inside the `--jq` program: `gh`'s
# built-in jq takes no `--arg`, so an embedded title would be jq source built
# from an input, and a title carrying a quote would be a filter rather than a
# string. The number is likewise checked before it is used to address anything.
existing=""
while IFS=$'\t' read -r number title; do
  [ -n "$number" ] || continue
  case "$number" in
    *[!0-9]*)
      echo "report-workflow-failure: gh listed an issue whose number is not a number (\"$number\")" >&2
      echo "  ACTION: \`gh issue list --repo $REPO --state open --json number,title\` no longer answers what this expects — refusing rather than addressing a comment at it. Check the installed gh version." >&2
      exit 1
      ;;
  esac
  if [ "$title" = "$TITLE" ]; then
    existing="$number"
    break
  fi
done <<<"$listed"

# On success `gh` answers with the URL it wrote to, which is the one thing a
# reader of this log actually wants next.
if [ -n "$existing" ]; then
  status=0
  where="$(gh issue comment "$existing" --repo "$REPO" --body "$body" 2>"$said")" || status=$?
  [ "$status" -eq 0 ] || gh_failed "commenting on #$existing" "$status" "$(cat "$said")"
  echo "report-workflow-failure: commented on #$existing — $where"
else
  status=0
  where="$(gh issue create --repo "$REPO" --title "$TITLE" --body "$body" 2>"$said")" || status=$?
  [ "$status" -eq 0 ] || gh_failed "opening an issue titled \"$TITLE\"" "$status" "$(cat "$said")"
  echo "report-workflow-failure: opened a new issue — $where"
fi
