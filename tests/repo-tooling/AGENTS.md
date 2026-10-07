# The repository's own tooling (`oneagentgraph-repo-tooling`)

These journeys prove the gate rather than the crate: the justfile, `scripts/`,
the Nx graph and its affected selection, the judged tier's cache, and what the
workflows schedule. They exercise no line of the crate, so this tier runs
uninstrumented and counts toward no coverage floor.

- **Drive the real thing in a throwaway copy.** `checkout.rs` copies what git
  would commit into a temporary directory and links this checkout's
  `node_modules`; journeys commit there, never here. The real justfile,
  `scripts/nx.sh`, `scripts/nx-affected.sh`, Nx, and git run; only a paid or
  non-deterministic boundary (the `llmlint` judge) is stood in for.
- **The inputs are the tooling.** `repoToolingReads` in `nx.json` names
  `scripts/**`, every `project.json`, the workflows, and `.gitignore`; the
  justfile and `nx.json` arrive through `sharedGlobals`. A journey that reads
  another file adds it there, or a change to that file never reaches this tier.
- **The required contexts are written down once, here.** `workflow_contract.rs`
  holds the workflows to the status-check contexts main's branch protection
  requires; when a job or matrix leg is added or renamed, that list moves with it
  and branch protection is re-applied.
- Unix-only where the subject is bash: the Windows leg reaches those scripts
  through a shell these journeys cannot assume.
