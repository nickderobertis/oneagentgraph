# Canonical command surface for oneagentgraph.
#
# `just bootstrap` works from a clean clone; `just check` is the deterministic
# quality gate and `just gate` is the complete pre-push bar (check + the llmlint
# diff tier). Recipes are quiet on success and specific on failure.
#
# This is a monorepo: the repo-wide verbs delegate to Nx, which fans the
# uniformly-named target out across every project. They never loop over projects
# by hand. What a target *does* stays with its project — the `_crate-*` recipes
# below are the Rust crate's own tools, and packaging/project.json names its.

set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

# Recipe parameters reach the shell as `$1`, `$2`, ... rather than only as `{{name}}`
# text spliced into the command. `lint-llm-diff` needs that: its base ref and its
# passthrough Nx arguments come from a caller, and interpolating them would let the
# shell parse whatever they contain before anything could validate it. Additive —
# `{{name}}` still works, so the recipes below are unchanged.
set positional-arguments

# llmlint: ignore-file[tool_output_is_signal] recipes that hand straight to cargo,
# clippy, rustdoc, or cargo-deny inherit those tools' diagnostics, which already
# name the exact problem and its fix; a wrapper message would bury them. The
# recipes whose failure needs project-level context (_crate-fmt-check,
# _crate-test, msrv) add one explicitly.

# The MSRV has one source of truth — Cargo.toml's `rust-version` — so `just msrv`
# cannot promise a floor the manifest no longer declares. CI reads the same field.
msrv-version := `sed -n 's/^rust-version *= *"\([^"]*\)".*/\1/p' Cargo.toml`

# The one CLI this crate still spawns, pinned here because the e2e suite drives
# it for real. onejudge has no entry: it is a library dependency now, pinned by
# `Cargo.lock`, so there is nothing to install and nothing on `PATH` to shadow.
oneharness-version := "0.6.15"

# Keep the gate's own output to signal: successes are silent, failures are not.
export CARGO_TERM_QUIET := "true"

# List available recipes.
default:
    @just --list

# Every project's `bootstrap` target, so one clean-clone command provisions the
# whole graph rather than the crate alone. Serialized: the projects share
# installers, and two of those running at once race the same directory.
# Set up the project from a clean clone.
bootstrap:
    @bash scripts/nx.sh run-many -t bootstrap --parallel=1

# The Rust crate's own provisioning (the `oneagentgraph:bootstrap` target).
_crate-bootstrap:
    @rustup show active-toolchain >/dev/null 2>&1 || rustup toolchain install
    @rustup component add rustfmt clippy llvm-tools >/dev/null \
      || { echo "cannot add toolchain components — install rustup (https://rustup.rs/) and re-run" >&2; exit 1; }
    @just _ensure-tool cargo-nextest
    @just _ensure-tool cargo-llvm-cov
    @just _ensure-oneharness
    @cargo fetch --locked --quiet

# The e2e suite drives this for real, as a subprocess, so it is part of
# provisioning rather than something a developer is expected to have. It is
# version-checked rather than merely present: a stale `oneharness` on PATH is the
# failure mode `docs/onejudge-integration.md` records, where a run dies on a
# confusing broken pipe because the binary rejects flags the caller relies on.
# The probe checks the cargo bin directory as well as `PATH`, and so does the
# e2e suite's own resolution (tests/e2e/support.rs). Checking `PATH` alone leaves
# bootstrap unable to satisfy itself: where an older CLI precedes the cargo bin
# directory, the probe keeps failing on the shadow no matter how many times the
# install below writes the pin behind it.
# Install the pinned `oneharness` CLI. Quiet when already at the pin.
_ensure-oneharness:
    @for candidate in oneharness "${CARGO_HOME:-$HOME/.cargo}/bin/oneharness"; do \
       if [ "$("$candidate" --version 2>/dev/null)" = "oneharness {{oneharness-version}}" ]; then exit 0; fi; \
     done; \
     cargo install --locked oneharness --version {{oneharness-version}}

# These are test runners, not rules: their version cannot change the gate's
# verdict, so both here and CI take the latest rather than keeping two pins that
# drift apart.
# Install a cargo dev tool if it is missing. Quiet when already present.
_ensure-tool tool:
    @command -v {{tool}} >/dev/null 2>&1 || cargo install {{tool}} --locked --quiet

# The tiers run in fail-fast order as dependencies, each fanned across every
# project by Nx. The body then runs the per-project `check` aggregate — the same
# target `just check-affected` uses — which replays from the cache in a second
# and is what stops the full sweep and the affected sweep from covering
# different tiers.
# Deterministic quality gate, every project.
check: fmt-check lint test doc
    @bash scripts/nx.sh run-many -t check
    @echo "check: ok"

# What PR CI runs: the same gate, scoped to the projects this branch's diff can
# reach. Fails closed — with no derivable merge base it runs everything.
# Deterministic quality gate, affected projects only.
check-affected:
    @bash scripts/nx-affected.sh -t check
    @echo "check-affected: ok"

# The complete pre-push bar: the deterministic gate plus the LLM-judge tier
# scoped to what this branch changed. `check` stays offline and credential-free;
# this is where the non-deterministic tier joins it.
# Full gate: `check` plus the diff-scoped llmlint tier.
gate base="origin/main": check
    @just lint-llm-diff "{{base}}"
    @echo "gate: ok"

# Escape hatch for Nx itself, e.g. `just nx show projects` or `just nx graph`.
# Run an arbitrary Nx command against this workspace.
nx *ARGS:
    @bash scripts/nx.sh {{ARGS}}

# Verify formatting without modifying files.
fmt-check:
    @bash scripts/nx.sh run-many -t format-check

# Format the codebase in place.
format:
    @bash scripts/nx.sh run-many -t format

# Lint every project with its own linter; any warning is an error.
lint:
    @bash scripts/nx.sh run-many -t lint

# Every project's test suite; the crate's enforces its coverage floor.
test:
    @bash scripts/nx.sh run-many -t test

# Build the docs with warnings denied (kept in the gate so doc links don't rot).
doc:
    @bash scripts/nx.sh run-many -t doc

# Verify the crate's formatting without modifying files.
_crate-fmt-check:
    @cargo fmt --all -- --check || { echo "formatting drift above — run 'just format'" >&2; exit 1; }

# Format the crate in place.
_crate-format:
    @cargo fmt --all

# Lint the crate with clippy; any warning is an error.
_crate-lint:
    @cargo clippy --all-targets --all-features --locked --quiet -- -D warnings

# 95% line coverage is the gate; lower it only with a documented reason in
# AGENTS.md.
# The crate's full test suite (unit + contract + e2e) with coverage enforced.
_crate-test:
    @cargo llvm-cov nextest --locked --all-features --fail-under-lines 95 \
      --status-level fail --final-status-level fail \
      || { echo "tests failed, or coverage fell below 95% — cover the lines the table above counts as missed" >&2; exit 1; }

# Coverage instrumentation is measured on Linux only, so the cross-platform CI
# legs run the same suite through this instead of `test`.
#
# `--no-fail-fast` because this is the leg whose failures are hardest to
# reproduce: a round trip to a hosted macOS or Windows runner. Stopping at the
# first failure cancelled 54 of 224 tests once and reported four, which reads as
# "four broke" when the honest answer was unknown. The whole picture costs one
# run's wall clock and saves a round trip per hidden failure.
#
# llmlint: ignore-block[changed_behavior_has_e2e] this recipe has no test because
# it *is* the test run. What `--no-fail-fast` changes is how much of the suite
# reports when part of it fails, and a journey for it would have to make the
# suite fail on purpose and then read its own runner's summary — a test whose
# subject is the harness executing it. The product behaviour under this recipe is
# covered by every journey the recipe runs.
# Full test suite without coverage instrumentation.
test-quick:
    @cargo nextest run --locked --all-features --status-level fail --no-fail-fast
# llmlint: ignore-end[changed_behavior_has_e2e]

# Drives the compiled binary — never an in-process `main()`.
# The end-to-end binary journeys in isolation (also run by `test`/`check`).
test-e2e:
    @cargo nextest run --locked --all-features -E 'binary(e2e)' --status-level fail

# Build the crate's docs with warnings denied.
_crate-doc:
    @RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --locked --all-features --quiet

# Run the CLI, e.g. `just run validate examples/graph.yaml`.
run *ARGS:
    cargo run --locked --quiet -- {{ARGS}}

# Upgrade dependencies, then re-run the full gate.
upgrade:
    @cargo update --quiet
    @npm update --silent --no-audit --no-fund
    @just check

# Separate from `check`: `cargo deny` needs a network-fetched advisory DB.
# Advisory + license audit and unused-dependency check.
deps-check:
    @command -v cargo-deny >/dev/null || { echo "cargo-deny not installed: cargo install cargo-deny --locked" >&2; exit 1; }
    @command -v cargo-machete >/dev/null || { echo "cargo-machete not installed: cargo install cargo-machete --locked" >&2; exit 1; }
    @cargo deny --log-level error check
    @# machete prints the unused deps it finds on stdout, so keep it: hiding
    @# them would leave a failing gate with no actionable detail.
    @cargo machete

# Separate from `check` on the same terms `deps-check` is: two of the probe's
# three answers are what a public registry serves, and the deterministic gate
# stays offline. Its third answer — "not answered" — needs no network and is in
# `check`, at npm/test/release-probe.test.mjs.
# Prove scripts/release-probe.sh against the public registries.
release-probe-check:
    @command -v node >/dev/null || { echo "node not installed: this drives the probe from a node test — run 'just bootstrap'" >&2; exit 1; }
    @node --test npm/test/live/release-probe.test.mjs

# Outside `check` on the same terms `deps-check` and `release-probe-check` are: it
# needs something a clean clone does not have. It is the one of the three with no
# CI job — the reader it calls is unpublished, so nothing CI can install provides
# it. When it ships, `scripts/check-release-declaration.mjs` goes away and this
# becomes a job.
#
# llmlint: ignore-block[changed_behavior_has_e2e] what this recipe *does* is
# npm/test/live/release-declaration.test.mjs, which drives the canonical reader in
# both directions and is the test for it. What is left here is a delegation and two
# refusals to run at all, and a case for those would have to remove a tool the rest
# of the suite needs. `msrv` and `lint-windows` carry this exclusion for the same
# reason, and `release-probe-check` is the same shape.
# Hold release-targets.toml to onevcs's own reader — the schema's definition.
release-declaration-check:
    @command -v onevcs >/dev/null || { echo "onevcs not installed: this hands release-targets.toml to onevcs's own reader, which is the schema's definition — install the onevcs release carrying 'release declaration'" >&2; exit 1; }
    @command -v node >/dev/null || { echo "node not installed: this drives the reader from a node test — run 'just bootstrap'" >&2; exit 1; }
    @node --test npm/test/live/release-declaration.test.mjs
# llmlint: ignore-end[changed_behavior_has_e2e]

# Reads the floor from Cargo.toml's `rust-version`; that toolchain must be
# installed (`rustup toolchain install <version>`). Warnings are errors here too.
# Build under the declared MSRV.
msrv:
    @RUSTFLAGS="-D warnings" cargo +{{msrv-version}} check --locked --all-targets --quiet \
      || { echo "the {{msrv-version}} floor no longer builds — install that toolchain, or raise rust-version in Cargo.toml (and clippy.toml)" >&2; exit 1; }

# `src/scratch.rs` carries this crate's one large `cfg(windows)` body — the job
# objects the liveness rules rest on there — and a Linux or macOS `check` never
# compiles a line of it. Outside `check` for the reason `msrv` is: it needs a
# target and a cross compiler a clean clone does not have. The gnu target rather
# than msvc because clippy only has to *check*, and gnu is the one a Linux host
# can provision.
#
# llmlint: ignore-block[changed_behavior_has_e2e] this recipe has no test because
# it is a shortcut to a check that is already gated elsewhere, not a behaviour of
# the product. What it runs — clippy over the `cfg(windows)` body — is exactly
# what the required `cross (windows-latest)` job runs on a real Windows host, so
# the thing being asserted is asserted there; this only saves the round trip. Its
# two guards are refusals to run at all when the target or the cross compiler is
# absent, and a test for them would have to uninstall a toolchain the rest of the
# suite needs. `msrv` and `deps-check` sit outside the gate on the same terms.
# Reproduce the `cross (windows-latest)` leg here instead of waiting for CI.
lint-windows:
    @rustup target list --installed | grep -qx x86_64-pc-windows-gnu \
      || { echo "the Windows target is missing — run 'rustup target add x86_64-pc-windows-gnu'" >&2; exit 1; }
    @command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1 \
      || { echo "no cross compiler for the Windows target — install mingw-w64 (a dependency's build script needs one)" >&2; exit 1; }
    @cargo clippy --target x86_64-pc-windows-gnu --all-targets --all-features --locked -- -D warnings
# llmlint: ignore-end[changed_behavior_has_e2e]

# Ensures `just`, verifies the rest, then runs setup-llmlint. Runs automatically
# via the Claude Code SessionStart hook; this is the manual entry point.
# Provision the dev toolchain for a session. Idempotent, no-ops in CI.
session-setup:
    ./scripts/session-setup.sh

# Install/refresh the llmlint toolchain (oneharness + llmlint). Idempotent.
setup-llmlint:
    ./scripts/setup-llmlint.sh

# Kept OUT of `check` on purpose: the deterministic gate stays offline and
# credential-free. Config is the composed `llmlint.yml`.
# LLM-judge lint — the non-deterministic, harness-backed tier.
lint-llm *paths:
    @command -v llmlint >/dev/null 2>&1 || { echo "llmlint not installed — run 'just setup-llmlint'" >&2; exit 1; }
    llmlint {{paths}}

# CI runs this before the model tier so a broken config fails in milliseconds
# instead of spending a harness call.
# Fast, deterministic llmlint gate — no model calls, no harness credential.
lint-llm-validate *args:
    @command -v llmlint >/dev/null 2>&1 || { echo "llmlint not installed — run 'just setup-llmlint'" >&2; exit 1; }
    llmlint validate {{args}}

# The blocking `llmlint` PR check; `just gate` runs it before you push.
#
# The judge is non-deterministic — this tier has returned opposite verdicts on one
# unchanged tree — so the run goes through the cached Nx `lint-llm-diff` target
# rather than straight to llmlint: an unchanged tree judged against an unchanged
# base replays that run's own report instead of rolling the dice again. There is no
# verdict record to write, restore, or race on; Nx's task cache is the whole
# mechanism. The base ref is resolved to a commit *here*, before Nx hashes it, so a
# rebased or advanced base misses rather than replaying a verdict computed against
# a different comparison — and the resolved commit is reported with the verdict,
# because "green" means green against that commit.
#
# Only a clean run is cached, because Nx caches successful tasks only. Findings
# (llmlint exit 1) and a toolchain that never reached a verdict (exit >= 2) both
# re-judge on the next invocation. A wrong *green* sticks until the tree, the base
# commit, or the judge configuration moves — `--skip-nx-cache` re-judges but
# neither reads nor writes the cache, so the next ordinary run replays the same
# entry.
#
# `just lint-llm-diff <base> --skip-nx-cache` is the one supported way to force a
# real re-judge, and it is deliberately per-invocation: the trailing arguments go
# to Nx now rather than to llmlint. An ambient global cache skip
# (`NX_SKIP_NX_CACHE` / `NX_DISABLE_NX_CACHE`, exported to re-judge this tier and
# inherited by everything else) is reported and ignored here, because it would
# re-roll a non-deterministic judge from every unrelated command and break the
# journeys whose contract is cache replay. Every other Nx target still honours it.
#
# The report comes from `.logs/llmlint-diff.report`, the target's declared Nx
# output, rather than from what Nx forwarded: Nx resolves a task on its child's
# `exit` event, which can beat the drain of a pipe the child wrote to just before
# exiting, so a report finished milliseconds before exit is lost from the stream
# intermittently. The file is stored on a clean run and restored on a hit, so a
# replayed verdict says exactly what the judged one did. It is also the thing to
# `tail -f` while the judge is still thinking.
#
# The base ref and the passthrough Nx arguments are a caller's, so they reach the
# shell as positional arguments rather than as interpolated text — nothing the
# caller supplies is parsed as shell before it is checked. What checks it is
# `git rev-parse --verify`, which either yields a commit id or refuses the run: the
# target is handed that resolved id and re-checks its shape, and the passthrough
# arguments go to `scripts/nx.sh` as separate argv words for Nx to accept or reject.
#
# Provenance comes from Nx's own cache reporting, which Nx writes itself: the task
# line it annotates, or the summary line it prints only when it replayed a task
# instead of running it. Both are matched because only the first is safe at any
# size, and both are matched with the colour stripped — Nx renders them with ANSI
# escapes under some parents (a nextest-driven run is one), which pushes the escape
# in front of the anchor and reports every replay as a fresh judgement.
# `tests/llmlint_cache.rs` asserts both the judged and the replayed wording, so an
# Nx upgrade that renames them fails the suite rather than quietly reporting every
# run as freshly judged.
# llmlint scoped to the files this branch changed since it forked from main.
lint-llm-diff base="origin/main" *nx_args:
    @command -v llmlint >/dev/null 2>&1 || { echo "llmlint not installed — run 'just setup-llmlint'" >&2; exit 1; }
    @# llmlint: ignore[tool_output_is_signal] The judge's per-rule report and its one-line provenance are this tier's product; a quiet success here would delete the tier's result and leave a replayed run saying less than a fresh one. `@#` so the directive itself stays out of that report.
    @base_sha=$(git rev-parse --verify --quiet "$1^{commit}") || { echo "lint-llm-diff: '$1' does not resolve to a commit; fetch it or pass an existing base" >&2; exit 1; }; if [ -n "${NX_SKIP_NX_CACHE:-}${NX_DISABLE_NX_CACHE:-}" ]; then echo "lint-llm-diff: ignoring the ambient global Nx cache skip; force a fresh judgement of this tier alone with 'just lint-llm-diff $1 --skip-nx-cache'" >&2; fi; unset NX_SKIP_NX_CACHE NX_DISABLE_NX_CACHE; report=.logs/llmlint-diff.report; echo "lint-llm-diff: base $base_sha; the judge's report lands in $report ('tail -f' it to follow a fresh run)" >&2; capture=$(mktemp) || { echo "lint-llm-diff: could not open temporary storage for Nx's output; free disk space and retry" >&2; exit 1; }; trap 'rm -f "$capture"' EXIT; status=0; LLMLINT_DIFF_BASE_SHA="$base_sha" ONEAGENTGRAPH_NX_SHOW_OUTPUT=1 bash scripts/nx.sh run oneagentgraph:lint-llm-diff "${@:2}" >"$capture" 2>&1 || status=$?; if [ "$status" -eq 0 ]; then cat "$report" 2>/dev/null || echo "lint-llm-diff: the task left no report at $report" >&2; else { cat "$report" 2>/dev/null || echo "lint-llm-diff: the task left no report at $report"; } >&2; fi; cat "$capture" >&2; esc=$(printf '\033'); if sed "s/${esc}\[[0-9;]*[a-zA-Z]//g" "$capture" | grep -qE '^Nx read the output from the cache instead of running the command|^> nx run oneagentgraph:lint-llm-diff +\[(local cache|remote cache|existing outputs match the cache)'; then echo "lint-llm-diff: replayed the recorded verdict for base $base_sha (Nx cache hit)" >&2; else echo "lint-llm-diff: judged this diff against base $base_sha (Nx cache miss)" >&2; fi; exit "$status"
