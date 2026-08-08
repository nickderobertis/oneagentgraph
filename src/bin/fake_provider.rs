//! `oneagentgraph-fake-provider` — a onejudge `CommandProvider` backend.
//!
//! The contract lets a two-party member's judge side be `judge: {command:
//! ["…"]}` instead of a harness. That side speaks onejudge's JSON-lines protocol
//! (onejudge's `docs/protocol.md`): onejudge spawns the command once per
//! operation, writes **one** JSON request object to stdin, and reads **one** JSON
//! response object from stdout.
//!
//! This is that backend, deterministic, so a journey can drive the real
//! `split`-provider path — real onejudge, real oneharness on the agent side —
//! with only the paid supervisor replaced.

// The JSON-lines protocol IS stdout, and a diagnostic IS stderr: onejudge reads
// one response object from the first and a classified failure from the second.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::Read as _;

use serde_json::{json, Value};

/// The score a numeric judgement answers when the request names no usable bound.
const DEFAULT_MAX_SCORE: u64 = 5;

/// Whether the task carries `sentinel`.
///
/// The same `fake:` prefix the harness double documents, for the same reason and
/// on the same text: what arrives here is a rendered user message, persona
/// included, so an unprefixed word matches prose nobody meant it to. Both doubles
/// read one steering protocol; `src/bin/fake_harness.rs` is where it is written
/// down.
fn steers(task: &str, sentinel: &str) -> bool {
    task.contains(&format!("fake:{sentinel}"))
}

fn main() -> std::process::ExitCode {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        eprintln!("fake-provider: could not read the request");
        return std::process::ExitCode::from(1);
    }
    let Ok(request) = serde_json::from_str::<Value>(&raw) else {
        eprintln!("fake-provider: the request is not JSON");
        return std::process::ExitCode::from(1);
    };
    // `messages` is the half of the protocol every answer below is derived from:
    // the task steers the verdict, and the turn count decides whether the
    // supervisor lets the conversation finish. A request without it is one this
    // double cannot answer, and defaulting to an empty conversation would answer
    // *something* — a journey passing against a request it never received. So it
    // is refused here, once, rather than absorbed twice below.
    let Some(messages) = request.get("messages").and_then(Value::as_array) else {
        eprintln!("fake-provider: the request has no `messages` array to answer from");
        return std::process::ExitCode::from(1);
    };
    let task = first_user_message(messages);
    let turns = assistant_turns(messages);
    let response = match request.get("op").and_then(Value::as_str) {
        // The unified per-turn supervisor: it decides completion, or supplies the
        // next simulated-user message.
        Some("supervisor") => {
            let complete = !steers(&task, "should-fail") && turns >= 1;
            if complete {
                json!({"completion": true, "reason": "fake supervisor verified completion"})
            } else {
                json!({"completion": false, "message": "verify it before you call it done",
                       "reason": "fake supervisor requires another turn"})
            }
        }
        Some("user") => json!({"message": "verify it before you call it done", "stop": false}),
        Some("judge") if request.get("kind").and_then(Value::as_str) == Some("boolean") => {
            json!({"value": !steers(&task, "should-fail"), "reason": "fake judge verdict"})
        }
        // A numeric verdict has to be a number, and `max` is the *request's* —
        // another process's JSON. Reflecting it unread would answer a string or a
        // mapping as this side's "score", and onejudge would reject a response
        // this double claimed to have produced. Anything that is not a usable
        // bound falls back to the default rather than being echoed.
        Some("judge") => json!({
            "value": request
                .get("max")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_MAX_SCORE),
            "reason": "fake numeric verdict",
        }),
        Some("assess") => json!({"text": "None"}),
        other => {
            eprintln!("fake-provider: unknown op {other:?}");
            return std::process::ExitCode::from(1);
        }
    };
    println!("{response}");
    std::process::ExitCode::SUCCESS
}

/// The task, which onejudge always sends as the first user message.
///
/// A conversation with no user message at all is not malformed — the supervisor is
/// asked before the first one exists — so an absent task is empty rather than a
/// refusal. That the array itself is present is settled by the caller.
fn first_user_message(messages: &[Value]) -> String {
    messages
        .iter()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .and_then(|message| message.get("content").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

/// How many turns the agent has taken so far.
fn assistant_turns(messages: &[Value]) -> usize {
    messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .count()
}
