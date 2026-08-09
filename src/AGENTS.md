# The crate

Rules that hold as this grows:

- **Add no public item the contract does not name** without a reason a reader can
  check. A convenience method, a `Result` alias, a builder: each is interface
  drift, and a consumer that pins to it gets a breaking change later.
- **Optionality is a decision, not a default.** Where the contract states a
  default (`stream: true`) or shows a field as `null` / `[]`, that reading is
  encoded. Where it neither states a default nor marks a field optional, the
  field is *required* — do not quietly relax one to make a config parse.
- **`#![warn(missing_docs)]` with `clippy -D warnings` means undocumented public
  items fail the gate.** Say what a thing is for, not what it is.

## Where each part of the contract lives

| module | what it owns |
| --- | --- |
| `config` | the graph YAML schema, and what `validate` can check without launching |
| `resolve` | a `ConfigRef` → bytes, content-addressed for the run record |
| `persona` | the persona delta schema, the merge onto a onejudge base, the shipped catalog |
| `invoke` | one member's launch, its generated configs, and the model pairing rule |
| `member` | a single-sided member's child process: its stream, its watchdogs, its death |
| `judge` | a two-party member, driven through onejudge's library in this process |
| `run` | dependency order, cron members, the merged stream, the exit code |
| `scratch` | `owner.lock` ownership, proven descendant reaping |
| `event` / `render` | the wire envelope, and the text rendering of the same events |
| `history` / `health` / `smoke` | the read-only verbs |

## The seams that are easy to get wrong

**Each conversation side is pinned without a wrapper script, and without a `cd`.**
onejudge gives the judge side `oneharness run --config <judge_config>` and the
agent side none, so the agent side is pinned by *placing* its resolved config at
`<member scratch>/oneharness.toml` and **naming that directory as the
conversation's worktree** — oneharness discovers a project config upward from
`--cwd`. Naming rather than entering, because every member of a graph now shares
one process: a member that `cd`-ed would pin its siblings too.

**Nothing per-member is exported.** Same reason. A member's `mode` and its
`ONEAGENTGRAPH_SCRATCH_DIR` ownership stamp go into that member's own resolved
oneharness configs (`mode`, and `[env]`, which oneharness gives to every harness
process it starts), so the stamp still reaches the harness fixed at `exec`. Only
the graph's own `env:` block is exported, once, before any member starts.

**A thread cannot be killed.** A watchdog on a two-party member therefore
escalates the way `cancel --kill` does — set the abort flag so the sink answers
the engine's next event with `ControlFlow::Break`, reap everything stamped for
the member, and after `TEARDOWN_GRACE` report the member dead and abandon the
thread. Never wait forever: a run that hangs on a member it already condemned is
the failure the watchdog exists to prevent.

**The two sides read their outcomes differently.** onejudge settles `1` for a
task it drove but did not complete; `oneharness` exits non-zero when it could not
run the turn at all, which is a death. `member::Kind` is the one place that
distinction lives.

**A test chain names bare identities, never variants.** `ONEHARNESS_BIN_*` keys
on a harness id and no spelling of it reaches a variant, so a chain naming
`claude-code:alternate` spawns the real paid provider with the double sitting
unused beside it. That is a money hazard, not a style point. `src/bin/` holds the
two doubles, behind the non-default `test-doubles` feature; keep them
deterministic and free of anything the crate does not already depend on.

**Provisioning installs one CLI.** `just bootstrap` pins `oneharness`, and the
version lives at the top of the `justfile`. `onejudge` has no entry: it is a
cargo dependency from crates.io, pinned by `Cargo.lock`, so there is nothing to
install and nothing on `PATH` to shadow it.

## Two things that bite

**A sentinel in a prompt matches prose.** The fake harness is steered by markers
in the prompt it is given, and that prompt is the whole rendered system prompt —
persona included. Every marker therefore carries a `fake:` prefix, because `hang`
is a substring of `change`, and a persona telling an agent to state a change's
blast radius once parked every turn of the suite. Both doubles ask through one
`steers` function that applies the prefix, so a marker added later cannot arrive
bare — which is how `complete-now` and `should-fail` stayed unprefixed for a
while after the rule was written.

**`writeln!` reaches a writer as two calls.** The body, then the newline. Any
writer that treats one call as one line — the text renderer did — emits a blank
line after every event. Buffer to the newline instead.
