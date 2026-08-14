//! The shared event envelope.
//!
//! Every process in the stack emits NDJSON in one envelope shape, defined by
//! `docs/contract.md`. These types are deliberately duplicated per repository —
//! there is no shared util crate — so each producer owns its own copy and a
//! cross-repo contract test holds them together.
//!
//! Nothing here emits, orders, truncates, or redacts anything: this is the wire
//! shape and its documented bounds, not the machinery that honours them.

// llmlint: ignore-file[invalid_states_unrepresentable] two of the shapes below are fixed
// by `docs/contract.md` rather than chosen: `v` is the wire integer a consumer reads
// before it knows whether it can decode the rest, and `member-died` is specified as
// sibling fields (`rule`, `cause`, `detail`, and the three a child process adds), so
// folding the exit code into an `exited` variant would change what this stack emits.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::clock::now_rfc3339;

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
    /// An operator asked a member's in-flight turn to do something else; see
    /// [`TurnInterrupted`].
    TurnInterrupted,
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

/// The payload of an [`EventKind::MemberStarted`] event.
///
/// A member says what it is about to run before it runs it, and the two runners
/// describe different things — so the runner is a typed alternation carrying its
/// own facts rather than a string beside a bag of optional ones, and no consumer
/// can meet a `library` member with an argv or a `process` one with a worktree.
///
/// [`start_after`](Self::start_after) is the one field orthogonal to both: it is
/// present when the member came up *without* taking a turn, and says how many
/// seconds until the first one. Only a schedule defers a turn, so today only a
/// single-sided member carries it — but that is the graph schema's rule rather
/// than this payload's, and a consumer reads the field the same way whichever
/// runner it arrives on.
// `deny_unknown_fields` sits on [`Runner`] rather than here, because serde cannot
// carry it across a `flatten`: the flattened field is what the remaining keys are
// handed to, so it is the one that refuses a key nobody declared — including a
// field belonging to the *other* runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberStarted {
    /// What runs this member, and what that runner is.
    #[serde(flatten)]
    pub runner: Runner,
    /// Seconds until this member's first turn, on the event a member publishes
    /// when it comes up without taking one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_after: Option<u64>,
}

/// What runs one member, and the facts that runner has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "runner", rename_all = "lowercase", deny_unknown_fields)]
pub enum Runner {
    /// Driven in this process through a library.
    Library {
        /// The engine driving it.
        engine: String,
        /// The effective config it was given.
        config: String,
        /// The directory the harness works in. Not `cwd`: this member has no
        /// working directory of its own, and naming one would claim a thing that
        /// is not true.
        worktree: String,
    },
    /// A child process of its own.
    Process {
        /// The program spawned.
        program: String,
        /// Its arguments.
        args: Vec<String>,
        /// The directory the child runs in.
        cwd: String,
    },
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

/// The payload of an [`EventKind::TurnInterrupted`] event.
///
/// Published for every `interrupt`, delivered or not, because "the lever was
/// pulled and nothing happened" is exactly what an operator watching a run needs
/// to see — a verb that stayed silent unless it worked would leave the three
/// not-delivered cases visible only in an exit code somebody has to be watching
/// for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnInterrupted {
    /// The member whose turn was addressed. On the payload as well as in the
    /// labels, because that is what `docs/contract.md` names for this event.
    pub member: String,
    /// Whether the run took ownership of the redirection.
    pub delivered: bool,
    /// How many bytes of redirection were offered; `0` for an interrupt that
    /// only asks the turn to stop.
    pub input_bytes: u64,
    /// Why the delivery did not land — present exactly when
    /// [`delivered`](Self::delivered) is false, and never otherwise, so a
    /// consumer can never read a served interrupt as having had a reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The payload of an [`EventKind::FallbackAdvanced`] event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FallbackAdvanced {
    /// The identity the chain moved past.
    pub identity: String,
    /// oneharness's classification of why it could not run.
    pub reason: String,
    /// Which side of a two-party conversation the chain belonged to. Absent for
    /// a single-sided member, which has one side and so nothing to distinguish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// The turn the chain advanced on, for the same reason: a two-party member
    /// runs one chain per side per turn, and an operator restoring a
    /// subscription needs to know which of them refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
}

/// Which side of a two-party conversation an event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The side that does the work.
    Agent,
    /// The side that supervises it.
    Judge,
}

impl From<onejudge::TelemetryRole> for Role {
    fn from(role: onejudge::TelemetryRole) -> Self {
        match role {
            onejudge::TelemetryRole::Agent => Role::Agent,
            onejudge::TelemetryRole::Judge => Role::Judge,
        }
    }
}

/// The payload of an [`EventKind::MemberDied`] event.
///
/// The first three fields answer for every member. The last three are a *child
/// process's* facts, and a two-party member no longer has them: `docs/contract.md`
/// runs onejudge through its library, in this process, so there is no exit status
/// to read and no stderr to tail. What that member has instead is a typed error,
/// which is what [`cause`](Self::cause) and [`detail`](Self::detail) carry — for
/// both kinds, so a consumer branches on one field rather than on which shape
/// arrived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberDied {
    /// The liveness rule that fired.
    pub rule: String,
    /// What killed the member, classified.
    pub cause: Cause,
    /// What that cause said, bounded by [`MAX_PAYLOAD_TEXT_BYTES`]: the engine's
    /// own error for an in-process member, the tail of standard error for a child
    /// process.
    pub detail: String,
    /// Whether [`detail`](Self::detail) was cut to its documented bound.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    /// The process's exit code, when the member *was* a process and it exited
    /// rather than being signalled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// How the process ended; absent for a member this process ran in-library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<Disposition>,
    /// The tail of the process's standard error; absent for a member this process
    /// ran in-library, which has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
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

/// The classified cause of a [`MemberDied`].
///
/// A closed set, for the same reason [`crate::member::Rule`] is one: this is what
/// a supervisor branches on, and a cause spelled two ways is a branch that
/// silently stops matching. The ten classified kinds are onejudge's own
/// `ProviderErrorKind` — which is in turn oneharness's normalized `failure_kind` —
/// mapped **totally**, so a category added upstream is a compile error here rather
/// than a new bare string on the wire. The last three are the causes that exist
/// only outside that taxonomy: a child process's two dispositions, and an engine
/// failure that named no kind at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// Credentials were missing or rejected.
    Auth,
    /// The provider rate-limited the call.
    RateLimit,
    /// The harness did not recognize the requested model.
    ModelNotFound,
    /// The account's quota or billing limit is exhausted.
    Quota,
    /// A transient server-side overload.
    Overloaded,
    /// The call exceeded its deadline.
    Timeout,
    /// The turn was torn down because it was cancelled.
    Cancelled,
    /// The provider process could not be started.
    Spawn,
    /// The provider ran but violated its protocol.
    Protocol,
    /// A classified failure with no more specific category.
    Other,
    /// The member's own process exited with a status of its own.
    Exited,
    /// The member's own process was terminated by a signal.
    Signaled,
    /// The member failed without any classification at all.
    Unclassified,
}

impl Cause {
    /// This cause's spelling on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::Auth => "auth",
            Cause::RateLimit => "rate_limit",
            Cause::ModelNotFound => "model_not_found",
            Cause::Quota => "quota",
            Cause::Overloaded => "overloaded",
            Cause::Timeout => "timeout",
            Cause::Cancelled => "cancelled",
            Cause::Spawn => "spawn",
            Cause::Protocol => "protocol",
            Cause::Other => "other",
            Cause::Exited => "exited",
            Cause::Signaled => "signaled",
            Cause::Unclassified => "unclassified",
        }
    }
}

impl From<Disposition> for Cause {
    fn from(disposition: Disposition) -> Self {
        match disposition {
            Disposition::Exited => Cause::Exited,
            Disposition::Signaled => Cause::Signaled,
        }
    }
}

impl From<onejudge::ProviderErrorKind> for Cause {
    /// Total, so a category onejudge adds later fails this build instead of
    /// reaching the wire as an unhandled string.
    fn from(kind: onejudge::ProviderErrorKind) -> Self {
        use onejudge::ProviderErrorKind as Kind;
        match kind {
            Kind::Auth => Cause::Auth,
            Kind::RateLimit => Cause::RateLimit,
            Kind::ModelNotFound => Cause::ModelNotFound,
            Kind::Quota => Cause::Quota,
            Kind::Overloaded => Cause::Overloaded,
            Kind::Timeout => Cause::Timeout,
            Kind::Cancelled => Cause::Cancelled,
            Kind::Spawn => Cause::Spawn,
            Kind::Protocol => Cause::Protocol,
            Kind::Other => Cause::Other,
        }
    }
}

/// `skip_serializing_if` helper: an unset truncation flag is omitted rather than
/// written as `false`, so a payload that was never near its bound stays quiet.
fn is_false(value: &bool) -> bool {
    !*value
}

/// Bound one payload text field to [`MAX_PAYLOAD_TEXT_BYTES`], reporting whether
/// it had to be cut.
///
/// The cut lands on a character boundary, and it takes the **tail**: what names a
/// failure is the last of a harness's output, not its startup chatter. A caller
/// that wants the head says so by trimming before it gets here.
#[must_use]
pub fn bound_text(text: &str) -> (String, bool) {
    if text.len() <= MAX_PAYLOAD_TEXT_BYTES {
        return (text.to_string(), false);
    }
    let mut start = text.len() - MAX_PAYLOAD_TEXT_BYTES;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    (text[start..].to_string(), true)
}

/// Bound one [`TurnActivity::detail`] to [`MAX_ACTIVITY_DETAIL_CHARS`],
/// reporting whether it had to be cut.
///
/// Characters, not bytes — the contract says "160-char detail", and a tool
/// summary is prose a person reads.
#[must_use]
pub fn bound_detail(detail: &str) -> (String, bool) {
    let collapsed: String = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_ACTIVITY_DETAIL_CHARS {
        return (collapsed, false);
    }
    (
        collapsed.chars().take(MAX_ACTIVITY_DETAIL_CHARS).collect(),
        true,
    )
}

/// Stamps and numbers this process's events, and writes them where the run said.
///
/// One emitter per producing process, because `seq` is "monotonic per `stream`"
/// and `stream` is "a unique id per producing process" — the two are the same
/// fact, so they are the same object. It is cheap to clone and safe to share
/// across the reader threads a graph runs one per member: the counter is atomic
/// and the sink is behind one lock, so a line is written whole.
#[derive(Clone)]
pub struct Emitter {
    stream: String,
    seq: Arc<AtomicU64>,
    sink: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    labels: Labels,
}

impl std::fmt::Debug for Emitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Emitter")
            .field("stream", &self.stream)
            .field("seq", &self.seq.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Emitter {
    /// An emitter for `stream`, writing to `sink`.
    #[must_use]
    pub fn new(stream: impl Into<String>, sink: Box<dyn std::io::Write + Send>) -> Self {
        Self {
            stream: stream.into(),
            seq: Arc::new(AtomicU64::new(0)),
            sink: Arc::new(Mutex::new(sink)),
            labels: Labels::default(),
        }
    }

    /// The same emitter with `labels` stamped on everything it writes next.
    ///
    /// Enrichers never rewrite: a label already on the derived emitter stays, so
    /// a member's own `member`/`persona` cannot be overwritten by a graph-level
    /// stamp added later.
    #[must_use]
    pub fn with_labels(&self, labels: Labels) -> Self {
        let mut merged = labels;
        merged.run_id = merged.run_id.or_else(|| self.labels.run_id.clone());
        merged.round = merged.round.or(self.labels.round);
        merged.node = merged.node.or_else(|| self.labels.node.clone());
        merged.step = merged.step.or_else(|| self.labels.step.clone());
        merged.member = merged.member.or_else(|| self.labels.member.clone());
        merged.persona = merged.persona.or_else(|| self.labels.persona.clone());
        for (key, value) in &self.labels.extra {
            merged
                .extra
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        Self {
            stream: self.stream.clone(),
            seq: Arc::clone(&self.seq),
            sink: Arc::clone(&self.sink),
            labels: merged,
        }
    }

    /// The stream id every envelope this emitter writes carries.
    #[must_use]
    pub fn stream(&self) -> &str {
        &self.stream
    }

    /// Write one event, returning the envelope as it was written.
    ///
    /// A sink that cannot be written to is not a run failure — the events are
    /// also on disk — so the write is best-effort and the envelope is still
    /// returned to whatever else the run does with it.
    pub fn emit(&self, kind: EventKind, payload: Map<String, Value>) -> Envelope {
        self.emit_with(kind, payload, Vec::new())
    }

    /// [`Emitter::emit`], with artifacts attached.
    pub fn emit_with(
        &self,
        kind: EventKind,
        payload: Map<String, Value>,
        artifacts: Vec<Artifact>,
    ) -> Envelope {
        let envelope = Envelope {
            v: ENVELOPE_VERSION,
            ts: now_rfc3339(),
            stream: self.stream.clone(),
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            source: Source::Agentgraph,
            kind,
            labels: self.labels.clone(),
            payload,
            artifacts,
        };
        if let Ok(mut sink) = self.sink.lock() {
            let line = serde_json::to_string(&envelope).unwrap_or_default();
            let _ = writeln!(sink, "{line}");
            let _ = sink.flush();
        }
        envelope
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shared sink a test can read back, standing in for stdout or the run's
    /// events file.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Recorder {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("recorder").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn lines(recorder: &Recorder) -> Vec<Envelope> {
        let raw = recorder.0.lock().expect("recorder").clone();
        String::from_utf8(raw)
            .expect("utf-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("envelope"))
            .collect()
    }

    /// `seq` is monotonic per stream, and it stays so across the derived
    /// emitters a graph hands to its members — they are one producing process.
    #[test]
    fn seq_is_monotonic_across_every_derived_emitter() {
        let recorder = Recorder::default();
        let emitter = Emitter::new("run-1", Box::new(recorder.clone()));
        let member = emitter.with_labels(Labels {
            member: Some("worker".into()),
            ..Labels::default()
        });
        emitter.emit(EventKind::GraphStarted, Map::new());
        member.emit(EventKind::MemberStarted, Map::new());
        emitter.emit(EventKind::GraphSettled, Map::new());

        let written = lines(&recorder);
        assert_eq!(
            written.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(written
            .iter()
            .all(|e| e.stream == "run-1" && e.v == ENVELOPE_VERSION));
        assert_eq!(written[1].labels.member.as_deref(), Some("worker"));
        assert_eq!(written[0].labels.member, None);
        assert_eq!(emitter.stream(), "run-1");
    }

    /// An enricher never rewrites: a member's own label survives a later stamp,
    /// and a graph-level label reaches a member that did not set one.
    #[test]
    fn labels_are_stamped_but_never_rewritten() {
        let recorder = Recorder::default();
        let base = Emitter::new("s", Box::new(recorder.clone())).with_labels(Labels {
            run_id: Some("R".into()),
            member: Some("graph".into()),
            extra: [("tier".to_string(), Value::from("gate"))]
                .into_iter()
                .collect(),
            ..Labels::default()
        });
        let member = base.with_labels(Labels {
            member: Some("worker".into()),
            round: Some(2),
            extra: [("tier".to_string(), Value::from("member"))]
                .into_iter()
                .collect(),
            ..Labels::default()
        });
        member.emit(EventKind::MemberStarted, Map::new());
        let written = lines(&recorder);
        assert_eq!(written[0].labels.run_id.as_deref(), Some("R"));
        assert_eq!(written[0].labels.member.as_deref(), Some("worker"));
        assert_eq!(written[0].labels.round, Some(2));
        assert_eq!(written[0].labels.extra["tier"], Value::from("member"));
    }

    /// Text fields are bounded at the documented byte count, keeping the tail and
    /// never splitting a character.
    #[test]
    fn payload_text_is_bounded_at_its_documented_size() {
        let (short, cut) = bound_text("brief");
        assert_eq!((short.as_str(), cut), ("brief", false));

        // `é` is two bytes, so a five-byte ASCII tail puts the cut at an odd
        // offset — *inside* a character — and it has to walk forward to the next
        // boundary. That is the case a naive slice panics on.
        let long = format!("{}TAILS", "é".repeat(MAX_PAYLOAD_TEXT_BYTES));
        let (bounded, cut) = bound_text(&long);
        assert!(cut);
        assert!(
            bounded.len() < MAX_PAYLOAD_TEXT_BYTES,
            "the cut did not move to a boundary"
        );
        assert!(bounded.ends_with("TAILS"));
        assert!(bounded.chars().all(|c| c == 'é' || c.is_ascii_uppercase()));

        // And an even offset needs no walk, so the whole bound is used.
        let aligned = format!("{}TAIL", "é".repeat(MAX_PAYLOAD_TEXT_BYTES));
        let (bounded, cut) = bound_text(&aligned);
        assert!(cut);
        assert_eq!(bounded.len(), MAX_PAYLOAD_TEXT_BYTES);
    }

    /// A tool summary is bounded in characters and collapsed to one line, so a
    /// multi-line command cannot break the rendering it lands in.
    #[test]
    fn an_activity_detail_is_collapsed_and_bounded_in_characters() {
        let (detail, cut) = bound_detail("just   check\n  --all");
        assert_eq!((detail.as_str(), cut), ("just check --all", false));

        let (detail, cut) = bound_detail(&"é".repeat(MAX_ACTIVITY_DETAIL_CHARS + 10));
        assert!(cut);
        assert_eq!(detail.chars().count(), MAX_ACTIVITY_DETAIL_CHARS);
    }

    /// Every kind onejudge classifies a provider failure as has a cause of its
    /// own here, and each keeps onejudge's own spelling.
    ///
    /// The spelling is the point: `cause` is what a supervisor branches on, and
    /// oneharness names the same categories in its `failure_kind`, so a consumer
    /// joining one against the other must not have to translate. This walks
    /// onejudge's own `classify`, so a kind renamed upstream fails here.
    #[test]
    fn every_provider_error_kind_maps_to_a_cause_with_the_same_name() {
        for name in [
            "auth",
            "rate_limit",
            "model_not_found",
            "quota",
            "overloaded",
            "timeout",
            "cancelled",
            "spawn",
            "protocol",
            "other",
        ] {
            let kind = onejudge::ProviderErrorKind::classify(name);
            assert_eq!(kind.as_str(), name, "onejudge renamed {name:?}");
            assert_eq!(Cause::from(kind).as_str(), name);
        }
        // The three causes that exist outside that taxonomy.
        assert_eq!(Cause::from(Disposition::Exited), Cause::Exited);
        assert_eq!(Cause::from(Disposition::Signaled), Cause::Signaled);
        assert_eq!(Cause::Unclassified.as_str(), "unclassified");
    }

    /// An emitter whose sink is gone still numbers and returns its events: the
    /// stream is a view, and the run does not fail because nobody is reading.
    #[test]
    fn a_broken_sink_does_not_stop_the_run() {
        struct Broken;
        impl std::io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("gone"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("gone"))
            }
        }
        let emitter = Emitter::new("s", Box::new(Broken));
        assert_eq!(emitter.emit(EventKind::GraphStarted, Map::new()).seq, 0);
        assert_eq!(emitter.emit(EventKind::GraphSettled, Map::new()).seq, 1);
        assert!(format!("{emitter:?}").contains("seq: 2"));
    }
}
