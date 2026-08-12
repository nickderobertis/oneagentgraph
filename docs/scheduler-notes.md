# Scheduler notes

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
the contract's 0/1/2 surface. `MemberOutcome`'s new serialized spelling makes
run records schema version 3 (`run::RECORD_SCHEMA_VERSION`).

## Scheduled members

`run::run_wave` executes only one firing and returns when it settles. It never
contains a schedule loop, so a cron member cannot hold its wave open and later
waves are reachable after that first outcome.

After the first firing, `run::spawn_cron` owns the member's clock. `run::cron`
watches the existing stop, member-stop, trigger, and reset files. Before a due
or triggered firing, it checks the shared count of live work whose ancestry is
not solely cron members. `run::solely_cron_descended` computes that property
from the validated DAG. When the count reaches zero the clock exits without
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

The event vocabulary is unchanged. There is no wave-boundary event; consumers
continue to infer concurrency and ordering from member event timings.
