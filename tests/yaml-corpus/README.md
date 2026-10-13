# YAML corpus

The real YAML this crate reads, plus what it read from each file before its
reader moved from `serde_norway` to serde-saphyr. `tests/yaml_corpus.rs` holds
the current reader to that record.

## Provenance

- `host/graphs/`, `host/personas/` and `host/config/onejudge.base.yaml` are
  copied byte for byte from the host's `ai-orchestrator` checkout at commit
  `9eb8acffaf6d1e5782a18f415314d168d9febe5e`. Those are the graphs, personas
  and base config every dispatch on that host runs. Nothing in them was edited.
- The `host/oneharness*.toml` files and `host/config/plan-checklist.llmlint.yml`
  are stand-ins, not copies. They exist only so the real graphs' relative refs
  resolve. The effective onejudge config names where a side's config is written
  and never what that config says, and it checks only that the llmlint config
  is a readable file.

## What was recorded

Both records were written by this crate at `fb3fd94` (v0.5.9, still on
`serde_norway`), through the same entry points the test calls now: graphs
through the production parse and `to_value`/`from_value` hop, personas through
`Persona::parse` and `persona::merge`, and the effective configs through
`invoke::build`.

- `expected.json`:
  - `graphs` holds every `tests/golden/graph.v*.yaml`, the
    `screenshots/fixture/` graphs and the host graphs, each as the `GraphConfig`
    it read to, rendered as JSON.
  - `personas` holds every shipped persona, the host's personas and the broken
    screenshot persona, each as its label and its merge over the host's base
    config, or as a refusal.
  - `bases` holds the host's base config and the screenshot base, each merged
    with no persona.
- `effective/<graph>.<member>.yaml` holds the `onejudge.yaml` that each two-party
  member of the host's graphs wrote. `effective/node-scope.worker.persona-<name>.yaml`
  holds what the node-scope worker wrote under each host persona. In these
  files the scratch root and this directory's `host/` path are replaced by
  `__SCRATCH__` and `__HOST__`, and the test substitutes its own paths back in
  before reading them. The host's `_template.yaml` was refused as an override,
  so no file was written for it.
