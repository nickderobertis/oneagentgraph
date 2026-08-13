# AGENTS.md

Durable instructions for anyone — human or agent — working in this repo.
Terse on purpose: this file is always-loaded context.

> `CLAUDE.md` is a symlink to this file — edit `AGENTS.md` only.

## What this is

`oneagentgraph` composes agents into a **graph**, prepares each member's launch,
and merges their outputs into **one NDJSON event stream**. It is the reusable
multi-agent layer extracted from `ai-orchestrator`: one config file and one CLI
call, usable outside that repo's opinionated workflow.

It owns **no** harness/model/fallback logic. The graph YAML names an oneharness
config file per role/side; oneharness keeps owning identity chains, fallback,
model pins, and quota classification, and onejudge keeps owning the two-party
conversation. Do not grow harness selection here.

**onejudge is a library dependency, not a CLI.** A two-party member is driven
in-process, on a thread of this process, through onejudge's own config, plan, and
streamed run driver. `oneharness run` is still a child process — its library
surface prints its report to the process's stdout and returns only an exit code,
and this process's stdout is the merged stream — and so are the agent harness and
a `judge: {command: [...]}` provider. A **supervised** `oneharness run` stays a
child process whatever entrypoints appear upstream: the process group is what a
cancel, a watchdog, and a reap all reach a member's tree through, and an
in-process run has no tree to hold. `oneagentgraph interrupt` stays one too, and
for a reason the others do not share — the control socket is version-*equal*, not
negotiated, so the client has to be the same build as the run that bound it;
`src/control.rs` records the whole argument. `health` used to be on that list and
is not: `oneharness_core::io::usage::report` is the `usage` verb as a call, so the
sweep runs in this process from oneharness's own code. **Prefer the library at
every new site** — a hop that stays has to name what the library does not expose,
in the code, at the site.

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

The contract is implemented: `run` resolves a graph, builds every member's
invocation before launching anything, and merges what the members publish into
one NDJSON stream.

## Stack and composition

- **Product shape:** cli (a Rust library + the `oneagentgraph` binary)
- **Language(s):** rust (plus Bash provisioning, Node packaging scripts, and
  YAML/JSON/TOML config)
- **References composed:** base.md, shapes/cli.md, languages/rust.md,
  intersections/rust-cli.md, ci.md, llmlint.md, releasing.md, monorepo.md
- **Cross-repo dependencies: every one is a published version, and there is no
  git ref anywhere in the graph.** `cargo deny`'s `unknown-git = "deny"` with no
  `allow-git` beside it is what holds that, so the tree resolves from crates.io
  alone rather than from whichever host has a checkout. The `onejudge` floor is a
  floor rather than a preference — each bump of it buys a seam this crate's
  supervision is built on, and dropping below it stops compiling. The number and
  the reason for it live at the dependency in `Cargo.toml`, which is also where
  the next one goes; do not copy either here.
- **Excluded, and why:** `install.sh` / a composite `action.yml` / a container
  image — the documented install surfaces are crates.io, PyPI, and npm, all of
  which *carry* the artifact rather than downloading a release asset by name, so
  there is no second asset-naming contract to drift. The GitHub Release archives
  are attached for manual download only. asdf / direnv — the committed
  `rust-toolchain.toml`, `Cargo.lock`, and `package-lock.json` already pin the
  workspace. A benchmark tier — nothing here is a hot path yet.

## Command surface

`just --list` is the index; do not hand-roll equivalents. `just check` is the
deterministic gate and `just gate` is the complete pre-push bar — `check` plus
the diff-scoped llmlint tier — and a change is not done until `gate` is green.
`deps-check`, `msrv`, and `lint-windows` sit outside both because each needs
something a clean clone does not have — a network advisory database, a second
toolchain, a cross compiler; CI covers all three as jobs of their own.

The repo-wide verbs delegate to **Nx**, which fans a uniformly-named target out
across every project; what a target *does* stays with its project. Never loop
over projects by hand in a recipe, and declare a cross-project dependency in the
consuming `project.json` — an undeclared one silently drops that project out of
`nx affected`, so a pull request runs a gate that never touched it.

## Invariants (non-negotiable)

- **Coverage is enforced at 95% line coverage.** `just check` fails below it.
  Lower the bar only with the reason written here.
- **Tests are realistic — never mock the layer under test.** Drive the compiled
  binary as a subprocess and assert on exit code, stdout, and stderr; assemble
  the real package around the real binary. An in-process `main()` call is not an
  e2e, and every journey it covers runs inside `just check` rather than behind
  `#[ignore]`.
- **One seam may be faked, and only one:** the paid harness process, at
  oneharness's own `ONEHARNESS_BIN_<ID>` binary override. Everything else in a
  journey is real — this binary and `oneharness` as subprocesses, and the real
  onejudge engine linked into the binary under test.
- **Validate external input at its trust boundary.** Graph configs and event
  envelopes are external input: the schema structs reject unknown fields, so a
  typo fails loudly instead of being silently dropped.
- **Secrets never enter the tree.** `gh-secrets.json` names the required secrets
  and where they come from; the values live in the platform secret store.

When a journey lands, its real e2e lands with it.

## Commits, releases, and merging

**Squash-merge only**, auto-merge on, head branches deleted on merge. The PR
title becomes the squash subject and the PR body the squash message, so the PR
title *is* the release-driving commit and is linted against Conventional Commits
as a required check. PRs follow `.github/pull_request_template.md` (terse
**What** / **Why**).

`.github/CODEOWNERS` routes review, and each nested `AGENTS.md` owns the rules
for the tree it sits in; this file keeps only what is true repo-wide.

Branch protection on `main` requires **every** gating job in `ci.yml`, each
matrix leg by its rendered name, with linear history, no force-pushes, and admins
able to override. Apply it with the create-repo skill's
`setup_github_governance.py`, passing every context, and **re-apply it whenever a
job or a matrix is added or renamed** — GitHub holds the required set, nothing
reconciles it against the workflow, and a leg nobody required is advisory, which
auto-merge lands straight past.

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
