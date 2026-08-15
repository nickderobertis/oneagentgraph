//! `smoke`: spend one real harness turn, and judge the record it left.
//!
//! The point is the *launch path*, not the model: a smoke proves that a chain on
//! this host can still reach an identity that runs a turn. So it drives the real
//! `oneharness run` in a throwaway directory and then reads the report back.
//!
//! The path has to be the *members'* path, which is why this spawn is not the one
//! `docs/oneharness-library.md` inventories: it has no boundary of its own to
//! convert, it just follows theirs. A smoke run on the linked engine would prove
//! something no member depends on, and would pass on a host whose `oneharness` is
//! missing — so this collapses when a member's turn does, and not before.
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
//!
//! **Two signals, not one.** The report is judged, *and* oneharness's exit status
//! is required to be success — because the report alone cannot express the case
//! above. A candidate billed for work it did not finish is recorded as the
//! identity that `ran`, with an empty `fell_through`, so its report is
//! indistinguishable from a healthy launch; only the non-zero exit says
//! otherwise. Reading the report and not the status reported a spent, failed turn
//! as a pass, which is the same class of error in the opposite direction.
//!
//! **The launch is retried; the turn is not.** A smoke proves a launch *path*,
//! and the launch is the half a merely busy host can break while the path is
//! perfectly fine — a saturated machine refuses to start the provider, and
//! reporting that as an outage sends an operator hunting a subscription that was
//! never down. So a failed launch is tried up to [`LAUNCH_ATTEMPTS`] times, with
//! a widening pause so the host can drain rather than being asked the same
//! question three times at once.
//!
//! What bounds that retry is the one thing a smoke must never do: pay twice.
//! An attempt is relaunched only when its own report *proves* it spent nothing —
//! every candidate recording no tokens, no cost, and no failure classification.
//! Measured against oneharness 0.6.6, a provider that exits without publishing
//! and one that was rate-limited after billed work are both `status: "nonzero"`
//! with the same exit code, and the accounting is the only thing that separates
//! them. Anything short of that proof — a report that will not parse, a turn that
//! was spent and failed, a chain that reached nothing — settles the smoke where
//! it stands.

// llmlint: ignore-file[invalid_states_unrepresentable] the harness *identity* on a
// `FellThrough` and a `Verdict` stays a `String` on purpose: `docs/contract.md` is
// explicit that this crate owns no harness logic, and what counts as an identity —
// `codex`, `claude-code:alternate2` — is oneharness's to define and to change.
// Minting a validated identity type here would be this crate claiming that domain,
// and would refuse an identity oneharness accepts. What this module *does* decide,
// the classification, is the closed `Reason` set above.

use std::path::Path;

use serde_json::Value;

use crate::error::Error;

/// The prompt one smoke turn spends. Short on purpose: what is being proven is
/// that a turn happened at all.
pub const PROMPT: &str = "Reply with the single word: ok";

/// How many times the paid turn may be launched before the smoke gives up.
///
/// Only a launch that provably spent nothing is ever relaunched, so this is a
/// bound on wasted starts rather than on money.
pub const LAUNCH_ATTEMPTS: u32 = 3;

/// The pause before a retry, multiplied by the attempt just spent.
///
/// A saturated host needs time to drain more than it needs to be asked again
/// immediately; retrying without a pause is the same load that caused the
/// refusal, arriving sooner.
const RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_secs(2);

/// Why one candidate stepped aside, as oneharness classified it.
///
/// A closed set with an explicit `Other`, because the *whole* judgment turns on
/// which side of the line a classification falls: `quota` and `auth` mean the
/// candidate never ran the task, while everything else — `rate_limit` above all
/// — describes a record that carries work the provider already billed for.
/// Comparing strings at each site is how one of those gets excused as the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// The identity was out of quota, having spent nothing.
    Quota,
    /// The identity was not authenticated, so nothing ran.
    Auth,
    /// The candidate was never started at all.
    Skipped,
    /// Anything else oneharness said, kept verbatim so a refusal can name it.
    Other(String),
}

impl Reason {
    /// oneharness's own word for this classification.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Reason::Quota => "quota",
            Reason::Auth => "auth",
            Reason::Skipped => "skipped",
            Reason::Other(word) => word,
        }
    }

    /// Whether this classification means the candidate never ran the task, so a
    /// chain handing the turn on is the chain doing its job.
    #[must_use]
    pub fn is_fallthrough(&self) -> bool {
        matches!(self, Reason::Quota | Reason::Auth | Reason::Skipped)
    }
}

impl From<&str> for Reason {
    fn from(word: &str) -> Self {
        match word {
            "quota" => Reason::Quota,
            "auth" => Reason::Auth,
            "skipped" => Reason::Skipped,
            other => Reason::Other(other.to_string()),
        }
    }
}

/// One candidate the chain stepped past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FellThrough {
    /// The identity that stepped aside.
    pub identity: String,
    /// Why, as oneharness classified it.
    pub reason: Reason,
}

/// What one smoke turn proved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// The identity that actually ran the turn.
    pub ran: String,
    /// The candidates the chain stepped past, and why.
    pub fell_through: Vec<FellThrough>,
    /// How many launches it took to record one turn.
    ///
    /// Reported rather than smoothed over: a host that needed two starts is
    /// healthy *now* and worth knowing about, and the operator is the only one
    /// who can act on it.
    pub attempts: u32,
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
    launch(oneharness_bin, dir, RETRY_BACKOFF)
}

/// [`run`], with the pause between attempts named so a test does not wait it out.
fn launch(
    oneharness_bin: &str,
    dir: &Path,
    backoff: std::time::Duration,
) -> Result<Verdict, Error> {
    // Every attempt but the last: a launch that provably spent nothing gets
    // another start, and anything else settles the smoke where it stands.
    for attempt in 1..LAUNCH_ATTEMPTS {
        match once(oneharness_bin, dir) {
            Ok(verdict) => {
                return Ok(Verdict {
                    attempts: attempt,
                    ..verdict
                })
            }
            Err(refusal) if refusal.spent_nothing => std::thread::sleep(backoff * attempt),
            Err(refusal) => return Err(refusal.error),
        }
    }
    // The last attempt answers for the smoke whichever way it goes, and a
    // launch path that failed every time is named as one rather than as a turn.
    match once(oneharness_bin, dir) {
        Ok(verdict) => Ok(Verdict {
            attempts: LAUNCH_ATTEMPTS,
            ..verdict
        }),
        Err(refusal) if refusal.spent_nothing => Err(after_attempts(refusal.error)),
        Err(refusal) => Err(refusal.error),
    }
}

/// One launch that did not produce a verdict.
struct Refusal {
    /// What the operator is told, carrying the exit code this failure exits on.
    error: Error,
    /// Whether this attempt's own report proves it spent nothing, so relaunching
    /// bills nothing. Absence of proof is not proof: this is `false` unless the
    /// accounting says so.
    spent_nothing: bool,
}

/// The same refusal, saying how many launches reached it.
///
/// The exit code is the one the refusal already carried: how many times a smoke
/// tried does not change what kind of failure it hit.
fn after_attempts(error: Error) -> Error {
    let note = format!(
        " (after {LAUNCH_ATTEMPTS} attempts, none of which spent anything): rerun \
         `oneagentgraph smoke` on an idle host to tell a saturated one from a launch path that \
         does not work"
    );
    match error {
        Error::InvalidConfig(reason) => Error::InvalidConfig(reason + &note),
        Error::MemberFailed { member, reason } => Error::MemberFailed {
            member,
            reason: reason + &note,
        },
    }
}

/// Spend one turn in `dir`, once.
fn once(oneharness_bin: &str, dir: &Path) -> Result<Verdict, Refusal> {
    let output = crate::harness_process::command(oneharness_bin)
        .args(["run", "--cwd"])
        .arg(dir)
        .args(["--compact", "--prompt", PROMPT])
        .current_dir(dir)
        .output()
        .map_err(|err| Refusal {
            error: Error::InvalidConfig(format!("cannot run {oneharness_bin}: {err}")),
            // A process that never started billed nothing, so a host that could
            // not fork one deserves another go — but a binary that is not there
            // will not appear, and retrying it only delays saying so.
            spent_nothing: err.kind() != std::io::ErrorKind::NotFound,
        })?;
    let report: Value = serde_json::from_slice(&output.stdout).map_err(|err| Refusal {
        error: Error::InvalidConfig(format!(
            "{oneharness_bin} answered no report ({err}): {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        // A report that will not parse says nothing about what was spent, and
        // relaunching on a guess is how a smoke pays twice.
        spent_nothing: false,
    })?;
    // oneharness's own exit status, before the report is judged at all. The
    // report is not enough on its own: a candidate that was *billed* and failed
    // — a rate limit after work, the case this module refuses to excuse — is
    // recorded as the identity that `ran`, with an empty `fell_through`, so the
    // report reads exactly like a healthy launch. Measured against oneharness
    // 0.6.7: that run exits 1 while a plain success and a fall-through to a
    // working identity both exit 0, so the status is the one signal that
    // separates them, and ignoring it reported a spent, failed turn as a pass.
    if !output.status.success() {
        let ran = report
            .get("fallback")
            .and_then(|fallback| fallback.get("ran"))
            .and_then(Value::as_str)
            .unwrap_or("no candidate");
        let record = failed_record(&report, ran);
        return Err(Refusal {
            error: Error::MemberFailed {
                member: MEMBER.into(),
                reason: format!(
                    "{oneharness_bin} exited {} having run {ran}: the turn was attempted and did \
                     not succeed, so this launch path is not proven{record} — {}",
                    output
                        .status
                        .code()
                        .map_or_else(|| "on a signal".to_string(), |code| code.to_string()),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            },
            // The one place the two shapes are told apart. Both exit non-zero
            // with the same code; only the accounting says whether this was a
            // provider that never got going or one that was charged for work it
            // did not finish.
            spent_nothing: spent_nothing(&report),
        });
    }
    judge_report(&report).map_err(|reason| Refusal {
        error: failed(reason),
        // The chain answered, so whatever it says is the launch path's outcome:
        // an exhausted subscription is not a busy host, and asking it twice more
        // exhausts it twice more.
        spent_nothing: false,
    })
}

/// The selected provider's actionable fields, retained from oneharness's report.
fn failed_record(report: &Value, ran: &str) -> String {
    let Some(result) = report
        .get("results")
        .and_then(Value::as_array)
        .and_then(|results| {
            results
                .iter()
                .find(|result| result.get("harness").and_then(Value::as_str) == Some(ran))
        })
    else {
        return String::new();
    };
    let mut fields = Vec::new();
    for (label, key) in [
        ("status", "status"),
        ("failure", "failure_kind"),
        ("error", "error"),
    ] {
        if let Some(value) = result.get(key).and_then(Value::as_str) {
            fields.push(format!("{label} {value}"));
        }
    }
    if let Some(code) = result.get("exit_code").and_then(Value::as_i64) {
        fields.push(format!("exit {code}"));
    }
    if fields.is_empty() {
        String::new()
    } else {
        format!(" (provider record: {})", fields.join(", "))
    }
}

/// Whether this report proves the attempt spent nothing.
///
/// The bar is proof, not the absence of contrary evidence: a report with no
/// results at all, or a candidate oneharness gave any failure classification,
/// leaves open that a provider was charged — and a smoke that guesses wrong here
/// pays for the same question twice.
fn spent_nothing(report: &Value) -> bool {
    let Some(results) = report.get("results").and_then(Value::as_array) else {
        return false;
    };
    !results.is_empty()
        && results.iter().all(|result| {
            // Any classification at all stops a relaunch: oneharness naming a
            // failure kind means it knows something this module does not.
            let unclassified = result.get("failure_kind").is_none_or(Value::is_null);
            // The accounting is read into oneharness's **own** `Usage`, and
            // judged by oneharness's **own** predicate for whether a provider
            // billed real work — the same one its quota classifier and its
            // fallback chain share. Restating that rule here is how this module
            // would come to disagree with the chain it is judging, on the one
            // decision that can pay for the same question twice.
            //
            // Absent accounting is deliberately *not* work, which is oneharness's
            // own reading of it too: a plain-text harness reports none at all, and
            // a candidate that published nothing is exactly the launch worth
            // trying again. What stops a relaunch is a classification, above.
            //
            // Accounting that is *present but unreadable* is the opposite case,
            // and `is_ok_and` is what separates them: a `usage` object this build
            // cannot parse proves nothing at all, least of all that the turn was
            // free. Treating it as free is how a relaunch pays for the same
            // question twice — the one thing this module must never do — and an
            // upstream `Usage` that gained a required field would have been
            // exactly that, silently.
            let free = result
                .get("usage")
                .filter(|usage| !usage.is_null())
                .is_none_or(|usage| {
                    serde_json::from_value::<oneharness_core::domain::signals::Usage>(usage.clone())
                        .is_ok_and(|usage| !usage.reports_billed_work())
                });
            unclassified && free
        })
}

/// Judge one oneharness report, without spending anything.
///
/// # Errors
///
/// [`Error::MemberFailed`] when nothing ran the turn, or when a candidate stepped
/// past on a classification a chain does not step past.
pub fn judge(report: &Value) -> Result<Verdict, Error> {
    judge_report(report).map_err(failed)
}

/// The name every smoke failure is reported under: a smoke is a one-member run.
const MEMBER: &str = "smoke";

/// One smoke refusal, as the error it exits on.
fn failed(reason: String) -> Error {
    Error::MemberFailed {
        member: MEMBER.into(),
        reason,
    }
}

/// [`judge`], answering with the reason alone so a caller can decide what kind
/// of failure it is.
fn judge_report(report: &Value) -> Result<Verdict, String> {
    let fell_through: Vec<FellThrough> = report
        .get("fallback")
        .and_then(|fallback| fallback.get("fell_through"))
        .and_then(Value::as_array)
        .map(|candidates| {
            candidates
                .iter()
                .map(|candidate| FellThrough {
                    identity: text(candidate.get("harness")),
                    reason: Reason::from(text(candidate.get("reason")).as_str()),
                })
                .collect()
        })
        .unwrap_or_default();

    for candidate in &fell_through {
        if !candidate.reason.is_fallthrough() {
            let (identity, reason) = (&candidate.identity, candidate.reason.as_str());
            return Err(format!(
                "{identity} stepped aside as {reason:?}, which is not a classification a chain \
                 steps past — that record carries work the provider already billed for"
            ));
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
            .map(|candidate| format!("{} [{}]", candidate.identity, candidate.reason.as_str()))
            .collect();
        return Err(if named.is_empty() {
            "no candidate ran the turn, and the chain recorded none".into()
        } else {
            format!("no candidate ran the turn: {}", named.join(", "))
        });
    };
    // One launch until a caller that made several says otherwise.
    Ok(Verdict {
        ran,
        fell_through,
        attempts: 1,
    })
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
            vec![FellThrough {
                identity: "claude-code".into(),
                reason: Reason::Quota,
            }]
        );
        assert_eq!(verdict.attempts, 1, "one launch recorded one turn");
        // The line the whole judgment turns on: `skipped` is a candidate that
        // was never started, and `rate_limit` is one that ran and was billed.
        assert!(Reason::Skipped.is_fallthrough());
        assert!(!Reason::Other("rate_limit".into()).is_fallthrough());
        assert_eq!(Reason::from("auth"), Reason::Auth);
        assert_eq!(Reason::Other("odd".into()).as_str(), "odd");
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

    /// The line the retry turns on, against the two report shapes a real
    /// oneharness 0.6.6 produces for them. Both exit non-zero with the same
    /// code; only the accounting separates a provider that never got going from
    /// one that was charged for work it did not finish.
    #[test]
    fn only_a_launch_that_spent_nothing_is_worth_relaunching() {
        let crashed = json!({"results": [{
            "harness": "claude-code", "status": "nonzero", "exit_code": 1,
            "usage": {"input_tokens": Value::Null, "output_tokens": Value::Null,
                      "cache_read_tokens": Value::Null, "cache_write_tokens": Value::Null,
                      "cost_usd": Value::Null},
            "failure_kind": Value::Null,
            "stderr": "fake-harness: exiting 1 having published nothing\n"}]});
        assert!(spent_nothing(&crashed), "a provider that published nothing");

        let billed = json!({"results": [{
            "harness": "claude-code", "status": "nonzero", "exit_code": 1,
            "usage": {"input_tokens": 900, "output_tokens": 120, "cost_usd": 0.42},
            "failure_kind": "rate_limit"}]});
        assert!(!spent_nothing(&billed), "a turn the provider billed for");

        // Proof, not the absence of contrary evidence: a report that records no
        // results at all leaves open that something was charged.
        assert!(!spent_nothing(&json!({"results": []})));
        assert!(!spent_nothing(&json!({})));
        // A classification alone is enough to stop a relaunch, with no usage at
        // all — oneharness naming a failure kind means it knows something this
        // module does not.
        assert!(!spent_nothing(
            &json!({"results": [{"failure_kind": "quota"}]})
        ));
        // And one spent candidate condemns the attempt even beside a clean one.
        assert!(!spent_nothing(&json!({"results": [
            {"usage": {"input_tokens": 0}},
            {"usage": {"input_tokens": 12}}]})));

        // Accounting that is present but unreadable proves nothing, so it is not
        // proof of a free turn. The distinction against the absent case below is
        // the whole of it: nothing published is a launch worth retrying, and
        // something published this build cannot parse is not.
        assert!(!spent_nothing(
            &json!({"results": [{"usage": {"input_tokens": "lots"}}]})
        ));
        assert!(!spent_nothing(
            &json!({"results": [{"usage": "the provider wrote prose"}]})
        ));
        assert!(spent_nothing(&json!({"results": [{"usage": Value::Null}]})));
        assert!(spent_nothing(&json!({"results": [{}]})));
    }

    /// Every launch failing having spent nothing is a launch path, not a turn:
    /// the refusal says how many starts reached it and keeps its own exit code.
    #[test]
    fn a_launch_path_that_never_starts_is_named_with_its_attempts() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A directory in place of a binary: the spawn fails with something other
        // than `NotFound`, which is the retryable shape a saturated host makes.
        let err = launch(
            &dir.path().display().to_string(),
            dir.path(),
            std::time::Duration::ZERO,
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains(&format!("after {LAUNCH_ATTEMPTS} attempts")),
            "{message}"
        );
        assert!(message.contains("rerun `oneagentgraph smoke`"), "{message}");
        // The exit code the refusal already carried: how many times a smoke
        // tried does not change what kind of failure it hit.
        assert!(matches!(err, Error::InvalidConfig(_)), "{err:?}");
    }

    /// A binary that is not there is named rather than reported as a failed turn.
    #[test]
    fn a_missing_oneharness_is_named() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = run("oneharness-that-is-not-installed", dir.path()).unwrap_err();
        assert!(err.to_string().contains("cannot run"), "{err}");
        // Said once, not three times over two seconds of backoff: a binary that
        // is not there will not appear, so retrying only delays the diagnosis.
        assert!(!err.to_string().contains("attempts"), "{err}");
        let err = run("echo", dir.path()).unwrap_err();
        assert!(err.to_string().contains("answered no report"), "{err}");
        // Unparseable output says nothing about what was spent, and relaunching
        // on a guess is how a smoke pays twice.
        assert!(!err.to_string().contains("attempts"), "{err}");
    }
}
