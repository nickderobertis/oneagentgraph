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

// llmlint: ignore-file[boundary_inputs_validated] this whole file is a test
// double, behind the non-default `test-doubles` feature so a published
// `cargo install oneagentgraph` never builds it. The request it reads comes from
// the real `onejudge` this suite drives, one process over, and answering it is
// the double's entire job; a validating parse here would be this crate asserting
// onejudge's protocol against onejudge, and a refusal would surface as a journey
// failure naming the double rather than the thing under test.

// The JSON-lines protocol IS stdout, and a diagnostic IS stderr: onejudge reads
// one response object from the first and a classified failure from the second.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::Read as _;

use serde_json::{json, Value};

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
        Some("judge") => json!({"value": request.get("max").cloned().unwrap_or(json!(5)),
                                "reason": "fake numeric verdict"}),
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
