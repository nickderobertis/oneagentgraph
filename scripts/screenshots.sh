#!/usr/bin/env bash
# Capture the terminal screenshots screencomp gates, galleries, and posts to pull
# requests (see screencomp.toml + .github/workflows/visual-docs.yml).
#
# Every scene drives the REAL release `oneagentgraph` binary. The one thing
# standing in for something real is the paid harness process, replaced at
# oneharness's own `ONEHARNESS_BIN_<ID>` seam by this crate's own `test-doubles`
# stand-in — exactly the seam the e2e suite uses, and the only one it is allowed.
# So there is no model call, no paid turn, no network, and no credential; and the
# harness chain names the bare `claude-code` identity rather than a variant,
# because `ONEHARNESS_BIN_<ID>` keys on a harness id and a variant would reach the
# real paid provider (screenshots/fixture/oneharness.toml says so at the chain).
#
# The hash-gated scenes here need no `oneharness` CLI: the two runs behind the
# `history` shots use single-sided `kind: oneharness` members, which run on the
# linked `oneharness-core` in process. The animated hero (scripts/demo-gif.py)
# drives the two-party member instead, whose sides onejudge spawns `oneharness run`
# for — that is the one part of this capture that wants the pinned CLI, and it is
# not hash-gated.
#
# Each scene's text is rendered to a deterministic SVG by `freeze` using the
# VENDORED, pinned font, so the bytes — and therefore screencomp's digests — are
# identical on every machine and runner without a pinned container. That
# byte-determinism is the whole contract: change a verb's output, or its
# formatting, and that scene's SVG (and hash) changes; otherwise it does not.
#
# Scenes — one per surface, each embedded in the README section that explains it:
#   help      `--help`, the one colourised surface this tool has (clap's own).
#   validate  a graph that checks out, then the refusal over one with a bad
#             judge label — which names the member, the entry, and the value.
#   health    the per-identity report, swept with nothing installed to probe.
#   sweep     the operator report: examined families, would-reclaim lines, and a
#             retained-because line.
#   history   `view=list`, the listing of two real runs; `view=record`, the whole
#             record `history show` prints for one of them.
#   persona   the shipped catalogue validating, then the refusal over the
#             `agent:` block the persona format no longer has.
#
# Output (screencomp's capture contract):
#   $SHOTS_OUT/captures.json   index: {schema, shots:[{name,toggles,hash,image}]}
#   $SHOTS_OUT/<scene>.svg     one SVG per scene
# $SHOTS_OUT defaults to shots/current/<lane> (the reusable workflow exports it
# per lane). The SVGs are also copied to docs/screenshots/ (committed) for the
# README and the gallery.
#
# Requires `freeze` on PATH — `just screenshots-tools` installs the pinned one.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# The pinned renderer and vendored font, from the one file that declares them.
# shellcheck source=../screenshots/tools.env
. screenshots/tools.env
font="$repo_root/screenshots/$FONT_FILE"

# The one `[capture].arches` lane, read from screencomp.toml so the lane is
# declared in exactly one place — the guard reads it from there too. The guard
# assumes a single lane (that is what byte-identity buys); a second one would need
# its own baseline and CI job, so refuse loudly rather than guess.
LANE="$(sed -n 's/^arches *= *\[ *"\([^"]*\)" *\].*/\1/p' screencomp.toml)"
if ! [[ "$LANE" =~ ^[A-Za-z0-9_]+$ ]]; then
  echo "screenshots: expected exactly one lane in [capture].arches of screencomp.toml;" >&2
  echo "             got: [${LANE:-none}]" >&2
  exit 1
fi
SHOTS_OUT="${SHOTS_OUT:-shots/current/$LANE}"
docs_dir="$repo_root/docs/screenshots"

# Byte-determinism starts with the environment. Every scene below is launched
# through `env -i`, so the child's environment is *only* what that scene names —
# an exported `ONEAGENTGRAPH_STATE_DIR`, `ONEHARNESS_MODEL`, `CLAUDE_CONFIG_DIR`
# or `ONEHARNESS_HISTORY_POINTER_FILE` cannot reach a shot, put a run on an
# identity nobody chose, or write this capture's throwaway sessions into the
# caller's own history store. Nothing is unset here because nothing is inherited.

if ! command -v freeze >/dev/null 2>&1; then
  echo "screenshots: 'freeze' not on PATH. Install the pinned version with:" >&2
  echo "             just screenshots-tools" >&2
  exit 1
fi

# The binaries the capture drives: the real CLI, release, like a user would run
# it — and the paid-harness double beside it, from this crate's own
# `test-doubles` feature.
bin="$repo_root/target/release/oneagentgraph"
fake="$repo_root/target/release/oneagentgraph-fake-harness"
if [ -z "${SCREENSHOTS_NO_BUILD:-}" ] || [ ! -x "$bin" ] || [ ! -x "$fake" ]; then
  cargo build --release --locked --features test-doubles \
    --bin oneagentgraph --bin oneagentgraph-fake-harness >&2
fi

# Portable SHA-256 (Linux coreutils vs macOS/BSD).
sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Deterministic freeze flags. The vendored font — embedded into the SVG as base64
# — is what makes the output reproducible across machines; everything else is
# fixed window styling.
freeze_flags=(
  # Force terminal/ANSI mode. freeze's content-based auto-detection intermittently
  # misreads text as a source file, then ignores --font.file and hangs fetching a
  # default font over the network. `--language ansi` is unconditional, offline, and
  # byte-identical to the auto-detected render: it preserves the `help` scene's real
  # ANSI and renders the plain scenes verbatim.
  --language ansi
  --font.file "$font"
  --font.family "JetBrains Mono"
  --font.size 14
  --window
  --background "#0d1117"
  --padding "20,30"
  --margin 0
  --border.radius 8
  # A fixed window width and line wrap, so EVERY scene renders at the SAME pixel
  # width. The gallery and the README display each SVG at one fixed width, so a
  # per-scene auto-width makes the on-page text size wildly inconsistent — a
  # narrow scene scales up huge and a wide one shrinks. 100 columns clears
  # `--help`'s widest line (97) with margin and folds the two genuinely over-wide
  # lines (a refusal diagnostic, and `sweep`'s verdict line, which is one
  # sentence by design); 902px = 30+30 padding + 100*~8.42px/char.
  --width 902
  --wrap 100
)

rm -rf "$SHOTS_OUT"
mkdir -p "$SHOTS_OUT" "$docs_dir"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# The fixture, copied out of the tree so no scene can write into the checkout.
# The bytes are the committed ones, which is what keeps the `sha256` a run records
# for a graph — and therefore the `history show` shot — identical everywhere.
fixture="$tmp/fixture"
cp -R "$repo_root/screenshots/fixture" "$fixture"

# The double, on `PATH` under the bare name the fixture graphs name it by, so no
# graph document has to carry an absolute path.
mkdir -p "$tmp/bin"
cp "$fake" "$tmp/bin/oneagentgraph-fake-harness"

# The fixed placeholders every per-run value is normalised to. A run id embeds
# epoch milliseconds and a pid, and a record restates the same milliseconds; there
# is no clock or id override to reach for (and adding one would be a change to the
# approved CLI contract), so the capture rewrites them — to these, consistently,
# so the id and the `started_ms` beside it still agree.
REVIEW_MS=1790101010101
REVIEW_PID=40771
OFFLINE_MS=1790101020202
OFFLINE_PID=40772
REVIEW_ID="review-panel-$REVIEW_MS-$REVIEW_PID"
OFFLINE_ID="offline-probe-$OFFLINE_MS-$OFFLINE_PID"
# `health` stamps the one clock read of its sweep.
OBSERVED_AT="2026-05-14T09:41:07Z"

# captures.json identity is `name + JSON.stringify(toggles)`; entries collect one
# "name|toggles|hash|image" record per rendered scene, sorted at the end.
entries=()

# Render one captured text file to a scene SVG, hash it, and record it.
render_scene() {
  local name="$1" toggles="$2" image="$3" src="$4"
  if [ ! -s "$src" ]; then
    echo "screenshots: scene '$name' produced no output — cannot render." >&2
    exit 1
  fi
  # Tabs to eight-column stops, which is what a terminal does with them and what
  # an SVG does not: `history`'s listing is tab-separated, and rendered literally
  # its exit-code column landed against the run id with no gap at all. This
  # renders the tabs rather than reformatting the output — the columns land where
  # a terminal puts them, ragged ones included.
  expand "$src" >"$src.expanded" && mv "$src.expanded" "$src"
  # `< /dev/null`: freeze reads stdin whenever it is not a character device (its
  # IsPipe check), so under CI's piped stdin it would ignore the file argument and
  # render empty input ("No input"). Pointing stdin at /dev/null (a char device)
  # forces it down the read-the-file path on every runner.
  freeze "$src" "${freeze_flags[@]}" -o "$SHOTS_OUT/$image" </dev/null >&2
  entries+=("$name|$toggles|$(sha256 "$SHOTS_OUT/$image")|$image")
  # The committed copies: same bytes, just outside the gitignored shots/ tree.
  cp "$SHOTS_OUT/$image" "$docs_dir/$image"
}

# Append one invocation to a scene: the prompt line a reader sees, then what the
# binary wrote. The displayed words ARE the executed words — they come from the
# same argv — so a caption here can never drift from the command that produced
# the output under it.
#
# `home`/`tmpdir`/`path`/`cwd` are set by the caller for the scene it is building,
# because the scenes want different worlds: `health` wants an empty `PATH` so
# every probe reports the bare program it could not find, and the two runs want
# the double on theirs.
say_and_run() {
  local out="$1"
  shift
  printf '$ oneagentgraph %s\n' "$*" >>"$out"
  ( cd "$cwd" \
      && env -i HOME="$home" PATH="$path" TMPDIR="$tmpdir" \
           ${CLICOLOR:+CLICOLOR_FORCE=1} "$bin" "$@" ) >>"$out" 2>&1 || true
}

# A fresh hermetic world for one scene: its own HOME (which is where the state
# directory lives when nothing overrides it, so the shot shows the path a reader
# would have) and its own temp root.
world() {
  home="$tmp/$1-home"
  tmpdir="$tmp/$1-tmp"
  mkdir -p "$home" "$tmpdir"
  path="$tmp/bin:/usr/bin:/bin"
  cwd="$fixture"
  unset CLICOLOR
}

# Rewrite this scene's world out of its output: the fixture's HOME becomes `~`,
# which is the path a reader who set nothing would see, and its temp root becomes
# `/tmp`, which is where `TMPDIR` points on an ordinary host.
normalize_world() {
  sed -i -e "s|$home|~|g" -e "s|$tmpdir|/tmp|g" "$1"
}

# --- help: the command surface, and the one colourised output there is ---------
# `CLICOLOR_FORCE` is clap's own switch, read by the argument parser rather than
# by anything this crate wrote; no flag or variable was added to the CLI for it.
world help
CLICOLOR=1
out="$tmp/help.txt"
say_and_run "$out" --help
render_scene "help" "{}" "help.svg" "$out"

# --- validate: a graph that checks out, then one that does not -----------------
# The refusal is the informative half: it names the member, which judge entry, the
# value it rejected, and why that value matters. The passing line above it is what
# makes the pair read as one verb rather than as a screenshot of an error.
world validate
out="$tmp/validate.txt"
say_and_run "$out" validate ./graph.yaml
printf '\n' >>"$out"
say_and_run "$out" validate ./graph.broken.yaml
normalize_world "$out"
render_scene "validate" "{}" "validate.svg" "$out"

# --- health: the per-identity report ------------------------------------------
# Swept with an EMPTY `PATH` and an empty `HOME`, on purpose. `health` asks
# oneharness for its own sweep of every identity a host has, so what it answers on
# a developer's machine is that machine's credentials and quota — not this tool's
# output, and not the same twice. With nothing to resolve, every identity reports
# the bare program it looked for or the static reason it has no headroom reader,
# which is the same report on every host and still shows the whole shape a caller
# reads: the schema, the clock read, and per identity how it was selected, its
# auth mode, and why there is or is not an answer. The empty `PATH` is also what
# keeps the Copilot probe from making the one HTTP request in the set: with no
# token variable in the environment it refuses before reaching for `curl`.
world health
path=""
cwd="$tmp/health-home"
out="$tmp/health.txt"
say_and_run "$out" health
sed -i -e "s|\"observed_at\": \"[^\"]*\"|\"observed_at\": \"$OBSERVED_AT\"|" "$out"
render_scene "health" "{}" "health.svg" "$out"

# --- history: two real runs, then one of their records -------------------------
# Both runs are single-sided `kind: oneharness` members driven in process, so this
# needs no `oneharness` CLI. The first settles; the second is a chain whose only
# candidate has no binary anywhere, so it dies for want of a harness on every host
# — which is what makes the listing show both exit codes rather than one.
world history
mkdir -p "$tmp/history-work"
run_graph() {
  ( cd "$fixture" \
      && env -i HOME="$home" PATH="$path" TMPDIR="$tmpdir" \
           "$bin" run "$1" --task "$2" --dir "$tmp/history-work" \
           --output text >/dev/null 2>&1 ) || true
}
run_graph ./review.yaml "fake:complete-now: add the retry"
run_graph ./offline.yaml "probe the chain"

runs="$home/.local/state/oneagentgraph/runs"
real_review="$(basename "$(find "$runs" -maxdepth 1 -name 'review-panel-*' | sort | head -1)")"
real_offline="$(basename "$(find "$runs" -maxdepth 1 -name 'offline-probe-*' | sort | head -1)")"
for real in "$real_review" "$real_offline"; do
  if [ -z "$real" ]; then
    echo "screenshots: a fixture run left no record under $runs — cannot build the history scenes." >&2
    exit 1
  fi
done

# Every per-run value, to the fixed placeholders: the two run ids, and the epoch
# milliseconds a record restates. The id's own milliseconds and the `started_ms`
# beside it are rewritten to the same number, so the record still reads as one run.
normalize_runs() {
  sed -i \
    -e "s|$real_review|$REVIEW_ID|g" \
    -e "s|$real_offline|$OFFLINE_ID|g" \
    -e "s|\"started_ms\": [0-9]*|\"started_ms\": $REVIEW_MS|" \
    -e "s|\"finished_ms\": [0-9]*|\"finished_ms\": $((REVIEW_MS + 31))|" \
    "$1"
  normalize_world "$1"
}

out="$tmp/history-list.txt"
say_and_run "$out" history
normalize_runs "$out"
render_scene "history" '{"view":"list"}' "history-list.svg" "$out"

out="$tmp/history-record.txt"
say_and_run "$out" history show "$real_review"
normalize_runs "$out"
# The prompt line carries the run id too, and it was the real one when it was
# typed — the substitution above already rewrote it, so the command and the record
# under it still name the same run.
render_scene "history" '{"view":"record"}' "history-record.svg" "$out"

# --- sweep: the operator report ------------------------------------------------
# The scratch here is built rather than run: `sweep` reports on directories, and a
# real run's directory holds a stream whose length moves by a few bytes per run,
# which would move the `KiB` beside it and break the digest. So the fixture is the
# filesystem — four directories standing for what an operator actually finds — and
# everything the shot shows is the real verb's real reading of them.
world sweep
sweep_runs="$home/.local/state/oneagentgraph/runs"
mkdir -p "$sweep_runs"
# Two in the `runs` family: one a finished run left behind, claimed by a process
# that is gone, and one a run killed before it could record its identity — which
# is exactly the directory `sweep` may never remove, because nothing proves it is
# done with.
dead="$sweep_runs/review-panel-1790094510515-38204"
mkdir -p "$dead/members/implementer" "$dead/members/reviewer"
printf '38204 4213551\n' >"$dead/owner.lock"
head -c 4096 /dev/zero | tr '\0' 'x' >"$dead/events.jsonl"
head -c 512 /dev/zero | tr '\0' 'x' >"$dead/record.json"
unclaimed="$sweep_runs/node-scope-1790087211003-37988"
mkdir -p "$unclaimed"
head -c 1024 /dev/zero | tr '\0' 'x' >"$unclaimed/events.jsonl"
# One in the `temp` family, and one directory beside it that is not this crate's —
# it carries no `oneagentgraph-` prefix, so the family never counts it and the
# report never claims anything about it.
smoke="$tmpdir/oneagentgraph-smoke-38866"
mkdir -p "$smoke"
printf '38866 4213604\n' >"$smoke/owner.lock"
head -c 2048 /dev/zero | tr '\0' 'x' >"$smoke/transcript.jsonl"
mkdir -p "$tmpdir/some-other-tools-cache"
# Past the sweep's default 24-hour floor, so the report is about what the three
# proofs decided rather than about how long ago this script ran. Stamped last,
# because writing inside a directory moves its own timestamp.
for dir in "$dead" "$unclaimed" "$smoke"; do touch -d '2026-05-12T21:08:33Z' "$dir"; done
out="$tmp/sweep.txt"
say_and_run "$out" sweep --dry-run
normalize_world "$out"
render_scene "sweep" "{}" "sweep.svg" "$out"

# --- persona: the shipped catalogue, then a refused fragment --------------------
# The catalogue line is the shipped `personas/` directory really validating, which
# is what says the verb takes a directory; the refusal under it is the `agent:`
# block the format no longer has, refused with the migration in the message.
world persona
cp -R "$repo_root/personas" "$tmp/persona-catalogue"
cwd="$tmp"
out="$tmp/persona.txt"
say_and_run "$out" persona validate ./persona-catalogue/
printf '\n' >>"$out"
say_and_run "$out" persona validate ./fixture/persona.broken.yaml
# The catalogue is the tree's own `personas/`, copied out so no scene writes into
# the checkout; the shot names it as a reader would type it.
sed -i -e 's|\./persona-catalogue/|personas/|g' -e 's|\./fixture/persona\.broken\.yaml|./persona.broken.yaml|g' "$out"
normalize_world "$out"
render_scene "persona" "{}" "persona.svg" "$out"

# Write captures.json, shots sorted by identity, schema 1, trailing newline — the
# exact shape screencomp's classify/manifest/gallery read. All fields are safe
# ASCII (names, toggle values, hex digests, file names), so plain printf is sound.
{
  printf '{\n  "schema": 1,\n  "shots": [\n'
  IFS=$'\n' read -r -d '' -a sorted < <(printf '%s\n' "${entries[@]}" | sort && printf '\0')
  last=$((${#sorted[@]} - 1))
  for i in "${!sorted[@]}"; do
    IFS='|' read -r name toggles hash image <<<"${sorted[$i]}"
    comma=","
    [ "$i" -eq "$last" ] && comma=""
    printf '    {\n      "name": "%s",\n      "toggles": %s,\n      "hash": "%s",\n      "image": "%s"\n    }%s\n' \
      "$name" "$toggles" "$hash" "$image" "$comma"
  done
  printf '  ]\n}\n'
} >"$SHOTS_OUT/captures.json"

echo "screenshots: wrote ${#entries[@]} shots to $SHOTS_OUT and docs/screenshots/" >&2
