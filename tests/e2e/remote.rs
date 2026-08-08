//! Config resolution by URL, against the real HTTPS origin in [`crate::origin`].
//!
//! The contract: a graph names its base, its personas, and each side's oneharness
//! config "by path or URL", and "remote refs (https) are fetched, checksummed, and
//! recorded content-addressed in the run record; replay/audit never depends on the
//! URL staying stable."
//!
//! So these drive the compiled binary against a served document over a real TLS
//! socket — a retrieval that succeeds and is recorded by digest, a certificate the
//! binary must refuse, and an answer past the size ceiling. The scheme rule and
//! the unreachable-host refusal are held next door in `verbs.rs`, which needs no
//! origin to prove them.

use crate::origin::{Origin, Trust};
use crate::support::{fake_harness, two_party_graph, Workspace};

/// The onejudge base one of these journeys serves instead of writing to disk.
const SERVED_BASE: &str = concat!(
    "provider:\n  kind: oneharness\n",
    "agent:\n  instructions: |\n    Standing bar: verify before you claim done.\n",
    "    Served marker: this base arrived over https.\n",
    "user:\n  done_when: \"the task is complete\"\n  max_turns: 4\n",
);

/// A member's `base_config` named by URL is fetched over real TLS, the run
/// completes on it, and the record keeps it content-addressed by digest.
///
/// The digest is the assertion that matters: the contract promises replay and
/// audit never depend on the URL staying stable, which is only true if what was
/// recorded is a hash of the bytes actually read. So it is checked against the
/// SHA-256 of exactly what the origin served, not merely for being hex.
#[test]
fn a_base_config_named_by_url_is_fetched_over_tls_and_recorded_by_digest() {
    let workspace = Workspace::new();
    let origin = Origin::start(
        vec![("/base.yaml".to_string(), SERVED_BASE.as_bytes().to_vec())],
        Trust::Trusted,
    );
    let url = origin.url("/base.yaml");
    workspace.graph(&two_party_graph(&fake_harness(), "").replace("./base.yaml", &url));

    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "complete-now: served over tls",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[("SSL_CERT_FILE", &origin.ca_file().display().to_string())],
    );
    run.expect_code(0);

    let record = workspace.record();
    let refs = record["refs"]
        .as_array()
        .expect("the recorded refs")
        .clone();
    let recorded = refs
        .iter()
        .find(|entry| entry["origin"] == serde_json::json!(url))
        .unwrap_or_else(|| panic!("the served ref was not recorded: {refs:#?}"));
    assert_eq!(
        recorded["sha256"],
        serde_json::json!(digest(SERVED_BASE.as_bytes())),
        "the record's digest is not of the bytes the origin served"
    );
    assert_eq!(
        recorded["bytes"],
        serde_json::json!(SERVED_BASE.len() as u64)
    );

    // And it was really this document that ran, not a local file of the same
    // name: its own marker reached the agent.
    assert!(
        !run.of_kind("member-settled").is_empty(),
        "{:?}",
        run.kinds()
    );
}

/// A certificate the binary cannot verify is refused, naming the URL — the run
/// never starts against a document nobody could authenticate.
///
/// The origin is real and reachable and answers a perfectly good graph; the only
/// thing wrong is who signed its certificate. That is the case a cleartext or
/// unreachable-host journey cannot reach, and the one that says verification is
/// actually on.
#[test]
fn a_certificate_from_an_untrusted_authority_refuses_the_ref() {
    let workspace = Workspace::new();
    let origin = Origin::start(
        vec![("/base.yaml".to_string(), SERVED_BASE.as_bytes().to_vec())],
        Trust::Untrusted,
    );
    let url = origin.url("/base.yaml");
    workspace.graph(&two_party_graph(&fake_harness(), "").replace("./base.yaml", &url));

    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "complete-now: never gets here",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[("SSL_CERT_FILE", &origin.ca_file().display().to_string())],
    );
    run.expect_code(2);
    assert!(run.stderr.contains(&url), "{}", run.stderr);
    assert!(run.stderr.contains("cannot fetch"), "{}", run.stderr);
    assert!(
        run.stdout.is_empty(),
        "a refusal must not read as an event stream: {}",
        run.stdout
    );
}

/// An answer past the size ceiling is refused for being one, rather than read
/// into memory and then judged.
///
/// The ceiling exists for a redirect onto something else — a release archive, an
/// endless stream — so what is asserted is the refusal naming the ceiling, on a
/// body served one byte past it.
#[test]
fn a_remote_answer_past_the_size_ceiling_is_refused_by_the_ceiling() {
    let workspace = Workspace::new();
    let oversized = vec![b'x'; oneagentgraph::resolve::MAX_REMOTE_BYTES + 1];
    let origin = Origin::start(vec![("/base.yaml".to_string(), oversized)], Trust::Trusted);
    let url = origin.url("/base.yaml");
    workspace.graph(&two_party_graph(&fake_harness(), "").replace("./base.yaml", &url));

    let run = workspace.run_with(
        &["validate", "./graph.yaml"],
        &[("SSL_CERT_FILE", &origin.ca_file().display().to_string())],
    );
    run.expect_code(2);
    assert!(
        run.stderr.contains("ceiling"),
        "the refusal did not name the ceiling: {}",
        run.stderr
    );
    assert!(run.stderr.contains(&url), "{}", run.stderr);
}

/// A document that is served but is not text is refused as unreadable, rather
/// than reaching the schema as replacement characters.
#[test]
fn a_remote_answer_that_is_not_text_is_refused() {
    let workspace = Workspace::new();
    let origin = Origin::start(
        vec![("/base.yaml".to_string(), vec![0xff, 0xfe, 0x00, 0x9f])],
        Trust::Trusted,
    );
    let url = origin.url("/base.yaml");
    workspace.graph(&two_party_graph(&fake_harness(), "").replace("./base.yaml", &url));

    let run = workspace.run_with(
        &["validate", "./graph.yaml"],
        &[("SSL_CERT_FILE", &origin.ca_file().display().to_string())],
    );
    run.expect_code(2);
    assert!(run.stderr.contains(&url), "{}", run.stderr);
}

/// Lowercase hex SHA-256 of `bytes`, the way a [`ResolvedRef`] records it.
///
/// Computed here rather than taken from the crate, so the assertion is against
/// the digest an auditor would compute themselves and not against whatever the
/// code under test happened to produce.
///
/// [`ResolvedRef`]: oneagentgraph::resolve::ResolvedRef
fn digest(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
