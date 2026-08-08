# The crate

Every public item here exists because `docs/contract.md` names it. Rules that
hold as it grows:

- **Add no public item the contract does not name** without a reason a reader can
  check. A convenience method, a `Result` alias, a builder: each is interface
  drift, and a consumer that pins to it gets a breaking change later.
- **Optionality is a decision, not a default.** Where the contract states a
  default (`stream: true`) or shows a field as `null` / `[]`, that reading is
  encoded. Where it neither states a default nor marks a field optional, the
  field is *required* — do not quietly relax one to make a config parse.
- **`#![warn(missing_docs)]` with `clippy -D warnings` means undocumented public
  items fail the gate.** Say what a thing is for, not what it is.
- **`tests/contract.rs` reads `docs/contract.md` itself**, so a type added here
  without a matching assertion there leaves the document unproven. Extend it in
  the same change.

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

`src/bin/` holds the two test doubles, behind the non-default `fake-provider`
feature. They are spawned as real subprocesses by the e2e suite, so keep them
deterministic and free of anything the crate does not already depend on.

## Two things that bite

**A sentinel in a prompt matches prose.** The fake harness is steered by markers
in the prompt it is given, and that prompt is the whole rendered system prompt —
persona included. Every marker therefore carries a `fake:` prefix, because `hang`
is a substring of `change`, and a persona telling an agent to state a change's
blast radius once parked every turn of the suite.

**`writeln!` reaches a writer as two calls.** The body, then the newline. Any
writer that treats one call as one line — the text renderer did — emits a blank
line after every event. Buffer to the newline instead.
