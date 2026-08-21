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
        // Whichever runner this member has: a child process names its program,
        // and one driven in this process names the engine driving it.
        EventKind::MemberStarted => match field(envelope, "program") {
            program if program.is_empty() => field(envelope, "engine"),
            program => program,
        },
        EventKind::TurnStarted => turn_head(envelope),
        EventKind::TurnActivity => {
            // A result names no tool — it answers one — so its `kind` is what
            // identifies it, and what it returned is where the detail is. Only
            // the last line of that, as `member-died` renders its own: an
            // observation runs to the payload bound and a line does not.
            let head = match field(envelope, "name") {
                name if name.is_empty() => field(envelope, "kind"),
                name => name,
            };
            let body = match field(envelope, "detail") {
                detail if detail.is_empty() => field(envelope, "output")
                    .lines()
                    .next_back()
                    .unwrap_or_default()
                    .to_string(),
                detail => detail,
            };
            joined(&[head, body])
        }
        EventKind::TurnMessage => {
            // The opening line of what was said, which is what a person watching
            // a live dispatch reads the event for; the whole of it is on the
            // wire for a consumer that wants it.
            let text = field(envelope, "text");
            let opening = text.lines().next().unwrap_or_default().to_string();
            joined(&[turn_head(envelope), opening])
        }
        EventKind::TurnCompleted => joined(&[turn_head(envelope), usage(envelope)]),
        EventKind::TurnInterrupted => {
            let delivered = envelope
                .payload
                .get("delivered")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let bytes = field(envelope, "input_bytes");
            // The reason only for a delivery that did not land, which is the one
            // that has one — a served interrupt says how much redirection it
            // carried instead.
            if delivered {
                format!("delivered ({bytes} bytes)")
            } else {
                let reason = field(envelope, "reason");
                if reason.is_empty() {
                    "not delivered".to_string()
                } else {
                    format!("not delivered: {reason}")
                }
            }
        }
        EventKind::MemberHeartbeat => "alive".to_string(),
        EventKind::FallbackAdvanced => format!(
            "{} ({})",
            field(envelope, "identity"),
            field(envelope, "reason")
        ),
        EventKind::OneharnessSession => format!(
            "{} turn {} {}",
            field(envelope, "role"),
            field(envelope, "turn"),
            field(envelope, "history_id")
        ),
        EventKind::MemberDied => {
            // The three fields answering for every member, then the exit status
            // only a member that was a child process has — omitted rather than
            // rendered empty, so a line never claims a process that never was.
            let mut head = format!("{} ({})", field(envelope, "rule"), field(envelope, "cause"),);
            let code = field(envelope, "exit_code");
            if !code.is_empty() {
                head.push_str(&format!(" exit={code}"));
            }
            let detail = field(envelope, "detail");
            if detail.is_empty() {
                head
            } else {
                format!("{head}: {}", detail.lines().next_back().unwrap_or_default())
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
    let kind = envelope.kind.as_str();
    if detail.is_empty() {
        format!("{} {member} {kind}", envelope.ts)
    } else {
        format!("{} {member} {kind} {detail}", envelope.ts)
    }
}

/// The parts of a rendering that had something to say, in order.
fn joined(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|part| !part.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Which turn of whose conversation an event belongs to.
fn turn_head(envelope: &Envelope) -> String {
    let turn = field(envelope, "turn");
    if turn.is_empty() {
        return String::new();
    }
    match field(envelope, "role") {
        role if role.is_empty() => format!("turn {turn}"),
        role => format!("turn {turn} ({role})"),
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
                json!({"runner": "process", "program": "oneharness"}),
                "member-started oneharness",
            ),
            (
                EventKind::MemberStarted,
                json!({"runner": "library", "engine": "onejudge"}),
                "member-started onejudge",
            ),
            (
                EventKind::TurnStarted,
                json!({"turn": 2, "role": "assistant",
                       "instruction": "verify it", "started_at": "2026-08-08T06:12:20.000Z"}),
                "turn-started turn 2 (assistant)",
            ),
            (
                EventKind::TurnActivity,
                json!({"kind": "tool_call", "name": "bash", "detail": "just check", "index": 0}),
                "turn-activity bash just check",
            ),
            // A result names no tool, so its kind identifies it and the last line
            // of what it returned is the detail.
            (
                EventKind::TurnActivity,
                json!({"kind": "tool_result", "name": Value::Null, "detail": "",
                       "output": "running\n2 passed; 0 failed", "index": 1}),
                "turn-activity tool_result 2 passed; 0 failed",
            ),
            (
                EventKind::TurnMessage,
                json!({"turn": 2, "role": "assistant", "text": "done\nand verified"}),
                "turn-message turn 2 (assistant) done",
            ),
            (
                EventKind::TurnCompleted,
                json!({"turn": 2, "role": "assistant",
                       "usage": {"input_tokens": 10, "output_tokens": 5,
                                 "cache_read_tokens": 4, "cache_write_tokens": null,
                                 "cost_usd": 0.002},
                       "started_at": "2026-08-08T06:12:20.000Z",
                       "finished_at": "2026-08-08T06:12:22.847Z"}),
                "turn-completed turn 2 (assistant) in=10 out=5 cache_r=4 cache_w=- cost=0.002",
            ),
            (
                EventKind::TurnInterrupted,
                json!({"member": "worker", "delivered": true, "input_bytes": 31}),
                "turn-interrupted delivered (31 bytes)",
            ),
            (
                EventKind::TurnInterrupted,
                json!({"member": "worker", "delivered": false, "input_bytes": 0,
                       "reason": "the member is between turns"}),
                "turn-interrupted not delivered: the member is between turns",
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
                json!({"rule": "activity", "cause": "exited", "exit_code": 1,
                       "disposition": "exited",
                       "detail": "first\nharness failed (quota)"}),
                "member-died activity (exited) exit=1: harness failed (quota)",
            ),
            // The same event from a member that was never a process: the typed
            // cause carries it, and no exit status is claimed.
            (
                EventKind::MemberDied,
                json!({"rule": "provider-failure", "cause": "quota",
                       "detail": "the subscription is exhausted"}),
                "member-died provider-failure (quota): the subscription is exhausted",
            ),
            (
                EventKind::OneharnessSession,
                json!({"role": "agent", "turn": 2, "history_id": "01a00d0f"}),
                "oneharness-session agent turn 2 01a00d0f",
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

    /// The branches with nothing to add render the bare fact rather than a
    /// trailing separator: a death with no stderr, a settle with no reason, and
    /// a payload field the producer left null.
    #[test]
    fn an_event_with_nothing_to_add_renders_the_bare_fact() {
        let died = line(&envelope(
            EventKind::MemberDied,
            json!({"rule": "heartbeat", "cause": "signaled", "exit_code": Value::Null,
                   "disposition": "signaled", "detail": ""}),
        ));
        assert!(died.ends_with("member-died heartbeat (signaled)"), "{died}");

        let settled = line(&envelope(
            EventKind::MemberSettled,
            json!({"completed": false, "completion_reason": Value::Null}),
        ));
        assert!(settled.ends_with("member-settled incomplete"), "{settled}");

        let usage = line(&envelope(
            EventKind::TurnCompleted,
            json!({"usage": "not an object"}),
        ));
        assert!(usage.ends_with("turn-completed"), "{usage}");

        // A refusal a producer left unexplained still renders as the refusal it
        // is, rather than as a line ending in a colon.
        let unexplained = line(&envelope(
            EventKind::TurnInterrupted,
            json!({"member": "worker", "delivered": false, "input_bytes": 0}),
        ));
        assert!(
            unexplained.ends_with("turn-interrupted not delivered"),
            "{unexplained}"
        );
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
