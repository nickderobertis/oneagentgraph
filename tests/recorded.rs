//! A stream this crate really wrote, read back through the vocabulary it now
//! declares for itself.
//!
//! The words in an envelope used to be `onemessagebus-agent`'s and are this
//! crate's; `tests/recorded/oneagentgraph-run.ndjson` is a whole run recorded
//! while they were the profile's, copied byte for byte (see the `README.md`
//! beside it). Nothing about the bytes was meant to move with them, and the only
//! way to know that is to re-serialize them: a field renamed, a reserved label
//! reordered, an empty collection that stopped being written, or a `session`
//! extra that landed among the reserved keys would each change a byte here while
//! leaving every hand-built fixture in the suite green.

use std::io::Write;
use std::sync::{Arc, Mutex};

use oneagentgraph::event::{
    Emitter, Envelope, EventKind, Labels, TurnStarted, ENVELOPE_VERSION, SESSION_LABEL, SOURCE_WORD,
};
use serde_json::Value;

/// The recorded run, shipped with the crate so a consumer's `cargo test` runs
/// this suite too.
const RECORDED: &str = include_str!("recorded/oneagentgraph-run.ndjson");

/// Every non-empty line of it.
fn lines() -> Vec<&'static str> {
    RECORDED
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect()
}

/// The keys of a JSON object in the order the document lists them.
///
/// `serde_json`'s `preserve_order` is on across this workspace, so a parsed
/// object keeps its document's order and this reads the real one rather than an
/// alphabetized reconstruction of it.
fn keys(object: &Value) -> Vec<String> {
    object
        .as_object()
        .expect("an object")
        .keys()
        .map(ToString::to_string)
        .collect()
}

/// Every line of the recorded run reads as one of this crate's envelopes and
/// serializes back to the bytes it arrived as.
#[test]
fn every_recorded_line_re_serializes_with_no_byte_changed() {
    let lines = lines();
    assert_eq!(lines.len(), 9, "the recorded run is nine envelopes");

    for (at, line) in lines.iter().enumerate() {
        let envelope: Envelope = serde_json::from_str(line)
            .unwrap_or_else(|err| panic!("line {} is not an envelope ({err}): {line}", at + 1));
        let written = serde_json::to_string(&envelope)
            .unwrap_or_else(|err| panic!("line {} does not serialize: {err}", at + 1));
        assert_eq!(
            written,
            *line,
            "line {} did not survive the round trip",
            at + 1
        );
    }
}

/// And it is this crate's own stream it is reading: every line carries the
/// source word this crate stamps, the version it writes, and a label set whose
/// reserved keys landed in their own slots rather than among the extras.
#[test]
fn the_recorded_run_is_this_crates_own_words() {
    for line in lines() {
        let envelope: Envelope = serde_json::from_str(line).expect("an envelope");
        assert_eq!(envelope.source.as_str(), SOURCE_WORD);
        assert_eq!(envelope.v, ENVELOPE_VERSION);
        assert_eq!(envelope.dimensions.phase, None, "no phase is stamped here");
        assert!(
            envelope.labels.run_id.is_some(),
            "`run_id` belongs in its own slot: {:?}",
            envelope.labels
        );
        assert!(
            !envelope.labels.extra.contains_key("run_id")
                && !envelope.labels.extra.contains_key("member")
                && !envelope.labels.extra.contains_key("persona"),
            "a reserved key fell among the extras: {:?}",
            envelope.labels.extra
        );
    }
}

/// A sink the emitter writes into and this test reads back.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<u8>>>);

impl Write for Recorder {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("the sink").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// An envelope the **real** emitter writes, carrying every reserved label and
/// the `session` extra, lands in the key order the recorded stream shows.
///
/// The one envelope in the suite that exercises all six reserved keys at once:
/// the recorded run stamps four of them, so the order of `round`, `node` and
/// `step` between them is asserted here and nowhere else. The recorded stream is
/// still what decides it — its own order is this one restricted to the keys it
/// carries, checked below rather than taken on trust.
#[test]
fn an_emitted_envelope_carries_the_recorded_streams_key_order() {
    let recorder = Recorder::default();
    let emitter = Emitter::new("run-1", Box::new(recorder.clone())).with_labels(Labels {
        run_id: Some("R".into()),
        round: Some(2),
        node: Some("service".into()),
        step: Some("implement".into()),
        member: Some("corpus".into()),
        persona: Some("crozier-corpus".into()),
        ..Labels::default()
    });
    let payload = match serde_json::to_value(TurnStarted {
        turn: 1,
        role: "assistant".into(),
        instruction: "stream something".into(),
        started_at: "2026-09-13T05:32:52.875Z".into(),
        origin: None,
        instruction_truncated: false,
    }) {
        Ok(Value::Object(map)) => map,
        _ => unreachable!("a struct serializes to an object"),
    };
    emitter.emit(EventKind::TurnStarted, payload);

    let written = String::from_utf8(recorder.0.lock().expect("the sink").clone()).expect("utf-8");
    let written = written.trim_end();
    let emitted: Value = serde_json::from_str(written).expect("the emitter wrote an envelope");

    assert_eq!(emitted["source"], Value::from(SOURCE_WORD));
    assert_eq!(emitted["v"], Value::from(ENVELOPE_VERSION));

    // The recorded stream's own turn-started line is what this order is read
    // from: its keys, in its order.
    let recorded: Value = serde_json::from_str(
        lines()
            .into_iter()
            .find(|line| line.contains(r#""kind":"turn-started""#))
            .expect("the recorded run has a turn"),
    )
    .expect("an envelope");
    assert_eq!(keys(&emitted), keys(&recorded));
    assert_eq!(
        keys(&emitted),
        [
            "v",
            "ts",
            "stream",
            "seq",
            "source",
            "kind",
            "labels",
            "payload",
            "artifacts"
        ]
    );

    let emitted_labels = keys(&emitted["labels"]);
    assert_eq!(
        emitted_labels,
        [
            "run_id",
            "round",
            "node",
            "step",
            "member",
            "persona",
            SESSION_LABEL
        ],
        "the reserved keys are written in wire order, with the extras after them"
    );
    let recorded_labels = keys(&recorded["labels"]);
    assert_eq!(
        emitted_labels
            .iter()
            .filter(|key| recorded_labels.contains(key))
            .collect::<Vec<_>>(),
        recorded_labels.iter().collect::<Vec<_>>(),
        "the recorded stream's own label order is this one, restricted to the keys it stamps"
    );
}
