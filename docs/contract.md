# oneagentgraph contract

The approved contract for this crate, committed verbatim below. It is the source
of truth: the public types, the config schema, and the CLI surface are written to
match this text, and `tests/contract.rs` drives the fenced blocks below through
those types so the two cannot drift. Changing the interface is a proposal to the
owner of this contract, never a unilateral edit here.

---

### Shared event envelope (duplicate these types in this crate; there is deliberately no shared util crate)

Every process in the stack emits NDJSON, one envelope shape:

```json
{"v": 1, "ts": "<RFC3339, millisecond, UTC>", "stream": "<unique id per producing process>",
 "seq": 42, "source": "agentgraph|vcs|pipeline", "kind": "<event kind>",
 "labels": {"run_id": "R", "round": 2, "node": "service", "step": "implement",
            "member": "worker", "persona": "engineer"},
 "payload": {}, "artifacts": [{"id": "a-91", "kind": "log", "bytes": 21400}]}
```

- `seq` is a u64, monotonic per `stream`. Merge order across streams is `(ts, stream, seq)`; a consumer detects loss via per-stream seq gaps. No cross-stream ordering promises beyond timestamps.
- `labels`: the reserved keys shown plus free-form extras; producers stamp what they know, enrichers never rewrite.
- Bounded payloads: payload text fields truncate at 4096 bytes with `"truncated": true`. Large evidence (gate logs, check logs, transcripts) is an artifact: stored by the producing library, referenced by id, fetched via that library's CLI.
- Redaction of credential-shaped values happens before an event or artifact leaves the producing library.
- Text output mode is a deterministic rendering of the same events, never separate content.
- A cross-repo contract test asserts this crate's envelope serialization against the spec fixtures committed in docs/contract.md.

### oneagentgraph contract

oneagentgraph owns NO harness/model/fallback logic. The graph YAML names an oneharness config file per role/side (path or URL); oneharness keeps owning identity chains, fallback, model pins, quota classification; onejudge keeps owning the two-party conversation. oneagentgraph composes agents into a graph, prepares each member's launch, and merges outputs into one event stream.

How a member is launched, and why the two kinds differ:

- A `kind: onejudge` member is **driven in-process through the onejudge library** — onejudge's own config, plan, and streamed run driver, called on a thread of this process. There is no `onejudge` binary in the chain, so that member has no argv, no exit status, and no stderr; what it has instead is a typed error, which is what `member-died` carries.
- A `kind: oneharness` member is still `oneharness run`, a child process. oneharness's library surface writes its report to the **process's** stdout and returns only an exit code — it neither returns the report nor accepts an event sink — and this process's stdout is the merged stream. The hop collapses when oneharness grows a non-printing run entrypoint or an event-sink parameter.
- The agent harness itself, and a `judge: {command: [...]}` provider, are subprocesses by definition and stay so.
- Because every two-party member shares one process, nothing per-member is exported: a member's `mode` and its scratch-ownership stamp are written into that member's own resolved oneharness configs (`mode`, and `[env]`, which oneharness gives to every harness process it starts). A graph's own `env:` block is still exported — it is one block for the whole graph, applied before any member starts.

Graph config (YAML, by path or URL):

```yaml
version: 2
name: node-scope
env:                                  # exported to every member process; values may reference ${HOME}
  MY_VAR: value
members:
  worker:
    kind: onejudge                    # two-party: agent + judge
    base_config: ./onejudge.base.yaml # path or URL
    persona: https://example.com/engineer.yaml  # path or URL; optional
    task: null                        # usually supplied by --task
    agent:
      oneharness_config: ./oneharness.toml      # path or URL
      model: null                     # optional override, forwarded not validated
      stream: true                    # default true; false = report-only
    judge:
      oneharness_config: ./oneharness.judge.toml
      model: null
    mode: bypass                      # onejudge approval mode
    max_turns: null
    deps: []                          # members whose settle precedes this member's first run
  reporter:
    kind: oneharness                  # single-sided: one agent, no judge
    oneharness_config: ./oneharness.toml
    persona: ./reporter.yaml
    schedule: {every: 1800, resettable: true}   # cron member; seconds
    deps: []                          # members whose settle precedes this member's first run
```

- A `kind: onejudge` member's judge side may instead be `judge: {command: ["..."]}` — a command provider.
- A `model` override must be paired with a config whose declared chain is one harness family; validated pre-launch. The model value itself is forwarded unchecked.
- Remote refs (https) are fetched, checksummed, and recorded content-addressed in the run record; replay/audit never depends on the URL staying stable.

CLI:

```
oneagentgraph run GRAPH [--task TEXT] [--task-file F] [--dir PATH]
               [--label k=v]... [--set members.worker.agent.model=NAME]...
               [--output json|text] [--detach]
oneagentgraph validate GRAPH
oneagentgraph trigger RUN MEMBER          # fire a scheduled member now
oneagentgraph reset-timer RUN MEMBER      # restart a resettable schedule's clock
oneagentgraph cancel RUN [MEMBER] [--kill]
oneagentgraph interrupt RUN MEMBER [--input TEXT | --input-file F]   # redirect a member's in-flight turn
oneagentgraph history [RUN] | history show ID
oneagentgraph health                      # per-identity binding/utilization/reset, read from oneharness data
oneagentgraph smoke [--dir PATH]
oneagentgraph sweep [--dry-run] [--min-age-hours H]   # report this crate's own scratch, and reclaim what is provably dead
oneagentgraph persona new NAME | persona validate PATH
```

`run` streams envelopes to stdout. Exit 0 = every member settled successfully; 1 = a member failed or died (the stream says which and why); 2 = invalid config. `--detach` prints `{run_id, events_path, pid}` and exits 0.

Event kinds: `graph-started`, `member-started` (`runner: library|process`, plus what that runner is: the `engine`, `config`, and `worktree` of an in-process member — which has no working directory of its own — and the `program`, `args`, and `cwd` of a child one), `turn-started`, `turn-activity` (bounded tool summary: kind, name, 160-char detail), `turn-completed` (usage: tokens in/out, cache r/w, cost, duration), `turn-interrupted` (`member`, `delivered`, `input_bytes`, and the `reason` a delivery that did not land names), `member-heartbeat`, `fallback-advanced` (identity, classified reason), `member-died`, `cron-fired`, `cron-reset`, `member-settled` (full onejudge report as artifact, verdict inline, and the `report_path` that artifact is stored at), `graph-settled`.

`member-died` describes an in-process failure honestly, and a member that *died* stays distinct from one that *failed its task* — the latter is a `member-settled` with `completed: false`, never this event. Its payload:

- `rule` — the liveness rule that fired: `unstartable`, `signalled`, `provider-failure`, `heartbeat`, `activity`.
- `cause` — the typed cause. Ten of these are onejudge's `ProviderErrorKind`, which is oneharness's own normalized `failure_kind`, mapped totally: `auth`, `rate_limit`, `model_not_found`, `quota`, `overloaded`, `timeout`, `cancelled`, `spawn`, `protocol`, `other`. Three exist outside that taxonomy: `exited` and `signaled` for a member that was a child process, and `unclassified` for an engine failure that named no kind.
- `detail` — what that cause said, bounded like every payload text field: the engine's own error for an in-process member, the tail of standard error for a child one. `truncated` when it was cut.
- `exit_code`, `disposition: exited|signaled`, `stderr_tail` — a **child process's** facts, present only for a member that was one. An in-process member has none of them; `cause` and `detail` are how it says the same thing.

`fallback-advanced` gains two fields a two-party member can now answer for, and only it: `role: agent|judge` and the `turn`. onejudge's report carries no fallback chain of its own, so while that hop was a subprocess a two-party member published no `fallback-advanced` at all; in-process, its per-invocation telemetry names every candidate each side stepped past — including for a run that failed and produced no report, which is exactly when an operator needs to know which subscription refused. A single-sided member stamps neither field.

`interrupt` is `cancel`'s sibling — same addressing, different intent. `cancel` ends a turn and the worker's whole accumulated context goes with it; `interrupt` redirects one that keeps running, so a turn that went the wrong way is corrected rather than restarted. Its exit codes: `0` delivered, `3` the member has no controllable turn in flight, `2` invalid arguments or an unknown run/member, `1` a delivery that was attempted and failed. Exit `3` is **a fact, not an error**, and the answer names which one it is: the member is between turns, it has already settled, it runs on a harness with no out-of-band turn control, or it opens no controllable turn at all (a single-sided member has no onejudge agent side to open one). Every outcome publishes one `turn-interrupted`. The address comes from onejudge's report `control: {session, session_dir, cwd}` — the three values `oneharness interrupt` takes — and a member whose report carries `control: null` is the exit-`3` case; nothing about a control socket, a store directory, or a harness's control mechanism is derived here.

`sweep` is the liveness rules below, made invokable. It reports every **family** of scratch this crate creates — `runs`, the run state directory, and `temp`, the throwaway directories it leaves under `TMPDIR` — naming for each the directories it examined and what became of them, and naming every family it could **not** examine and why. Every family lands in exactly one of those two lists, so `reclaimed 0 bytes` can never hide a family that was never looked at; a family whose root does not exist yet is an examined zero, and one that cannot be read is unexamined with the reason. A directory is reclaimed only when nothing can still be using it: the `owner.lock` is free, the pid-with-start-token it records no longer names a live process, and no live process carries that directory as its scratch stamp — anything else is retained, with the reason, and ending a process a directory still names is `cancel --kill`'s job rather than this verb's. `--min-age-hours` (default 24, `0` to sweep whatever is provably dead) keeps a sweep run in anger from taking run records their operator is about to read; `--dry-run` reports without removing anything. Exit 0 whatever it finds — an unexamined family is a reported fact, not a refusal.

Liveness (ported from ai-orchestrator intact): heartbeat wrapper (default deadline 60s, `ONEAGENTGRAPH_HEARTBEAT_TIMEOUT`), activity watchdog (default 600s, `ONEAGENTGRAPH_STALL_TIMEOUT`), scratch ownership via a non-blocking kernel-exclusive lock on `owner.lock` + pid-with-start-token, descendant reaping, successor contract for processes meant to outlive their launcher.
