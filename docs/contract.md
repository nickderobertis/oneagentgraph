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
- **Filtering is owned by the stream's source.** Consumers have different attention budgets, so each producer takes a filter and narrows what it emits, rather than every consumer downstream re-filtering the same firehose. One grammar across the stack: `{include: [matcher, ...], exclude: [matcher, ...]}`. A matcher's fields are all optional and conjoin — every field it names must hold — and they are `source` (exact equality), `kind` (a glob over the kind's kebab-case wire string, where `*` stands for any run of characters including none and every other character is itself), and the reserved labels `run_id`, `node`, `step`, `member`, `persona` (exact equality; a matcher naming a label the envelope did not stamp does not match it). An absent or empty `include` admits everything; a match in `exclude` always rejects, whatever `include` said. `stream` is deliberately not matchable — it names a producing process rather than anything a consumer means — and neither are payload fields, so a turn's `role` is not a matcher key. A **relayed** envelope's `kind` is matched as the wire string it arrived as: a sibling library's kind is not the reading library's own set, and is never rejected for being unknown. `seq` numbers what the stream carries, so a filtered stream has no gaps and per-stream loss detection keeps working. Filtering decides what is *emitted*, never what the producing library acts on. Like the envelope, the filter type is duplicated per repository by design and held together the same way: the grammar committed in this document is the one source, and each producer's own contract test drives the example below through its types — so a producer whose filter stops matching this text fails its own gate rather than drifting quietly away from the other two.
- Bounded payloads: payload text fields truncate at 4096 bytes with `"truncated": true`. Large evidence (gate logs, check logs, transcripts) is an artifact: stored by the producing library, referenced by id, fetched via that library's CLI.
- Redaction of credential-shaped values happens before an event or artifact leaves the producing library.
- Text output mode is a deterministic rendering of the same events, never separate content.
- A cross-repo contract test asserts this crate's envelope serialization against the spec fixtures committed in docs/contract.md.

### oneagentgraph contract

oneagentgraph owns NO harness/model/fallback logic. The graph YAML names an oneharness config file per role/side (path or URL); oneharness keeps owning identity chains, fallback, model pins, quota classification; onejudge keeps owning the two-party conversation. oneagentgraph composes agents into a graph, prepares each member's launch, and merges outputs into one event stream.

How a member is launched, and why the two kinds differ:

- A `kind: onejudge` member is **driven in-process through the onejudge library** — onejudge's own config, plan, and streamed run driver, called on a thread of this process. There is no `onejudge` binary in the chain, so that member has no argv, no exit status, and no stderr; what it has instead is a typed error, which is what `member-died` carries.
- A `kind: oneharness` member is **driven in-process through the oneharness library** — oneharness's own run driver, called on a thread of this process with this crate's event sink, cancel token, and claim on each harness the run starts. There is no `oneharness` binary in the chain either, so that member has no argv, no exit status, and no stderr; its report is the call's return value rather than a document printed on a pipe, and its fallback chain is the typed report rather than a line parsed back apart. The agent harness the run selects is still a child process, and it is oneharness that starts it.
- The agent harness itself, and a `judge: {command: [...]}` provider, are subprocesses by definition and stay so.
- Because every two-party member shares one process, nothing per-member is exported: a member's `mode` and its scratch-ownership stamp are written into that member's own resolved oneharness configs (`mode`, and `[env]`, which oneharness gives to every harness process it starts). A graph's own `env:` block is still exported — it is one block for the whole graph, applied before any member starts.

Graph config (YAML, by path or URL):

```yaml
version: 6
name: node-scope
env:                                  # exported to every member process; values may reference ${HOME}
  MY_VAR: value
personas: ./personas                  # this graph's own persona catalog; requires version: 6
events:                               # what this run puts on its merged stream; requires version: 5
  filter:
    include:                          # absent or empty = everything passes include
      - {kind: "member-*"}
      - {member: worker, persona: engineer}
    exclude:                          # a match here always rejects, whatever include said
      - {kind: turn-activity}
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
    task: null                        # this member's own job; usually --task instead
    dir: null                         # this member's own directory; default the run's --dir
    schedule: {every: 1800, start_after: 1800, resettable: true}   # cron member; seconds
    deps: []                          # members whose settle precedes this member's first run
```

- A `kind: onejudge` member's judge side may instead be `judge: {command: ["..."]}` — a command provider.
- A member is a **job**, not a copy of its graph. A `kind: oneharness` member may carry its own `task` and its own `dir`, and each beats the graph's: a scheduled member whose whole job is to write one status update receives its own prose rather than the run's `--task`, and works in its own directory rather than the run's `--dir`. Both are optional and both default to the graph's, so a document that omits them runs exactly as before; a relative `dir` resolves against the run's `--dir`, and an absolute one is used as written. Both require `version: 3`. There is no per-member `env`: a member's environment is its oneharness config's own `[env]` table, which a graph already names per member, and the graph's `env:` block stays one block for the whole graph.
- From `version: 4`, a member's own `task` may name the run's with `{task}`, which expands to the whole of `--task`/`--task-file` wherever it appears — so two members share one run's context and differ only in what they are told to do with it, instead of one of them restating that context by hand. A member with no `task` is unaffected and still receives the run's; a `task` naming no token still replaces it outright, exactly as before. `{task}` in a run whose `--task` is absent expands to nothing rather than refusing. The one escape is `{{task}}`, which is the literal text `{task}`: a member's `task` is prose rather than a template language, so every other brace — including a lone `{` or `}`, and `{anything-else}` — is itself. Nothing expands inside the run's own `--task`, or inside a persona, a `dir`, or an `env:` value. Under version 1, 2, or 3 a member's `task` is the literal prose it has always been, token or no token — the gate is on the reading, so a document written before this existed keeps saying exactly what it said.
- From `version: 5`, a graph may name an `events` block, whose `filter` is the shared filter grammar above, applied to every envelope this run emits or relays on its merged stream — both member kinds' events and the graph's own. `oneagentgraph run --event-filter SPEC` names one instead, as a path to a file holding it or as the document itself inline as JSON, and beats the graph's. A run naming neither streams exactly what it always did. The filter is checked before the graph starts and refused with the offending matcher named — beside a bad persona ref and an unpairable model, and for the same reason: a matcher that could match nothing, or one that names no field at all and so matches everything, is a spec the run cannot honour, and finding it after a paid turn has been spent is finding it too late. `events` requires `version: 5` and a document declaring an older schema is refused by the block's name, so a graph written before this existed keeps streaming everything, which is what it said. What the run itself acts on is unfiltered whatever the spec says: liveness, scheduling, and settle detection go on seeing every heartbeat, death, and settlement.
- From `version: 6`, a graph may name its own persona catalog with `personas: PATH`, a directory (relative to the graph document, like every other ref a graph names) holding `<name>.yaml` files. A member's `persona:` that parses as a persona **name** — one or more `/`-separated segments of lowercase letters, digits, and hyphens, so `engineer` and `crozier/crozier-corpus` but never `./roles/lead.yaml` — is looked up in that catalog and in the personas this crate ships; anything else is the path or URL it has always been. A name that is in **both** catalogs is refused before the graph starts, naming the file and the shipped persona it collides with: which one a member runs under is not this build's to decide, and preferring the shipped one shadows an operator's file of the same name without a word. Naming the file by path is the explicit selection, and reaches it whatever it collides with. A name in neither is refused with both catalogs named. A graph naming no `personas` resolves exactly as it always did — a shipped name, or a ref — so a document written before this existed is unaffected; a document declaring an older schema and naming the key is refused by the key's name, because the key changes what a bare `persona: NAME` means.
- A `model` override must be paired with a config whose declared chain is one harness family; validated pre-launch. The model value itself is forwarded unchecked.
- A `schedule`'s `start_after` is how many seconds pass before that member's **first** turn, and from `version: 4` a schedule naming none waits `every` — "every 1800 seconds" means *from now on*, and a member whose job is to report progress has nothing to report at t=0. `start_after: 0` is the first turn taken the moment the graph starts, which is what every schedule did before this field existed and what a document declaring version 1, 2, or 3 still does: the field requires `version: 4`, and so does the default it moves, so no document already written changes behaviour. A version 3 document naming `start_after` is refused by the field's name rather than run at t=0 regardless. It defers the **turn**, never the **start**: a scheduled member comes up with its own wave whatever its cadence — with the graph, for one that waits on nothing — and publishes `member-started` there, and a bad persona ref, an unpairable model, or an unreadable config is still refused before the graph starts — so a member ship-broken on a half-hour schedule is heard from within seconds rather than half an hour in. A graph whose every member is scheduled, or descends only from scheduled members, has nothing to hold the run open past the moment its clocks tick, so a first turn deferred past that tick never comes due — the member would start, wait, and the run would exit 0 without it ever having run. Such a member is refused with the reason, and `start_after: 0`, or a member outside the schedules for it to pace, is the answer.
- A member's oneharness config is **the operator's**, and oneagentgraph overrides nothing in it that oneharness already owns. A `kind: oneharness` member's run streams when its own resolved config says so: `stream` decides, and a config declaring neither `stream` nor a schema streams — which is what every graph already written does, and what publishes `turn-activity`. A config declaring `schema_file` does not stream, because oneharness has no such run: `--stream` and a schema are mutually exclusive there, so a flag forced on by this crate is what made structured output unreachable. A config asking for both — `stream = true` beside a `schema_file` — is refused before launch, naming both keys, rather than run with one of the operator's settings silently dropped; so is a `stream` that is not a boolean or a `schema_file` that is not a path, because both are read here to build the member's invocation. A member that does not stream publishes one report at the end instead of a transcript; its `member-settled` still names the same `report_path`, and the document stored there is oneharness's own result — `structured`, `schema_valid`, `schema_attempts`, and `schema_error` included. That report is the whole return channel for a validated answer; there is no member field for a schema, because the setting is oneharness's.
- A **relative path** in that config means what it meant where the file was written. oneharness resolves a config-declared path against the directory the harnesses run in (`--cwd`, which for a member is that member's own directory), and this crate writes the resolved config into the run's scratch before oneharness reads it — so such a path would point at neither the file's own directory nor anywhere the operator can predict. Each one is therefore anchored to the source config's own directory as that copy is stamped, the same rule every other ref in a graph follows. The path-valued keys this applies to: `schema_file`, `history_dir`, and `[harness.<id>.variant.<name>] env_file`. An absolute path is used as written; an empty value is left alone, which is what oneharness reads `history_dir = ""` as — unset — while an empty `schema_file` names no file at all and is refused pre-launch with the other unreadable values above; and a config fetched over https has no directory for a relative path to mean anything against, so its paths are carried exactly as written and oneharness answers for them by name. A key naming a *program* rather than a path — `[harness.<id>] bin`, a `[[hooks]] command` — is not anchored: it resolves on `PATH`.
- Remote refs (https) are fetched, checksummed, and recorded content-addressed in the run record; replay/audit never depends on the URL staying stable.

CLI:

```
oneagentgraph run GRAPH [--task TEXT] [--task-file F] [--dir PATH]
               [--label k=v]... [--set members.worker.agent.model=NAME]...
               [--event-filter SPEC] [--output json|text] [--detach]
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

Event kinds: `graph-started`, `member-started` (`runner: library|process`, plus what that runner is: the `engine`, `config`, and `worktree` of an in-process member — which has no working directory of its own — and the `program`, `args`, and `cwd` of a child one; plus `start_after`, the seconds until the first turn, on the one published by a scheduled member that came up without taking one), `turn-started` (`turn`, `role`, the `instruction` that turn answers, and `started_at`), `turn-activity` (one tool call or the observation answering one: kind, name, 160-char detail, the `output` a result returned, the `tool_call_id` joining the two, and the `index` within the turn), `turn-message` (one party's own words for a turn: `turn`, `role`, `text`), `turn-completed` (**one turn**: `turn`, `role`, `started_at`, `finished_at`, and that turn's own usage — input/output tokens, cache reads and writes, cost in USD), `turn-interrupted` (`member`, `delivered`, `input_bytes`, and the `reason` a delivery that did not land names), `member-heartbeat`, `fallback-advanced` (identity, classified reason), `oneharness-session` (where one oneharness invocation's conversation was written down), `member-died`, `cron-fired`, `cron-reset`, `member-settled` (full onejudge report as artifact, verdict inline, and the `report_path` that artifact is stored at), `graph-settled`.

A turn's own events describe **that turn**, and a member of several turns publishes a set per turn rather than one for the run. `turn-completed` carries what that one turn consumed; the run's totals are in the settled report artifact, which has always held them. Every usage figure is independently optional and an absent one means the provider reported none — never zero, which is a turn that really cost nothing. `turn-activity` publishes a `tool_result` as well as a `tool_call`: a result names no tool because it answers one already named, so its `name` is `null` — serialized rather than omitted, so the shape is the same either way — and `tool_call_id` is what joins it back. An `output` that is absent is an observation the trace did not expose; one that was genuinely empty is `""`.

The text these events carry is bounded, and the bound is a property of the **live journal event alone**: `instruction`, `text`, and `output` are cut to 4096 bytes on a character boundary, with `instruction_truncated`, `truncated`, and `output_truncated` saying so. `instruction` and `text` keep their **head** — a turn's opening is where the model says what it is about to do — and `output` keeps its **tail**, because what names a failure is the last of a tool's output. A reader meeting one of those flags is being told where the rest is, not that anything was lost: the report-backed route serves the same text whole.

`member-died` describes an in-process failure honestly, and a member that *died* stays distinct from one that *failed its task* — the latter is a `member-settled` with `completed: false`, never this event. Its payload:

- `rule` — the liveness rule that fired: `unstartable`, `signalled`, `provider-failure`, `heartbeat`, `activity`.
- `cause` — the typed cause. Ten of these are onejudge's `ProviderErrorKind`, which is oneharness's own normalized `failure_kind`, mapped totally: `auth`, `rate_limit`, `model_not_found`, `quota`, `overloaded`, `timeout`, `cancelled`, `spawn`, `protocol`, `other`. Three exist outside that taxonomy: `exited` and `signaled` for a member that was a child process, and `unclassified` for an engine failure that named no kind.
- `detail` — what that cause said, bounded like every payload text field: the engine's own error for an in-process member, the tail of standard error for a child one. `truncated` when it was cut.
- `exit_code`, `disposition: exited|signaled`, `stderr_tail` — a **child process's** facts, present only for a member that was one. An in-process member has none of them; `cause` and `detail` are how it says the same thing.

`fallback-advanced` gains two fields a two-party member can now answer for, and only it: `role: agent|judge` and the `turn`. onejudge's report carries no fallback chain of its own, so while that hop was a subprocess a two-party member published no `fallback-advanced` at all; in-process, its per-invocation telemetry names every candidate each side stepped past — including for a run that failed and produced no report, which is exactly when an operator needs to know which subscription refused. A single-sided member stamps neither field.

`oneharness-session` names the conversation one oneharness invocation actually had, which is the surface an operator reads to see what an agent did. One is published per invocation that names **both** a history id and a history file — the same per-invocation telemetry `fallback-advanced` is read from, and the only place either is reachable for a run that failed and produced no report. An invocation naming neither publishes nothing, because history was off or no candidate ran. Its payload:

```json oneharness-session
{
  "role": "agent",
  "turn": 1,
  "identity": "claude-code:alternate",
  "session_id": "54e7ad34-ce6d-4979-8b4d-531b88026e15",
  "history_id": "01a00d0f-c094-7660-b26c-8a53baaf9c3b",
  "history_dir": "/home/nick.guest/.local/state/oneharness/history",
  "history_project": "home-nick.guest-ai-orchestrator",
  "history_session": "node-scope-1786925518098-3163646-worker-skill-20260817T001158Z-3163805"
}
```

`role` is `agent|judge` and `turn` is that side's invocation index, as on `fallback-advanced`; `identity` is the candidate that ran, by the composed id that reproduces it; and `session_id` is present only when the harness exposed a continuation id of its own. The three path fields are the three the reader takes rather than one path, and together they name `<history_dir>/<history_project>/<history_session>` with oneharness's own session extension. They are published only for a history file that is already in that layout — an absolute path to a named session file, inside a project directory, inside a store named by at least one component of its own, with no component that climbs (`..`). Anything else publishes nothing at all rather than three fields resolved against it: the reader joins them back together and opens the result on its own host, so a component that leaves the store is a file read there rather than a transcript. Exactly one artifact rides the event — `{"id": "<history_id>", "kind": "oneharness_session", "bytes": <the session file's size, 0 when it cannot be read>}` — and those bytes stay in oneharness's history store: nothing is copied into the run, and a consumer reads them back by linking that library rather than by spawning its CLI.

A `session` label, among an envelope's free-form extras, names the conversation a turn belongs to. Exactly five kinds carry it — `turn-started`, `turn-activity`, `turn-message`, `turn-completed`, `turn-interrupted` — and **no other kind a member or the graph emits carries one**, including `oneharness-session` itself. Its value is the emitting stream's id, a `.`, and the member id — `node-scope-1786925518098-3163646.worker` — sanitised to the consumer's identifier rule: non-empty, at most 128 characters, ASCII letters, digits, `-`, `_` and `.` only, never starting with `.`. Neither id is bounded, so a pair past that length keeps a share of each — the end of the stream, which is what tells two runs of one graph apart, and the start of the member — followed by a `-` and a 16-character digest of the whole pair, so two members of one run are never one conversation. The exclusion is the load-bearing half rather than a detail: a consumer treats any labelled envelope that is not `turn-activity`, `turn-message` or `turn-interrupted` as one transcript turn, so a `session` on the thousands of heartbeats one member publishes over a run would render a transcript of heartbeats and make every turn count served from it wrong.

`interrupt` is `cancel`'s sibling — same addressing, different intent. `cancel` ends a turn and the worker's whole accumulated context goes with it; `interrupt` redirects one that keeps running, so a turn that went the wrong way is corrected rather than restarted. Its exit codes: `0` delivered, `3` the member has no controllable turn in flight, `2` invalid arguments or an unknown run/member, `1` a delivery that was attempted and failed. Exit `3` is **a fact, not an error**, and the answer names which one it is: the member is between turns, it has already settled, it runs on a harness with no out-of-band turn control, or it opens no controllable turn at all (a single-sided member has no onejudge agent side to open one). Every outcome publishes one `turn-interrupted`. The address comes from onejudge's report `control: {session, session_dir, cwd}` — the three values `oneharness interrupt` takes — and a member whose report carries `control: null` is the exit-`3` case; nothing about a control socket, a store directory, or a harness's control mechanism is derived here.

`sweep` is the liveness rules below, made invokable. It reports every **family** of scratch this crate creates — `runs`, the run state directory, and `temp`, the throwaway directories it leaves under `TMPDIR` — naming for each the directories it examined and what became of them, and naming every family it could **not** examine and why. Every family lands in exactly one of those two lists, so `reclaimed 0 bytes` can never hide a family that was never looked at; a family whose root does not exist yet is an examined zero, and one that cannot be read is unexamined with the reason. A directory is reclaimed only when nothing can still be using it: the `owner.lock` is free, the pid-with-start-token it records no longer names a live process, and no live process carries that directory as its scratch stamp — anything else is retained, with the reason, and ending a process a directory still names is `cancel --kill`'s job rather than this verb's. `--min-age-hours` (default 24, `0` to sweep whatever is provably dead) keeps a sweep run in anger from taking run records their operator is about to read; `--dry-run` reports without removing anything. Exit 0 whatever it finds — an unexamined family is a reported fact, not a refusal.

Liveness (ported from ai-orchestrator intact): heartbeat wrapper (default deadline 60s, `ONEAGENTGRAPH_HEARTBEAT_TIMEOUT`), activity watchdog (default 600s, `ONEAGENTGRAPH_STALL_TIMEOUT`), scratch ownership via a non-blocking kernel-exclusive lock on `owner.lock` + pid-with-start-token, descendant reaping, successor contract for processes meant to outlive their launcher.

The activity watchdog condemns on **silence plus an idle process tree**, never on silence alone. A member is condemned when it has published nothing for the bound *and* the tree stamped for it — the same evidence a reap and a sweep rest on, so it reaches a descendant whose parent has already exited — did nothing in that time: between two observations the tree was charged CPU at no more than 1% of one core, measured against the wall time that actually elapsed between them. A rate rather than "was charged anything at all", because *anything at all* asks how finely a platform counts rather than what the member did — a parked harness's own bookkeeping is invisible to a counter of ten-millisecond ticks and plainly visible to one of nanoseconds, so the same wedged member was condemned on one platform and spared on another. That share of a core is two orders of magnitude above such bookkeeping and two below a child doing work, and it is the same number on every platform. A member blocked on a child that is doing the work publishes nothing for far longer than the bound and is healthy, and condemning it destroys the live work underneath it; silence alone also made the verdict depend on whether an agent happened to drive its round by polling or by blocking, which is a choice it makes freely turn by turn. Two consequences follow and both are deliberate: the bound is a **floor** rather than an exact deadline, since establishing that a tree is idle takes two observations; and a member whose tree spins forever is left to `cancel` and to the heartbeat rule. A platform that can enumerate neither a tree nor its CPU is condemned on silence alone, as before.
