//! A real HTTPS origin, for the journeys that resolve a config ref by URL.
//!
//! The contract says a graph may name its base, its personas, and each side's
//! oneharness config "by path or URL", and that remote refs are "fetched,
//! checksummed, and recorded content-addressed". That is a trust boundary, so it
//! is driven the way every other one here is: the compiled binary, over a real
//! socket, through the production transport.
//!
//! Nothing about the transport is stood in for. `rcgen` mints a throwaway CA and
//! a leaf for `127.0.0.1`, `rustls` serves it, and the CA is named to the binary
//! through `SSL_CERT_FILE` — the same mechanism an operator uses for an internal
//! host or a TLS-inspecting proxy, and the reason `src/resolve.rs` asks for the
//! host's trust anchors rather than a compiled-in set. The binary's own
//! `https`-only rule, certificate verification, and hostname check all stay in
//! force: what the CA file changes is *which* certificates are believed, never
//! whether one is required.

use std::io::{BufRead as _, BufReader};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// A throwaway certificate authority and one leaf it signed for `127.0.0.1`.
struct Authority {
    /// The CA certificate, PEM encoded — what `SSL_CERT_FILE` names.
    ca_pem: String,
    /// The chain the server presents: the leaf, then its issuer.
    chain: Vec<CertificateDer<'static>>,
    /// The leaf's private key.
    key: PrivateKeyDer<'static>,
}

/// Mint a CA and a leaf for `127.0.0.1`.
fn authority() -> Authority {
    let ca_key = rcgen::KeyPair::generate().expect("a CA key");
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA params");
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key).expect("a self-signed CA");

    // The URL is an IP rather than a name, so the leaf carries an IP SAN and the
    // journey never depends on how this host resolves `localhost`.
    let leaf_key = rcgen::KeyPair::generate().expect("a leaf key");
    let mut leaf_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("leaf params");
    leaf_params
        .subject_alt_names
        .push(rcgen::SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let leaf_cert = leaf_params
        .signed_by(&leaf_key, &issuer)
        .expect("a leaf signed by the CA");

    Authority {
        ca_pem: ca_cert.pem(),
        chain: vec![leaf_cert.der().clone(), ca_cert.der().clone()],
        key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der())),
    }
}

/// Whose certificate the origin presents, relative to the CA the binary trusts.
pub enum Trust {
    /// The CA named to the binary is the one that signed what it is served.
    Trusted,
    /// A second, unrelated CA signed what it is served — so verification must
    /// fail, and the refusal must be about the certificate rather than the shape
    /// of whatever was behind it.
    Untrusted,
}

/// One running HTTPS origin, serving fixed bodies by path.
pub struct Origin {
    address: SocketAddr,
    ca_file: PathBuf,
    /// Held so the temp directory outlives the journey.
    _root: tempfile::TempDir,
}

impl Origin {
    /// Start serving `routes` (path, body), and trust it or not.
    ///
    /// The listener is bound before the thread starts, so the address is real by
    /// the time this returns and a journey never races the accept loop.
    pub fn start(routes: Vec<(String, Vec<u8>)>, trust: Trust) -> Self {
        let serving = authority();
        // What the binary is told to trust: this origin's own CA, or an unrelated
        // one whose key never touched the certificate being served.
        let trusted_pem = match trust {
            Trust::Trusted => serving.ca_pem.clone(),
            Trust::Untrusted => authority().ca_pem,
        };

        let root = tempfile::tempdir().expect("a directory for the CA");
        let ca_file = root.path().join("ca.pem");
        std::fs::write(&ca_file, &trusted_pem).expect("write the CA");

        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(serving.chain, serving.key)
            .expect("a server config");
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a loopback listener on any port");
        let address = listener.local_addr().expect("the bound address");

        let config = Arc::new(config);
        std::thread::spawn(move || {
            // One connection at a time is enough: a graph resolves its refs in
            // sequence. The loop ends when the listener is dropped at process
            // exit, which is what a test binary does.
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let Ok(connection) = rustls::ServerConnection::new(Arc::clone(&config)) else {
                    continue;
                };
                let mut tls = rustls::StreamOwned::new(connection, &mut stream);
                // A handshake the client abandons — which is exactly what an
                // untrusted certificate looks like from here — is not an error
                // this origin reports; the journey asserts on what the *binary*
                // said about it.
                let _ = answer(&mut tls, &routes);
            }
        });

        Self {
            address,
            ca_file,
            _root: root,
        }
    }

    /// The `https` URL one served path is reachable at.
    pub fn url(&self, path: &str) -> String {
        format!("https://{}{path}", self.address)
    }

    /// The CA file to name in `SSL_CERT_FILE`.
    pub fn ca_file(&self) -> &Path {
        &self.ca_file
    }
}

/// Read one request and answer it from `routes`.
fn answer(
    tls: &mut (impl std::io::Read + std::io::Write),
    routes: &[(String, Vec<u8>)],
) -> std::io::Result<()> {
    // `&mut S` is itself a reader, so the request is read through a borrow and the
    // answer is written to the same stream afterwards.
    let requested = {
        let mut reader = BufReader::new(&mut *tls);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        // Drain the headers, so the client's whole request is consumed before the
        // answer goes back.
        let mut header = String::new();
        while reader.read_line(&mut header)? > 2 {
            header.clear();
        }
        line.split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_string()
    };

    match routes.iter().find(|(path, _)| *path == requested) {
        Some((_, body)) => {
            write!(
                tls,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n",
                body.len()
            )?;
            tls.write_all(body)?;
        }
        None => write!(tls, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")?,
    }
    tls.flush()
}
