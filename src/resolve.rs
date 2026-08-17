//! Turning a [`ConfigRef`] into bytes, and recording what was read.
//!
//! `docs/contract.md`: a graph names its onejudge base, its personas, and each
//! side's oneharness config *by path or URL*, and "remote refs (https) are
//! fetched, checksummed, and recorded content-addressed in the run record;
//! replay/audit never depends on the URL staying stable."
//!
//! So resolution has two products, not one. The bytes are what the run uses; the
//! [`ResolvedRef`] is what the run record keeps, and it names the digest of the
//! exact bytes rather than the URL they came from. Local paths are recorded the
//! same way — an audit that can distinguish "the same file" from "the same path"
//! for a remote ref and not for a local one has the weaker of the two answers
//! everywhere.

// llmlint: ignore-file[invalid_states_unrepresentable] `ResolvedRef.sha256` stays a
// `String` because this crate is its only producer — `record()` writes it from
// `Sha256::digest`, and the one reader is a run record this same crate wrote. A
// newtype would add a validated parse to a value that never arrives unvalidated,
// and the field it protects is provenance rather than a decision anything branches
// on. Revisit if a record ever arrives from somewhere this crate did not write.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::config::ConfigRef;
use crate::error::Error;

/// How much of a remote document this crate will read.
///
/// A config file is kilobytes. This bound is what stops a redirect onto
/// something else — a release archive, an endless stream — from being read into
/// memory before its shape is ever checked.
pub const MAX_REMOTE_BYTES: usize = 4 * 1024 * 1024;

/// The scheme a remote ref must use. `http` is deliberately absent: a config
/// fetched in the clear is one an intermediary chooses.
const REMOTE_SCHEME: &str = "https://";

/// One config ref as the run record keeps it.
///
/// `origin` is where it was read from and `sha256` is what was read. A replay
/// matches on the digest; the origin is provenance, not identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedRef {
    /// The path or URL the graph named, exactly as it named it.
    pub origin: String,
    /// Lowercase hex SHA-256 of the bytes that were read.
    pub sha256: String,
    /// How many bytes were read.
    pub bytes: u64,
}

/// A resolved ref's bytes, alongside the record of them.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// What the run record keeps.
    pub record: ResolvedRef,
    /// The document itself.
    pub content: String,
    /// Where a relative ref *inside* this document resolves against: the
    /// directory a local file sits in, or `None` for a remote one, whose
    /// relative refs have no filesystem to resolve against.
    pub base_dir: Option<PathBuf>,
}

/// Resolves refs and remembers every one it resolved.
///
/// One resolver per run, so the record it accumulates is the run's whole
/// content-addressed inventory, and a document read twice is fetched once.
#[derive(Debug, Default)]
pub struct Resolver {
    seen: BTreeMap<String, Resolved>,
}

impl Resolver {
    /// A resolver with nothing resolved yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve one ref, relative to `base_dir` when it names a relative path.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidConfig`] when the ref cannot be read: an unreadable file,
    /// a non-`https` URL, a fetch that failed or answered too much, or bytes
    /// that are not UTF-8.
    pub fn resolve(
        &mut self,
        reference: &ConfigRef,
        base_dir: Option<&Path>,
    ) -> Result<&Resolved, Error> {
        let key = cache_key(reference, base_dir);
        if !self.seen.contains_key(&key) {
            let resolved = read(reference, base_dir)?;
            self.seen.insert(key.clone(), resolved);
        }
        Ok(self.seen.get(&key).expect("just inserted"))
    }

    /// Every ref resolved so far, ordered, for the run record.
    #[must_use]
    pub fn inventory(&self) -> Vec<ResolvedRef> {
        let mut records: Vec<ResolvedRef> = self.seen.values().map(|r| r.record.clone()).collect();
        records.sort();
        records.dedup();
        records
    }
}

/// The key one ref is cached under. A relative path means different documents
/// under different base directories, so the base is part of the identity.
fn cache_key(reference: &ConfigRef, base_dir: Option<&Path>) -> String {
    if is_remote(reference) {
        return reference.0.clone();
    }
    let base = base_dir.map(Path::to_path_buf).unwrap_or_default();
    format!("{}\u{0}{}", base.display(), reference.0)
}

/// Whether this ref names a remote document rather than a file.
#[must_use]
pub fn is_remote(reference: &ConfigRef) -> bool {
    let value = reference.0.trim();
    value.starts_with(REMOTE_SCHEME) || value.starts_with("http://")
}

/// Read one ref, whichever kind it is.
fn read(reference: &ConfigRef, base_dir: Option<&Path>) -> Result<Resolved, Error> {
    if is_remote(reference) {
        return fetch(reference);
    }
    let path = local_path(reference, base_dir);
    let content = std::fs::read_to_string(&path).map_err(|err| {
        Error::InvalidConfig(format!(
            "cannot read {} ({}): {err}",
            path.display(),
            reference.0
        ))
    })?;
    Ok(Resolved {
        record: record(&reference.0, &content),
        base_dir: path.parent().map(Path::to_path_buf),
        content,
    })
}

/// Where a path-shaped ref points, once a relative one is anchored to its base.
///
/// Anchored through this crate's own `anchor` module rather than by a `join`,
/// which the module explains gets a rooted-but-driveless ref wrong. This is the
/// widest of its callers: every path-shaped ref in a graph resolves here.
#[must_use]
pub fn local_path(reference: &ConfigRef, base_dir: Option<&Path>) -> PathBuf {
    crate::anchor::anchored_path(base_dir, Path::new(&reference.0))
}

/// The HTTP client every remote ref is fetched through.
///
/// Built rather than taken from `ureq`'s free functions for one reason: the trust
/// anchors. `ureq`'s default is the Mozilla set compiled into `webpki-roots`,
/// which no operator can inspect or extend; this asks for the *host's* store
/// instead, so a graph served from an internal host under a private CA — or
/// through a TLS-inspecting proxy — resolves without this crate growing a trust
/// knob of its own, and `SSL_CERT_FILE` / `SSL_CERT_DIR` are how a bundle is
/// named. The cost is that trust now depends on the host: a machine whose store
/// is empty fails where the compiled-in set would have worked, and a CA installed
/// for interception is one this crate honours. `curl` and `git` make the same
/// trade, and it is the one an operator can see and change.
///
/// The scheme check in [`fetch`] is what this does *not* soften: the store says
/// which certificates are believed, never whether a certificate is required.
///
/// `SSL_CERT_FILE` / `SSL_CERT_DIR` are read here rather than left to the
/// verifier, because only one platform reads them on its own. The OpenSSL-shaped
/// stores behind Linux's verifier honour both; Darwin's Security framework
/// honours neither, so a bundle named there was silently ignored and the
/// documented way to trust an internal CA worked on one leg of the matrix and
/// not the other. Reading them once, here, is what makes that sentence true
/// everywhere.
fn agent() -> Result<ureq::Agent, Error> {
    let roots = match named_bundle()? {
        Some(certificates) => ureq::tls::RootCerts::Specific(std::sync::Arc::new(certificates)),
        None => ureq::tls::RootCerts::PlatformVerifier,
    };
    Ok(ureq::Agent::config_builder()
        .tls_config(ureq::tls::TlsConfig::builder().root_certs(roots).build())
        .build()
        .new_agent())
}

/// Whether a path was named as a bundle in its own right or found inside one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strictness {
    /// `SSL_CERT_FILE`: the operator pointed at this file and said "trust what
    /// is in here", so content it cannot read is a refusal.
    Declared,
    /// A member of `SSL_CERT_DIR`: a real certificate directory carries README
    /// files and hash symlinks beside its anchors, so a member that parses to
    /// no certificate is passed over rather than failing the run. An entry that
    /// cannot be *read at all* is still a refusal.
    Collected,
}

/// Every trust anchor one PEM file carries.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when the file cannot be read, or — for a
/// [`Strictness::Declared`] one — when its PEM cannot be parsed.
fn anchors(
    path: &Path,
    strictness: Strictness,
) -> Result<Vec<ureq::tls::Certificate<'static>>, Error> {
    let pem = std::fs::read(path)
        .map_err(|err| Error::InvalidConfig(format!("cannot read {}: {err}", path.display())))?;
    let mut found = Vec::new();
    for item in ureq::tls::parse_pem(&pem) {
        match item {
            Ok(ureq::tls::PemItem::Certificate(certificate)) => found.push(certificate),
            // A private key beside a certificate is ordinary, and not an anchor.
            Ok(_) => {}
            Err(err) if strictness == Strictness::Declared => {
                return Err(Error::InvalidConfig(format!(
                    "cannot read a certificate from {}: {err}",
                    path.display()
                )))
            }
            // Whatever this file is, it is not more certificates.
            Err(_) => break,
        }
    }
    Ok(found)
}

/// The trust anchors `SSL_CERT_FILE` / `SSL_CERT_DIR` name, when either does.
///
/// # Errors
///
/// A variable that is set but yields no certificate is a refusal, not a reason
/// to carry on with the platform's own store: an operator who named a bundle
/// asked for *that* trust set, and quietly believing a different one is how a
/// machine ends up trusting anchors nobody chose.
fn named_bundle() -> Result<Option<Vec<ureq::tls::Certificate<'static>>>, Error> {
    let file = std::env::var_os("SSL_CERT_FILE");
    let dir = std::env::var_os("SSL_CERT_DIR");
    if file.is_none() && dir.is_none() {
        return Ok(None);
    }

    let mut named: Vec<PathBuf> = Vec::new();
    let mut certificates = Vec::new();
    if let Some(file) = file {
        // A file named on its own is a *declaration*: every byte of it is meant
        // to be a trust anchor, so anything in it this cannot read is a mistake
        // worth stopping for.
        let path = PathBuf::from(file);
        certificates.extend(anchors(&path, Strictness::Declared)?);
        named.push(path);
    }
    if let Some(dir) = dir {
        let dir = PathBuf::from(dir);
        let listing = std::fs::read_dir(&dir).map_err(|err| {
            Error::InvalidConfig(format!("cannot read SSL_CERT_DIR {}: {err}", dir.display()))
        })?;
        let mut entries: Vec<PathBuf> = Vec::new();
        for entry in listing {
            // Not skipped: an entry the directory will not describe is a
            // filesystem this cannot enumerate, and carrying on would assemble
            // a bundle quietly missing an anchor the operator put there.
            let entry = entry.map_err(|err| {
                Error::InvalidConfig(format!(
                    "cannot read an entry of SSL_CERT_DIR {}: {err}",
                    dir.display()
                ))
            })?;
            let path = entry.path();
            if path.is_file() {
                entries.push(path);
            }
        }
        // Sorted, so a bundle assembled from a directory is the same bundle on
        // every host rather than whatever order the filesystem answers in.
        entries.sort();
        for path in &entries {
            certificates.extend(anchors(path, Strictness::Collected)?);
        }
        named.extend(entries);
    }
    if certificates.is_empty() {
        return Err(Error::InvalidConfig(format!(
            "SSL_CERT_FILE/SSL_CERT_DIR named {} but no certificate could be read from it, so \
             there is nothing to trust",
            named
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(Some(certificates))
}

/// Fetch one remote ref over `https`.
fn fetch(reference: &ConfigRef) -> Result<Resolved, Error> {
    let url = reference.0.trim();
    if !url.starts_with(REMOTE_SCHEME) {
        return Err(Error::InvalidConfig(format!(
            "remote ref {url} must use https: a config fetched in the clear is one an \
             intermediary chooses"
        )));
    }
    let mut response = agent()?
        .get(url)
        .call()
        .map_err(|err| Error::InvalidConfig(format!("cannot fetch {url}: {err}")))?;
    ingest(url, response.body_mut().as_reader())
}

/// Read one remote answer, bound it, and record it.
///
/// Split from the request so the bound and the record are exercised by a reader
/// this crate hands over, rather than only by whatever a network happened to
/// answer during a test run.
fn ingest(url: &str, body: impl std::io::Read) -> Result<Resolved, Error> {
    let mut content = String::new();
    // One byte past the ceiling, so a document that is exactly at it still reads
    // and anything larger is *seen* to be larger rather than silently cut.
    body.take(MAX_REMOTE_BYTES as u64 + 1)
        .read_to_string(&mut content)
        .map_err(|err| Error::InvalidConfig(format!("cannot read {url}: {err}")))?;
    if content.len() > MAX_REMOTE_BYTES {
        return Err(Error::InvalidConfig(format!(
            "{url} answered more than the {MAX_REMOTE_BYTES}-byte ceiling a config ref is \
             read under; it is not the document this graph names"
        )));
    }
    Ok(Resolved {
        record: record(url, &content),
        content,
        base_dir: None,
    })
}

/// The content-addressed record of one document.
fn record(origin: &str, content: &str) -> ResolvedRef {
    let digest = Sha256::digest(content.as_bytes());
    ResolvedRef {
        origin: origin.to_string(),
        sha256: digest.iter().map(|byte| format!("{byte:02x}")).collect(),
        bytes: content.len() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digest is of the bytes, so the same content under two names records
    /// one hash — which is what makes a replay independent of the URL.
    #[test]
    fn the_record_addresses_content_not_origin() {
        let one = record("./a.yaml", "version: 1\n");
        let two = record("https://example.com/b.yaml", "version: 1\n");
        assert_eq!(one.sha256, two.sha256);
        assert_ne!(one.origin, two.origin);
        assert_eq!(one.bytes, 11);
        // The digest a caller can reproduce with `sha256sum`, so the record is
        // checkable outside this crate.
        assert_eq!(
            one.sha256,
            "09bfcc6a14b83e2192b8673677725c84883ee9cd0c70e45c9ec09daa8f2b2847"
        );
    }

    /// A relative ref resolves against the document that named it, not the
    /// process's working directory — a graph fetched from elsewhere would
    /// otherwise read a neighbour's file.
    #[test]
    fn a_relative_ref_resolves_against_its_base() {
        let path = local_path(
            &ConfigRef("./oneharness.toml".into()),
            Some(Path::new("/graphs/a")),
        );
        assert_eq!(path, PathBuf::from("/graphs/a/./oneharness.toml"));
        let absolute = local_path(
            &ConfigRef("/etc/x.toml".into()),
            Some(Path::new("/graphs/a")),
        );
        assert_eq!(absolute, PathBuf::from("/etc/x.toml"));
    }

    /// A ref that names its own root is read from where its author wrote it,
    /// whatever base it was named under — and a relative one beside it still
    /// anchors to that base.
    ///
    /// The base is a real temporary directory rather than a typed-out `/graphs/a`,
    /// because only a base carrying a drive prefix is one a `join` could re-root
    /// `/graphs/api/oneharness.toml` under, and Windows is where bases have those.
    #[test]
    fn a_ref_that_names_its_own_root_is_not_re_rooted_under_its_base() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            local_path(
                &ConfigRef("/graphs/api/oneharness.toml".into()),
                Some(dir.path())
            ),
            PathBuf::from("/graphs/api/oneharness.toml")
        );
        assert_eq!(
            local_path(&ConfigRef("./oneharness.toml".into()), Some(dir.path())),
            dir.path().join("oneharness.toml")
        );
    }

    /// `http` is refused before a byte is read.
    #[test]
    fn plain_http_is_refused() {
        let err = fetch(&ConfigRef("http://example.com/graph.yaml".into())).unwrap_err();
        assert!(err.to_string().contains("must use https"), "{err}");
    }

    /// Remote-ness is decided by scheme, so a path that merely mentions one is
    /// still a path.
    #[test]
    fn remoteness_is_decided_by_scheme() {
        assert!(is_remote(&ConfigRef("https://example.com/a.yaml".into())));
        assert!(is_remote(&ConfigRef("http://example.com/a.yaml".into())));
        assert!(!is_remote(&ConfigRef("./https-notes/a.yaml".into())));
    }

    /// A remote answer is recorded content-addressed and carries no base
    /// directory: a relative ref inside a fetched document has no filesystem to
    /// resolve against.
    #[test]
    fn a_remote_answer_is_recorded_with_no_base_directory() {
        let resolved = ingest(
            "https://example.com/g.yaml",
            std::io::Cursor::new(b"version: 1\n"),
        )
        .unwrap();
        assert_eq!(resolved.content, "version: 1\n");
        assert_eq!(resolved.base_dir, None);
        assert_eq!(resolved.record.origin, "https://example.com/g.yaml");
        assert_eq!(resolved.record.bytes, 11);
    }

    /// A document exactly at the ceiling still reads; one byte past it is
    /// refused, because a redirect onto something else must not be read into
    /// memory and then treated as the config the graph named.
    #[test]
    fn an_answer_past_the_ceiling_is_refused_rather_than_cut() {
        let at_bound = vec![b'x'; MAX_REMOTE_BYTES];
        assert_eq!(
            ingest("https://example.com/a", std::io::Cursor::new(at_bound))
                .unwrap()
                .content
                .len(),
            MAX_REMOTE_BYTES
        );

        let past = vec![b'x'; MAX_REMOTE_BYTES + 1];
        let err = ingest("https://example.com/a", std::io::Cursor::new(past)).unwrap_err();
        assert!(err.to_string().contains("ceiling"), "{err}");
    }

    /// A body that is not UTF-8 is a document this crate cannot read, and says
    /// which URL answered it.
    #[test]
    fn a_body_that_is_not_utf8_names_the_url_that_answered_it() {
        let err = ingest(
            "https://example.com/a",
            std::io::Cursor::new(vec![0xff, 0xfe]),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("cannot read https://example.com/a"),
            "{err}"
        );
    }

    /// A host that cannot be reached is a refusal naming the URL, not a panic.
    #[test]
    fn an_unreachable_host_is_refused_by_url() {
        let err = fetch(&ConfigRef(
            "https://oneagentgraph.invalid/graph.yaml".into(),
        ))
        .unwrap_err();
        assert!(err.to_string().contains("cannot fetch"), "{err}");
    }

    /// A remote ref is cached by its URL and a relative one by its base, so two
    /// members naming the same document read it once — and two naming the same
    /// relative path under different bases do not collide.
    #[test]
    fn refs_are_cached_by_what_identifies_them() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.yaml"), "version: 1\n").expect("write");
        let mut resolver = Resolver::new();
        let reference = ConfigRef("a.yaml".into());
        assert_eq!(
            resolver
                .resolve(&reference, Some(dir.path()))
                .unwrap()
                .content,
            "version: 1\n"
        );
        // Second read comes from the cache; the inventory still has one entry.
        assert_eq!(
            resolver
                .resolve(&reference, Some(dir.path()))
                .unwrap()
                .content,
            "version: 1\n"
        );
        assert_eq!(resolver.inventory().len(), 1);
        assert_eq!(
            cache_key(&ConfigRef("https://x/a".into()), Some(dir.path())),
            "https://x/a"
        );

        let err = resolver
            .resolve(&ConfigRef("nowhere.yaml".into()), Some(dir.path()))
            .unwrap_err();
        assert!(err.to_string().contains("nowhere.yaml"), "{err}");
    }
}
