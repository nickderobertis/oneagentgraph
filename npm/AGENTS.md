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
npm — the release-target declaration, in both halves (its schema, and its drift
against what a release really publishes), and the release probe — because this is
the packaging project and they read the same `release.yml` the matrix gate does.

Two tiers live under `test/live/`, outside the `npm/test/*.test.mjs` the `test`
target runs, so `just check` stays offline: the probe's registry-served answers,
which `just release-probe-check` runs, and the canonical reader's verdict on the
declaration, which `just release-declaration-check` runs. Each needs something a
clean clone does not have — the public registries, and an `onevcs` new enough to
read a declaration.

Nothing in this directory is published from a developer's machine:
`.github/workflows/release.yml` assembles, packs, and publishes it, and
`scripts/publish-npm.sh` makes that publish idempotent.
