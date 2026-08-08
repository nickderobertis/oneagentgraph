//! `--output text`: a deterministic rendering of the same events.
//!
//! `docs/contract.md` is explicit that text mode is "a deterministic rendering
//! of the same events, never separate content." So this module is a pure
//! function of one [`Envelope`], and the text writer is the JSON writer with
//! this applied — there is no second path that could report something the JSON
//! does not.

use serde_json::Value;

use crate::event::{Envelope, EventKind};

/// One envelope, as a line of text.
#[must_use]
pub fn line(envelope: &Envelope) -> String {
    let member = envelope.labels.member.as_deref().unwrap_or("graph");
    let detail = match envelope.kind {
        EventKind::GraphStarted => field(envelope, "name"),
        EventKind::MemberStarted => field(envelope, "program"),
        EventKind::TurnStarted => format!("turn {}", field(envelope, "turn")),
        EventKind::TurnActivity => {
            let (name, detail) = (field(envelope, "name"), field(envelope, "detail"));
            [name, detail]
                .iter()
                .filter(|part| !part.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(" ")
        }
        EventKind::TurnCompleted => usage(envelope),
        EventKind::MemberHeartbeat => "alive".to_string(),
        EventKind::FallbackAdvanced => format!(
            "{} ({})",
            field(envelope, "identity"),
            field(envelope, "reason")
        ),
        EventKind::MemberDied => {
            let tail = field(envelope, "stderr_tail");
            let head = format!(
                "{} exit={} {}",
                field(envelope, "rule"),
                field(envelope, "exit_code"),
                field(envelope, "disposition"),
            );
            if tail.is_empty() {
                head
            } else {
                format!("{head}: {}", tail.lines().next_back().unwrap_or_default())
            }
        }
        EventKind::CronFired => "fired".to_string(),
        EventKind::CronReset => "clock restarted".to_string(),
        EventKind::MemberSettled => {
            let completed = envelope
                .payload
                .get("completed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let reason = field(envelope, "completion_reason");
            let verdict = if completed { "completed" } else { "incomplete" };
            if reason.is_empty() {
                verdict.to_string()
            } else {
                format!("{verdict}: {reason}")
            }
        }
        EventKind::GraphSettled => format!("exit {}", field(envelope, "exit_code")),
    };
    let kind = serde_json::to_value(envelope.kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default();
    if detail.is_empty() {
        format!("{} {member} {kind}", envelope.ts)
    } else {
        format!("{} {member} {kind} {detail}", envelope.ts)
    }
}

/// One payload field, rendered for a person.
fn field(envelope: &Envelope, name: &str) -> String {
    match envelope.payload.get(name) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// A turn's usage, in the order the contract lists it.
fn usage(envelope: &Envelope) -> String {
    let Some(usage) = envelope
        .payload
        .get("usage")
        .filter(|value| value.is_object())
    else {
        return String::new();
    };
    let number = |name: &str| {
        usage
            .get(name)
            .filter(|value| !value.is_null())
            .map_or_else(|| "-".to_string(), std::string::ToString::to_string)
    };
    format!(
        "in={} out={} cache_r={} cache_w={} cost={}",
        number("input_tokens"),
        number("output_tokens"),
        number("cache_read_tokens"),
        number("cache_write_tokens"),
        number("cost_usd"),
    )
}

/// Wraps a sink so every envelope written through it renders as text.
///
/// Line-buffered on purpose: `writeln!` reaches a writer as *two* calls — the
/// formatted body, then the newline — so a renderer that treated each call as a
/// whole line emitted a blank one after every event. The unit is the NDJSON
/// line, so that is what this buffers to.
pub struct Text<W> {
    sink: W,
    pending: String,
}

impl<W: std::io::Write> Text<W> {
    /// Render onto `sink`.
    pub const fn new(sink: W) -> Self {
        Self {
            sink,
            pending: String::new(),
        }
    }

    /// Render one complete line.
    ///
    /// A line that does not parse is not an envelope this crate wrote — it is
    /// forwarded rather than dropped, because swallowing it would make the text
    /// view say less than the stream it renders.
    fn emit(&mut self, entry: &str) -> std::io::Result<()> {
        match serde_json::from_str::<Envelope>(entry) {
            Ok(envelope) => writeln!(self.sink, "{}", line(&envelope)),
            Err(_) => writeln!(self.sink, "{entry}"),
        }
    }
}

impl<W: std::io::Write> std::io::Write for Text<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.pending.push_str(&String::from_utf8_lossy(buf));
        while let Some(at) = self.pending.find('\n') {
            let entry: String = self.pending.drain(..=at).collect();
            let entry = entry.trim_end_matches('\n').to_string();
            if !entry.is_empty() {
                self.emit(&entry)?;
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // A stream that ended mid-line still has something to say; render it
        // rather than dropping it on the way out.
        if !self.pending.is_empty() {
            let entry = std::mem::take(&mut self.pending);
            self.emit(&entry)?;
        }
        self.sink.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use serde_json::json;

    use super::*;
    use crate::event::{Labels, Source, ENVELOPE_VERSION};

    fn envelope(kind: EventKind, payload: Value) -> Envelope {
        Envelope {
            v: ENVELOPE_VERSION,
            ts: "2026-08-08T06:12:22.847Z".into(),
            stream: "run-1".into(),
            seq: 0,
            source: Source::Agentgraph,
            kind,
            labels: Labels {
                member: Some("worker".into()),
                ..Labels::default()
            },
            payload: payload.as_object().cloned().unwrap_or_default(),
            artifacts: Vec::new(),
        }
    }

    /// Every kind renders, and each says the thing a reader is watching for.
    #[test]
    fn every_kind_renders_its_own_detail() {
        let cases = [
            (
                EventKind::GraphStarted,
                json!({"name": "node-scope"}),
                "graph-started node-scope",
            ),
            (
                EventKind::MemberStarted,
                json!({"program": "onejudge"}),
                "member-started onejudge",
            ),
            (
                EventKind::TurnStarted,
                json!({"turn": 2}),
                "turn-started turn 2",
            ),
            (
                EventKind::TurnActivity,
                json!({"name": "bash", "detail": "just check"}),
                "turn-activity bash just check",
            ),
            (
                EventKind::TurnCompleted,
                json!({"usage": {"input_tokens": 10, "output_tokens": 5,
                                 "cache_read_tokens": 4, "cache_write_tokens": null,
                                 "cost_usd": 0.002}}),
                "turn-completed in=10 out=5 cache_r=4 cache_w=- cost=0.002",
            ),
            (
                EventKind::MemberHeartbeat,
                json!({}),
                "member-heartbeat alive",
            ),
            (
                EventKind::FallbackAdvanced,
                json!({"identity": "claude-code", "reason": "auth"}),
                "fallback-advanced claude-code (auth)",
            ),
            (
                EventKind::MemberDied,
                json!({"rule": "activity", "exit_code": 1, "disposition": "exited",
                       "stderr_tail": "first\nharness failed (quota)"}),
                "member-died activity exit=1 exited: harness failed (quota)",
            ),
            (EventKind::CronFired, json!({}), "cron-fired fired"),
            (
                EventKind::CronReset,
                json!({}),
                "cron-reset clock restarted",
            ),
            (
                EventKind::MemberSettled,
                json!({"completed": true, "completion_reason": "verified"}),
                "member-settled completed: verified",
            ),
            (
                EventKind::GraphSettled,
                json!({"exit_code": 0}),
                "graph-settled exit 0",
            ),
        ];
        for (kind, payload, expected) in cases {
            let rendered = line(&envelope(kind, payload));
            assert!(
                rendered.starts_with("2026-08-08T06:12:22.847Z worker "),
                "{kind:?}: {rendered}"
            );
            assert!(rendered.ends_with(expected), "{kind:?}: {rendered}");
        }
    }

    /// A member-less event renders under the graph, and an absent detail leaves
    /// the line at its kind rather than a trailing space.
    #[test]
    fn a_graph_level_event_renders_without_a_member() {
        let mut envelope = envelope(EventKind::MemberHeartbeat, json!({}));
        envelope.labels.member = None;
        envelope.payload.clear();
        assert!(line(&envelope).ends_with("graph member-heartbeat alive"));

        let bare = envelope_without_detail();
        assert_eq!(
            line(&bare),
            "2026-08-08T06:12:22.847Z worker turn-completed"
        );
    }

    fn envelope_without_detail() -> Envelope {
        envelope(EventKind::TurnCompleted, json!({"usage": Value::Null}))
    }

    /// The text writer renders what came through it, forwards anything that is
    /// not an envelope rather than swallowing it, and emits exactly one line per
    /// event however the bytes were split across writes.
    ///
    /// The split matters: `writeln!` reaches a writer as the body and then the
    /// newline, which a per-call renderer turned into a blank line after every
    /// event.
    #[test]
    fn the_text_writer_renders_one_line_per_event_however_the_bytes_arrive() {
        let mut rendered = Vec::new();
        {
            let mut text = Text::new(&mut rendered);
            let line = serde_json::to_string(&envelope(EventKind::CronFired, json!({})))
                .expect("an envelope");
            // Exactly how the emitter's `writeln!` arrives: body, then newline.
            text.write_all(line.as_bytes()).expect("write");
            text.write_all(b"\n").expect("write");
            text.write_all(b"not an envelope\n").expect("write");
            // And a final line the stream ended without terminating.
            text.write_all(b"trailing").expect("write");
            text.flush().expect("flush");
        }
        let out = String::from_utf8(rendered).expect("utf-8");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "{out}");
        assert!(lines[0].ends_with("cron-fired fired"), "{out}");
        assert_eq!(lines[1], "not an envelope");
        assert_eq!(lines[2], "trailing");
    }
}
