# AGENTS.md

Durable instructions for anyone — human or agent — working in this repo.
Terse on purpose: this file is always-loaded context.

> `CLAUDE.md` is a symlink to this file — edit `AGENTS.md` only.

## What this is

`oneagentgraph` composes agents into a **graph**, constructs onejudge/oneharness
invocations for each member, and merges their outputs into **one NDJSON event
stream**. It is the reusable multi-agent layer extracted from `ai-orchestrator`:
one config file and one CLI call, usable outside that repo's opinionated
workflow.

It owns **no** harness/model/fallback logic. The graph YAML names an oneharness
config file per role/side; oneharness keeps owning identity chains, fallback,
model pins, and quota classification, and onejudge keeps owning the two-party
conversation. Do not grow harness selection here.

Ships as a Rust library plus the `oneagentgraph` binary, distributed on
crates.io, PyPI (`oneagentgraph-cli`), and npm (`oneagentgraph-cli`).

## The contract is the source of truth

[`docs/contract.md`](docs/contract.md) is the **approved, verbatim** contract:
the shared event envelope, the graph config schema, the CLI surface, the event
kinds, and the liveness rules. It is committed as approved and is not edited to
match the code — the code is written to match it. A change to the interface is a
proposal to the planner who owns that contract, never a unilateral edit.

`tests/contract.rs` parses the fenced blocks **out of `docs/contract.md` itself**
and drives them through the public types, so the doc and the types cannot drift.
Adding a contract type without extending that test leaves the doc unproven.

### Interface-only, for now

This repo is deliberately at an **interface-only** stage: public types, enums,
error types, serde config-schema structs, and the clap CLI argument surfaces
compile and are proven by the contract tests — and nothing implements them.

Rules while that holds:

- No method bodies beyond derives, trivial field constructors, and serde
  `default` helpers.
- Every CLI subcommand parses per the contract and then refuses loudly with
  `NOT IMPLEMENTED` on stderr and **exit code 3**. That code is scaffolding for
  this stage; the contract's own codes are `0` success, `1` a member failed or
  died, `2` invalid config.
- Add no public item `docs/contract.md` does not name. A helpful-looking
  convenience method is interface drift.

The implementation lands as its own change; delete this subsection with it.

## Stack and composition

- **Product shape:** cli (a Rust library + the `oneagentgraph` binary)
- **Language(s):** rust (plus Bash provisioning, Node packaging scripts, and
  YAML/JSON/TOML config)
- **References composed:** base.md, shapes/cli.md, languages/rust.md,
  intersections/rust-cli.md, ci.md, llmlint.md, releasing.md, monorepo.md
- **Excluded, and why:** `install.sh` / a composite `action.yml` / a container
  image — the documented install surfaces are crates.io, PyPI, and npm, all of
  which *carry* the artifact rather than downloading a release asset by name, so
  there is no second asset-naming contract to drift. The GitHub Release archives
  are attached for manual download only. asdf / direnv — the committed
  `rust-toolchain.toml`, `Cargo.lock`, and `package-lock.json` already pin the
  workspace. A benchmark tier — nothing here is a hot path yet.

## Command surface

`just --list` is the index; do not hand-roll equivalents.

- `just bootstrap` works from a clean clone.
- `just check` is the deterministic gate: format check, clippy `-D warnings`,
  tests (unit + contract + e2e) with coverage enforced, and rustdoc with
  warnings denied. It fails on any issue — no warnings-only mode.
- `just gate` is the complete pre-push bar: `check` plus the diff-scoped llmlint
  tier. A change is not done until `just gate` is green.
- `just deps-check` (`cargo deny` + `cargo machete`) and `just msrv` are their
  own steps: the first needs a network advisory DB, the second another
  toolchain, so neither belongs in the offline deterministic gate. CI runs both.

The repo-wide verbs delegate to **Nx**, which fans a uniformly-named target out
across every project; what a target *does* stays with its project. Never loop
over projects by hand in a recipe.

## Invariants (non-negotiable)

- **Coverage is enforced at 95% line coverage** (`cargo llvm-cov
  --fail-under-lines 95`). `just check` fails below it.
- **Tests are realistic — never mock the layer under test.** The e2e suite
  spawns the *compiled binary* as a subprocess and asserts on exit code, stdout,
  and stderr; the packaging suite assembles the real npm package around the real
  binary and runs the launcher. An in-process `main()` call is not an e2e.
- **E2E runs inside `just check`**, never `#[ignore]`-d out of it.
- **Validate external input at its trust boundary.** Graph configs and event
  envelopes are external input: the schema structs reject unknown fields where
  serde allows it, so a typo fails loudly instead of being silently dropped.
- **Secrets never enter the tree.** `gh-secrets.json` names the required secrets
  and where they come from; the values live in the platform secret store.

## Tests are context engineering

Agents do nearly all the work here, so the suite is the only QA loop.

- `tests/contract.rs` — the committed contract text drives the public types.
- `tests/e2e/` — every CLI journey a user reaches, happy path and failure:
  `--help`, `--version`, each subcommand's refusal and its exit code, an unknown
  subcommand, and a missing required argument.
- `npm/test/` — the packaged launcher resolves and execs the real binary, and
  reports a missing platform package as an actionable error.

When a journey lands, its real e2e lands with it.

## Commits, releases, and merging

**Squash-merge only**, auto-merge on, head branches deleted on merge. The PR
title becomes the squash subject and the PR body the squash message, so the PR
title *is* the release-driving commit and is linted against Conventional Commits
as a required check. PRs follow `.github/pull_request_template.md` (terse
**What** / **Why**).

Branch protection on `main` requires every gating check — `gate`, `pr-title`,
and `llmlint` — with linear history, no force-pushes, and admins able to
override. Re-apply it with the create-repo skill's
`setup_github_governance.py gate pr-title llmlint`.

**Releases are fully automated; the only human action is merging a PR.**
`release-plz` is the single version driver: it opens a release PR, and merging it
tags `vX.Y.Z` and cuts the GitHub Release. That Release — created with a PAT,
because a tag from the default `GITHUB_TOKEN` triggers nothing — fires
`release.yml`, which builds the archives, wheels, and npm packages and publishes
them. **Nothing else writes a version:** maturin reads it from `Cargo.toml` via
`dynamic = ["version"]` and `scripts/npm-build.mjs` stamps it from the same
place.

Bump policy, **pre-1.0**: `feat` → minor, `feat!` / `BREAKING CHANGE` → minor (a
breaking change pre-1.0 is not yet a major), `fix` / `perf` / `refactor` /
`build` → patch, and `chore` / `docs` / `ci` / `test` / `style` → no release.

## After the main task

Two standing goals beyond the ask: (1) engineer the context for next time — a
real e2e for any journey a bug slipped through, a script for a step done by
hand, a terse note here for what the code doesn't show; (2) keep the repo and
its environment clean and reproducible. Fold either in when it is the
lowest-error path; otherwise propose it. Skip busywork.
