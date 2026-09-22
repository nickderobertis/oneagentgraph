# Recorded streams

One stream this crate itself produced, copied byte for byte from the file it
wrote on a real host, and held by `tests/recorded.rs` to round-trip through
`Envelope` and `serde_json::to_string` with no byte changed. Nothing in the file
was edited; a fixture that had been touched would prove the touch rather than
the producer.

- `oneagentgraph-run.ndjson` — the whole `events.jsonl` of the `oneagentgraph`
  run `review-bar-probe-1789277572872-1933894` (9 envelopes, `v: 1`, source
  `agentgraph`, the `session` extra label on the turn kinds).

## Provenance

The file is copied byte for byte from `onemessagebus` tag
`onemessagebus-agent-v0.8.0`, path
`crates/onemessagebus-agent/tests/recorded/oneagentgraph-run.ndjson`, and the
paragraph above is that directory's `README.md` entry for it. It lived there
while the envelope's words were the bus's agent profile's; it lives here now
that they are this crate's. The bytes did not move with it, which is the whole
point of keeping it: a stream written against the profile still reads back
through this crate's own vocabulary, unchanged.
