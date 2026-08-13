//! `health`: per-identity binding, utilization, and reset.
//!
//! `docs/contract.md` says this is "read from oneharness data", and it is meant
//! literally: this crate owns no notion of an identity, a subscription, or a
//! quota window, so `health` asks oneharness for its own sweep and forwards what
//! it answers. Every probe there is free — no harness takes a model turn — which
//! is what makes this a pre-flight check rather than a cost.
//!
//! # Why this is a library call rather than a child process
//!
//! It used to be `oneharness usage --format json`, spawned, with its stdout
//! parsed back. The hop was kept for one stated reason: *which* identities a host
//! has was assembled by the `oneharness` CLI rather than by the library, and
//! rebuilding that assembly here would be this crate growing harness logic, which
//! `AGENTS.md` forbids. [`oneharness_core::io::usage::report`] closed exactly
//! that gap — it **is** the `usage` verb as a call, selection and variant
//! identities and bounded concurrency included, and the CLI is now a shell that
//! prints what it returns. So the sweep is reached without a process, from the
//! same code the CLI runs, and nothing about identities is decided here.
//!
//! What this module adds is the one thing a caller cannot get from that call
//! alone: a refusal that says *why* there is no answer, in this crate's own
//! terms.

use oneharness_core::domain::usage::UsageReport;
use oneharness_core::errors::OneharnessError;
use oneharness_core::io::usage::{report, UsageRequest};

use crate::error::Error;

/// Read oneharness's own per-identity report.
///
/// The report is oneharness's type rather than this crate's rendering of it:
/// what it *says* stays oneharness's to define, and a consumer reading it in
/// process gets the same document `oneharness usage --format json` prints.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when oneharness refuses the sweep before it probes
/// anything. Nothing a harness does can fail this call — a missing binary, an
/// unauthenticated identity, and a probe that timed out are all *identities in
/// the report* — so the only refusal left is configuration oneharness cannot
/// load, which is a fault an operator fixes in their own file.
pub fn read() -> Result<UsageReport, Error> {
    report(&request()).map_err(|err| refusal(&err))
}

/// What `health` sweeps for.
///
/// Every identity the host has, which is what a pre-flight check is for.
/// Everything else is oneharness's own default: its config discovery from this
/// process's working directory — the same one the spawned CLI inherited — and its
/// own per-probe timeout.
fn request() -> UsageRequest {
    UsageRequest {
        all: true,
        ..UsageRequest::default()
    }
}

/// One refusal, in the words `health` reports.
///
/// oneharness's own diagnostic is carried through untouched, because it names the
/// file and the fault; what this adds is which verb was refused and that a
/// refusal here is always about configuration rather than about a harness.
fn refusal(err: &OneharnessError) -> Error {
    Error::InvalidConfig(format!(
        "oneharness cannot report on this host's identities: {err}. `health` is a free pre-flight \
         read of oneharness's own data, so the only thing that refuses it is configuration it \
         cannot load."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Configuration oneharness will not load is reported as that, carrying its
    /// own diagnostic — not as an empty report a caller would read as "no
    /// identities".
    ///
    /// The error is a real one from the same call [`read`] makes, against a file
    /// oneharness really refuses; only the file is named, because the config a
    /// sweep discovers is discovered from the *process's* working directory and a
    /// test that moved that would move it for every test running beside it. The
    /// journey driving the whole verb against a broken `oneharness.toml` in its
    /// own workspace is in `tests/e2e/verbs.rs`.
    #[test]
    fn configuration_oneharness_will_not_load_is_reported_as_that() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("oneharness.toml");
        std::fs::write(&config, "harnesses = [\n").expect("write");

        let err = report(&UsageRequest {
            config: Some(config.clone()),
            ..request()
        })
        .map(|_| ())
        .expect_err("oneharness refuses a config it cannot parse");

        let reported = refusal(&err).to_string();
        assert!(reported.contains("this host's identities"), "{reported}");
        assert!(
            reported.contains(&config.display().to_string()),
            "oneharness's own diagnostic did not survive: {reported}"
        );
    }
}
