# The test tiers

The suite is split into Nx projects so a change runs only the tiers that read
what it touched. One Cargo package and one lockfile still hold all of it; the
split is in which test binaries each project's targets hand to cargo.

| project | `project.json` | runs | instrumented |
| --- | --- | --- | --- |
| `oneagentgraph` | `src/` | the library's and binaries' unit tests (`--lib --bins`) | yes |
| `oneagentgraph-contract` | `tests/` (this directory) | `tests/*.rs`: the contract, schema, golden, inventory, release-declaration, and packaging tests | yes |
| `oneagentgraph-e2e` | `tests/e2e/` | the binary journeys | yes |
| `oneagentgraph-repo-tooling` | `tests/repo-tooling/` | the justfile, `scripts/`, Nx, and workflow journeys | no |

Rules that hold as this grows:

- **A test target belongs to exactly one tier.** The justfile's `contract-tests`,
  `e2e-tests`, and `repo-tooling-tests` lists are the assignment, and
  `tests/repo-tooling/tier_partition.rs` fails the gate on a target in no list,
  in two, or outside the directory its tier's project owns. Cargo discovers a new
  `tests/*.rs` by itself; add its name to `contract-tests` in the same change.
- **A file a test reads is an input of that tier's `test` target.** Nx cannot see
  through `include_str!` or a path joined onto `CARGO_MANIFEST_DIR`, so the
  repository files each tier reads are named in `nx.json` (`contractReads`,
  `e2eReads`, `repoToolingReads`). A file missing there is a change that replays a
  stale green from the cache, and one Nx does not count as reaching the tier.
- **Coverage is one floor over the three instrumented tiers.** Each writes its
  raw profiles under its own name (`target/llvm-cov-target/oneagentgraph-<tier>-*`,
  declared as its `outputs`), and `oneagentgraph:coverage` merges them at 95%
  lines. Every instrumented tier's `check` depends on that target, so a change to
  any one of them re-proves the floor.
- **The golden files are contracts.** A change to a serialized shape bumps its
  schema version and adds the new golden beside the old, in the same change.
