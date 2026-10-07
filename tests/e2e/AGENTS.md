# The binary journeys (`oneagentgraph-e2e`)

<!-- llmlint: ignore-block[e2e_not_mocked, tests_mirror_real_usage] the paid
harness process is the one seam the root AGENTS.md invariants permit to be faked:
it is a billed, non-deterministic model process, and every other boundary a
journey crosses here — this binary, `oneharness`, onejudge — is real. -->
Every journey here drives the compiled `oneagentgraph` binary as a subprocess and
asserts on its exit code, stdout, stderr, and the files it leaves — never an
in-process `main()`. The paid harness process is the one seam that is faked, at
oneharness's own `ONEHARNESS_BIN_<ID>` override (`support.rs`); everything else
is real, including the pinned `oneharness` CLI the justfile's `oneharness-version`
names, which `just bootstrap` installs and `support.rs` reads from the justfile
rather than restating.
<!-- llmlint: ignore-end[e2e_not_mocked, tests_mirror_real_usage] -->

- **What this tier reads is its Nx input.** `e2eSource` and `e2eReads` in
  `nx.json` name it: these sources, the crate, `README.md`, and
  `scripts/smoke-published.sh`, plus the justfile through `sharedGlobals`. A
  journey that starts reading another repository file adds it there.
- **Host tools are prerequisites, not skips.** `history_store.rs` records a run's
  file-system calls with `strace`; a journey whose tool is missing fails and says
  how to install it rather than passing without having run.
- **The tier runs alone.** Its `test` and `test-quick` targets carry
  `parallelism: false`, as every Rust tier's do, so its liveness and watchdog
  journeys never share the machine with a second suite — the load they saw when
  the whole suite was one nextest run.
