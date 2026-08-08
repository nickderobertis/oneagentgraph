//! `health`: per-identity binding, utilization, and reset.
//!
//! `docs/contract.md` says this is "read from oneharness data", and it is meant
//! literally: this crate owns no notion of an identity, a subscription, or a
//! quota window, so `health` shells out to `oneharness usage --format json` and
//! forwards what it answers. Every probe there is free — no harness takes a
//! model turn — which is what makes this a pre-flight check rather than a cost.
//!
//! What this module adds is the one thing a caller cannot get from that command
//! alone: a refusal that says *why* there is no answer, in this crate's own
//! terms, rather than a stack trace or an empty document.

use std::process::Command;

use serde_json::Value;

use crate::error::Error;

/// Read oneharness's own per-identity report.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when the binary is not there, cannot be run, refuses
/// the probe, or answers something that is not JSON — each named as itself, so
/// an operator knows whether to install oneharness or to look at their config.
pub fn read(oneharness_bin: &str) -> Result<Value, Error> {
    let output = Command::new(oneharness_bin)
        .args(["usage", "--format", "json"])
        .output()
        .map_err(|err| {
            Error::InvalidConfig(format!(
                "cannot run {oneharness_bin}: {err}. `health` reports what oneharness knows about \
                 each identity, so oneharness has to be on PATH."
            ))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::InvalidConfig(format!(
            "{oneharness_bin} usage refused the probe: {}",
            stderr.trim()
        )));
    }
    let report: Value = serde_json::from_slice(&output.stdout).map_err(|err| {
        Error::InvalidConfig(format!(
            "{oneharness_bin} usage answered something that is not JSON: {err}"
        ))
    })?;
    // A trust boundary: this is another process's stdout, and the one thing this
    // crate promises about `health` is that it forwards a *report*. A scalar or
    // a bare list is not one, and forwarding it would let a caller read "no
    // identities" off something that never described identities at all. What the
    // report *says* stays oneharness's to define — this crate owns no notion of
    // an identity, so validating further would be inventing a schema.
    if !report.is_object() && !report.is_array() {
        return Err(Error::InvalidConfig(format!(
            "{oneharness_bin} usage answered a bare value, not a report; a caller would read \
             that as an answer about its identities"
        )));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A binary that is not there is named as that, with what `health` needs it
    /// for — not as a bare spawn error.
    #[test]
    fn a_missing_oneharness_says_what_health_needs_it_for() {
        let err = read("oneharness-that-is-not-installed").unwrap_err();
        assert!(err.to_string().contains("has to be on PATH"), "{err}");
    }

    /// A binary that refuses forwards its own diagnostic, which is the part an
    /// operator can act on.
    #[test]
    fn a_refused_probe_forwards_oneharness_s_own_words() {
        let err = read("false").unwrap_err();
        assert!(err.to_string().contains("refused the probe"), "{err}");
    }

    /// A binary that answers something else is named as that rather than
    /// producing an empty report — whether it is not JSON at all, or JSON that
    /// does not describe identities.
    #[test]
    fn an_answer_that_is_not_a_report_is_refused_rather_than_forwarded() {
        let err = read("echo").unwrap_err();
        assert!(err.to_string().contains("not JSON"), "{err}");

        // `echo 7 usage --format json` is valid JSON and not a report.
        let err = read("printf").unwrap_err();
        assert!(
            err.to_string().contains("not JSON") || err.to_string().contains("bare value"),
            "{err}"
        );
    }
}
