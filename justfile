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
oneharness-version := "0.6.9"

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
# Full test suite without coverage instrumentation.
test-quick:
    @cargo nextest run --locked --all-features --status-level fail

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
# Reproduce the `cross (windows-latest)` leg here instead of waiting for CI.
lint-windows:
    @rustup target list --installed | grep -qx x86_64-pc-windows-gnu \
      || { echo "the Windows target is missing — run 'rustup target add x86_64-pc-windows-gnu'" >&2; exit 1; }
    @command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1 \
      || { echo "no cross compiler for the Windows target — install mingw-w64 (a dependency's build script needs one)" >&2; exit 1; }
    @cargo clippy --target x86_64-pc-windows-gnu --all-targets --all-features --locked -- -D warnings

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
# llmlint scoped to the files this branch changed since it forked from main.
lint-llm-diff base="origin/main" *args:
    @command -v llmlint >/dev/null 2>&1 || { echo "llmlint not installed — run 'just setup-llmlint'" >&2; exit 1; }
    llmlint --diff --diff-base "{{base}}" {{args}}
