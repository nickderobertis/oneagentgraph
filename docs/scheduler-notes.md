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
the wave at all. It is `start_after` when the document names one and `every`
otherwise, and `run::defers_first_turn` is the predicate the wave splits on:

- **`start_after: 0`** — the member is in `run_wave`'s runnable set, takes its
  turn at t=0, and `run::spawn_cron` takes its clock over once that turn settles.
  This is what every schedule did before the field existed.
- **anything else** — the member is not in the runnable set. `member::announce`
  publishes its `member-started` — the same `runner` and launch description a
  turn's own would carry, plus `start_after` — and `run::spawn_cron` starts its
  clock **before** `run_wave` blocks. Before, not after, because `run_wave` waits
  for every member in the wave: a clock started on the far side of that call
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
in a graph where every member is scheduled or descends only from scheduled
members. Such a graph has nothing to hold it past the quiescence rule below, so
the deferred turn never comes due and the run exits 0 without it. The check is
per member rather than per graph, because a sibling firing at t=0 does not rescue
it — a scheduled member is not counted as live work either.
`run::refuse_a_turn_that_never_comes_due` is that check, at the end of `ready_order`
so `run` and `validate` share it and so it walks a dependency graph already proven
acyclic and complete.

`config::MAX_SCHEDULE_SECONDS` bounds both of a schedule's spans — a typo guard
rather than a policy about cadence, since a `u64` of seconds is a member that
never fires and never says why. `run::cron` compares `run::pending_interval`
against an elapsed span rather than computing a deadline, so no span a document
can carry reaches arithmetic that could panic on it.

`run::spawn_cron` owns the member's clock either way. `run::cron`
watches the existing stop, member-stop, trigger, and reset files. It counts down
the interval currently pending — `start_after` until the member has taken a turn
and `every` from then on, which is also what a `reset-timer` on a resettable
schedule restarts, so a reset before the first turn restores the whole delay
rather than promoting the member to its steady cadence. A schedule that fired at
t=0 has nothing left to defer, so its pending interval is `every` from the start.

Before a due or triggered firing, it checks the shared count of live work whose
ancestry is not solely cron members. `run::solely_cron_descended` computes that
property from the validated DAG. When the count reaches zero the clock exits without
waiting for its next interval, which is the scheduler's quiescence boundary.

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
created, while a callback already running completes before the cron thread
joins. `run::run` joins those threads before emitting `graph-settled`, so the
last admitted chain is present in the final stream and record.

The event vocabulary gains no kind. `member-started` gains one payload field,
`start_after`, on the one a deferred member publishes when it comes up without
taking a turn. There is still no wave-boundary event; consumers continue to infer
concurrency and ordering from member event timings.
