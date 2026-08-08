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

oneagentgraph owns NO harness/model/fallback logic. The graph YAML names an oneharness config file per role/side (path or URL); oneharness keeps owning identity chains, fallback, model pins, quota classification; onejudge keeps owning the two-party conversation. oneagentgraph composes agents into a graph, constructs onejudge/oneharness invocations, and merges outputs into one event stream.

Graph config (YAML, by path or URL):

```yaml
version: 1
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
oneagentgraph history [RUN] | history show ID
oneagentgraph health                      # per-identity binding/utilization/reset, read from oneharness data
oneagentgraph smoke [--dir PATH]
oneagentgraph persona new NAME | persona validate PATH
```

`run` streams envelopes to stdout. Exit 0 = every member settled successfully; 1 = a member failed or died (the stream says which and why); 2 = invalid config. `--detach` prints `{run_id, events_path, pid}` and exits 0.

Event kinds: `graph-started`, `member-started`, `turn-started`, `turn-activity` (bounded tool summary: kind, name, 160-char detail), `turn-completed` (usage: tokens in/out, cache r/w, cost, duration), `member-heartbeat`, `fallback-advanced` (identity, classified reason), `member-died` (payload: `rule` fired, `exit_code`, `disposition: exited|signaled`, `stderr_tail`), `cron-fired`, `cron-reset`, `member-settled` (full onejudge report as artifact, verdict inline), `graph-settled`.

Liveness (ported from ai-orchestrator intact): heartbeat wrapper (default deadline 60s, `ONEAGENTGRAPH_HEARTBEAT_TIMEOUT`), activity watchdog (default 600s, `ONEAGENTGRAPH_STALL_TIMEOUT`), scratch ownership via `owner.lock` flock + pid-with-start-token, descendant reaping, successor contract for processes meant to outlive their launcher.
