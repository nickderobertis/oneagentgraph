// How this repository's own tiers *read* `release-targets.toml`: as a consumer
// with a standard TOML parser and no knowledge of this repository reads it.
//
// Reading is all this does. Whether the document conforms to the canonical
// release-target schema is decided by that schema's own implementation —
// `onevcs`'s reader, called from `tests/release_declaration.rs` — and never by
// anything here: a schema restated in the repository that writes against it is a
// second opinion presented as the first one, and the copy is what drifts. So there
// is no validation below, and a document that is wrong is wrong at that gate.
//
// What a caller gets back is the identifier split the way the schema spells it,
// `<registry>:<name>`, because both callers ask a registry a question about a name.
//
// Shared by the drift tier (`npm/test/release-targets.test.mjs`, which holds this
// list against what a release really publishes) and the live probe tier
// (`npm/test/live/release-probe.test.mjs`, which asks each registry about it).

import { readFileSync } from "node:fs";
import { join } from "node:path";
import assert from "node:assert/strict";

import { parse } from "smol-toml";

/// The one name a repository's release declaration is found under, at its root.
/// Fixed rather than configured: a consumer reads this file across repositories it
/// does not own, and a location it would have to be told is one it cannot discover.
export const FILE = "release-targets.toml";

/// One identifier, split into the registry that serves it and the name it is served
/// under. The qualification is the whole point — `oneagentgraph-cli` is a PyPI
/// project *and* an npm package — so nothing here ever carries a bare name alone.
function identifier(id, where) {
  const colon = id.indexOf(":");
  assert.notEqual(colon, -1, `${where} names ${id}, which is not <registry>:<name>`);
  return { id, registry: id.slice(0, colon), artifact: id.slice(colon + 1) };
}

/// What one repository declares it publishes, read off the document at `path` —
/// either a repository root or the `release-targets.toml` in it.
export function readDeclaration(path) {
  const document = path.endsWith(FILE) ? path : join(path, FILE);
  const declared = parse(readFileSync(document, "utf8"));
  const targets = declared.target ?? [];
  assert.ok(targets.length > 0, `${document} declares no [[target]]`);
  return {
    schema_version: declared.schema_version,
    probe: declared.probe,
    targets: targets.map((target) => ({
      ...identifier(target.id, `${document}'s ${target.name} target`),
      // The short name a host document and a consumer's plan wait on this target
      // by. It cannot be derived from the identifier, which is why it is declared.
      name: target.name,
      // What the target's release also ships and nothing depends on by name.
      covers: (target.covers ?? []).map((id) =>
        identifier(id, `${document}'s ${target.name} target covers`),
      ),
    })),
    retired: (declared.retired ?? []).map((entry) => ({
      ...identifier(entry.id, `${document}'s retired entry`),
      why: entry.why,
    })),
  };
}
