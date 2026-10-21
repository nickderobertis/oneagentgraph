# Spike: the journal's turn content from oneharness history

A spike branch. It never lands. Its report is the project document
`dag-ui-project-centric-spike-transcript-source-report` in
`authoring:dag-ui-project-centric`.

The question: can `turn-started`, `turn-message` and `turn-activity` carry a
reference into oneharness's history file instead of their bounded copies
(`instruction`, `text`, `detail`, `output`, cut at 4096 bytes or 160 characters)?

```sh
spikes/transcript-source/probe.sh                       # two real turns, then the recorded runs
spikes/transcript-source/probe.sh live                  # the two real turns only
spikes/transcript-source/probe.sh recorded              # this host's recorded runs only, no quota spent
spikes/transcript-source/probe.sh replay --scratch DIR  # re-resolve a saved live probe, no quota spent
```

`live` runs one `kind: onejudge` member and one `kind: oneharness` member through
the installed `oneagentgraph`. The prompt, the shell command it asks for, that
command's output and the reply are each longer than the journal's bounds. The
judge side is `judge.py`, a command judge, so the only quota spent is the agent's
turn. History is on, and the pointer file and labels belong to the probe, so these
turns do not land in the run of whatever dispatch starts the probe. Each journal
line is timestamped as it arrives, and the history store is read at that same
moment. Each record is then resolved by the candidate reference: the pointer line
oneharness wrote before it spawned the harness (history file plus `history_id`),
the record's `index`, and for the instruction the closing run line's `prompt`. The
resolved text is compared with the full text the probe knows. Everything goes to
`$ONEPIPELINE_NODE_SCRATCH_DIR` (or `/tmp`). `capture.json` keeps the stamps, so
`replay` re-reads a probe without spending another turn.

`recorded` reads `~/projects/ai-orchestrator/runs/*/events.jsonl` and each run's
`oneharness-sessions.jsonl` read-only. For each member kind it reports:

- whether history was written;
- whether the journal carries an `oneharness-session` record;
- whether the files outlived their worktree and their scratch;
- whether each two-party instruction equals its turn's history prompt;
- whether the change-request drafter received the whole of a prompt the journal
  marks `instruction_truncated`.

A non-zero exit means that a record whose full text the probe knows did not resolve
to it. The two-party run itself exits 1 (`completed: false`), because the probe's
base config declares no `evals`. That says nothing about the transcript.
