// The schema gate for `release-targets.toml`, driven end to end.
//
// `scripts/check-release-declaration.mjs` holds this repository's declaration to
// the canonical release-target schema — the one `onevcs`'s `docs/contract.md`
// defines and every repository in this stack writes against. A declaration is
// written once and then read by machinery that has no way to fix it, so the whole
// value of the check is in what it REFUSES: a document that has gone out of shape
// must fail here, at the gate its author runs, rather than at a consumer that can
// only wait.
//
// So the script is run as a real subprocess — never imported and called — over
// this repository's own document and over a document mutated one field at a time,
// and the assertions are its exit code and what it said. Each mutation is made by
// parsing the real declaration and changing exactly one thing about it, so every
// refusal below is a refusal of a document somebody could plausibly have written.

import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

import { parse, stringify } from "smol-toml";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const CHECKER = join(REPO_ROOT, "scripts", "check-release-declaration.mjs");
const DECLARATION = join(REPO_ROOT, "release-targets.toml");

/// Run the real checker as a program, the way `just check` and a person both run
/// it: a subprocess, from the repository root, with the document it is to read as
/// its one argument (or none at all, which is the repository's own).
function check(...args) {
  const result = spawnSync(process.execPath, [CHECKER, ...args], {
    cwd: REPO_ROOT,
    encoding: "utf8",
    timeout: 30_000,
  });
  assert.equal(result.error, undefined, `the checker could not be spawned: ${result.error}`);
  return result;
}

/// The real declaration, as data, for a case to change one thing about.
function declared() {
  return parse(readFileSync(DECLARATION, "utf8"));
}

/// Write `document` — TOML text, or a declaration to render as TOML — into a
/// directory of this case's own, and answer what the checker made of it.
function checkDocument(document) {
  const dir = mkdtempSync(join(tmpdir(), "release-declaration-"));
  try {
    const path = join(dir, "release-targets.toml");
    writeFileSync(path, typeof document === "string" ? document : stringify(document));
    return check(path);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/// A refusal: a non-zero exit, nothing on stdout — a caller reads stdout as the
/// answer — and a reason naming the problem, followed by a next action.
function assertRefused(result, because, reason) {
  assert.notEqual(
    result.status,
    0,
    `${because}: expected a non-zero exit, got 0\n${result.stdout}`,
  );
  assert.equal(result.stdout, "", `${because}: wrote to stdout while refusing`);
  assert.match(result.stderr, reason, because);
  assert.match(result.stderr, /ACTION:/, `${because}: gave no next action on stderr`);
}

describe("the release-target declaration", () => {
  it("passes the schema check this repository ships", () => {
    const result = check();
    assert.equal(result.status, 0, `release-targets.toml was refused:\n${result.stderr}`);
    assert.match(result.stdout, /declares 3 targets against schema_version 1/);
    // The short name each artifact is waited on by is the point of the document,
    // so the one line a green run prints is what names them.
    for (const target of declared().target) {
      assert.match(result.stdout, new RegExp(`${target.name} \\(${target.id}\\)`));
    }
  });

  it("refuses a target missing a required field", () => {
    const document = declared();
    delete document.target[0].what;
    assertRefused(
      checkDocument(document),
      "a target with no `what`",
      /\[\[target\]\] 1 with no what/,
    );
  });

  it("refuses an identifier that names no registry", () => {
    const document = declared();
    document.target[0].id = "oneagentgraph";
    assertRefused(
      checkDocument(document),
      "an unqualified identifier",
      /'oneagentgraph', which names no registry/,
    );
  });

  it("refuses an identifier a registry could not serve", () => {
    const document = declared();
    document.target[0].id = "crate:one agentgraph";
    assertRefused(
      checkDocument(document),
      "an identifier with a space in its name",
      /is not one a registry serves/,
    );
  });

  it("refuses two targets taking one short name", () => {
    const document = declared();
    document.target[1].name = document.target[0].name;
    assertRefused(
      checkDocument(document),
      "a repeated short name",
      /taking the short name '.+', which \[\[target\]\] 1 already takes/,
    );
  });

  it("refuses two targets declaring one identifier", () => {
    const document = declared();
    document.target[1].id = document.target[0].id;
    assertRefused(
      checkDocument(document),
      "a repeated identifier",
      /declaring the identifier \[\[target\]\] 1 already declares/,
    );
  });

  it("refuses a key the schema does not declare, by name", () => {
    const document = declared();
    document.target[0].name_from = "Cargo.toml [package] name";
    assertRefused(
      checkDocument(document),
      "a key from a shape this schema replaced",
      /names 'name_from' in \[\[target\]\] 1, which schema_version 1 does not declare/,
    );
  });

  it("refuses covering something the same document declares as a target", () => {
    const document = declared();
    document.target[2].covers = [document.target[0].id];
    assertRefused(
      checkDocument(document),
      "a covered identifier that is also a target",
      /declares as a target of its own/,
    );
  });

  it("refuses one identifier covered by two targets", () => {
    const document = declared();
    const covered = document.target[2].covers[0];
    document.target[0].covers = [covered];
    assertRefused(
      checkDocument(document),
      "an identifier two releases both claim to ship",
      /which \[\[target\]\] 1 already covers/,
    );
  });

  it("refuses a path that leaves the repository", () => {
    const document = declared();
    document.probe = "/usr/local/bin/release-probe.sh";
    assertRefused(checkDocument(document), "an absolute probe", /which is absolute/);
  });

  it("refuses prose a reader would learn nothing from", () => {
    const document = declared();
    document.target[0].what = "   ";
    assertRefused(checkDocument(document), "a blank `what`", /blank, and a reader learns nothing/);
  });

  it("refuses prose that would not render on the line it is printed on", () => {
    const document = declared();
    document.target[0].what = "The library and the binary.\nAnd a second line nobody will see.";
    assertRefused(
      checkDocument(document),
      "a `what` carrying a newline",
      /carrying a control character/,
    );
  });

  it("refuses a declaration with no target at all", () => {
    const document = declared();
    document.target = [];
    assertRefused(
      checkDocument(document),
      "a declaration that names nothing",
      /declares no \[\[target\]\]/,
    );
  });

  it("refuses a document with no schema_version", () => {
    const document = declared();
    delete document.schema_version;
    assertRefused(checkDocument(document), "an unversioned document", /declares no schema_version/);
  });

  it("refuses a schema older than the one it reads", () => {
    const document = declared();
    document.schema_version = 0;
    assertRefused(
      checkDocument(document),
      "a schema this checker predates",
      /declares schema_version 0; this checker reads schema_version 1 and newer/,
    );
  });

  it("refuses a document that is not TOML", () => {
    assertRefused(
      checkDocument("{ this is json }\n"),
      "a document in another format",
      /is not TOML/,
    );
  });

  it("refuses a repository that carries no declaration at all", () => {
    // "This repository publishes nothing" and "nobody has said what this
    // repository publishes" are different answers, and a consumer waiting on a
    // release acts differently on each.
    const dir = mkdtempSync(join(tmpdir(), "release-declaration-"));
    try {
      assertRefused(check(dir), "a checkout with no declaration", /declares no release targets/);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("reads a later schema leniently, so a consumer one release behind still learns what this publishes", () => {
    const document = declared();
    document.schema_version = 2;
    document.target[0].something_later = "a key this schema does not know";
    const result = checkDocument(document);
    assert.equal(result.status, 0, `a later schema was refused:\n${result.stderr}`);
    assert.match(result.stdout, /declares 3 targets against schema_version 2/);
  });
});
