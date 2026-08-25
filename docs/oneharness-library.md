# The `oneharness run` boundary inventory

> **Status: converted.** A `kind: oneharness` member's turn is
> `oneharness_core::io::run::run_supervised`, called on a thread of this process.
> No `oneharness` process is spawned for the turn itself — the harness the turn
> selects is still a child process, and it is oneharness that starts it. Every
> "replaced by …" below is now past tense: it names the seam that took over and
> where it lives. `src/harness.rs` is the module; `src/judge.rs` is its twin for
> the other member kind, and the two are deliberately the same shape.
>
> **What provides it** is `oneharness-core` **0.12.0**, the release this crate
> links today: a
> [`ProcessSupervisor`](#grouping-the-seam-this-whole-document-was-written-to-get)
> trait with a `spawning(&mut Command)` / `spawned(&Child)` pair, and
> `io::run::run_supervised` as the entry point that takes one. That is the seam
> this document proposed, added upstream as proposed — a second entry point
> rather than a field on `RunControls`, so the exhaustive literals embedders had
> already written kept compiling. Both names arrived in **0.10.1** and are
> unchanged here, re-measured against 0.12.0 rather than assumed; the floor is a
> **compile floor**, not a preference, because below 0.10.1 neither exists and
> `src/harness.rs` does not build, which is what stops a future edit from quietly
> falling back to unsupervised `run`. The version emphasised above is the linked
> one rather than the first one for the reason `tests/inventory.rs` checks it: a
> reader asks this document what *is* driving their run, and a fixed blocker once
> went unnoticed for two dispatches behind a version nobody had re-read.

Why `src/harness.rs` calls a library where `src/member.rs` used to spawn a
process, what that process boundary provided, and the seam that replaced each of
those things — written down *before* the conversion, because this conversion's
two predecessors each dropped an OS-level guarantee nobody had named
(onejudge→oneharness dropped no-output teardown; oneagentgraph→onejudge dropped
process grouping), and both were found from a red platform check afterwards
rather than from a list beforehand. The list came first here, and this is it.

Nothing here owns the names it argues from, so **`tests/inventory.rs` is the
drift gate**: it reads this file at compile time and holds every upstream field
against the real type, every wire name against the types this crate serializes,
and the version it names against `Cargo.toml`.

## The call

`run_supervised` returns the report, takes an event sink, takes a cancel token,
and takes this crate's claim on each harness child — the four things that make
the hop collapsible. Every argument `src/invoke.rs` used to build for a
single-sided member maps onto a `RunRequest` field, and
`crate::invoke::HarnessLaunch` is the value it builds instead of an argv:

| argument | field |
|---|---|
| `--config <p>` | `config` |
| `--cwd <d>` | `cwd` |
| `--events` | `events` |
| `--stream` | `stream: Some(true)`, and only for a member whose own resolved config leaves its run streaming |
| `--compact` | none, by design: it is how the CLI *prints* a report onto a pipe a reader took a line at a time — which is exactly why it took the streaming flag's place for a member whose config asks for a schema, and why the library call needs neither, the report being the return value |
| `--prompt <text>` | `prompt` |

`tests/inventory.rs` parses that column and names each field in a `RunRequest`
literal, so a rename upstream fails there rather than here.

The `prompt` is the one value built **per turn** rather than once per member:
`HarnessLaunch::request` takes it, because a member's declared `pre_turn` views
run immediately before each turn and what they printed goes in front of the
member's own prose — see `src/preturn.rs`. A member declaring none is handed
`HarnessLaunch::prompt` exactly, so nothing about this request changes for it.

## What the process boundary provided, and what replaced it

**A per-member environment — replaced by the process's own.** `src/member.rs`
used to spawn with `env_remove(invoke::PROCESS_WIDE_HARNESS_ENV).envs(graph_env)`,
so an ambient `ONEHARNESS_HARNESSES` could not beat the config the graph named. A
library run reads *this* process's environment instead — and that environment is
byte for byte the same one: `src/run.rs`'s `export` removes that variable here,
once, before the first member thread starts, and adds the graph's own `env:`
block over it. It has to, because a two-party member was already in-process and
inherits exactly this environment; `tests/e2e/selection.rs`'s
`each_side_runs_the_config_it_was_given` is the journey that holds it. So the
`RunRequest` needs no environment layer of its own, and `RunRequest::no_config` —
which would discard `RunRequest::config` along with the `ONEHARNESS_*` layer — is
not set.

**The `--cwd` contract — replaced by a parameter, and by an anchor.**
`RunRequest::cwd` takes the directory per call, so one process hosts every member
without any of them touching the process's own working directory. That is the
same rule `src/invoke.rs` already stated for a two-party member's
`JudgeLaunch::worktree`: everything per-member rides a value or a generated file,
never process-wide state.

The parameter alone is not enough, and this is the trap the conversion had to
step over. The child's own working directory *was* load-bearing: this crate set
it to the member's scratch, so a **relative** `--cwd` — the default `.`, or a
relative `--dir` — was resolved by that child against the scratch. Delete the
child and `RunRequest::cwd` is resolved by the host instead, silently relocating
every default-directory member to wherever the run was launched. `invoke::
scratch_anchored` performs that resolution before the value leaves this crate, so
the member-visible directory is byte for byte the one the spawned turn produced.
It is applied to a single-sided member only: onejudge starts the two-party
member's `oneharness run` from *this* process, so a relative worktree has always
resolved against the host there, and anchoring it would be the same regression in
the other direction.

Two journeys hold the pair, and each is worthless without the other.
`tests/e2e/library.rs`'s
`a_members_relative_directory_resolves_in_its_scratch_and_the_host_stays_put`
asserts both halves at once — the harness ran in the member's scratch, and the
hosting process never moved — from the one vantage point that can see the second,
which is inside the hosting process.
`tests/e2e/dispatch.rs`'s
`a_run_that_names_no_directory_hands_both_member_kinds_the_same_default` holds the
two member kinds against each other, so the anchor cannot spread to the kind that
must not have it.

What did disappear is the child's working directory as a *thing to read*; nothing
read it, because `--config` was written absolute and oneharness starts project
discovery from `--cwd`.

**Streaming — replaced by a typed sink.** `RunControls::events` with
`RunRequest::stream: Some(true)` delivers each normalized `ActionEvent` as it
occurs, which is what the `--stream` NDJSON lines carried. The reader in
`src/member.rs` that parsed those lines back apart is gone, exactly as the
onejudge conversion's was, and `SinkStep::Stop` is that path's
`ControlFlow::Break` — answered as soon as the run is cancelled, so a condemned
member stops at its next event rather than only when the terminate path reaches
it.

One detail the typed sink makes visible: oneharness's stream envelope carries no
turn index, so the NDJSON reader defaulted every event to turn 1. `oneharness run`
*is* one turn. `src/harness.rs` therefore opens turn 1 on the first event and
never renumbers — the same stream, from the shape rather than from a default.

That shape is also what closes the turn. A single-sided member's turn *is* its
run, so its `turn-completed` carries the accounting off the candidate that ran —
the one `FallbackReport::ran` names, else the first of `RunReport::results`, which
is oneharness's own ordering rule and the one onejudge reads it by — and its
bounds are the run's:
opened before the request is handed to the engine, closed when the engine answers.
`RunResult::usage` is field for field with `crate::event::Usage`, and
`src/harness.rs` destructures it rather than round-tripping through JSON so a
signal added upstream is a compile error here.

Whether a given member streams at all is not this crate's to decide, and the
conversion kept it that way: `src/invoke.rs`'s `reporting` reads the member's own
resolved config, because a config asking for a schema cannot stream in oneharness
and one asking for both is refused before launch. So `HarnessLaunch::stream`
carries that decision rather than a constant, and it reaches `RunRequest::stream`
as `Some` in both directions — never `None`, which would hand the question back to
the config layer that has already answered it.

**Control binding — available, and deliberately not taken.** A single-sided
member's argv carried no `--control`, so it recorded no `control::Turn` and
`oneagentgraph interrupt` had no lever on one. `RunRequest::control` and
`RunReport::control` are the same pair `src/judge.rs` reads for a two-party
member, so the library path *could* grow the lever. It does not, in this change:
the bar here is that a converted member behaves identically, and giving one kind
of member a new lever is a contract-visible feature rather than a conversion.
Named here because the seam is now free — see the follow-ups.

**Accounting — replaced by the return value.** The exit status fed
`member::Kind::settled` and the stderr tail fed `member-died`'s `detail`; both
come from `RunOutcome` (`exit_code`, `failure_summary`) and from the
`OneharnessError` a refused *request* returns. The fallback chain is no longer
re-parsed out of a terminal `result` line: it is read off `RunReport::fallback` as
the type it already is, and each candidate's reason is published through
oneharness's own `as_str()`, so a respelling upstream fails a test here rather
than reaching a consumer.

**Isolation of failure — replaced by the thread seam.** A crashed child could not
take the graph down; an in-process panic can. `src/judge.rs` already answered this
and its answer is reused: the engine runs on a thread and answers over an
`mpsc::channel`, so a panic drops the sender and `RecvTimeoutError::Disconnected`
becomes *that member's* `provider-failure` death.
`harness::tests::a_panicking_engine_kills_its_own_member_and_not_the_process`
drives the real supervision loop against a real panicking thread. Nothing here
builds with `panic = "abort"`. The thread is started through
`std::thread::Builder`, not `std::thread::spawn`, because the plain spawn answers
a host that will not give this run one more thread by panicking — which would be
one member's resource limit taking the graph down. That is the half `src/judge.rs`
did *not* already have, and it has it now: see the blocker below.

**Test seams — replaced by the same seam.** The suite substitutes a paid harness
at oneharness's own `ONEHARNESS_BIN_<ID>`, set through the graph's `env:` block —
which `export` has already put on this process, so the override is read from one
environment either way (`RunRequest::bin` is the explicit form, unused). What the
spawn *also* gave the suite was `member-started`'s `program`/`args`/`cwd` as the
record of what was decided; those are now `runner: library`'s `config`/`worktree`,
which is why `HarnessLaunch` is a value this crate builds and can assert on rather
than a `RunRequest` assembled inline at the call.

**Teardown of the harness's descendants — replaced by the cancel token.**
`RunControls::cancel` terminates each harness tree through
`oneharness_core::io::runner`'s `Finish::Terminate`, which is the seam grown
upstream for the *first* of the two prior regressions. It covers every leg of the
CI matrix, because oneharness owns the harness's own tree on each: a POSIX harness
is its own process-group leader and is signalled as a group, and a Windows one is
spawned `CREATE_SUSPENDED` into oneharness's own `KILL_ON_JOB_CLOSE` job and ended
with `TerminateJobObject`. A graph process that dies outright is covered on
Windows by that same flag on *this crate's* job, since the handle is this
process's.

This is why `src/harness.rs`'s watchdog is shorter than `src/judge.rs`'s: there
is one lever rather than an ask-then-reap escalation. It cancels, reaps whatever
the stamp still finds, and gives the engine a bounded grace before giving up on
the thread rather than on the run.

## Grouping: the seam this whole document was written to get

Teardown is not grouping. `scratch::Group` is how a **second** process — and this
crate's own activity watchdog — finds a member's tree, and the two platforms
prove membership by opposite means. `crate::harness::HarnessSpawn` is the
`ProcessSupervisor` that holds both, and it is the same two moments, against the
same `scratch::Group`, that `crate::judge::MemberSpawn` hands onejudge. One seam,
two engines.

- **POSIX.** The group *is* the `scratch::SCRATCH_ENV` stamp, fixed at `exec` and
  shed by nothing. `spawning` puts it on the harness's own `Command`, which is
  the last look before the fork, so membership needs nothing of the child itself
  and reparented descendants are in the group by construction. Nothing here
  re-parents a process group — upstream is explicit that a `pre_exec` `setpgid`
  in the hook would *transfer* teardown ownership — so oneharness keeps owning
  the teardown of the tree it spawned and this crate's reap reaches the same
  processes through the stamp. `spawned` is a no-op.
- **Windows.** The group is a *named* job object, and joining it is
  `AssignProcessToJobObject` on the `Child`. `spawned` runs while the harness is
  still `CREATE_SUSPENDED` — oneharness resumes it once the hook returns — so the
  assignment cannot miss a descendant. A job assignment **nests**, so the harness
  sits in oneharness's job *and* one of this member's, and either side's teardown
  ends it. `spawning` sets `CREATE_SUSPENDED` too, because on Windows
  `creation_flags` *replaces* rather than adds and dropping it would reopen the
  race it exists to close.

**"One of this member's", not "this member's", and that is the rule the
conversion had to learn.** Nesting is not free composition: assigning a process
that is already in a job makes the job it is assigned to a **child** of the one
it was in, and a job has exactly one parent. oneharness creates a fresh job per
spawn, so a member's own job can take the first harness — becoming a child of
that spawn's job — and the *second* assignment asks it for a second parent, which
the kernel refuses. `scratch::Group::join` therefore mints one job per
already-contained child, named from the member's scratch and numbered, and
records each in the same `owner.job` the base name is in; `groups_under` opens
every name the directory derives. `Group::adopt` is unchanged and still uses the
member's own job, because the children it takes — `Group::spawn`'s, and the bare
`oneharness run` onejudge starts for each side — are in no job when this crate
first sees them, and their harnesses join by parentage rather than by assignment.

This is the regression the conversion introduced and the CI matrix caught: with
the shared job, a member whose fallback chain reached a second candidate had that
candidate's grouping refused, which cancels the run — so the candidate came back
`Status::Cancelled`, which oneharness (rightly) does not step past, and the chain
stopped one candidate early with only the first published as `fallback-advanced`.
`selection::a_chain_that_refuses_every_candidate_reports_each_one_and_fails` is
what fails on that, and it is a two-candidate chain because the whole point of
A17 is that a chain names every identity that refused.

**The resume is oneharness's, and that is why `Group::adopt` was split.**
`Group::adopt` assigns *and* resumes, which is right for a caller that spawned
the child itself — `Group::spawn`, and `crate::judge`'s hook, where onejudge
knows nothing of suspension. The supervisor hook must not resume: upstream
enumerates the child's primary thread after the hook returns and reads a thread
it cannot find as a spawn failure, tearing the tree down. A hook that resumed
first would let the harness run, possibly exit, and lose that thread. So
`Group::join` is adoption without the start, and `src/harness.rs` uses it.

**Neither hook can refuse a spawn** — upstream's methods return nothing, because
the run owns the child's lifetime. So a grouping failure cancels the run and is
kept for the death it becomes. That is the same direction the other two call sites
choose (a harness this process cannot reach is not left running), reached by the
only lever a hook has; oneharness still owns the tree it spawned, so the
cancellation is what ends the ungrouped harness.

What this buys, stated as the two guarantees that would otherwise have dropped:

1. `scratch::work` is what the activity watchdog observes, and it is the CPU
   charged to the stamped tree. An empty tree is an idle tree, so a member blocked
   on a harness that is working would be condemned by the rule written to stop
   exactly that — `member::Stall`.
2. `scratch::reap`, which is `oneagentgraph cancel --kill` reaching a run from
   another process, would find nothing to end and report a cancelled member that
   is still billing.

## What the conversion changed on the wire, and what it did not

`member-started` for a single-sided member is `runner: library` with
`engine: oneharness`, its `config`, and its `worktree` — the same three fields a
two-party member has always published, distinguished by the `engine` the contract
put there for exactly this. `member-died` no longer carries `exit_code`,
`disposition` or `stderr_tail` for any member, which is what `docs/contract.md`
already says: those are "a **child process's** facts, present only for a member
that was one", and none is.

Everything else is the same stream. Both `runner` values remain declared types —
a consumer that reads `process` still parses one.

**`docs/contract.md`'s launch-boundary bullet is the one thing the conversion
changed there**, and it is prose rather than schema: the contract owner approved
replacing "still `oneharness run`, a child process" with what the code does. No
*interface* moved, which is why the conversion needed nothing of the schema:
`runner: library|process`, the `engine` that tells the two kinds apart, the
`cause` set, and `member-died`'s three process-scoped facts are all exactly as
they were. The evidence the bullet now rests on is `src/harness.rs`, which calls
`oneharness_core::io::run::run_supervised` — a call that returns `RunOutcome` and
takes `RunControls::events`, the two capabilities the old bullet named as its own
sunset — and `tests/e2e/dispatch.rs`'s
`a_single_sided_members_turn_spawns_no_oneharness_process`, which settles a member
with `ONEAGENTGRAPH_ONEHARNESS_BIN` pointing at a binary that does not exist.

## The one blocker left

**The panic-containment criterion has no real-interface journey, because
nothing user-reachable panics.** The bar wants a member turn driven through the
compiled binary whose library path panics. The `RunRequest` this crate builds is
a closed template — only the config's content, the directory, the prompt text and
one boolean vary with graph input — and every layer between that input and the
engine is total: `bound_detail` counts `chars`, `summarize` matches every `Value`
shape, `Emitter` holds a poison-tolerant lock, and `oneharness_core` documents
request problems as `OneharnessError` and harness behaviour as a report, never a
panic. So no graph, task or config reaches the arm, and driving one would need a
fault-injection seam in the production path — which is a change to this
repository's single-fake testing invariant, not a test to write under it. What
clears this is that invariant's owner widening it, or an upstream hook that makes
a run panic on demand. Until then the containment is covered at the supervision
loop by `harness::tests::a_panicking_engine_kills_its_own_member_and_not_the_process`
and its twin in `crate::judge` — each drives its module's real loop against a real
panicking thread, which is a unit test and not the journey the bar asks for.

What *is* settled is that both member kinds are contained the same way. Before the
conversion a harness panic was a dead child process and a onejudge panic was a
dead member, so only one path needed the seam; now neither kind has a process to
crash instead. `src/judge.rs` therefore spawns through `std::thread::Builder` as
`src/harness.rs` does — a host that will not give this run one more thread refuses
that *member* rather than panicking the graph — and its supervision loop is split
out of `run` for the same reason `harness::supervise` is: so the containment can
be driven rather than asserted.

## Follow-ups this change deliberately did not make

Neither is a correction owed; each is an enhancement the conversion makes
reachable, and each would widen the approved contract — so each is a proposal to
its owner rather than an edit.

1. **`cause` could name four more failure kinds.** `oneharness_core`'s
   `FailureKind` is a wider set than the closed `cause` vocabulary:
   `session_not_found`, `tool_deferred`, `untrusted_directory` and
   `input_too_large` have no spelling there. A dead single-sided member therefore
   reports `unclassified` with the run's `failure_summary` as its detail, rather
   than a partial map that would report four kinds as something they are not.
   Naming them widens a closed set consumers branch on, so it is a proposal.
2. **A single-sided member could become interruptible.** `RunRequest::control`
   and `RunReport::control` are right there, and `src/judge.rs` already reads that
   pair. Adding it would give `oneagentgraph interrupt` a lever on a member kind
   that has never had one — a feature, and a contract-visible one.

## Two things that are not this hop

**`src/smoke.rs`'s `oneharness run`** proves that *this host's* launch path
reaches an identity, so it has to take the path an operator's install takes — the
binary on `PATH`, at whatever version is there. Collapsing it into the linked
library would prove the library works, which is not the question it asks. The
reason is written at that site.

**`src/control.rs`'s `oneharness interrupt`** is the other remaining hop, and
`src/control.rs` documents its own collapse condition: there is no
`io::control::interrupt` equivalent on the public surface. That one is unchanged
by this conversion.

Both are why `just`'s `oneharness-version` pin still exists and is still separate
from `Cargo.toml`'s. What changed is which question each answers: the linked
version now selects the chain, classifies each refusal and writes the report for
a `kind: oneharness` member, while the installed CLI governs `interrupt`, `smoke`,
and the `oneharness run` onejudge starts per side per turn.
