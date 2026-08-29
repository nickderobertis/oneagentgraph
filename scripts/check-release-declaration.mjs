#!/usr/bin/env node
// The schema gate for `release-targets.toml`, this repository's declaration of
// what it publishes.
//
// The schema is not this repository's. It is the canonical one every repository
// in this stack writes against, defined by `onevcs`'s `docs/contract.md` and by
// the `declaration` module beside it: `schema_version`, an optional `probe`, one
// `[[target]]` per consumable artifact carrying `id`, `name`, `what`,
// `published_by` and optionally `manifest` and `covers`, and an optional
// `[[retired]]` carrying `id` and `why`. Nothing here invents a field beside it,
// and a key this schema does not declare is refused BY NAME rather than ignored:
// a typo is the likeliest defect in a hand-written document, and reading
// `manifset` as an absent `manifest` publishes an answer nobody declared.
//
// Why the check lives here at all, when the definition lives elsewhere: the
// document is read across repositories by machinery that has no way to fix it, so
// a declaration that has gone out of shape has to fail in the repository that owns
// it, at the gate its author runs, rather than at the consumer that cannot. When
// the onevcs release carrying `read_release_declaration` reaches crates.io, this
// is the thing to replace with a call to it — that reader is the definition, and
// this is that definition enforced where the file is written.
//
// A declaration written against a LATER schema is read leniently — as this shape,
// with whatever it names beyond it ignored — so a checkout one release behind
// still learns what a repository one release ahead publishes. At the version this
// build knows, it is strict.
//
// Run it as a program to check this repository's own document:
//
//     node scripts/check-release-declaration.mjs [path]
//
// Quiet but for one line on success; on a refusal, what is wrong and where in the
// document it is, then a non-zero exit. `npm/test/release-declaration.test.mjs`
// drives it both ways.

import { readFileSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import { parse } from "smol-toml";

/// The one name a declaration is found under, at a repository's root. Fixed
/// rather than configured: a consumer reads it across repositories it does not
/// own, and a location it would have to be told is one it cannot discover.
export const FILE = "release-targets.toml";

/// The schema version this checker knows, and the oldest it reads.
export const SCHEMA_VERSION = 1;

/// The keys schema version 1 declares, by the table they belong to.
const TOP_LEVEL_KEYS = ["schema_version", "probe", "target", "retired"];
const TARGET_KEYS = ["id", "name", "what", "published_by", "manifest", "covers"];
const RETIRED_KEYS = ["id", "why"];
/// The fields every target must carry. The rest of `TARGET_KEYS` are optional.
const REQUIRED_TARGET_KEYS = ["id", "name", "what", "published_by"];

/// How long each field may be. Prose is rendered on one line beside the entry it
/// describes; the reasoning behind it belongs in a comment.
const MAX_PROSE = 400;
const MAX_IDENTIFIER = 128;
const MAX_TARGET_NAME = 64;

/// A declaration nobody could act on, and where in the document it went wrong.
export class DeclarationError extends Error {}

function refuse(origin, problem) {
  throw new DeclarationError(`the release declaration at ${origin} ${problem}`);
}

/// A registry-qualified identifier, `<registry>:<name>`.
///
/// The registry half is an open vocabulary — `crate`, `pypi` and `npm` are what
/// this repository's probe answers for, but a closed set at this boundary would
/// refuse an artifact somebody genuinely publishes with no way to grant an
/// exception. What is closed is the shape.
function registryId(value, origin, where) {
  if (typeof value !== "string") refuse(origin, `has ${where} that is not a string`);
  if (value.length > MAX_IDENTIFIER) {
    refuse(origin, `has ${where} longer than ${MAX_IDENTIFIER} characters`);
  }
  const split = value.indexOf(":");
  if (split === -1) {
    refuse(
      origin,
      `has ${where} '${value}', which names no registry; spell every identifier as ` +
        "<registry>:<name>, e.g. crate:oneagentgraph, because one name published to two " +
        "registries is two artifacts",
    );
  }
  const registry = value.slice(0, split);
  const name = value.slice(split + 1);
  if (!/^[a-z0-9-]+$/.test(registry)) {
    refuse(
      origin,
      `has ${where} '${value}', whose registry '${registry}' is not one word of lowercase ` +
        "letters, digits, and '-'",
    );
  }
  // The name becomes a path segment of a registry URL wherever one is asked, so it
  // is held to the alphabet crates.io, PyPI and npm all serve.
  if (!/^[A-Za-z0-9][A-Za-z0-9._@/-]*$/.test(name)) {
    refuse(
      origin,
      `has ${where} '${value}', whose name '${name}' is not one a registry serves; spell the ` +
        "name exactly as its registry does",
    );
  }
  return { id: value, registry, name };
}

/// The short name a host document and a consumer's plan wait on this target by.
/// It is deliberately not derivable from the identifier: one repository in this
/// stack publishes both `pypi:onejudge-cli` and `pypi:onejudge`.
function targetName(value, origin, where) {
  if (typeof value !== "string") refuse(origin, `has ${where} that is not a string`);
  if (value.length === 0) refuse(origin, `has ${where} that is empty`);
  if (value.length > MAX_TARGET_NAME) {
    refuse(origin, `has ${where} '${value}' longer than ${MAX_TARGET_NAME} characters`);
  }
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(value)) {
    refuse(
      origin,
      `has ${where} '${value}', which must start with a letter or a digit and hold only ` +
        "letters, digits, '-', '_', and '.'",
    );
  }
  return value;
}

/// One line of operator-written text a reader acts on. A blank one leaves them
/// the identifier alone where they were promised a sentence, and a control
/// character renders as something other than what it is wherever it lands.
function prose(value, origin, where) {
  if (typeof value !== "string") refuse(origin, `has ${where} that is not a string`);
  if (value.trim().length === 0) refuse(origin, `has ${where} blank, and a reader learns nothing`);
  if (value.length > MAX_PROSE) {
    refuse(
      origin,
      `has ${where} longer than ${MAX_PROSE} characters; it is rendered on one line beside the ` +
        "entry it describes, and the reasoning behind it belongs in a comment",
    );
  }
  if (!rendersOnOneLine(value)) {
    refuse(origin, `has ${where} carrying a control character; it is rendered on one line`);
  }
  return value;
}

/// Whether text renders as the one line it is printed on. By code point rather
/// than by a character class, because a class holding control characters is a
/// class nobody can read back — and what is being decided here is precisely
/// whether this text holds one.
function rendersOnOneLine(value) {
  return ![...value].some((character) => {
    const code = character.codePointAt(0);
    return code < 0x20 || code === 0x7f;
  });
}

/// A path to something the repository being released carries, decided on how it is
/// SPELLED rather than on what the reading platform's own path type makes of it:
/// every repository writing one of these shares the document with consumers that
/// resolve it on whichever machine they run on, so a path either names a place in a
/// checkout everywhere or is refused everywhere.
function repositoryPath(value, origin, where) {
  if (typeof value !== "string") refuse(origin, `has ${where} that is not a string`);
  if (value.length === 0) refuse(origin, `has ${where} empty`);
  if (/^[/\\]/.test(value)) {
    refuse(
      origin,
      `has ${where} '${value}', which is absolute; it is a path relative to the repository root, ` +
        "because it names something the repository being released carries",
    );
  }
  if (/^[A-Za-z]:/.test(value)) {
    refuse(origin, `has ${where} '${value}', which names a drive on the reader's own machine`);
  }
  if (value.split(/[/\\]/).includes("..")) {
    refuse(origin, `has ${where} '${value}', which leaves the repository root`);
  }
  return value;
}

/// Refuse a key this schema does not declare, naming it and the table it is in.
function refuseUnknownKeys(document, origin) {
  const unknown = (table, key) =>
    refuse(
      origin,
      `names '${key}' in ${table}, which schema_version ${SCHEMA_VERSION} does not declare; a ` +
        "misspelled key would otherwise be read as an absent one",
    );
  for (const key of Object.keys(document)) {
    if (!TOP_LEVEL_KEYS.includes(key)) unknown("the document", key);
  }
  for (const [array, keys] of [
    ["target", TARGET_KEYS],
    ["retired", RETIRED_KEYS],
  ]) {
    const entries = document[array];
    if (!Array.isArray(entries)) continue;
    entries.forEach((entry, index) => {
      if (entry === null || typeof entry !== "object") return;
      for (const key of Object.keys(entry)) {
        if (!keys.includes(key)) unknown(`[[${array}]] ${index + 1}`, key);
      }
    });
  }
}

/// One `[[target]]`: every field it must carry, and every field it may.
function declaredTarget(entry, index, origin) {
  const at = `[[target]] ${index + 1}`;
  if (entry === null || typeof entry !== "object" || Array.isArray(entry)) {
    refuse(origin, `has ${at}, which is not a table`);
  }
  for (const key of REQUIRED_TARGET_KEYS) {
    if (entry[key] === undefined) {
      refuse(
        origin,
        `has ${at} with no ${key}; schema_version ${SCHEMA_VERSION} requires ` +
          `${REQUIRED_TARGET_KEYS.join(", ")} of every target`,
      );
    }
  }
  const id = registryId(entry.id, origin, `${at}'s id`);
  const covers = entry.covers ?? [];
  if (!Array.isArray(covers)) {
    refuse(origin, `has ${at} (${id.id}) with a covers that is not a list of identifiers`);
  }
  return {
    id: id.id,
    registry: id.registry,
    artifact: id.name,
    name: targetName(entry.name, origin, `${at} (${id.id})'s name`),
    what: prose(entry.what, origin, `${at} (${id.id})'s what`),
    published_by: prose(entry.published_by, origin, `${at} (${id.id})'s published_by`),
    manifest:
      entry.manifest === undefined
        ? undefined
        : repositoryPath(entry.manifest, origin, `${at} (${id.id})'s manifest`),
    covers: covers.map((covered, position) =>
      registryId(covered, origin, `${at} (${id.id})'s covers entry ${position + 1}`),
    ),
  };
}

/// One `[[retired]]`: an artifact this repository once published, recorded rather
/// than deleted so a consumer still naming it is told it is gone.
function retiredArtifact(entry, index, origin) {
  const at = `[[retired]] ${index + 1}`;
  if (entry === null || typeof entry !== "object" || Array.isArray(entry)) {
    refuse(origin, `has ${at}, which is not a table`);
  }
  for (const key of RETIRED_KEYS) {
    if (entry[key] === undefined) refuse(origin, `has ${at} with no ${key}`);
  }
  const id = registryId(entry.id, origin, `${at}'s id`);
  return { id: id.id, why: prose(entry.why, origin, `${at} (${id.id})'s why`) };
}

/// Refuse a declaration whose fields are each readable but which together say
/// something no repository can mean. What is wrong on its own is refused by the
/// conversions above; what only a whole document can be wrong about is here, and
/// each refusal names the entry it is about by its position and its identifier.
function validate(declaration, origin) {
  if (declaration.targets.length === 0) {
    refuse(
      origin,
      "declares no [[target]]; a declaration that names nothing says less than no declaration " +
        "at all, because a consumer cannot tell whether this repository publishes nothing or " +
        "nobody has said what it publishes",
    );
  }
  const ids = new Map();
  const names = new Map();
  declaration.targets.forEach((target, index) => {
    const at = `[[target]] ${index + 1} (${target.id})`;
    if (names.has(target.name)) {
      refuse(
        origin,
        `has ${at} taking the short name '${target.name}', which [[target]] ` +
          `${names.get(target.name) + 1} already takes; the short name is what a host document ` +
          "and a consumer's plan name this target by, so two of them are two answers to one " +
          "question",
      );
    }
    names.set(target.name, index);
    if (ids.has(target.id)) {
      refuse(
        origin,
        `has ${at} declaring the identifier [[target]] ${ids.get(target.id) + 1} already ` +
          "declares; one artifact is one target",
      );
    }
    ids.set(target.id, index);
  });
  refuseMiscovered(declaration, ids, origin);
  refuseMisretired(declaration, ids, origin);
}

/// Hold every `covers` entry to what covering means. A covered identifier is
/// shipped by a target's release and is NOT a target of its own — that is the
/// whole distinction the key exists to draw — so an identifier that is both is a
/// document saying two things about one artifact, and an identifier two targets
/// both cover is a document with no answer for which release ships it.
function refuseMiscovered(declaration, ids, origin) {
  const seen = new Map();
  declaration.targets.forEach((target, index) => {
    const at = `[[target]] ${index + 1} (${target.id})`;
    for (const entry of target.covers) {
      if (entry.id === target.id) {
        refuse(
          origin,
          `has ${at} covering its own identifier; covers names what a target's release also ` +
            "ships and that is not a target of its own",
        );
      }
      if (ids.has(entry.id)) {
        refuse(
          origin,
          `has ${at} covering ${entry.id}, which [[target]] ${ids.get(entry.id) + 1} declares as ` +
            "a target of its own; an artifact is one or the other, because a consumer waits on a " +
            "target by name and never waits on something covered",
        );
      }
      if (seen.has(entry.id)) {
        refuse(
          origin,
          `has ${at} covering ${entry.id}, which [[target]] ${seen.get(entry.id) + 1} already ` +
            "covers; one artifact is shipped by one release",
        );
      }
      seen.set(entry.id, index);
    }
  });
}

/// Hold every `[[retired]]` entry to what retirement means: it is not published
/// any more, so a document that also publishes it is two answers about one
/// artifact.
function refuseMisretired(declaration, ids, origin) {
  const seen = new Map();
  declaration.retired.forEach((entry, index) => {
    const at = `[[retired]] ${index + 1} (${entry.id})`;
    if (ids.has(entry.id)) {
      refuse(
        origin,
        `has ${at} retiring what [[target]] ${ids.get(entry.id) + 1} publishes; a retired ` +
          "artifact is one this repository does not publish any more",
      );
    }
    if (seen.has(entry.id)) {
      refuse(origin, `has ${at} repeating what [[retired]] ${seen.get(entry.id) + 1} records`);
    }
    seen.set(entry.id, index);
  });
}

/// Validate one declaration's text and answer what it declares. `origin` is what
/// the refusals name the document by — a path, a URL, or whatever the caller
/// knows it as.
export function parseDeclaration(raw, origin) {
  let document;
  try {
    document = parse(raw);
  } catch (failure) {
    refuse(origin, `is not TOML: ${failure.message}`);
  }
  // The version is read before the shape is enforced, and refused before it too:
  // which keys a document may carry is a fact about the schema it declares, so one
  // this checker cannot read is answered as that rather than as whichever of its
  // keys was unrecognized first.
  const declared = document.schema_version;
  if (!Number.isInteger(declared)) {
    refuse(
      origin,
      "declares no schema_version; every declaration opens with " +
        `'schema_version = ${SCHEMA_VERSION}', before any table`,
    );
  }
  if (declared < SCHEMA_VERSION) {
    refuse(
      origin,
      `declares schema_version ${declared}; this checker reads schema_version ` +
        `${SCHEMA_VERSION} and newer`,
    );
  }
  // Only at the version this checker knows. A later schema's keys are not its to
  // have an opinion on, and are ignored.
  if (declared === SCHEMA_VERSION) refuseUnknownKeys(document, origin);

  const targets = document.target ?? [];
  if (!Array.isArray(targets)) {
    refuse(origin, "has a target that is not a list of [[target]] tables");
  }
  const retiredEntries = document.retired ?? [];
  if (!Array.isArray(retiredEntries)) {
    refuse(origin, "has a retired that is not a list of [[retired]] tables");
  }
  const declaration = {
    schema_version: declared,
    probe:
      document.probe === undefined ? undefined : repositoryPath(document.probe, origin, "a probe"),
    targets: targets.map((entry, index) => declaredTarget(entry, index, origin)),
    retired: retiredEntries.map((entry, index) => retiredArtifact(entry, index, origin)),
  };
  validate(declaration, origin);
  return declaration;
}

/// Read the declaration a repository carries. `path` is either the repository
/// root or the `release-targets.toml` in it, so a caller with a checkout and a
/// caller with a file both spell what they have.
///
/// A repository carrying no declaration is refused rather than answered with an
/// empty one: "this repository publishes nothing" and "nobody has said what this
/// repository publishes" are different answers, and a consumer waiting on a
/// release acts differently on each.
export function readDeclaration(path) {
  let document = path;
  try {
    if (statSync(path).isDirectory()) document = join(path, FILE);
  } catch {
    // Not there at all, which the guard below reports in the one sentence that
    // answers it.
  }
  let raw;
  try {
    raw = readFileSync(document, "utf8");
  } catch (failure) {
    if (failure.code === "ENOENT") {
      throw new DeclarationError(
        `${document} declares no release targets: there is no ${FILE} there, so nothing says ` +
          "what this repository publishes",
      );
    }
    throw new DeclarationError(
      `cannot read the release declaration at ${document}: ${failure.message}`,
    );
  }
  return parseDeclaration(raw, document);
}

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function main(argv) {
  const path = argv[0] ?? join(REPO_ROOT, FILE);
  let declaration;
  try {
    declaration = readDeclaration(path);
  } catch (failure) {
    if (!(failure instanceof DeclarationError)) throw failure;
    console.error(`check-release-declaration: ${failure.message}`);
    console.error(
      "ACTION: fix what the refusal above names — the schema is the canonical release-target " +
        "declaration, in onevcs's docs/contract.md",
    );
    return 1;
  }
  const named = declaration.targets.map((target) => `${target.name} (${target.id})`).join(", ");
  console.log(
    `check-release-declaration: ${path} declares ${declaration.targets.length} targets against ` +
      `schema_version ${declaration.schema_version} — ${named}`,
  );
  return 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(main(process.argv.slice(2)));
}
