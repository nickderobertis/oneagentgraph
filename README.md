# oneagentgraph

Compose agents into a graph over [oneharness] and [onejudge], and merge their
outputs into **one NDJSON event stream**.

One config file and one CLI call: the graph names an oneharness config per role
and side, and `oneagentgraph` prepares each member's launch, supervises liveness,
and emits every turn, tool call, fallback, and settle as an event you can pipe
into anything.

## Install

```bash
pip install oneagentgraph-cli      # prebuilt binary, no Rust toolchain
npm install -g oneagentgraph-cli   # the same binary, via npm
cargo install oneagentgraph        # from crates.io, compiled locally
```

To install a revision that has not been released yet, build it from the
repository:

```bash
cargo install --git https://github.com/nickderobertis/oneagentgraph --locked
```

Prebuilt archives for Linux (x86-64, arm64), macOS (Intel, Apple silicon), and
Windows (x86-64) are attached to every [release], with `sha256` checksums.

[onejudge] is a **library dependency**, linked into this binary — there is
nothing to install for it. `run`, `smoke`, and `health` drive the [oneharness]
CLI, so that has to be on `PATH`; `validate`, `history`, `persona`, `trigger`,
`reset-timer`, and `cancel` need nothing at all.
`ONEAGENTGRAPH_ONEHARNESS_BIN` names a pinned install instead.

## What it does

```bash
oneagentgraph run graph.yaml --task "add the retry" --output json
```

A graph is YAML — members, the oneharness config each side uses, personas,
schedules, and dependencies:

```yaml
version: 1
name: node-scope
members:
  worker:
    kind: onejudge
    base_config: ./onejudge.base.yaml
    agent: { oneharness_config: ./oneharness.toml }
    judge: { oneharness_config: ./oneharness.judge.toml }
    mode: bypass
```

`run` streams one envelope per line. Exit `0` means every member settled, `1`
that one failed or died, `2` that the config is invalid.

**oneagentgraph owns no harness, model, or fallback logic.** oneharness keeps
owning identity chains, fallback, model pins, and quota classification; onejudge
keeps owning the two-party conversation. This composes them.


## Develop

```bash
just bootstrap   # from a clean clone; installs the oneharness CLI
just check       # the deterministic gate: format, clippy, tests, coverage, docs
just gate        # check + the LLM-judge tier; the pre-push bar
```

## Licence

MIT. See [LICENSE](LICENSE).

[oneharness]: https://github.com/nickderobertis/oneharness
[onejudge]: https://pypi.org/project/onejudge/
[release]: https://github.com/nickderobertis/oneagentgraph/releases
