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
    let task = first_user_message(&request);
    let turns = assistant_turns(&request);
    let response = match request.get("op").and_then(Value::as_str) {
        // The unified per-turn supervisor: it decides completion, or supplies the
        // next simulated-user message.
        Some("supervisor") => {
            let complete = !task.contains("should-fail") && turns >= 1;
            if complete {
                json!({"completion": true, "reason": "fake supervisor verified completion"})
            } else {
                json!({"completion": false, "message": "verify it before you call it done",
                       "reason": "fake supervisor requires another turn"})
            }
        }
        Some("user") => json!({"message": "verify it before you call it done", "stop": false}),
        Some("judge") if request.get("kind").and_then(Value::as_str) == Some("boolean") => {
            json!({"value": !task.contains("should-fail"), "reason": "fake judge verdict"})
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
fn first_user_message(request: &Value) -> String {
    request
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .and_then(|message| message.get("content").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

/// How many turns the agent has taken so far.
fn assistant_turns(request: &Value) -> usize {
    request
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
                .count()
        })
        .unwrap_or(0)
}
