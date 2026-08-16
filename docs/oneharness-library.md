# The `oneharness run` boundary inventory

> **Status: blocked, and not started. This document describes a conversion that
> has not happened.** A `kind: oneharness` member's turn is `oneharness run`, a
> child process, exactly as it has always been; nothing here has been applied to
> the code. Every "replaced by …" below is the *future* tense — the seam that
> would take over if the conversion ran — not a report of what
> `src/harness_process.rs` does now. For current behaviour read that module and
> `docs/contract.md`.
>
> **The blocker** is Windows process grouping, and its cause is
> `oneharness-core` 0.10.0's public API: [`RunControls`](#what-is-still-holding-the-hop-open-grouping-on-windows-only)
> exposes only `events`, `cancel`, `signal_cancel`, and `version`, and
> `io::process::Process` — which holds the two moments a caller would need — is
> `pub(crate)` inside a private `io::process` module. So an embedder has no
> pre-spawn or post-spawn hook at which to put each harness child into the
> member's named job object.
>
> **What unblocks it** is a `spawning(&mut Command)` / `spawned(&Child)` pair, or
> any equivalent seam that hands the caller the harness `Command` before the fork
> and the `Child` after it. That is upstream's to add, and it now **is** added —
> as the "any equivalent seam" arm rather than the `RunControls` arm. It is
> unpublished: oneharness PR #1260 is open, so the newest release crates.io serves
> is still the 0.10.0 named above, which does not carry it.
> [The proposal](#the-proposal-which-is-upstreams-rather-than-this-crates-to-build)
> below records the shape upstream settled on. Converting before that release
> exists silently breaks the activity watchdog and leaves a killed run's paid
> harnesses billing.

Why `src/harness_process.rs` is still a subprocess hop, what its process boundary
provides, and the seam that would replace each of those things — written down
*before* the conversion, because this conversion's two predecessors each dropped
an OS-level guarantee nobody had named (onejudge→oneharness dropped no-output
teardown; oneagentgraph→onejudge dropped process grouping), and both were found
from a red platform check afterwards rather than from a list beforehand.

The short version: `oneharness_core::io::run::run` is not the obstacle, and only
**one** guarantee is genuinely missing — process grouping, on Windows only. It is
upstream's to add and this document is the proposal.

Nothing here owns the names it argues from, so **`tests/inventory.rs` is the drift
gate**: it reads this file at compile time and holds every upstream field against
the real type and every wire name against `docs/contract.md`.

**Which test in it is load-bearing has changed, and the reason is worth keeping.**
This document used to rest that job on an *exhaustive* destructure of
`RunControls` — add the seam upstream, the suite stops compiling, and the compile
break is the notice. That signal can no longer fire. Upstream settled on an
entry point instead of a field, and did so *because* embedders destructure and
construct that struct exhaustively: a field there would break every literal
already written, making a purely additive capability a major bump. So
`RunControls` is now committed to staying the four fields it has, the destructure
will pass forever, and it is a pin on that commitment rather than a tripwire.

The signal is **`the_status_block_names_the_version_the_manifest_takes`** instead:
the status block above blames a version, that test reads the version out of
`Cargo.toml`, and the two have to agree. Bumping `oneharness-core` to the release
carrying the seam therefore fails the suite until this document is rewritten —
which is the same notice, taken from the one event that actually marks the
unblock. The conversion needs a *published* engine, not a merged upstream commit,
and the manifest is where that arrives.

## The call is there

`run` returns the report, takes an event sink, and takes a cancel token — the
three things `docs/contract.md` names as collapsing this hop — and every argument
`src/invoke.rs` builds for a single-sided member maps onto a `RunRequest` field:

| argument | field |
|---|---|
| `--config <p>` | `config` |
| `--cwd <d>` | `cwd` |
| `--events` | `events` |
| `--stream` | `stream: Some(true)`, and only for a member whose own resolved config leaves its run streaming |
| `--compact` | none, by design: it is how the CLI *prints* a report — which is exactly why it takes the streaming flag's place for a member whose config asks for a schema, and why a library call would need neither, the report being the return value |
| `--prompt <text>` | `prompt` |

`tests/inventory.rs` parses that column and names each field in a `RunRequest`
literal, so a rename upstream fails there rather than here.

## What the process boundary provides, and what would replace it

**A per-member environment — replaced, and already in place.** `src/member.rs`
spawns with `env_remove(invoke::PROCESS_WIDE_HARNESS_ENV)`, so an ambient
`ONEHARNESS_HARNESSES` cannot beat the config the graph named. A library run reads
*this* process's environment instead — and that environment has already been made
the same one: `src/run.rs`'s `export` removes that variable here, once, before the
first member thread starts, and re-adds it when the graph's own `env:` block asks
for it, which is byte for byte what the `Command` does. It has to, because a
two-party member is already in-process and inherits exactly this environment;
`tests/e2e/selection.rs`'s `each_side_runs_the_config_it_was_given` is the journey
that holds it. So a `RunRequest` needs no environment layer of its own, and
`RunRequest::no_config` — which would discard `RunRequest::config` along with the
`ONEHARNESS_*` layer — is not needed either.

**The `--cwd` contract — replaced by a parameter.** `RunRequest::cwd` takes the
directory per call, so one process hosts every member without any of them touching
the process's own working directory. That is the same rule `src/invoke.rs` already
states for a two-party member's `JudgeLaunch::worktree`: everything per-member
rides a value or a generated file, never process-wide state.
`tests/e2e/library.rs`'s `the_hosting_process_directory_never_moves_for_a_member_that_works_elsewhere`
is the journey that holds it, from the one vantage point that can — inside the
hosting process. What does disappear is the *child's* own working directory (today
the member's scratch); nothing reads it, because `--config` is written absolute and
oneharness starts project discovery from `--cwd`.

**Streaming — replaced by a typed sink.** `RunControls::events` with
`RunRequest::stream: Some(true)` delivers each normalized `ActionEvent` as it
occurs, which is what the `--stream` NDJSON lines carry today. The reader in
`src/member.rs` that parses those lines back apart goes away exactly as the
onejudge conversion's did, and `SinkStep::Stop` is that path's
`ControlFlow::Break`.

Whether a given member streams at all is not this crate's to decide, and the
conversion has to keep it that way: `src/invoke.rs`'s `streams` reads the
member's own resolved config, because a config asking for a schema cannot stream
in oneharness and one asking for both is refused before launch. So the value the
conversion carries is that decision rather than a constant, and what replaces
the buffered path is smaller than what replaces the streaming one — a member that
does not stream is on `--compact` today only so that its report reaches
`src/member.rs` down a pipe read a line at a time, and a library call has the
report as `run`'s return value with no printing in between.

**Control binding — gained, not held.** A single-sided member's argv carries no
`--control` at all, so it records no `control::Turn` and `oneagentgraph interrupt`
has no lever on one. `RunRequest::control` and `RunReport::control` are the same
pair `src/judge.rs` reads for a two-party member, so the conversion adds the lever
rather than having to preserve it.

**Accounting — replaced by the return value.** The exit status feeds
`member::Kind::settled` and the stderr tail feeds `member-died`'s `detail`; both
come from `RunOutcome` (`exit_code`, `failure_summary`) and `OneharnessError`
instead. The fallback chain stops being re-parsed out of a terminal `result` line
and is read off `RunReport::fallback` as the type it already is.

**Isolation of failure — replaced by the thread seam.** A crashed child cannot take
the graph down; an in-process panic can. `src/judge.rs` already answers this and
its answer is the one to reuse: the engine runs on `std::thread::spawn` and answers
over an `mpsc::channel`, so a panic drops the sender and
`RecvTimeoutError::Disconnected` becomes *that member's* `provider-failure` death.
Nothing here builds with `panic = "abort"`, and the shared state a reader holds is
taken through `src/member.rs`'s poison-tolerant `held`.

**Test seams — replaced by the same seam.** The suite substitutes a paid harness at
oneharness's own `ONEHARNESS_BIN_<ID>`, set through the graph's `env:` block —
which `export` has already put on this process, so the override is read from one
environment either way (`RunRequest::bin` is the explicit form if it is ever
wanted). What the spawn *also* gives the suite is `member-started`'s
`program`/`args`/`cwd` as the record of what was decided; those become
`runner: library`'s `config`/`worktree`, which is why the `RunRequest` has to be a
value this crate builds and can assert on rather than one assembled inline at the
call.

**Teardown of the harness's descendants — replaced by the cancel token.**
`RunControls::cancel` terminates each harness tree through
`oneharness_core::io::runner`'s `Finish::Terminate`, which is the seam grown
upstream for the *first* of the two prior regressions. It covers every leg of the
CI matrix, because oneharness owns the harness's own tree on each: a POSIX harness
is its own process-group leader and is signalled as a group, and a Windows one is
spawned `CREATE_SUSPENDED` into oneharness's own `KILL_ON_JOB_CLOSE` job and ended
with `TerminateJobObject`. A graph process that dies outright is covered on Windows
by that same flag, since the job handle is this process's.

## What is still holding the hop open: grouping, on Windows only

Teardown is not grouping. `scratch::Group` is how a **second** process — and this
crate's own watchdog — finds a member's tree, and the two platforms prove
membership by opposite means:

- **POSIX — replaced.** The group *is* the `scratch::SCRATCH_ENV` stamp, fixed at
  `exec` and shed by nothing, and `RunRequest::env` puts it on every harness
  process oneharness starts (`io::run` folds `--env` into each job's environment,
  last write wins). Membership is unchanged, reparented descendants included.
- **Windows — no seam.** The group is a *named* job object, and joining it is
  `AssignProcessToJobObject` on a `CREATE_SUSPENDED` child — it needs the `Child`,
  which an in-process call never yields. `oneharness_core::io::run` spawns the
  harness itself, into a job of its **own**, and the linked `RunControls` exposes no
  hook between the two moments (`io::process::Process::spawn`, which holds them, is
  `pub(crate)`). So the harness would sit outside this member's named job, and
  `scratch::stamped_for` — the only Windows evidence there is — would report an
  empty tree.

Two guarantees drop with it, and neither is theoretical:

1. `scratch::work` is what the activity watchdog observes, and it is the CPU
   charged to the stamped tree. An empty tree is an idle tree, so a member blocked
   on a harness that is working would be condemned by the rule written to stop
   exactly that — `member::Stall`.
2. `scratch::reap`, which is `oneagentgraph cancel --kill` reaching a run from
   another process, would find nothing to end and report a cancelled member that is
   still billing.

## The proposal, which is upstream's rather than this crate's to build

A spawn hook mirroring `onejudge::SpawnHook` — `spawning(&mut Command)` before the
fork and `spawned(&Child)` after it. That is precisely `scratch::Group`'s own
`prepare` and `adopt`, split for this exact reason; it is the seam onejudge grew
for the *second* of the two prior regressions; and `judge::MemberSpawn` is a
working implementation of the same two methods. Rebuilding it here is not an
option the composition rule leaves open.

**Upstream has built it, in that shape, and it is waiting on a release.** The two
methods are the ones asked for, on a `ProcessSupervisor` trait in
`oneharness_core::io::runner`; what differs from the ask is where a caller hands
it over — not a fifth field on `RunControls` but a second entry point beside
`run`, named `run_supervised`, which takes an optional supervisor and is `run`
exactly when given none.

**That sentence is a signpost, not a copy of the signature, and deliberately so.**
The authority is
[oneharness PR #1260](https://github.com/nickderobertis/oneharness/pull/1260),
and until it releases there is nothing here that could check a restatement of it:
the linked 0.10.0 engine has no such symbol, so a spelled-out signature in this
file would be a second source no compiler and no gate could reconcile — the exact
drift this document exists to avoid. Read the signature there. What is written
down here is only what the conversion has to *decide*, which survives a
signature tweak: which entry point to call, and what to pass it. When the release
lands, `tests/inventory.rs` resolves these names against the real crate the same
way it already resolves every `RunRequest` field, and this paragraph goes away.

Upstream's stated reason for the entry point is the one recorded above —
`RunControls` is exhaustively constructible by its embedders, so a field there
would be a breaking change — and oneharness keeps owning teardown of every tree
it spawns, with only the part a `spawning` hook re-parents moving to the caller.
That is the division
this crate wants: `scratch::Group::prepare` on `spawning` and `adopt` on
`spawned`, leaving `RunControls::cancel` as the teardown seam the section above
already assigns it.

So when the conversion runs, the call is `run_supervised` with a supervisor
wrapping the member's `scratch::Group`, and the POSIX/Windows split in the section
above collapses: the stamp still rides `RunRequest::env`, and the Windows job
object gets the `Child` it needs from `spawned`.

**It is not adoptable yet.** PR #1260 is open with two red checks, so crates.io
still serves 0.10.0 and the manifest still takes it. A git pin is not the
workaround — `cargo deny`'s `unknown-git = "deny"` refuses one, by the rule in
`AGENTS.md` that every cross-repo dependency is a published version. The
conversion waits for the release.

**The same gap is already filed upstream, by the other dependent, and it holds two
hops rather than one.** onejudge drives a turn in process through `io::run::run` by
default and keeps a spawning seam beside it for one stated reason — `RunControls`
cannot offer a spawned harness to the caller — written up with a proposal against
oneharness in *its* `docs/oneharness-library.md`. Installing a `SpawnHook` is what
*selects* that seam, and `src/judge.rs` installs one on every two-party member, for
this crate's grouping. So both `oneharness run` processes in this stack are held
open by the one missing hook: `src/harness_process.rs`'s, and the one per side per
turn that a `kind: onejudge` member is pushed back onto. That is what the addition
buys, and it is why the ask belongs upstream rather than in either dependent.

**Why the conversion is not done for POSIX alone in the meantime.** The two member
kinds are distinguishable on the wire: `member-started` carries
`runner: library|process`, and `member-died` carries `exit_code`, `disposition`,
and `stderr_tail` only for a member that was a process. A platform-conditional
conversion makes both of those depend on the host the run happened to land on, so a
consumer branching on `runner` would need to know the platform to read the stream.
The stream is the contract; a contract that differs by platform is worse than a
hop.

## Two things that are not this hop

**`src/smoke.rs`'s `oneharness run`** proves that *this host's* launch path reaches
an identity, so it has to take the path the members take — the binary on `PATH`, at
whatever version is installed there. It collapses when the members do, and not
before; the reason is written at that site.

**`docs/contract.md`'s own sentence.** It states this member "is still
`oneharness run`, a child process", on the grounds that oneharness's library
surface "neither returns the report nor accepts an event sink", and names the
collapse condition: "when oneharness grows a non-printing run entrypoint or an
event-sink parameter". Both were grown in 0.7.0, so that sentence is stale and its
condition is met — the reason the hop remains is the one above instead. Everything
the collapse needs from the *schema* is already spelled there — `runner: library`
with an `engine`, a `config`, and a `worktree`, and the three process facts scoped
to "a member that was one" — so this is a prose correction and not an interface
change. It is still the contract owner's to make.

Every sentence quoted in this section, and every wire name the section above
restates, is checked against `docs/contract.md` itself by `tests/inventory.rs` — a
quotation that stops being one fails there.
