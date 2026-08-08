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
// before it knows whether it can decode the rest, and `member-died` is specified as four
// sibling fields (`rule`, `exit_code`, `disposition`, `stderr_tail`), so folding the exit
// code into an `exited` variant would change what this stack emits.

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
