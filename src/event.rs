//! The shared event envelope.
//!
//! Every process in the stack emits NDJSON in one envelope shape, defined by
//! `docs/contract.md`. These types are deliberately duplicated per repository —
//! there is no shared util crate — so each producer owns its own copy and a
//! cross-repo contract test holds them together.
//!
//! Nothing here emits, orders, truncates, or redacts anything: this is the wire
//! shape and its documented bounds, not the machinery that honours them.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The envelope version this crate produces and understands.
pub const ENVELOPE_VERSION: u8 = 1;

/// The byte bound on a payload text field, past which it is truncated and the
/// payload carries `truncated: true`.
pub const MAX_PAYLOAD_TEXT_BYTES: usize = 4096;

/// The character bound on a [`TurnActivity::detail`] summary.
pub const MAX_ACTIVITY_DETAIL_CHARS: usize = 160;

/// One NDJSON event.
///
/// Merge order across streams is `(ts, stream, seq)`. A consumer detects loss
/// through per-stream [`seq`](Self::seq) gaps; there are no cross-stream
/// ordering promises beyond the timestamps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    /// Envelope version; [`ENVELOPE_VERSION`] for anything this crate writes.
    pub v: u8,
    /// RFC 3339 timestamp, millisecond precision, UTC.
    pub ts: String,
    /// Unique id of the producing process.
    pub stream: String,
    /// Monotonic per [`stream`](Self::stream).
    pub seq: u64,
    /// Which library in the stack produced the event.
    pub source: Source,
    /// What happened.
    pub kind: EventKind,
    /// Reserved keys plus free-form extras. Producers stamp what they know;
    /// enrichers never rewrite what is already there.
    #[serde(default)]
    pub labels: Labels,
    /// Kind-specific detail. Text fields are bounded by
    /// [`MAX_PAYLOAD_TEXT_BYTES`]; large evidence is an [`Artifact`] instead.
    #[serde(default)]
    pub payload: Map<String, Value>,
    /// Evidence stored by the producing library and referenced by id.
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
}

/// The library that produced an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// This crate.
    Agentgraph,
    /// `onevcs`.
    Vcs,
    /// `onepipeline`.
    Pipeline,
}

/// What an [`Envelope`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    /// The graph began running.
    GraphStarted,
    /// A member's first process started.
    MemberStarted,
    /// A turn began on one side of a member's conversation.
    TurnStarted,
    /// A bounded tool summary from inside a turn; see [`TurnActivity`].
    TurnActivity,
    /// A turn finished; see [`TurnCompleted`].
    TurnCompleted,
    /// A member is alive but has produced nothing since the last heartbeat.
    MemberHeartbeat,
    /// An identity chain moved past a candidate; see [`FallbackAdvanced`].
    FallbackAdvanced,
    /// A member's process died; see [`MemberDied`].
    MemberDied,
    /// A scheduled member fired.
    CronFired,
    /// A resettable schedule's clock restarted.
    CronReset,
    /// A member settled: the full onejudge report is an artifact and the verdict
    /// is inline in the payload.
    MemberSettled,
    /// Every member settled.
    GraphSettled,
}

/// The reserved label keys, plus whatever else a producer stamped.
///
/// Reserved keys are absent rather than empty when unknown, so an enricher can
/// tell "not stamped" from "stamped empty".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Labels {
    /// The run this event belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The round within the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u64>,
    /// The graph node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The step within the node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// The graph member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// The persona the member runs under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// Free-form extras, carried through untouched.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Evidence too large for a payload: stored by the producing library, referenced
/// by id, and fetched through that library's CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    /// Identifier, unique within the producing library's store.
    pub id: String,
    /// What the artifact is — a gate log, a check log, a transcript, a report.
    pub kind: String,
    /// Size of the stored artifact.
    pub bytes: u64,
}

/// The payload of an [`EventKind::TurnActivity`] event: a bounded tool summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnActivity {
    /// The kind of tool call.
    pub kind: String,
    /// The tool's name.
    pub name: String,
    /// A summary bounded by [`MAX_ACTIVITY_DETAIL_CHARS`].
    pub detail: String,
    /// Whether a text field above was cut to its documented bound.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

/// The payload of an [`EventKind::TurnCompleted`] event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnCompleted {
    /// What the turn consumed.
    pub usage: Usage,
}

/// What one turn consumed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    /// Input tokens.
    pub tokens_in: u64,
    /// Output tokens.
    pub tokens_out: u64,
    /// Tokens read from the prompt cache.
    pub cache_read: u64,
    /// Tokens written to the prompt cache.
    pub cache_write: u64,
    /// What the turn cost.
    pub cost: f64,
    /// How long the turn took.
    pub duration: f64,
}

/// The payload of an [`EventKind::FallbackAdvanced`] event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FallbackAdvanced {
    /// The identity the chain moved past.
    pub identity: String,
    /// oneharness's classification of why it could not run.
    pub reason: String,
}

/// The payload of an [`EventKind::MemberDied`] event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberDied {
    /// The liveness rule that fired.
    pub rule: String,
    /// The process's exit code, when it exited rather than being signalled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// How the process ended.
    pub disposition: Disposition,
    /// The tail of the process's standard error, bounded by
    /// [`MAX_PAYLOAD_TEXT_BYTES`].
    pub stderr_tail: String,
    /// Whether a text field above was cut to its documented bound.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

/// How a member's process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Disposition {
    /// The process exited with a code.
    Exited,
    /// The process was terminated by a signal.
    Signaled,
}

/// `skip_serializing_if` helper: an unset truncation flag is omitted rather than
/// written as `false`, so a payload that was never near its bound stays quiet.
fn is_false(value: &bool) -> bool {
    !*value
}
