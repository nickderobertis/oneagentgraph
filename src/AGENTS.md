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
| `invoke` | one member's argv, its generated configs, and the model pairing rule |
| `member` | one child process: its stream, its two watchdogs, its death |
| `run` | dependency order, cron members, the merged stream, the exit code |
| `scratch` | `owner.lock` ownership, proven descendant reaping |
| `event` / `render` | the wire envelope, and the text rendering of the same events |
| `history` / `health` / `smoke` | the read-only verbs |

## The seams that are easy to get wrong

**Each conversation side is pinned without a wrapper script.** onejudge gives the
judge side `oneharness run --config <judge_config>` and the agent side none, so
the agent side is pinned by *placing* its resolved config at
`<member scratch>/oneharness.toml` and running `onejudge` from there — oneharness
discovers it upward from its own working directory. The harness still works in
the graph's `--dir`, which onejudge passes through as `--cwd`.

**The two CLIs read their exit codes differently.** `onejudge` exits `1` for a
task it drove but did not complete, which is a settle; `oneharness` exits
non-zero when it could not run the turn at all, which is a death. `member::Kind`
is the one place that distinction lives.

**A test chain names bare identities, never variants.** `ONEHARNESS_BIN_*` keys
on a harness id and no spelling of it reaches a variant, so a chain naming
`claude-code:alternate` spawns the real paid provider with the double sitting
unused beside it. That is a money hazard, not a style point. `src/bin/` holds the
two doubles, behind the non-default `test-doubles` feature; keep them
deterministic and free of anything the crate does not already depend on.

**Provisioning installs both CLIs.** `just bootstrap` pins them, and the versions
live at the top of the `justfile`. `onejudge` is a **git pin** because its
streamed-provider contract is merged but unreleased — move it to a published
version as soon as one ships.

## Two things that bite

**A sentinel in a prompt matches prose.** The fake harness is steered by markers
in the prompt it is given, and that prompt is the whole rendered system prompt —
persona included. Every marker therefore carries a `fake:` prefix, because `hang`
is a substring of `change`, and a persona telling an agent to state a change's
blast radius once parked every turn of the suite.

**`writeln!` reaches a writer as two calls.** The body, then the newline. Any
writer that treats one call as one line — the text renderer did — emits a blank
line after every event. Buffer to the newline instead.
