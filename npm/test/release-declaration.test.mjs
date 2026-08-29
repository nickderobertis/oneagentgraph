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
//
// Every refusal the checker can reach from a document has a row in REFUSALS. A
// validator whose only exercised path is the one its own repository takes is a
// validator nobody has checked: the paths that matter are the ones a document that
// has gone wrong takes, and each of those is reached here through the same file the
// gate reads, rendered as real TOML.

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

/// Every refusal a declaration can walk into, one row each: what a document did,
/// and the sentence the checker must answer it with. `mutate` receives the real
/// declaration, parsed, and changes exactly one thing about it.
const REFUSALS = [
  {
    because: "a target with no `what`",
    mutate: (document) => {
      delete document.target[0].what;
    },
    reason: /\[\[target\]\] 1 with no what/,
  },
  {
    because: "an unqualified identifier",
    mutate: (document) => {
      document.target[0].id = "oneagentgraph";
    },
    reason: /'oneagentgraph', which names no registry/,
  },
  {
    because: "an identifier with a space in its name",
    mutate: (document) => {
      document.target[0].id = "crate:one agentgraph";
    },
    reason: /is not one a registry serves/,
  },
  {
    because: "an identifier that is not text at all",
    mutate: (document) => {
      document.target[0].id = 3;
    },
    reason: /id that is not a string/,
  },
  {
    because: "an identifier no refusal could quote in a sentence",
    mutate: (document) => {
      document.target[0].id = `crate:${"a".repeat(200)}`;
    },
    reason: /longer than 128 characters/,
  },
  {
    because: "a registry spelled in a way no URL is built from",
    mutate: (document) => {
      document.target[0].id = "Crates.io:oneagentgraph";
    },
    reason: /is not one word of lowercase letters, digits, and '-'/,
  },
  {
    because: "a short name that is not text",
    mutate: (document) => {
      document.target[0].name = 7;
    },
    reason: /name that is not a string/,
  },
  {
    because: "an empty short name",
    mutate: (document) => {
      document.target[0].name = "";
    },
    reason: /name that is empty/,
  },
  {
    because: "a short name longer than a refusal can quote",
    mutate: (document) => {
      document.target[0].name = "a".repeat(65);
    },
    reason: /longer than 64 characters/,
  },
  {
    because: "a short name spelled outside the alphabet a consumer types",
    mutate: (document) => {
      document.target[0].name = "crate/one";
    },
    reason: /must start with a letter or a digit and hold only/,
  },
  {
    because: "a repeated short name",
    mutate: (document) => {
      document.target[1].name = document.target[0].name;
    },
    reason: /taking the short name '.+', which \[\[target\]\] 1 already takes/,
  },
  {
    because: "a repeated identifier",
    mutate: (document) => {
      document.target[1].id = document.target[0].id;
    },
    reason: /declaring the identifier \[\[target\]\] 1 already declares/,
  },
  {
    because: "a key the schema does not declare beside `schema_version` itself",
    mutate: (document) => {
      document.probes = "scripts/release-probe.sh";
    },
    reason: /names 'probes' in the document, which schema_version 1 does not declare/,
  },
  {
    because: "a blank `published_by`",
    mutate: (document) => {
      document.target[0].published_by = "";
    },
    reason: /published_by blank, and a reader learns nothing/,
  },
  {
    because: "a retired artifact whose reason says nothing",
    mutate: (document) => {
      document.retired = [{ id: "pypi:oneagentgraph-legacy", why: "  " }];
    },
    reason: /why blank, and a reader learns nothing/,
  },
  {
    because: "a key from a shape this schema replaced",
    mutate: (document) => {
      document.target[0].name_from = "Cargo.toml [package] name";
    },
    reason: /names 'name_from' in \[\[target\]\] 1, which schema_version 1 does not declare/,
  },
  {
    because: "prose that is not text",
    mutate: (document) => {
      document.target[0].what = 1;
    },
    reason: /what that is not a string/,
  },
  {
    because: "a blank `what`",
    mutate: (document) => {
      document.target[0].what = "   ";
    },
    reason: /blank, and a reader learns nothing/,
  },
  {
    because: "prose long enough to be the reasoning rather than the sentence",
    mutate: (document) => {
      document.target[0].what = "a".repeat(401);
    },
    reason: /longer than 400 characters/,
  },
  {
    because: "a `what` carrying a newline",
    mutate: (document) => {
      document.target[0].what = "The library and the binary.\nAnd a line nobody will see.";
    },
    reason: /carrying a control character/,
  },
  {
    because: "an absolute probe",
    mutate: (document) => {
      document.probe = "/usr/local/bin/release-probe.sh";
    },
    reason: /which is absolute/,
  },
  {
    because: "an empty manifest path",
    mutate: (document) => {
      document.target[0].manifest = "";
    },
    reason: /manifest empty/,
  },
  {
    because: "a manifest on the reader's own drive",
    mutate: (document) => {
      document.target[0].manifest = "C:\\Cargo.toml";
    },
    reason: /names a drive on the reader's own machine/,
  },
  {
    because: "a manifest outside the repository, spelled the way Windows spells it",
    mutate: (document) => {
      document.target[0].manifest = "..\\elsewhere\\Cargo.toml";
    },
    reason: /which leaves the repository root/,
  },
  {
    because: "a covered identifier that is also a target",
    mutate: (document) => {
      document.target[2].covers = [document.target[0].id];
    },
    reason: /declares as a target of its own/,
  },
  {
    because: "an identifier two releases both claim to ship",
    mutate: (document) => {
      document.target[0].covers = [document.target[2].covers[0]];
    },
    reason: /which \[\[target\]\] 1 already covers/,
  },
  {
    because: "a target covering its own identifier",
    mutate: (document) => {
      document.target[0].covers = [document.target[0].id];
    },
    reason: /covering its own identifier/,
  },
  {
    because: "a covers that is not a list of identifiers",
    mutate: (document) => {
      document.target[2].covers = "npm:oneagentgraph-cli-linux-x64";
    },
    reason: /covers that is not a list of identifiers/,
  },
  {
    because: "a target that is not a table",
    mutate: (document) => {
      document.target = ["crate:oneagentgraph"];
    },
    reason: /\[\[target\]\] 1, which is not a table/,
  },
  {
    because: "a target key holding something other than tables",
    mutate: (document) => {
      document.target = "crate:oneagentgraph";
    },
    reason: /target that is not a list of \[\[target\]\] tables/,
  },
  {
    because: "a retired key holding something other than tables",
    mutate: (document) => {
      document.retired = "pypi:gone";
    },
    reason: /retired that is not a list of \[\[retired\]\] tables/,
  },
  {
    because: "a retired artifact with no reason given",
    mutate: (document) => {
      document.retired = [{ id: "pypi:oneagentgraph-legacy" }];
    },
    reason: /\[\[retired\]\] 1 with no why/,
  },
  {
    because: "retiring something the same document publishes",
    mutate: (document) => {
      document.retired = [{ id: document.target[1].id, why: "Nothing publishes it now." }];
    },
    reason: /retiring what \[\[target\]\] 2 publishes/,
  },
  {
    because: "one artifact retired twice",
    mutate: (document) => {
      document.retired = [
        { id: "pypi:oneagentgraph-legacy", why: "Nothing publishes it now." },
        { id: "pypi:oneagentgraph-legacy", why: "Said again." },
      ];
    },
    reason: /repeating what \[\[retired\]\] 1 records/,
  },
  {
    because: "a declaration that names nothing",
    mutate: (document) => {
      document.target = [];
    },
    reason: /declares no \[\[target\]\]/,
  },
  {
    because: "an unversioned document",
    mutate: (document) => {
      delete document.schema_version;
    },
    reason: /declares no schema_version/,
  },
  {
    because: "a schema this checker predates",
    mutate: (document) => {
      document.schema_version = 0;
    },
    reason: /declares schema_version 0; this checker reads schema_version 1 and newer/,
  },
];

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

  for (const { because, mutate, reason } of REFUSALS) {
    it(`refuses ${because}`, () => {
      const document = declared();
      mutate(document);
      assertRefused(checkDocument(document), because, reason);
    });
  }

  it("refuses a document that is not TOML", () => {
    assertRefused(
      checkDocument("{ this is json }\n"),
      "a document in another format",
      /is not TOML/,
    );
  });

  it("answers for a repository root as readily as for the document in it", () => {
    // A consumer with a checkout and a consumer with a file both spell what they
    // have, so the path this takes is either and the answer is the same.
    const dir = mkdtempSync(join(tmpdir(), "release-declaration-"));
    try {
      writeFileSync(join(dir, "release-targets.toml"), readFileSync(DECLARATION, "utf8"));
      const result = check(dir);
      assert.equal(result.status, 0, `a repository root was refused:\n${result.stderr}`);
      assert.match(result.stdout, /declares 3 targets against schema_version 1/);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
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

  it("refuses more documents than it can answer about", () => {
    // One answer per invocation: a caller reads the line on stdout as the answer
    // about the document it named, so a second path would make that line ambiguous.
    assertRefused(
      check("release-targets.toml", "somewhere-else"),
      "two paths in one invocation",
      /takes at most one argument, got 2/,
    );
  });

  it("accepts an artifact recorded as no longer published", () => {
    // This repository has retired nothing, so the field is proven on a document
    // that has: a consumer still naming a retired artifact has to be told it is
    // gone rather than told nothing.
    const document = declared();
    document.retired = [
      {
        id: "pypi:oneagentgraph-legacy",
        why: "Never published; recorded here to prove the field.",
      },
    ];
    const result = checkDocument(document);
    assert.equal(result.status, 0, `a retired artifact was refused:\n${result.stderr}`);
    assert.match(result.stdout, /declares 3 targets against schema_version 1/);
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
