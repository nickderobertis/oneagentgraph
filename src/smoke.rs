//! `smoke`: spend one real harness turn, and judge the record it left.
//!
//! The point is the *launch path*, not the model: a smoke proves that a chain on
//! this host can still reach an identity that runs a turn. So it drives the real
//! `oneharness run` in a throwaway directory and then reads the report back.
//!
//! What it judges is ported from ai-orchestrator, and the subtlety is worth
//! keeping: a fallback chain records **every candidate it attempts**, and only
//! the last of them is the launch path's outcome. A candidate counts as fallen
//! through when its own record says it never ran the task — a `quota` or `auth`
//! classification, or a `skipped` status. `rate_limit` is deliberately not in
//! that set: oneharness stops a chain on one, because that record carries work
//! the provider already billed for. Holding every candidate to the bar instead
//! of the last one is what failed this smoke for weeks of healthy launches while
//! one subscription's quota was gone and the next served every turn.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::error::Error;

/// The prompt one smoke turn spends. Short on purpose: what is being proven is
/// that a turn happened at all.
pub const PROMPT: &str = "Reply with the single word: ok";

/// The classifications that mean a candidate never ran the task, so the chain
/// handing the turn on is the chain doing its job.
pub const FALLTHROUGH_REASONS: &[&str] = &["quota", "auth"];

/// What one smoke turn proved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// The identity that actually ran the turn.
    pub ran: String,
    /// The candidates the chain stepped past, and why.
    pub fell_through: Vec<(String, String)>,
}

/// Spend one turn in `dir` and judge what it left behind.
///
/// # Errors
///
/// [`Error::MemberFailed`] when no candidate ran the turn, naming each identity
/// and its reason so an operator knows which subscription to restore;
/// [`Error::InvalidConfig`] when oneharness could not be run or did not answer a
/// report.
pub fn run(oneharness_bin: &str, dir: &Path) -> Result<Verdict, Error> {
    let output = Command::new(oneharness_bin)
        .args(["run", "--cwd"])
        .arg(dir)
        .args(["--compact", "--prompt", PROMPT])
        .current_dir(dir)
        .output()
        .map_err(|err| Error::InvalidConfig(format!("cannot run {oneharness_bin}: {err}")))?;
    let report: Value = serde_json::from_slice(&output.stdout).map_err(|err| {
        Error::InvalidConfig(format!(
            "{oneharness_bin} answered no report ({err}): {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    })?;
    judge(&report)
}

/// Judge one oneharness report, without spending anything.
///
/// # Errors
///
/// [`Error::MemberFailed`] when nothing ran the turn, or when a candidate stepped
/// past on a classification a chain does not step past.
pub fn judge(report: &Value) -> Result<Verdict, Error> {
    let fell_through: Vec<(String, String)> = report
        .get("fallback")
        .and_then(|fallback| fallback.get("fell_through"))
        .and_then(Value::as_array)
        .map(|candidates| {
            candidates
                .iter()
                .map(|candidate| {
                    (
                        text(candidate.get("harness")),
                        text(candidate.get("reason")),
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    for (identity, reason) in &fell_through {
        if !FALLTHROUGH_REASONS.contains(&reason.as_str()) {
            return Err(Error::MemberFailed {
                member: "smoke".into(),
                reason: format!(
                    "{identity} stepped aside as {reason:?}, which is not a classification a chain \
                     steps past — that record carries work the provider already billed for"
                ),
            });
        }
    }

    let ran = report
        .get("fallback")
        .and_then(|fallback| fallback.get("ran"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let Some(ran) = ran.filter(|ran| !ran.is_empty()) else {
        let named: Vec<String> = fell_through
            .iter()
            .map(|(identity, reason)| format!("{identity} [{reason}]"))
            .collect();
        return Err(Error::MemberFailed {
            member: "smoke".into(),
            reason: if named.is_empty() {
                "no candidate ran the turn, and the chain recorded none".into()
            } else {
                format!("no candidate ran the turn: {}", named.join(", "))
            },
        });
    };
    Ok(Verdict { ran, fell_through })
}

/// One report field as a string, whatever it was.
fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => "unidentified".into(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The chain doing its job: a candidate that never ran the task steps aside,
    /// and the one that ran is the launch path's outcome.
    #[test]
    fn a_candidate_that_never_ran_the_task_is_the_chain_working() {
        let verdict = judge(&json!({
            "fallback": {"ran": "codex", "fell_through": [{"harness": "claude-code", "reason": "quota"}]}
        }))
        .expect("a pass");
        assert_eq!(verdict.ran, "codex");
        assert_eq!(
            verdict.fell_through,
            vec![("claude-code".into(), "quota".into())]
        );
    }

    /// `rate_limit` is not a fall-through: that record carries billed work, so a
    /// chain stops on one and a smoke must fail rather than excuse it.
    #[test]
    fn a_rate_limited_candidate_fails_the_smoke() {
        let err = judge(&json!({
            "fallback": {"ran": "codex",
                         "fell_through": [{"harness": "claude-code", "reason": "rate_limit"}]}
        }))
        .unwrap_err();
        assert!(err.to_string().contains("already billed for"), "{err}");
    }

    /// A chain whose every candidate refused names each identity and its reason,
    /// so an operator knows which subscription to restore.
    #[test]
    fn a_chain_that_reached_nothing_names_every_candidate() {
        let err = judge(&json!({
            "fallback": {"ran": Value::Null, "fell_through": [
                {"harness": "claude-code", "reason": "auth"},
                {"harness": "codex", "reason": "quota"}]}
        }))
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("claude-code [auth]"), "{message}");
        assert!(message.contains("codex [quota]"), "{message}");
    }

    /// A report with no chain at all is a refusal too — there is nothing that
    /// says a turn was spent.
    #[test]
    fn a_report_with_no_chain_is_a_refusal() {
        let err = judge(&json!({})).unwrap_err();
        assert!(err.to_string().contains("recorded none"), "{err}");
    }

    /// A candidate that names no identity of its own is reported as
    /// unidentified rather than excused as fallback.
    #[test]
    fn a_candidate_that_names_no_identity_is_still_named() {
        let err = judge(&json!({
            "fallback": {"ran": Value::Null, "fell_through": [{"reason": 7}]}
        }))
        .unwrap_err();
        assert!(err.to_string().contains("unidentified"), "{err}");
    }

    /// A binary that is not there is named rather than reported as a failed turn.
    #[test]
    fn a_missing_oneharness_is_named() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = run("oneharness-that-is-not-installed", dir.path()).unwrap_err();
        assert!(err.to_string().contains("cannot run"), "{err}");
        let err = run("echo", dir.path()).unwrap_err();
        assert!(err.to_string().contains("answered no report"), "{err}");
    }
}
