//! `oneagentgraph-fake-harness` — a stand-in for a paid harness CLI.
//!
//! This is the **one** thing the e2e suite is allowed to fake, and it is faked
//! at oneharness's own designated seam: `ONEHARNESS_BIN_<ID>` names a binary,
//! and pointing it here replaces exactly the paid provider process. Real
//! `oneagentgraph` constructs the invocation, real `onejudge` runs the
//! conversation loop, real `oneharness` selects the identity, spawns this,
//! classifies its refusal, and falls through to the next candidate. Nothing
//! between them is stubbed.
//!
//! It speaks Claude Code's `stream-json` wire shape, because that is what
//! `oneharness run` asks a claude-code candidate for:
//!
//! ```jsonl
//! {"type":"system","subtype":"init","session_id":"…"}
//! {"type":"assistant","message":{"content":[{"type":"tool_use",…}]}}
//! {"type":"result","subtype":"success","result":"…","usage":{…}}
//! ```
//!
//! The turn is steered by sentinels in the prompt (which arrives on the argv,
//! after `-p`, because that is how oneharness spawns claude-code) and by
//! `FAKE_HARNESS_*` variables, so one binary covers every journey.
//!
//! Every sentinel carries the [`MARK`] prefix, and that is not decoration: a
//! prompt is the whole rendered system prompt, persona included, so a bare word
//! matches prose nobody meant it to. `hang` is a substring of `change`, which is
//! how a persona telling an agent to state a change's blast radius silently
//! parked every turn of the suite.
//!
//! | sentinel / variable | what this turn does |
//! | --- | --- |
//! | `complete-now` | the agent finishes on its first turn |
//! | `should-fail` | the agent never finishes, so the run hits its turn cap |
//! | `fake:hold=<path>` | block until `<path>` exists — an observably in-flight turn |
//! | `fake:hang` | never answer at all, for the watchdogs |
//! | `fake:record-prompt=<path>` | append the exact prompt this side was given |
//! | `fake:record-env=<path>` | append this process's selection-shaped environment |
//! | `fake:record-argv=<path>` | append the argv this side was spawned with |
//! | `FAKE_HARNESS_REFUSAL=quota` | a zero-work 429 the chain steps past |
//! | `FAKE_HARNESS_REFUSAL=auth` | an unauthenticated refusal, on stderr alone |
//! | `FAKE_HARNESS_REFUSAL=rate_limit` | the refusal a chain does **not** step past |
//! | `FAKE_HARNESS_CRASH=<code>` | exit that code having published nothing |
//!
//! Keep it deterministic and dependency-free beyond what the crate already
//! carries: it is spawned as a subprocess, many times per journey.

// This binary's whole product IS its stdout and stderr: it stands in for a
// harness CLI, and a harness CLI answers on those two streams.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::Write as _;

use serde_json::{json, Value};

/// The exit code a refusing subscription leaves behind, captured from a real
/// one: the turn failed, so the record is `nonzero` with exit code 1.
const REFUSAL_EXIT: i32 = 1;

/// The ways this double can refuse a turn.
///
/// A closed set parsed once, because an unknown value is a journey asserting
/// against a turn it never configured — which passes for the wrong reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// An unauthenticated identity, which fails before the turn.
    Auth,
    /// A subscription out of quota, which answers having spent nothing.
    Quota,
    /// The same shape after billed work, which a chain does not step past.
    RateLimit,
}

impl Refusal {
    /// The refusal a value asks for: `Some(None)` for no refusal at all, and
    /// `None` for a value this double cannot read.
    fn parse(requested: &str) -> Option<Option<Self>> {
        match requested {
            "" => Some(None),
            "auth" => Some(Some(Refusal::Auth)),
            "quota" => Some(Some(Refusal::Quota)),
            "rate_limit" => Some(Some(Refusal::RateLimit)),
            _ => None,
        }
    }
}

/// The prefix every steering sentinel carries.
///
/// A prompt is the whole rendered system prompt plus the task, so an unprefixed
/// word matches prose. This one appears in no persona and no report.
const MARK: &str = "fake:";

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // The turn arrives in two halves and both matter: the task is the `-p`
    // prompt, and the merged base-plus-persona instructions are the
    // `--append-system-prompt` claude-code takes a system prompt on. A double
    // that read only the first would report the agent side as having been given
    // no role at all.
    let task = flag(&argv, "-p")
        .or_else(|| flag(&argv, "--prompt"))
        .unwrap_or_default();
    let system = flag(&argv, "--append-system-prompt")
        .or_else(|| flag(&argv, "--system"))
        .unwrap_or_default();
    let prompt = format!("{system}\n{task}");

    record(&prompt, "record-prompt", &prompt);
    record(&prompt, "record-env", &selection_environment());
    // The argv is where a model reaches a harness, so it is where a journey can
    // see that the side it was written on is the side that got it.
    record(&prompt, "record-argv", &argv.join(" "));

    // These two variables are how a journey steers this double, and a value it
    // cannot read is a journey asserting against a turn it never configured —
    // which passes for the wrong reason. Refuse loudly instead.
    if let Ok(requested) = std::env::var("FAKE_HARNESS_CRASH") {
        let Ok(code) = requested.parse::<i32>() else {
            eprintln!("fake-harness: FAKE_HARNESS_CRASH must be an exit code, got {requested:?}");
            return exit(2);
        };
        eprintln!("fake-harness: exiting {code} having published nothing");
        return exit(code);
    }
    let requested = std::env::var("FAKE_HARNESS_REFUSAL").unwrap_or_default();
    let Some(refusal) = Refusal::parse(&requested) else {
        eprintln!(
            "fake-harness: FAKE_HARNESS_REFUSAL must be auth, quota, or rate_limit, got \
             {requested:?}"
        );
        return exit(2);
    };
    match refusal {
        // An unauthenticated identity never gets far enough to answer: it fails
        // before the turn and says so on stderr alone. oneharness classifies
        // that as `auth`, which is a classification a chain steps past.
        Some(Refusal::Auth) => {
            eprintln!("401 Unauthorized: no credentials");
            return exit(REFUSAL_EXIT);
        }
        // A subscription that is out of quota does not say so plainly: it
        // answers with a terminal record that reads as a success and declares
        // the rejection only through `terminal_reason` and an embedded
        // `api_error_status`, having spent nothing. The accounting is what
        // oneharness classifies on, which is why every counter here is zero.
        Some(Refusal::Quota) => {
            emit(&json!({
                "type": "result", "subtype": "success", "terminal_reason": "api_error",
                "api_error_status": 429, "result": "",
                "usage": {"input_tokens": 0, "output_tokens": 0}, "total_cost_usd": 0.0,
                "modelUsage": {},
            }));
            return exit(REFUSAL_EXIT);
        }
        // The same 429 shape *after* billed work. oneharness stops a chain on
        // one, because that record carries work the provider already charged
        // for — which is what `smoke` refuses to excuse as a fall-through.
        Some(Refusal::RateLimit) => {
            emit(&json!({
                "type": "result", "subtype": "success", "terminal_reason": "rate_limit",
                "api_error_status": 429, "result": "",
                "usage": {"input_tokens": 900, "output_tokens": 120}, "total_cost_usd": 0.42,
            }));
            return exit(REFUSAL_EXIT);
        }
        None => {}
    }

    if prompt.contains(&format!("{MARK}hang")) {
        // Never answer. The heartbeat and activity watchdogs are what ends this.
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }
    if let Some(path) = sentinel_path(&prompt, "hold") {
        while !path.exists() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    let session = std::env::var("FAKE_HARNESS_SESSION").unwrap_or_else(|_| "fake-session".into());
    emit(&json!({"type": "system", "subtype": "init", "session_id": session}));
    emit(&json!({
        "type": "assistant", "session_id": session,
        "message": {"id": "m1", "type": "message", "role": "assistant", "content": [
            {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "just check"}}]},
    }));
    emit(&json!({
        "type": "result", "subtype": "success", "is_error": false, "duration_ms": 5,
        "num_turns": 1, "result": answer(&prompt), "session_id": session,
        "usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 4,
                  "cache_creation_input_tokens": 1},
        "total_cost_usd": 0.002,
    }));
    exit(0)
}

/// What this side answers, decided by which side onejudge is asking.
///
/// onejudge renders a distinct framing per operation, so the prompt itself says
/// which side this invocation is — there is no flag that does.
fn answer(prompt: &str) -> String {
    if prompt.contains("completion supervisor") {
        // The two shapes are not symmetrical, and onejudge enforces that: a
        // completed response carries a `reason` and *no* `message`, because
        // there is no next turn for a message to be. A continuing one carries
        // both.
        if prompt.contains("should-fail") {
            return json!({
                "completion": false,
                "message": "verify it before you call it done",
                "reason": "fake supervisor requires another turn",
            })
            .to_string();
        }
        return json!({"completion": true, "reason": "fake supervisor verified completion"})
            .to_string();
    }
    if prompt.contains("role-playing the USER") {
        return "verify it before you call it done".into();
    }
    if prompt.contains("Criterion:") {
        return json!({
            "value": !prompt.contains("should-fail"),
            "reason": "fake judge verdict",
        })
        .to_string();
    }
    if prompt.contains("Assessment request:") {
        return "None".into();
    }
    if prompt.contains("complete-now") {
        "done".into()
    } else {
        "working on it".into()
    }
}

/// The selection-shaped environment this invocation was spawned with.
///
/// Selected by name shape rather than a restated list, so a variable added to
/// either half of the seam is recorded without this file learning about it. This
/// process is the far end of the inheritance a per-side choice travels down —
/// the place the choice either arrived or did not.
fn selection_environment() -> String {
    let mut selection: Vec<(String, String)> = std::env::vars()
        .filter(|(name, _)| {
            name.ends_with("_HARNESSES")
                || name.ends_with("_MODEL")
                || name.ends_with("_MODE")
                || name.starts_with("ONEAGENTGRAPH_TEST_")
        })
        .collect();
    selection.sort();
    let map: serde_json::Map<String, Value> = selection
        .into_iter()
        .map(|(k, v)| (k, Value::String(v)))
        .collect();
    Value::Object(map).to_string()
}

/// The path one sentinel names, once it is one this process will act on.
///
/// Every path here arrives inside a prompt — text that reaches this process from
/// somewhere else — so all of them are checked the same way, whether they will be
/// written to or merely waited on: absolute, no parent reference, and a directory
/// that already exists. A journey names a file in its own temp directory, so a
/// sentinel that does not describe one is a mistake in the prompt rather than a
/// path to act on, and saying so on stderr is what turns it into a failure a
/// reader can diagnose instead of a journey that silently records nothing or
/// waits forever.
fn sentinel_path(prompt: &str, key: &str) -> Option<std::path::PathBuf> {
    let named = sentinel(prompt, &format!("{MARK}{key}="))?;
    let path = std::path::PathBuf::from(&named);
    let usable = path.is_absolute()
        && !path
            .components()
            .any(|part| part == std::path::Component::ParentDir)
        && path.parent().is_some_and(std::path::Path::is_dir);
    if !usable {
        eprintln!(
            "fake-harness: {MARK}{key} must name an absolute path in an existing directory, got \
             {named:?}"
        );
        return None;
    }
    Some(path)
}

/// Append one line to the path a sentinel named, when it named a usable one.
fn record(prompt: &str, key: &str, line: &str) {
    let Some(path) = sentinel_path(prompt, key) else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "{}", line.replace('\n', "\\n"));
    }
}

/// The value of one `key=value` sentinel in the prompt, up to the next space.
fn sentinel(prompt: &str, key: &str) -> Option<String> {
    let at = prompt.find(key)? + key.len();
    let rest = &prompt[at..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// One argv flag's value.
fn flag(argv: &[String], name: &str) -> Option<String> {
    argv.iter()
        .position(|arg| arg == name)
        .and_then(|at| argv.get(at + 1))
        .cloned()
}

/// Write one NDJSON line, flushed, because a streamed turn is only useful if its
/// events arrive while it is still running.
fn emit(value: &Value) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

/// Exit with a code `ExitCode` can carry.
fn exit(code: i32) -> std::process::ExitCode {
    std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
}
