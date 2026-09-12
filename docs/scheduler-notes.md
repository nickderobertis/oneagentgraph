# Scheduler notes

<!-- llmlint: ignore-file[contracts_have_one_source_or_a_drift_gate] This
document is the task-required claim-to-symbol implementation map, not a second
normative contract; docs/contract.md remains authoritative and tests/contract.rs
drives its fenced examples through the public types. -->

This note describes the implementation behind the graph contract. The contract
itself remains `docs/contract.md`; this is a map from its scheduling vocabulary
to the code that enforces it.

## Dependency eligibility

`config::OneharnessMember::deps` and `config::OnejudgeMember::deps` carry the
same optional list. Both default to an empty list and omit it when serialized,
so graphs that do not declare dependencies retain their original shape.

`run::ready_order` validates every dependency before a run starts. A missing
name is an invalid config, and Kahn-style removal of alphabetically ordered
`BTreeMap`/`BTreeSet` entries both detects cycles and produces deterministic
waves. A onejudge member participates in that calculation exactly as a
oneharness member does.

Wave order is necessary but not sufficient to start a member. In `run::run`, a
member is put in the runnable part of its wave only when every dependency has a
`MemberOutcome::Settled` record. Any other outcome produces
`MemberOutcome::Skipped`, naming the unsuccessful dependencies, without calling
`member::run`. Because the next wave applies the same test to the skipped
outcome, the block propagates through the dependency graph.

An actual `member::Outcome` failure makes the graph exit 1. A skip does not add
another failure: it is the consequence recorded beside the member, while the
failed ancestor remains the cause of exit 1. Thus the stream and record
distinguish “failed” from “not attempted”, without inventing an exit code outside
the contract's 0/1/2 surface. `MemberOutcome`'s new serialized spelling advances
the version named by `run::RECORD_SCHEMA_VERSION`.

## Scheduled members

`run::run_wave` executes only one firing and returns when it settles. It never
contains a schedule loop, so a cron member cannot hold its wave open and later
waves are reachable after that first outcome.

`config::Schedule::first_turn_after` decides whether that first firing happens in
the wave at all, and takes the schema the document declares because that is what
an omitted `start_after` means: `every` from `config::FIRST_START_AFTER_VERSION`,
and `0` — a turn in the wave, as it always was — under every version before it.
The field itself is refused below that version rather than ignored, so a document
cannot ask for a delay and silently receive its opposite.
`run::defers_first_turn` is the predicate the wave splits on:

- **`start_after: 0`** — the member is in `run_wave`'s runnable set, takes its
  turn at t=0, and `run::spawn_cron` takes its clock over once that turn settles.
  This is what every schedule did before the field existed.
- **anything else** — the member is not in the runnable set. `run::run` publishes
  its `member-started` from the same `event::MemberStarted` a turn's own start
  builds — one `event::Runner` describing the launch, plus the `start_after` a
  member that took no turn carries — and `run::spawn_cron` starts its clock
  **before** `run_wave` blocks. Before, not after, because `run_wave` waits for
  every member in the wave: a clock started on the far side of that call
  would begin counting only once this member's siblings were done, and the
  sibling a pacemaker paces is exactly the one that takes the whole run. Both
  happen when the member's own wave is reached, which for a member with no `deps`
  — every pacemaker so far — is when the graph starts.

So a deferred member starts with its wave and only its *turn* waits. Everything
that could refuse it — a ref that cannot be read, a persona that does not
validate, a model paired with two harness families — is `invoke::build`, which
runs for every member before `graph-started`, and is untouched by any of this.

A deferred member has no `record.members` entry until it fires, which is the same
state every member is in for the whole of a live run: outcomes fill in as members
settle, and `Record::declared_members` is what names the members themselves.

`ready_order` refuses the one shape this default can silence: a deferred schedule
in a graph that nothing holds open — no member both foreground (below) and
either scheduled or able to take a turn in the initial waves. Such a graph has
nothing to hold it past the quiescence rule below, so the deferred turn never
comes due and the run exits 0 without it. The check is per member rather than
per graph, because a sibling firing at t=0 does not rescue it — a background
member is not what holds the run open, whenever it fires. `run::takes_an_initial_turn`
is the second half of "holds open": a member whose dependency (transitively)
defers its own first turn is skipped in the initial waves and reached only by
that dependency's chain, so it holds nothing open however it is declared.
`run::refuse_a_turn_that_never_comes_due` is that check, at the end of
`ready_order` so `run` and `validate` share it and so it walks a dependency graph
already proven acyclic and complete. Under a schema before
`config::FIRST_BACKGROUND_VERSION` it reduces to exactly the refusal those
documents have always met — every member scheduled or descended from schedules,
and one of them deferred — in the words it has always used; from that version its
words name `background: false` as an answer.

`config::MAX_SCHEDULE_SECONDS` bounds both of a schedule's spans — a typo guard
rather than a policy about cadence, since a `u64` of seconds is a member that
never fires and never says why. `run::cron` compares the span it is counting —
`run::first_span`, then `every` — against elapsed time rather than computing a
deadline, so no span a document can carry reaches arithmetic that could panic.

`run::spawn_cron` owns the member's clock either way, and is handed the span its
first turn owes — `run::first_span`, resolved where the schema is known, so the
clock itself needs no version. `run::cron` watches the existing stop,
member-stop, trigger, and reset files. It counts down the interval currently
pending — that first span until the member has taken a turn and `every` from then
on, which is also what a `reset-timer` on a resettable schedule restarts, so a
reset before the first turn restores the whole delay rather than promoting the
member to its steady cadence. A schedule that fired at
t=0 has nothing left to defer, so its pending interval is `every` from the start.

## Paced conversations

From `config::FIRST_TWO_PARTY_JOB_VERSION`, `config::OnejudgeMember::schedule` is
the same `config::Schedule` a single-sided member carries and is held to the same
rules — `config::schedule_spans` is the one function both `validate` arms call —
but what a firing *is* differs, and everything below follows from that: a
`kind: onejudge` member is one long-lived conversation, and a firing of it is one
turn of that conversation. `config::Member::schedule` answers for both kinds, so
`run::defers_first_turn`, `run::refuse_a_turn_that_never_comes_due`, and
`config::GraphConfig::is_background` treat a scheduled two-party member exactly
as a scheduled single-sided one; `run::clocked` answers for the single-sided
kind alone, and is what the run spawns a clock from after a wave.

- **The first delay is the run's clock's.** A deferred two-party member comes up
  with its wave, publishes `member-started` carrying `start_after`, and
  `run::spawn_cron` counts `run::first_span` down for it exactly as for a
  single-sided member — the same stop, member-stop, trigger and reset files, and
  the same `cron-fired` — and `run::cron` then calls `member::run` **once** and
  returns, because the conversation it opened was the member's whole life. A
  `start_after: 0` two-party member opens in the wave, and `run::run` spawns no
  clock for it afterwards.
- **The hold between turns is the conversation's own.** `invoke::onejudge` puts
  the schedule's `every` and `resettable`, and whether the member is background,
  on `invoke::JudgeLaunch::pace` (`judge::Pace`); `judge::run` gives it to a
  `judge::Hold` that lives inside the observation sink. The hold is taken on the
  supervisor's `TurnClosed` when that turn carried a `Message` — onejudge's own
  shape for a continuation — and not on a close with no message between, which is
  a completion or a settled decision and is followed by no worker turn. The
  engine publishes that close through the sink before it looks for notes or
  opens the next worker turn, so a sink that waits is a conversation that waits;
  `judge::hold_between_turns`, the suite's gate at the same boundary, is the
  demonstration.
- **What ends a hold** is read every `judge::HOLD_TICK` from the run's signal
  directory, derived from the member's scratch as `judge::cancellation_requested`
  derives it: `every` elapsing, or `<member>.trigger`, publish `cron-fired` and
  answer the engine `Continue`; `<member>.reset` on a `resettable` pace restarts
  the count and publishes `cron-reset`; `stop` or `<member>.stop` answers
  `Break`, and `judge::finish` reports the conversation as a cancel is reported
  mid-turn — `member-died`, `cause: cancelled`; and, for a background member,
  `run::QUIESCENT_FILE` answers `Break` and `judge::finish` settles the member
  with the report's `settled_reason` naming the run's quiescence. A note offered
  ends a hold too: `note::Courier` counts each note up *before* it blocks in
  `Notes::send`, the hold reads that count, opens the turn, and the engine's own
  `take_notes` delivers the note `queued` into it. A trigger or reset left while
  a turn is live sits in the directory until the next hold reads it.
- **A hold is not a stall.** The hold stores the activity clock every tick, so
  `member::Stall` on the supervising thread never sees the wait as silence; that
  thread's heartbeat is untouched by a sink that blocks.
- **Quiescence reaches the sink as a file.** The count of unfinished foreground
  members is `run::Foreground`, whose `finish` writes `run::QUIESCENT_FILE` into
  the signal directory when the count reaches zero — and at once for a graph
  seeded at zero. `run::run_wave` reports each member the moment it settles, so
  a background paced conversation opened in the same wave as the last foreground
  member learns of that member's finish while the wave is still open.
- `interrupt` during a hold is exit 3 by the existing route: the member's control
  record names its provisional address, nothing is listening on the agent side's
  socket between turns, and `control::deliver` reports the fact.

## Background and foreground

Whether a member holds the run open is a declaration the document makes per
member: `config::OnejudgeMember::background` and
`config::OneharnessMember::background`, the same optional field on both kinds,
read through `config::Member::declared_background` and — the only reading of what
it *means* — `config::GraphConfig::is_background`. That one function takes the
whole graph because what an omission means depends on the schema the document
declares, the way `config::Schedule::first_turn_after` takes the schema for
`start_after`: from `config::FIRST_BACKGROUND_VERSION` a member naming none is
background exactly when it carries a schedule, and under every older schema the
answer is the inference those documents have always run under — a member with a
schedule, or whose every dependency (transitively) is such a member, is
background, and every other member is foreground. That inference is the pre-8
half of `is_background`; `run.rs` carries no second reading of it.

Three spellings set the field, and `run::run` folds them into one before the
graph is read: `run::apply_background_env` turns `ONEAGENTGRAPH_BACKGROUND`
(`liveness::BACKGROUND_ENV`, read from the environment the run is *given* and
never from the graph's own `env:` block) into `members.<id>.background=true`
overrides, refuses a name the graph lacks with the variable and the name, and
applies them through `run::apply_overrides` before the request's own `--set`,
so a `--set` on the same member wins; and `config::validate` then holds whatever
landed — from the document, the variable, or the flag — to the schema version the
field requires, so a document older than version 8 is refused by the field's
name whichever way it was named. `validate` the verb reads no environment; a
`--detach` preflight does, because the child it launches inherits it.

## Quiescence

The quiescence rule, in one sentence: a run stays open while any foreground
member is unfinished — not yet started, waiting on its clock, or in a turn — and
settles once every unfinished member is background. `run::run` keeps that as one
shared count of unfinished foreground members, seeded from `is_background` over
every member. An unscheduled member finishes at its initial outcome — settled,
failed, died, or skipped for an unsuccessful dependency — and `run::run` counts
it down there. A scheduled member finishes when its clock stops, by the run's
`stop` or the member's own `cancel`, and `run::spawn_cron` counts it down after
`run::cron` returns — after the last turn that clock ran has been recorded — so
a scheduled foreground member never finishes on its own and holds the run open
until it is cancelled, and a skipped schedule, which never gets a clock, is
counted down where it is skipped. A paced two-party member is the one scheduled
member that does finish on its own: its clock returns when its conversation
does, and a foreground one holds the run open exactly that long. Before a due or triggered firing, `run::cron`
reads that count; when it is zero the clock exits without waiting for its next
interval, which is the scheduler's quiescence boundary. `run::run_cron_chain`
reads the same count before each wave it would start, so from the moment the last
foreground member finishes no background member starts a new turn — from a clock
or from a chain — and the only background turns that complete after it are the
ones already in flight at it, which run to their end and are recorded. A graph
whose every member is background runs its initial waves — those are
unconditional — and settles as soon as they are done, at the first tick after.

Each successful later firing calls `run::run_cron_chain`. The reachable
descendants are selected by `run::descendants_of`, traversed in the same
deterministic waves returned by `ready_order`, and launched through the ordinary
`run_wave` boundary. A failed firing does not call the chain at all. Within an
iteration, an unsuccessful dependency prevents its downstream members from
being selected, so failure propagation has the same direction as the first
run.

The chain callback is synchronous: a clock cannot start its next firing while
the previous firing's chain is still running. This is not retry behavior; it is
one fresh chain iteration per firing. When quiescence arrives, no new firing is
created and the chain starts no further wave, while the wave already running
completes before the cron thread joins. `run::run` joins those threads before
emitting `graph-settled`, so the last admitted turn is present in the final
stream and record, and a failure among them is the run's exit 1.

The event vocabulary gains no kind. `member-started` gains one payload field,
`start_after`, on the one a deferred member publishes when it comes up without
taking a turn; `tests/golden/member-started.json` commits every shape of that
payload, with and without it. There is still no wave-boundary event; consumers
continue to infer concurrency and ordering from member event timings.
