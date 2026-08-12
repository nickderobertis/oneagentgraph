//! Out-of-band turn control: where a member's in-flight turn is addressed, and
//! the `oneharness interrupt` process that redirects it.
//!
//! `cancel` ends a turn and the worker's whole accumulated context goes with it.
//! `interrupt` is its sibling — same addressing, different intent — and redirects
//! a turn that keeps running, which is the difference between correcting a worker
//! and restarting it.
//!
//! **This crate owns none of the mechanism.** onejudge asks its agent side for a
//! controllable turn (`provider.control`), oneharness opens the socket and knows
//! which harnesses have a lever at all, and a separate `oneharness interrupt`
//! process delivers the redirection. What lives here is the composition: the
//! address a run recorded for one member, and the mapping from oneharness's own
//! answer onto the verb's exit codes.
//!
//! # Why the address is written down rather than derived
//!
//! [`Address`] is the three values `oneharness interrupt` takes, and every one of
//! them is either this crate's own or read back from onejudge's report:
//!
//! * `session` is the handle [`crate::invoke::Context::session`] mints and writes
//!   into the member's onejudge config, which onejudge threads as
//!   `oneharness run --session`. oneharness sanitizes it the same way when it
//!   names the socket and when `interrupt` resolves one, so the handle this crate
//!   minted addresses the turn it started.
//! * `cwd` is the member's own worktree, which this crate chose.
//! * `session_dir` is oneharness's, and is left [`None`] until its report names
//!   one. `oneharness run --session-dir` has no config or environment layer, and
//!   onejudge never passes it, so a run and an `interrupt` that both say nothing
//!   resolve the same store — recomputing that platform default here would be a
//!   second source for one fact, and wrong for any run whose store *was* named.
//!
//! The record is written twice: [`Turn::Open`] when the member starts, so the
//! lever exists while the turn an operator wants to redirect is the one in flight,
//! and again when the member's report arrives — which is the first moment this
//! process learns whether the ask was honored at all, because onejudge reports
//! `control` only on the finished run.

use std::path::{Path, PathBuf};

use oneharness_core::domain::control::{ControlReason, ControlResponse};
use serde::{Deserialize, Serialize};

/// The file a run records one member's turn control in, inside that member's own
/// scratch — beside the `report.json` the same settle stores.
pub const CONTROL_FILE: &str = "control.json";

/// The shape this build writes into every [`Record`].
///
/// A run's state outlives the binary that wrote it, and `interrupt` reads it back
/// from a run that may have started under an earlier version. A record from a
/// *later* one is refused by number rather than parsed, because those were
/// written by a build that knew something this one does not.
pub const CONTROL_SCHEMA_VERSION: u32 = 1;

/// Where an `oneharness interrupt` process addresses a member's controllable
/// turn.
///
/// Exactly the three values that verb takes (`--session`, `--session-dir`,
/// `--cwd`) and nothing else: the socket path is not among them because
/// `interrupt` derives it from the store directory and the handle, and carrying
/// it here would be a second source for one fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Address {
    /// The caller-owned session handle the turn is addressed by.
    pub session: String,
    /// The session store holding the handle and its control socket; absent when
    /// the run left oneharness's own default, which an `interrupt` that says
    /// nothing resolves the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_dir: Option<String>,
    /// The working directory the turn runs in, which is how `interrupt` reports
    /// which project's session the handle belongs to.
    pub cwd: String,
}

/// What a run recorded about one member's turn control.
///
/// Two states rather than an `Option<Address>`, because "there is a turn to
/// address" and "control was asked for and refused" are different answers and the
/// second carries the reason an operator needs — a harness with no lever, a
/// platform with no unix sockets, a run shape oneharness will not control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Turn {
    /// The member's agent side asked for a controllable turn, addressed here.
    Open {
        /// Where to reach it.
        address: Address,
    },
    /// Control was asked for and could not be provided; this is what said so.
    Unavailable {
        /// oneharness's own reason, carried through untouched.
        reason: String,
    },
}

/// One member's turn control, as the run wrote it down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// The shape this record was written under — see [`CONTROL_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// What the run knew about the member's turn when it last wrote.
    pub turn: Turn,
}

/// Write what this run knows about `member`'s turn control into its scratch.
///
/// Best-effort, and deliberately: a member whose control record could not be
/// written is a member with no lever, which `interrupt` reports as the fact it is
/// — losing the file is not a reason to fail a run that is otherwise fine.
pub fn write(scratch: &Path, turn: &Turn) {
    let record = Record {
        schema_version: CONTROL_SCHEMA_VERSION,
        turn: turn.clone(),
    };
    if let Ok(rendered) = serde_json::to_string(&record) {
        let _ = std::fs::write(path(scratch), rendered);
    }
}

/// Read back what a run recorded for one member, or say why there is nothing to
/// act on.
///
/// # Errors
///
/// The reason the record cannot be acted on, in the words `interrupt` reports:
/// no member ever recorded one, or one written by a build this one cannot read.
pub fn read(scratch: &Path) -> Result<Turn, String> {
    let path = path(scratch);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Err(
            "this member opened no controllable turn: only a two-party member's agent side does, \
             and this run recorded none for it"
                .to_string(),
        );
    };
    let record: Record = serde_json::from_str(&raw).map_err(|err| {
        format!(
            "the turn-control record at {} is not one this build can read ({err})",
            path.display()
        )
    })?;
    if record.schema_version > CONTROL_SCHEMA_VERSION {
        return Err(format!(
            "the turn-control record at {} was written under schema version {} and this build \
             reads {CONTROL_SCHEMA_VERSION}",
            path.display(),
            record.schema_version
        ));
    }
    Ok(record.turn)
}

/// Where one member's record lives.
fn path(scratch: &Path) -> PathBuf {
    scratch.join(CONTROL_FILE)
}

/// What one `oneharness interrupt` answered.
///
/// A closed set because it is what the verb's exit code is decided from, and the
/// three outcomes are not interchangeable: a redirection that landed, a turn
/// there was nothing to redirect (a *fact*), and a delivery that was attempted
/// and failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// The run took ownership of the redirection.
    Delivered,
    /// There was no controllable turn to reach, and this is which of the reasons
    /// applied.
    NoTurn(String),
    /// The delivery was attempted and failed.
    Failed(String),
    /// The redirection itself was not one oneharness would take.
    Invalid(String),
}

/// Ask the run listening at `address` to stop what it is doing and do `input`
/// instead.
///
/// The whole delivery is `oneharness interrupt`, run as its own process — the
/// same hop `run` and `health` take, and for the same reason: the socket, the
/// protocol version, and the harness's own control mechanism are oneharness's,
/// and nothing about them is rebuilt here.
#[must_use]
pub fn deliver(bin: &str, address: &Address, input: Option<&str>) -> Delivery {
    let mut command = std::process::Command::new(bin);
    command.args(["interrupt", "--session", &address.session]);
    if let Some(dir) = &address.session_dir {
        command.args(["--session-dir", dir]);
    }
    command.args(["--cwd", &address.cwd, "--compact"]);
    if let Some(text) = input {
        command.args(["--input", text]);
    }
    let output = match command.output() {
        Ok(output) => output,
        Err(err) => {
            return Delivery::Failed(format!(
                "cannot run `{bin} interrupt`: {err}. Is oneharness installed and on PATH?"
            ))
        }
    };
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    // The answer frame first, whatever the exit code: it is the typed thing
    // oneharness produces, and reading the code alone would turn every refusal
    // into one undifferentiated failure — which is exactly the distinction this
    // verb's exit 3 exists to make.
    match serde_json::from_slice::<ControlResponse>(&output.stdout) {
        Ok(answer) => match answer.reason() {
            None => Delivery::Delivered,
            Some(reason) => Delivery::NoTurn(explain(reason)),
        },
        // No frame at all. oneharness answers a redirection it will not take
        // before it opens anything, on stderr and with its own usage code, and
        // that is an argument this caller got wrong rather than a lever that
        // broke.
        Err(_) if output.status.code() == Some(crate::error::EXIT_INVALID_CONFIG) => {
            Delivery::Invalid(stderr)
        }
        Err(err) => Delivery::Failed(if stderr.is_empty() {
            format!("`{bin} interrupt` answered nothing this build could read: {err}")
        } else {
            stderr
        }),
    }
}

/// One refusal, in the words the contract gives an operator.
///
/// Total over oneharness's own taxonomy, so a reason added upstream is a compile
/// error here rather than a refusal this verb reports without saying which.
fn explain(reason: ControlReason) -> String {
    match reason {
        ControlReason::NoActiveTurn => {
            "the member is between turns: its run is alive but nothing is in flight to redirect"
                .to_string()
        }
        ControlReason::NotRunning => {
            "nothing is listening on the member's control socket, so its turn has already ended"
                .to_string()
        }
        ControlReason::Unsupported => {
            "the member's run has no out-of-band turn control to serve the request".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address() -> Address {
        Address {
            session: "node-scope-1-worker".into(),
            session_dir: None,
            cwd: "/work/repo".into(),
        }
    }

    /// The record round-trips, and an absent one is the fact `interrupt` reports
    /// rather than a failure it invents.
    #[test]
    fn a_recorded_turn_reads_back_as_what_the_run_wrote() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(read(dir.path())
            .unwrap_err()
            .contains("opened no controllable turn"));

        write(
            dir.path(),
            &Turn::Open {
                address: address(),
            },
        );
        assert_eq!(
            read(dir.path()).expect("a record"),
            Turn::Open {
                address: address()
            }
        );

        // The second write is the report's answer, and it replaces the first:
        // a member whose ask was refused must not still read as addressable.
        write(
            dir.path(),
            &Turn::Unavailable {
                reason: "harness `qwen` has no out-of-band turn control".into(),
            },
        );
        let Turn::Unavailable { reason } = read(dir.path()).expect("a record") else {
            panic!("the refusal did not replace the address");
        };
        assert!(reason.contains("no out-of-band turn control"));
    }

    /// A record from a build that knew more is refused by version, and one that
    /// is not a record at all is refused by name — neither is guessed at.
    #[test]
    fn an_unreadable_record_is_refused_rather_than_guessed_at() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(path(dir.path()), "{\"schema_version\":99,\"turn\":{\"state\":\"open\",\"address\":{\"session\":\"s\",\"cwd\":\"/w\"}}}")
            .expect("write");
        assert!(read(dir.path()).unwrap_err().contains("schema version 99"));

        std::fs::write(path(dir.path()), "not a record").expect("write");
        assert!(read(dir.path())
            .unwrap_err()
            .contains("not one this build can read"));
    }

    /// Every refusal oneharness can answer with says which of the exit-3 causes
    /// applies, in words rather than in its own token.
    #[test]
    fn every_refusal_names_which_cause_applies() {
        assert!(explain(ControlReason::NoActiveTurn).contains("between turns"));
        assert!(explain(ControlReason::NotRunning).contains("already ended"));
        assert!(explain(ControlReason::Unsupported).contains("no out-of-band turn control"));
    }

    /// A delivery to a binary that is not there is a failure naming the binary,
    /// not a turn that was found to be absent — the two are different exit codes
    /// and an operator acts on them differently.
    #[test]
    fn a_missing_oneharness_is_a_failed_delivery_rather_than_an_absent_turn() {
        let delivery = deliver(
            "oneagentgraph-no-such-oneharness",
            &address(),
            Some("do this instead"),
        );
        let Delivery::Failed(reason) = delivery else {
            panic!("a binary that is not there was not reported as a failed delivery");
        };
        assert!(reason.contains("oneagentgraph-no-such-oneharness"), "{reason}");
    }
}
