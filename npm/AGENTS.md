# npm distribution

`oneagentgraph-cli` on npm is a **launcher** that carries no binary: the prebuilt
binary ships in a per-platform package (`oneagentgraph-cli-<platform>-<arch>`)
that npm selects by `os`/`cpu`, and `bin/oneagentgraph.js` resolves it and execs
it with the caller's argv.

Four places name that platform matrix and must move together:

1. `bin/oneagentgraph.js`'s `PACKAGES` map,
2. `package.json`'s `optionalDependencies`,
3. `scripts/npm-build.mjs`'s `TARGETS` table,
4. the `build-npm` matrix in `.github/workflows/release.yml`.

The committed `package.json` carries `0.0.0-managed`, not a real version. The
version has exactly one source — `Cargo.toml`, written by release-plz — and
`scripts/npm-build.mjs` stamps it into the launcher and every platform pin at
publish time. Never hand-edit a version here.

`test/` also carries the checks that are about the release rather than about
npm — the release-target declaration's drift against what a release really
publishes, and the release probe — because this is the packaging project and they
read the same `release.yml` the matrix gate does. Whether that declaration is a
*shape* its schema allows is not asked here at all:
`tests/release_declaration.rs` hands it to `onevcs`'s own reader, which is the one
implementation of that schema, and `test/support/declaration.mjs` merely parses
the document the way any consumer with a TOML parser does.

One tier lives under `test/live/`, outside the `npm/test/*.test.mjs` the `test`
target runs, so `just check` stays offline: the probe's registry-served answers,
which `just release-probe-check` runs. It needs what a clean clone does not have —
the public registries.

Nothing in this directory is published from a developer's machine:
`.github/workflows/release.yml` assembles, packs, and publishes it, and
`scripts/publish-npm.sh` makes that publish idempotent.
