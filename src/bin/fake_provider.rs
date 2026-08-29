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
//!
//! It answers each op from the request alone, with one exception worth knowing
//! about: the supervisor's release decision is taken from the **persona it was
//! handed**, not from anything written here. A worker whose turn reports
//! `blocker: <condition>` ends the conversation only when the persona's own
//! "not terminal" clause does not name that condition — see [`retryable`]. That
//! is what keeps the journeys driving it about `personas/engineer.yaml` rather
//! than about this file.
//!
//! # Why this is not onejudge's own `onejudge-echo-provider`
//!
//! onejudge does ship one — `src/bin/echo_provider.rs`, behind its `fake-provider`
//! feature, answering the same five ops. It is a **binary of a dependency**, and
//! cargo does not build those for a consumer: a `dev-dependencies` entry with that
//! feature compiles onejudge's *library*, never its `[[bin]]`, so there is no
//! `CARGO_BIN_EXE_*` to point a graph at. Reaching it means `cargo install
//! onejudge --features fake-provider` in `bootstrap` and in CI, which puts a
//! second pin on a crate `Cargo.lock` already pins and a second `onejudge` build
//! on `PATH` to shadow — the outage `tests/e2e/support.rs::required` exists to
//! diagnose, and one this development host already had: an
//! `onejudge-echo-provider` in the cargo bin directory built from a release older
//! than the engine `Cargo.lock` links.
//!
//! What would close this: `pub` access to that responder from the onejudge
//! *library* under the same feature, which would make this file three lines. That
//! is an upstream proposal, not a change to make from here.

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
    let op = request.get("op").and_then(Value::as_str);
    // The other half of the protocol this double reads, validated where `messages`
    // is and for the same reason: the supervisor's release decision is taken from
    // the persona it was handed, so a request carrying none — or carrying
    // something that is not a persona — is one this double cannot answer. Reading
    // an absent field as an empty persona would answer *something*: every blocker
    // would read as terminal, and a journey would pass against a decision no
    // persona made.
    let persona = match (op, request.get("persona").and_then(Value::as_str)) {
        (Some("supervisor"), None) => {
            eprintln!(
                "fake-provider: the supervisor request carries no `persona` string to judge a \
                 reported blocker against"
            );
            return std::process::ExitCode::from(1);
        }
        (_, persona) => persona.unwrap_or_default(),
    };
    let response = match op {
        // The unified per-turn supervisor: it decides completion, or supplies the
        // next simulated-user message.
        Some("supervisor") => {
            match reported_blocker(messages) {
                // The worker says it is blocked, and the persona this supervisor
                // was handed decides what that means — see [`retryable`].
                Some(blocker) if retryable(persona, &blocker) => json!({
                    "completion": false,
                    "message": format!("run it again and keep going: {blocker}"),
                    "reason": "the blocker reported is one the worker could retry in this run",
                }),
                // Released: `completion: false` with no message at all, which is
                // onejudge's own `NoInstruction` — the conversation ends on the
                // work it has, with the done-when verdict still to come.
                Some(blocker) => json!({
                    "completion": false,
                    "reason": format!("terminal blocker reported: {blocker}"),
                }),
                None if !steers(&task, "should-fail") && turns >= 1 => {
                    json!({"completion": true, "reason": "fake supervisor verified completion"})
                }
                None => json!({"completion": false,
                               "message": "verify it before you call it done",
                               "reason": "fake supervisor requires another turn"}),
            }
        }
        Some("user") => json!({"message": "verify it before you call it done", "stop": false}),
        // A worker that ended the conversation reporting a blocker did not finish
        // the task, so the boolean bar is false — which is what keeps a released
        // conversation from settling as a completed one.
        Some("judge") if request.get("kind").and_then(Value::as_str) == Some("boolean") => {
            json!({
                "value": !steers(&task, "should-fail") && reported_blocker(messages).is_none(),
                "reason": "fake judge verdict",
            })
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

/// The marker a journey's worker states a blocker with, and the condition after
/// it.
///
/// Read off the **last assistant turn** alone: the supervisor is asked after every
/// turn, and what it decides on is what the worker just said.
const BLOCKER: &str = "blocker:";

/// The condition the worker's latest turn reports itself blocked on, when it
/// reports one.
fn reported_blocker(messages: &[Value]) -> Option<String> {
    let last = messages
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))?;
    last.get("content")
        .and_then(Value::as_str)?
        .lines()
        .find_map(|line| line.trim().strip_prefix(BLOCKER))
        .map(|condition| condition.trim().trim_end_matches('.').to_string())
}

/// Whether the supervisor's own persona says `blocker` is one the worker could
/// retry inside this run — so the conversation goes on rather than being released.
///
/// This double is not a model and cannot weigh prose, so it does the one thing
/// that keeps this journey about the **persona** rather than about the double: it
/// asks the persona. The clause naming what is *not* terminal is located in the
/// text the supervisor was actually handed, and the condition the worker reported
/// is looked for inside it. A persona that stopped excluding a command that timed
/// out stops answering this way, and the journey that drives it goes red.
fn retryable(persona: &str, blocker: &str) -> bool {
    // The persona arrives as a wrapped block scalar, so the clause is as likely to
    // have a newline through the middle of it as a space.
    let flowing = persona.split_whitespace().collect::<Vec<_>>().join(" ");
    flowing
        .split(". ")
        .filter(|sentence| sentence.contains("not terminal"))
        .any(|sentence| sentence.contains(blocker))
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
