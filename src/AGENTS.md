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
| `persona` | reading a persona as a onejudge config fragment (`docs/persona-format.md`), the merge onto a onejudge base, the shipped catalog |
| `invoke` | one member's launch, its generated configs, and the model pairing rule |
| `member` | a single-sided member's child process: its stream, its watchdogs, its death |
| `judge` | a two-party member, driven through onejudge's library in this process |
| `control` / `note` | where an in-flight turn is addressed, and the transport that carries a role-addressed note into the conversation's own inbox |
| `run` | dependency order, cron members, the merged stream, the exit code |
| `scratch` | `owner.lock` ownership, proven descendant reaping |
| `event` / `render` | the wire envelope, the shared filter over it (`docs/event-filter-notes.md`), and the text rendering of the same events |
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
the graph's own `env:` block is exported, once, before any member starts. The
stamp also goes on each command as it is spawned, by `Group::prepare` — on POSIX
the stamp *is* the group, so a command that reached the kernel without it would
be outside its group whatever else was recorded.

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

**`scratch` is the one module a Linux `check` never compiles all of.** Its
`cfg(windows)` half is the whole liveness layer again in job objects, and the
first thing that reads a line of it is `cross (windows-latest)` — a required
check a CI round-trip away. `just lint-windows` runs the same clippy against it
here; it asks for a target and a cross compiler `just bootstrap` deliberately
does not install, and says which when either is missing.

**To prove a Windows journey red, compile the layer out rather than revert it** —
`cfg(all(windows, not(windows)))` on the `cfg(windows)` module, widening the
fallback to `cfg(not(unix))`. `tests/e2e/liveness.rs` records which journeys that
turns red and which cannot, and why.

**A two-party member is grouped through onejudge, not around it.** Only the
caller of `CreateProcess` can put a child in a job object, and since onejudge
became a library that caller is onejudge — so nothing this crate spawns is left
to group. onejudge's `Plan::with_spawn_hook` is the seam — and the reason the
floor in `Cargo.toml` is where it is: `judge::run` opens the member's `Group` and
hands it over,
and onejudge installs it on **both** backends of a `split`, so the worker's
harness and the judge's land in the same group. `Group::prepare` and
`Group::adopt` exist for that hook — its two methods are the before-the-fork and
after-the-process moments the two platforms need, and `Group::spawn` is the same
pair for the commands this crate does spawn. Never reach for a shim binary that
re-spawns itself into the job, or a local copy of onejudge's `execute`.

**A note is routed by the conversation; an interrupt is aimed at one socket.**
`control::interrupt` addresses the agent side and nothing else, which is why a
ruling delivered that way reached the worker and never the judge. `control::note`
offers the note to the **member**, and the routing is `onejudge`'s: only the
engine driving a conversation knows which side of it is live, so `judge::run`
hands that engine the inbox end of an `onejudge::note::Notes` channel through
`Plan::with_notes` and lets it decide. A note arriving during the worker's turn
reopens that turn carrying it *before* the supervisor is consulted — which is how
the judge receives the note together with the worker's response — and one
arriving while the supervisor is deciding re-takes that decision with the note in
hand and rides the response to the worker. `interrupt` is untouched and stays the
lever for a member with no conversation at all.

**The shapes are onejudge's, and `crate::note` re-exports them.** `Addressee`,
`Note`, `Accepted` and `Undelivered` are declared in `onejudge::note`, which is
where the approved contract puts them. Do not redeclare one here to add a field
or a method: a second declaration is a shape that drifts, and a note that
satisfies the copy is still refused by the conversation it was written for. The
`onejudge` floor in `Cargo.toml` is where it is *for* this — the seam does not
exist below 0.7.0.

**What this crate owns is the transport, and it is one thread.** A note is
offered by a different process from the one running the member, so it arrives
through a `Spool` in the member's scratch and a `Courier` carries it into
`Notes::send`. On a thread of its own, and that is load-bearing:
`send` blocks until the conversation has disposed of the note — for a
supervisor-side delivery, until its re-taken decision comes back — so servicing
the spool from the supervision loop would put a judge invocation between two
heartbeats. `Ending::end` is the other half: it records the conversation's own
terminal refusal so a note arriving afterwards is refused rather than spooled to
nobody, and it answers anything the courier had not reached.

**`onejudge::note::Accepted` and `Undelivered` are not `Serialize`, so the spool
mirrors them.** That mirror is what onejudge's own documentation asks a transport
for, and it is mapped exhaustively in both directions in `note.rs` — a variant
added upstream fails this build rather than being dropped in transit. It is the
one place a note shape is written out here, and it is a wire format rather than a
second contract.

**The endpoint is a spool directory, not a socket.** The approved contract says
socket; a member of this crate runs on Windows too, where a unix domain socket is
already why `control` reports *no controllable turn*, and a note seam that existed
on one platform only would be a delivery an operator could not rely on. What a
consumer sees is unchanged — `control.json` names the endpoint by path, and the
two ends meet nowhere else.

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
