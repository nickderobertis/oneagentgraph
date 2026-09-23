# Terminal screenshots

Deterministic SVGs of this CLI's **real** output, gated by
[screencomp](https://github.com/nickderobertis/screencomp), plus one animated GIF
that is not. Informational: **never part of `just check`, `just gate`, or the
`gate` job in `ci.yml`** — `.github/workflows/visual-docs.yml` is a workflow of
its own and owns the comparison, exactly as `deps-check`, `msrv`,
`lint-windows`, and `release-probe-check` sit outside those tiers.

> Per this repository's convention, only the root `AGENTS.md` carries a
> `CLAUDE.md` symlink; a nested one like this is loaded by path.

## The scenes, and what each one documents

`scripts/screenshots.sh` renders one SVG per scene from the **real release
binary's** output. Each scene opens with the prompt line of the invocation that
produced it, and that line is not a caption somebody typed: the script prints the
same argv it then executes, so the command shown and the output under it cannot
come apart.

| scene | what it shows | why the README needs it |
| --- | --- | --- |
| `help` | `oneagentgraph --help` | The one colourised surface this tool has — clap's own. It documents the command surface, and it is deliberately **not** the hero: a help screen is what a reader reaches for second. |
| `validate` | a graph that checks out, then the refusal over one with a bad judge label | The refusal is the informative half: it names the member, which judge entry, the value it rejected, and why that value matters (a label becomes a file name a run writes). The passing line above it is what makes the pair read as one verb rather than as a picture of an error. |
| `health` | the per-identity report | The verb was a list item in the README and nothing else. The shot shows the whole shape a caller reads: the schema, the one clock read, and per identity how it was selected, its auth mode, and why there is or is not an answer. |
| `sweep` | `sweep --dry-run`'s operator report | The longest report this tool prints, and the one a reader most needs to see before running the non-`--dry-run` form: the examined families, a would-reclaim line with a size, a retained-because line naming the proof that failed, and the verdict line that says the scope is these families and not the host. |
| `history` (`view=list`) | the listing of two real runs | Two rows, two exit codes, one of them a run that died for want of a harness — the listing is how an operator finds the run they want. |
| `history` (`view=record`) | `history show ID` | The whole record: the members, the declared members, the config refs with their digests, and where the merged stream went. It is the only place the record's shape is visible at all. |
| `persona` | the shipped `personas/` catalogue validating, then a refused fragment | The catalogue line says the verb takes a directory and that what this crate ships really passes it; the refusal under it is the `agent:` block earlier persona versions defined, refused with its migration in the message — which is a claim the README already makes and could not previously show. |
| `demo.gif` | `run --output text`, a two-party member's stream filling | The hero. See below. |

**What is deliberately absent.** A still of `run --output text` sits nowhere in
the README: the hero GIF directly above it is that surface, moving, and a frozen
copy underneath would say strictly less. `validate`'s bare success line
(`node-scope: 1 member(s) OK`) is not a scene of its own for the same reason — it
is one line, and the prose beside it already says as much. Scenes are a candidate
set, not a quota; a shot that would say less than its neighbouring paragraph is
dropped rather than padded in.

## Why a generated capture is not a second statement of the contract

`docs/contract.md` is the approved source of truth for the CLI surface, and
`tests/contract.rs` holds the types to it. A **hand-made** screenshot of CLI
output would be a second spelling of that contract — the shape
`contracts_have_one_source_or_a_drift_gate` exists to refuse — and it would
quietly go stale.

These are not hand-made. Every image is produced by running the real binary, and
CI refuses the pull request the moment its bytes diverge from the committed
digest. The baseline **is** the drift gate, and the binary remains the one
source: the contract says what the surface is, the binary implements it, and these
are a rendering of what it printed. Nothing here is edited to match a document,
and nothing in `docs/contract.md` was edited to make a capture easier — no verb's
formatting changed and no flag or environment variable was added for one.

## The fixture, and the one thing that is faked

`screenshots/fixture/` is a graph, its configs, and two documents written to be
refused. The scenes drive the **real** `oneagentgraph` binary against it. The one
thing standing in for something real is the **paid harness process**, replaced at
oneharness's own `ONEHARNESS_BIN_<ID>` seam by this crate's own `test-doubles`
stand-in — the same single seam `tests/e2e/support.rs` is allowed, and no other.
So a capture costs nothing: no model call, no paid turn, no network, no
credential.

**The chain names the bare `claude-code` identity, never a variant.**
`ONEHARNESS_BIN_<ID>` keys on a harness *id*, and there is no spelling of it that
reaches `claude-code:alternate` — a chain naming a variant would spawn the real
paid provider with the double sitting unused beside it. That is a money hazard
rather than a style point, and `screenshots/fixture/oneharness.toml` says so at
the chain itself. The member that has to die for want of a harness uses a
*second* identity (`codex`) whose override names a program that exists nowhere,
because aiming `claude-code` at nothing would take the working members' double
down with it.

**Sentinels carry the `fake:` prefix.** A prompt is the whole rendered system
prompt, persona included, so a bare word matches prose nobody meant it to — `hang`
is a substring of `change`, which once parked every turn of the e2e suite.
`fake:hold=` is the other trap: its value runs to the end of the string, so
anything after it, including the `:` that separates the other sentinels, is read
as part of the path and the turn waits on a file nobody writes.

**The hash-gated scenes need no `oneharness` CLI, and the hero does.** The two
runs behind the `history` shots use single-sided `kind: oneharness` members, whose
turns run on the linked `oneharness-core` in this process; `scripts/screenshots.sh`
points `ONEAGENTGRAPH_ONEHARNESS_BIN` at a path inside its own throwaway workspace
that nothing ever creates, so a still that reached for that CLI would die naming it
rather than quietly find one on the host. That is the property that keeps the gated
half runnable on a machine with nothing installed.

`scripts/demo-gif.py` is the one exception, and the reason is structural rather
than incidental — see "Why the hero spawns and the stills do not" below.

## Why it is byte-reproducible (and needs no container)

screencomp gates on the **hash** of each image, so capture has to be
deterministic. Unlike a rasterised PNG — whose anti-aliasing drifts across CPUs,
which is why a web-app capture runs inside a pinned browser container — an SVG is
pure layout maths. Both inputs are pinned:

- **`freeze` is version-pinned** and **the font is vendored**
  (`fonts/JetBrainsMono-Regular.ttf`, OFL — see `fonts/JetBrainsMono-OFL.txt`),
  passed with `--font.file` so freeze never fetches one over the network, and
  embedded into each SVG as base64 so the file renders the same on GitHub, on
  crates.io, and offline. Both numbers live in **`tools.env` and nowhere else**
  (see "One source per version" below).
- **Nothing is inherited.** Every scene is launched through `env -i`, so the
  child's environment is *only* what that scene names. An exported
  `ONEAGENTGRAPH_STATE_DIR` or `ONEHARNESS_MODEL` cannot print into a shot or put
  a run on an identity nobody chose, and an inherited
  `ONEHARNESS_HISTORY_POINTER_FILE` cannot write this capture's throwaway
  sessions into the caller's own history store — the leak `tests/e2e/support.rs`
  records as having cost four journeys their deadline.

The result is identical bytes on every machine and runner, so a **single
`x86_64` lane** and its baseline cover everyone, `arm64` hosts included. A shot
changes only when a verb's content or formatting does, which is exactly what the
gate should catch.

### What is normalised, and why it has to be

**There is no clock override and no id override in this CLI**, and adding one
would be a change to the approved contract rather than a capture decision. So the
per-run values are rewritten after the fact, to fixed placeholders declared at the
top of `scripts/screenshots.sh`:

- **The scene's own world**: its `HOME` becomes `~` and its temp root becomes
  `/tmp`, which are the paths a reader who set neither would actually see.
- **Run ids and the epoch milliseconds a record restates.** A run id is
  `<name>-<epoch ms>-<pid>`; the id's milliseconds and the `started_ms` beside it
  are rewritten to the *same* number, so the record still reads as one run.
- **`health`'s `observed_at`**, its one clock read.

Two scenes are also given a world rather than left to find one:

- **`health` is swept with an empty `PATH` and an empty `HOME`.** The verb asks
  oneharness for its own sweep of every identity a *host* has, so on a developer's
  machine it answers that machine's credentials and quota — not this tool's
  output, and not the same twice. With nothing to resolve, every identity reports
  the bare program it looked for, or the static reason it has no headroom reader:
  the same report on every host, and still the whole shape. The empty `PATH` is
  also what keeps the Copilot probe from making the one HTTP request in the set —
  with no token variable in the environment it refuses before reaching for `curl`.
- **`sweep`'s scratch is built rather than run.** A real run's directory holds a
  stream whose length moves by a few bytes per run, which would move the `KiB`
  beside it and break the digest. So the fixture is the filesystem — four
  directories standing for what an operator finds — and everything the shot shows
  is the real verb's real reading of them.

`expand` renders tabs at eight-column stops before a scene is handed to freeze,
because `history`'s listing is tab-separated and an SVG renders a literal tab as
nothing at all — its exit-code column landed against the run id with no gap. That
renders the tabs the way a terminal does rather than reformatting the output;
ragged columns stay ragged.

## One source per version

Contract 2 of the visual-docs adoption asks that every version the capture pins
and restates more than once have one authoritative source, or a check that fails
when the copies part. Each of them, and which it is:

| version | authoritative source | how the copies are kept honest |
| --- | --- | --- |
| the `freeze` release | `screenshots/tools.env` (`FREEZE_VERSION`) | derived. `scripts/screenshots.sh`, `just screenshots-tools`, and the workflow's `capture-command` all read that file; none carries a number. |
| the vendored font | `screenshots/tools.env` (`FONT_FILE`) | derived. `scripts/screenshots.sh` and `scripts/demo-gif.py` read it from there. |
| the arch lane | `screencomp.toml` (`[capture].arches`) | derived. `scripts/screenshots.sh` and `.githooks/pre-push` each parse that line, and both refuse loudly rather than guess if it ever names more than one lane. |
| the Rust toolchain | `rust-toolchain.toml` | derived. The workflow's container is deliberately *unversioned* (`rust:bookworm`) and sets no `RUSTUP_TOOLCHAIN`, so rustup installs the pinned toolchain from that file. Nothing about a shot depends on the compiler anyway. |
| the `oneharness` CLI the GIF wants | the justfile (`oneharness-version`) | derived. `scripts/demo-gif.py` reads the pin out of the justfile for its diagnostic, exactly as `tests/e2e/support.rs` does, so this note names *which* pin rather than restating the number. |
| the screencomp version | `.github/workflows/visual-docs.yml` (the `uses:` ref and `screencomp-version:`) | **checked**, not derived: the two copies are screencomp's own contract, and `screencomp doctor --env` reports a workflow pin that has drifted from the installed CLI as a problem. |

## The capture is a node of the Nx graph

`just screenshots` runs `oneagentgraph:screenshots`, like every other repo-wide
verb here, with the work in the private `_crate-screenshots` recipe beside the
other `_crate-*` tools. Two things follow, and both are the point.

**Its inputs are declared, so the cache cannot lie.** `screenshotSource` in
`nx.json` names everything that can change a rendered shot: the whole of `src/**`
and the manifests, lockfile and toolchain pin the capture builds the binary from;
`personas/**`, which the `persona` scene validates; `screenshots/**`, the fixture,
the vendored font and the tool pins; `scripts/screenshots.sh`; and
`screencomp.toml`, which the script reads the lane out of. Touch one and the next
capture runs; touch none and it replays the shots it already produced instead of
rebuilding a release binary to draw them again.

It is deliberately **wider** than `[guard].paths` above, and the two are not two
statements of one thing. The guard list is a cheap pre-filter answering "is this
push worth a slow capture?", so it names the handful of source files a shot
usually comes from. A cache key has to be conservative or it is wrong, so this one
takes `src/**` whole — every file the guard names is inside it, and an
over-approximation only costs a re-capture nobody needed.

**It is reachable only by name.** Nothing in `check`, `gate`, the `check` target's
`dependsOn`, or CI's gate job refers to it, and `nx show project oneagentgraph`
is where to confirm that after touching the graph. The `Visual docs` workflow
stays the only gate for these images.

Two callers deliberately do not come through the target. The pre-push guard runs
`scripts/screenshots.sh` directly, because it is deciding whether to block a push
and a replayed capture cannot answer that; and `just screenshots-bless` passes
`--skip-nx-cache`, because blessing is the one call whose answer must come from a
capture taken now — a hit would restore the last captured tree's shots and then
write a baseline over them.

## The animated hero (`docs/screenshots/demo.gif`)

The stills are static; the README **hero** is the event stream filling line by
line, because that is what using this tool looks like and a still of the same text
says strictly less. `scripts/demo-gif.py` drives one real run of the fixture
graph's two-party member and replays its `--output text` lines in the order they
arrived, at the pace its own timestamps give them (clamped at both ends, so a burst
does not flash past and a genuine wait does not stall the loop). A two-party member
is the hero rather than a single-sided one because the agent's own `turn-message` —
the reply the whole conversation is for — is published only for that member kind,
and a stream showing the machinery running without the thing the machinery is for
is the wrong picture of this tool. There is no
view to reconstruct: this stream is append-only, nothing redraws, so rendering it
faithfully is simply replaying it — which is what makes this renderer much
simpler than the `llmlint` one it was adapted from, whose live view redraws in
place.

The agent's first turn is **held in flight** past the heartbeat bound, with the
double's own `fake:hold` sentinel, and released by the renderer. Without it the
double answers in microseconds and the whole run settles inside one millisecond:
true, and a picture of nothing, because the `member-heartbeat` that says a turn is
still alive only has something to say while a turn is running.

### Why the hero spawns and the stills do not

Worth writing down, because it reads like an oversight and is not. onejudge's
`OneharnessProvider` runs a turn **in process** by default — `Execution::Library`,
through `oneharness_core::io::run::run` — and nothing needs to be on `PATH` for
that. What moves it to `Execution::Process`, which spawns an `oneharness`
executable, is installing a `SpawnHook`: onejudge documents that as the opt-in,
because a hook offers a *process* and an in-process turn has none to offer.

`src/judge.rs` installs one on **every** two-party member, and it is load-bearing
twice: it puts both sides in the process group liveness and `cancel` reap through
(`src/scratch.rs`, `tests/e2e/liveness.rs`), and it is how the agent side's stamped
config reaches that side on `--config` (`src/invoke.rs`). So a two-party member
spawns the CLI by this crate's own design, and the only way to make the hero
in-process would be to drop or weaken that hook — trading a member's paid harnesses
out of the tree the stall watchdog tears down, for a richer demo. That is the
failure the hook exists to prevent, so the GIF pays a regeneration prerequisite
instead. A single-sided `kind: oneharness` member has no such hook and no second
party, which is why every hash-gated still runs with nothing installed.

The frames are drawn with the same vendored font, with Pillow and nothing else —
no `ttyd`, no `ffmpeg`. Like the stills it is informational; **unlike** them it is
**not** hash-gated, because a GIF is not byte-reproducible across Pillow versions.
So it is regenerated on demand and committed. Regenerate it when the text
rendering (`src/render.rs`) or the set of events a run publishes changes.

The output is monochrome because the output **is** monochrome: this crate emits no
ANSI escape anywhere, and colouring the frames would be inventing a feature rather
than documenting one.

## Commands

- `just screenshots-tools` — install the pinned `freeze` (needs Go). screencomp is
  installed separately (see its README); CI installs both itself.
- `just screenshots` — capture, through the `oneagentgraph:screenshots` Nx target.
  Builds the release binaries, writes the shots and the README copies. Quiet on
  success, and a replay on a tree that touched none of `screenshotSource`.
- `just screenshots-gif` — regenerate the animated hero. Needs Python 3 with
  Pillow, **and** the `oneharness` CLI on `PATH` at the release the justfile pins
  as `oneharness-version`, which `just bootstrap` installs. That CLI is a
  prerequisite of **regenerating the GIF and nothing else**: `just screenshots`,
  the pre-push guard and the CI capture never reach it, and the number is not
  restated here because the justfile is its one source (see "One source per
  version"). Why only this one needs it is above.
- `just screenshots-bless` — **after an intended output change**: recapture, past
  the cache, and refresh `shots/baseline/<lane>.json`. Commit it alongside
  `docs/screenshots/`.

## The strict gate, and how it is activated

CI (`fail-on-drift: true`) fails when a capture diverges from the committed
baseline. The local half is `.githooks/pre-push`, which re-captures **only** when a
`[guard].paths` file changed (`screencomp.toml`) and, on drift, refreshes the
baseline, builds a review gallery at `shots/review/index.html`, and **blocks the
push** so the new baseline and README images are committed deliberately.

`core.hooksPath` is per-clone state and is never committed, so a committed hook
that nothing points git at is a guard that runs nothing. **`just bootstrap`
activates it** (`scripts/activate-hooks.sh`), and `screencomp doctor --env` is what
reports the difference between wired and merely present. Nothing was displaced:
this repository shipped no hook before, and `.git/hooks` holds only git's own
`.sample` files.

**`.githooks/` carries this guard and nothing else.** `just gate` — this
repository's complete pre-push bar — is deliberately *not* wired into it: putting
a full gate on `git push` changes the development loop of every clone, which is a
separate decision nobody has made. Run `just gate` yourself before you push, as
before.

Two things the gate is **not**, so nobody reads more into it than is there. Until
the repository owner adds the workflow's check to branch protection, a pull
request whose capture drifted still merges — the workflow fails, but the context
is not required, so the strict gate is advisory on the merge path. And there is no
gallery link or badge anywhere in this repository — not in the README, not here,
not in the workflow's comments — because this repository has no published gallery
to link to, and a link that 404s on the day it merges is wrong on the day it
merges. The gallery this capture does produce locally is `shots/review/`, which
the pre-push guard builds on drift and `.gitignore` keeps out of the tree. A
published one, and the badge for it, are the repository owner's to add once a
Pages site exists.

### The two halves run under different shells

The local guard hands `scripts/screenshots.sh` to **bash**; CI's `capture-command`
is a step inside a `container:`, which GitHub runs under **`sh -e {0}`** — dash on
this image. So the workflow's block is POSIX sh and the script it calls is bash,
and only the first of those constraints is invisible locally: a bashism in that
block ends the step at its first line with nothing captured, which is what
`set -euo pipefail` did on every run of the workflow from its adoption until it
was dropped. `.github/workflows/visual-docs.yml` says so at the block, and
`npm/test/visual-docs-capture.test.mjs` drives that block under a strict `sh -e`
so the next one fails in `just check` rather than in a workflow nobody requires.
That suite captures **nothing** — every tool the block reaches for is stubbed,
`bash` included — so what the gate gained is the portability of those six lines,
and the capture itself stays outside `just check` exactly as before.

## Outputs

- `shots/current/<lane>/captures.json` and the SVGs — the capture screencomp reads
  (gitignored; regenerated). `$SHOTS_OUT` overrides the directory; the reusable
  workflow exports it per lane.
- `shots/baseline/<lane>.json` — the committed digest baseline (no images).
- `shots/review/` — the pre-push guard's review gallery (gitignored).
- `docs/screenshots/*.svg` and `docs/screenshots/demo.gif` — the committed copies
  the README embeds.

## Changing the screenshots

Editing a verb's reporting (`src/main.rs`, `src/sweep.rs`), the argument surface
`--help` renders (`src/cli.rs`), a refusal's diagnostic (`src/config.rs`,
`src/persona.rs`), the shipped persona catalogue, the fixture, or the scenes in
`scripts/screenshots.sh` will change the SVGs. That is expected — run
`just screenshots-bless` and commit the new baseline together with
`docs/screenshots/`. Bumping `FREEZE_VERSION` or the vendored font reflows every
shot; bless once, in the same change.
