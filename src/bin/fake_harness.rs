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
//! parked every turn of the suite. [`steers`] is the only way this file asks the
//! question, so the prefix is applied by construction rather than remembered.
//!
//! | sentinel / variable | what this turn does |
//! | --- | --- |
//! | `fake:complete-now` | the agent finishes on its first turn |
//! | `fake:should-fail` | the agent never finishes, so the run hits its turn cap |
//! | `fake:hold=<path>` | block until `<path>` exists — an observably in-flight turn |
//! | `fake:park` | *controlled turn only:* do nothing until an interrupt arrives |
//! | `fake:started=<path>` | *controlled turn only:* write `<path>` the moment this turn begins |
//! | `fake:did-work=<path>` | *controlled turn only:* append this turn's prompt once it finishes its work — never written by a turn an interrupt stopped |
//! | `FAKE_HARNESS_FAIL_AFTER_MARKER` | let an `exec`-shaped provider run once, then crash later launches |
//! | `FAKE_HARNESS_FAIL_ONCE_MARKER` | crash an `exec`-shaped provider once, then allow later launches |
//! | `fake:hang` | never answer at all, for the watchdogs |
//! | `fake:tick=<path>` | while hanging, append to `<path>` — a descendant's own proof it is still alive |
//! | `fake:spawn-ticker=<path>` | leave a **detached** ticker behind, which no cascade down the chain reaches |
//! | `fake:record-prompt=<path>` | append the exact prompt this side was given |
//! | `fake:record-env=<path>` | append this process's selection-shaped environment |
//! | `fake:record-argv=<path>` | append the argv this side was spawned with |
//! | `FAKE_HARNESS_REFUSAL=quota` | a zero-work 429 the chain steps past |
//! | `FAKE_HARNESS_REFUSAL=auth` | an unauthenticated refusal, on stderr alone |
//! | `FAKE_HARNESS_REFUSAL=rate_limit` | the refusal a chain does **not** step past |
//! | `FAKE_HARNESS_CRASH=<code>` | exit that code having published nothing |
//! | `FAKE_HARNESS_ATTEMPT_LOG=<path>` | append a line per launch, so a journey can count starts |
//! | `FAKE_HARNESS_UNAVAILABLE_ATTEMPTS=<n>` | the first `n` launches fail before the turn, the rest run |
//! | `FAKE_HARNESS_IGNORE_TERM=1` | refuse `SIGTERM`, so only a `SIGKILL` stops this turn |
//!
//! The three marked *controlled turn only* answer nowhere else, and that is
//! load-bearing rather than tidy: a judge side's prompt embeds the **transcript**,
//! so every sentinel the operator's task carried arrives there a second time. A
//! `fake:did-work` that fired on it would report work the agent never did, and a
//! `fake:park` would stall the judge waiting for an interrupt only the agent side
//! can be sent.
//!
//! Keep it deterministic and dependency-free beyond what the crate already
//! carries: it is spawned as a subprocess, many times per journey.
//!
//! # Why this is not `oneharness mock-harness`
//!
//! oneharness ships a deterministic responder for exactly this seam, and it is
//! the right thing to use the moment it can answer these journeys. Measured
//! against oneharness 0.6.15, it cannot, and the blocker is one property rather
//! than a list:
//!
//! **The `MOCK_*` contract fixes a response per process environment; a member
//! needs several from one binary.** Every side of every member reaches
//! `ONEHARNESS_BIN_CLAUDE_CODE`, and the graph's `env:` block is graph-wide, so
//! one `MOCK_STDOUT` is what all of them get. onejudge renders a distinct framing
//! per operation and requires a distinct *shape* back — a completion decision, a
//! simulated user turn, a boolean or numeric verdict — which is why [`answer`]
//! branches on the prompt. Driven for real, a two-party member on the responder
//! dies as `provider error (oneharness:supervisor): supervisor did not return a
//! JSON object; got: done` — the agent's answer handed to the judge. Nothing in
//! `MOCK_*` is conditioned on the prompt (`MOCK_FAIL_IF_MODEL` conditions on
//! argv; `MOCK_STDOUT_<n>` with `MOCK_ATTEMPT_FILE` conditions on a global,
//! non-atomic launch counter, so it would script this suite's *own* assumption
//! about onejudge's loop — the thing under test — and race between parallel
//! members).
//!
//! Two smaller findings, so nobody re-derives them:
//!
//! * **The bytes stay here either way.** `MOCK_STDOUT` *is* the response, supplied
//!   by the caller, so moving would relocate these documents into env-var string
//!   literals rather than delete them. What the responder owns is the process
//!   behaviour around them — streaming, the controlled turn, a native descendant.
//! * **Selection.** `run --mock-harness <ID>` is a flag on an invocation onejudge
//!   constructs, not this crate. The env route underneath it
//!   (`ONEHARNESS_INTERNAL_MOCK_HARNESS`) is checked before argv parsing, so
//!   setting it for the run turns the *outer* `oneharness run` into the responder
//!   as well — it works only through a wrapper that sets it for the child alone.
//!
//! What would close this: a prompt-conditional response in the `MOCK_*` contract
//! — a rules file mapping a prompt substring to the document to answer with, the
//! shape `oneharness mock --rules` already has for tool calls. That is an
//! upstream proposal, not a change to make from here.

// This binary's whole product IS its stdout and stderr: it stands in for a
// harness CLI, and a harness CLI answers on those two streams.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::{BufRead as _, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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

/// A validated request to fail `exec`-shaped launches after the first one.
struct FailAfter {
    marker: std::path::PathBuf,
}

/// A validated request to fail an `exec`-shaped launch only once.
struct FailOnce {
    marker: std::path::PathBuf,
}

impl FailOnce {
    fn from_env() -> Result<Option<Self>, ()> {
        let marker = std::env::var("FAKE_HARNESS_FAIL_ONCE_MARKER").ok();
        marker.map_or(Ok(None), |named| {
            named_path(&named, "FAKE_HARNESS_FAIL_ONCE_MARKER")
                .map(|marker| Some(Self { marker }))
                .ok_or(())
        })
    }
}

impl FailAfter {
    fn from_env() -> Result<Option<Self>, ()> {
        let marker = std::env::var("FAKE_HARNESS_FAIL_AFTER_MARKER").ok();
        marker.map_or(Ok(None), |named| {
            named_path(&named, "FAKE_HARNESS_FAIL_AFTER_MARKER")
                .map(|marker| Some(Self { marker }))
                .ok_or(())
        })
    }
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

/// Whether `prompt` carries `sentinel`, which is asked only ever through here.
///
/// One function rather than a `contains` per site, so the prefix is applied by
/// construction: a sentinel added later cannot arrive unprefixed by being read
/// the way the two dispositions below once were.
fn steers(prompt: &str, sentinel: &str) -> bool {
    prompt.contains(&format!("{MARK}{sentinel}"))
}

fn main() -> std::process::ExitCode {
    // `args_os`/`vars_os` rather than the Unicode-only iterators: both panic on
    // input this process did not choose, and a double that panics reports itself
    // as a crashed provider — a journey failing for a reason that is not the one
    // under test. Anything that is not text is dropped, which is what every
    // reader below expects.
    let argv: Vec<String> = std::env::args_os()
        .skip(1)
        .filter_map(|arg| arg.into_string().ok())
        .collect();
    // The turn arrives in two halves and both matter: the task is the `-p`
    // prompt, and the merged base-plus-persona instructions are the
    // `--append-system-prompt` claude-code takes a system prompt on. A double
    // that read only the first would report the agent side as having been given
    // no role at all.
    //
    // A **controlled** turn is the exception: `oneharness run --control` selects
    // claude-code's streamed input protocol, so there is no `-p` positional at
    // all — the prompt is the first frame on this process's stdin, and stdin
    // stays open so an `oneharness interrupt` can abort the turn through it.
    let system = flag(&argv, "--append-system-prompt")
        .or_else(|| flag(&argv, "--system"))
        .unwrap_or_default();
    let task = if controlled(&argv) {
        String::new()
    } else {
        flag(&argv, "-p")
            .or_else(|| flag(&argv, "--prompt"))
            .unwrap_or_default()
    };
    let prompt = format!("{system}\n{task}");

    // A controlled turn has none of the task yet — it arrives on stdin — so its
    // recordings are made per turn, in [`turn`], against the prompt that turn was
    // really given. Doing both would record the same launch twice.
    if !controlled(&argv) {
        recordings(&prompt, &argv);
    }

    // Before anything that could block: a turn asked to refuse `SIGTERM` has to
    // be refusing it by the time a journey can reach it.
    ignore_term();

    // Likewise before the turn blocks, because the whole point of this one is
    // that it is already running when the supervisor decides to end the tree.
    if let Some(path) = sentinel_path(&prompt, "spawn-ticker") {
        spawn_ticker(&path);
    }

    // Before every refusal branch, because what a journey counts here is
    // *launches*: a smoke that relaunches a failed start is asserting on the
    // attempts that failed as much as on the one that worked.
    if let Some(exit_code) = unavailable_launch() {
        return exit_code;
    }

    let fail_after = match FailAfter::from_env() {
        Ok(control) => control,
        Err(()) => return exit(2),
    };
    if let Some(control) = fail_after {
        if argv.iter().any(|arg| arg == "exec") {
            if control.marker.exists() {
                eprintln!("fake-harness: later launch for codex failed");
                return exit(1);
            }
            if std::fs::write(&control.marker, "first launch completed").is_err() {
                eprintln!(
                    "fake-harness: cannot write fail-after marker {}",
                    control.marker.display()
                );
                return exit(2);
            }
        }
    }
    let fail_once = match FailOnce::from_env() {
        Ok(control) => control,
        Err(()) => return exit(2),
    };
    if let Some(control) = fail_once {
        if argv.iter().any(|arg| arg == "exec")
            && std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&control.marker)
                .is_ok()
        {
            eprintln!("fake-harness: first launch for codex failed");
            return exit(1);
        }
    }

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

    if controlled(&argv) {
        return control_stream(&system, &argv);
    }

    if steers(&prompt, "hang") {
        // Never answer. The heartbeat and activity watchdogs are what ends this.
        return hang(sentinel_path(&prompt, "tick").as_deref());
    }
    turn(&prompt, None);
    exit(0)
}

/// Everything a journey can ask this launch to write down about itself.
///
/// One function for both paths, because what a journey asserts on must not depend
/// on whether the turn happened to be a controlled one: the prompt it was given,
/// the selection-shaped environment it inherited, and the argv a model reaches a
/// harness on.
fn recordings(prompt: &str, argv: &[String]) {
    record(prompt, "record-prompt", prompt);
    record(prompt, "record-env", &selection_environment());
    record(prompt, "record-argv", &argv.join(" "));
}

/// Whether `oneharness run --control` selected claude-code's streamed input
/// protocol for this turn.
///
/// The flag pair is the whole tell, and it is oneharness's own: a controlled turn
/// is `-p --input-format stream-json` with **no** positional prompt, so a double
/// that read `-p`'s next argument as the task would take `--input-format` for
/// one.
fn controlled(argv: &[String]) -> bool {
    flag(argv, "--input-format").as_deref() == Some("stream-json")
}

/// Serve turns off stdin until it closes, aborting the one in flight whenever a
/// control frame asks.
///
/// This is the mechanism `oneagentgraph interrupt` rides all the way down: a
/// `{"type":"user"}` frame opens a turn, a `{"type":"control_request"}` frame
/// with subtype `interrupt` ends the one running, and oneharness delivers the
/// operator's redirection as the *next* user frame the moment it sees this turn's
/// terminal `result`. Nothing here is a shortcut around that: the turn really
/// stops where it was, and the replacement really is a second turn.
fn control_stream(system: &str, argv: &[String]) -> std::process::ExitCode {
    let interrupted = Arc::new(AtomicBool::new(false));
    let mut running: Option<std::thread::JoinHandle<()>> = None;
    let argv = Arc::new(argv.to_vec());
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match frame.get("type").and_then(Value::as_str) {
            Some("user") => {
                // The text is what makes it a turn, so a frame carrying none is
                // one this process cannot act on — running it as an empty prompt
                // would answer a request nobody made, and every sentinel a
                // journey steers with would silently be absent.
                let Some(text) = frame
                    .pointer("/message/content/0/text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                else {
                    eprintln!("fake-harness: a user frame carried no message text: {line}");
                    continue;
                };
                let prompt = format!("{system}\n{text}");
                let flag = Arc::clone(&interrupted);
                let argv = Arc::clone(&argv);
                // On a thread of its own so this loop keeps reading: the frame
                // that aborts the turn arrives *while* it runs, which is the
                // whole point of the mechanism.
                running = std::thread::Builder::new()
                    .spawn(move || {
                        recordings(&prompt, &argv);
                        turn(&prompt, Some(&flag));
                    })
                    .ok();
            }
            // `interrupt` is the only subtype this double answers, and it is
            // checked rather than assumed: a control protocol can carry verbs
            // that do *not* end a turn, and acknowledging one of those as an
            // abort would report a turn as stopped that is still running.
            Some("control_request")
                if frame.pointer("/request/subtype").and_then(Value::as_str)
                    == Some("interrupt") =>
            {
                emit(&json!({
                    "type": "control_response",
                    "response": {"subtype": "success",
                                 "request_id": frame.get("request_id").cloned()},
                }));
                interrupted.store(true, Ordering::SeqCst);
            }
            Some("control_request") => {
                eprintln!("fake-harness: a control frame this double does not answer: {line}");
            }
            _ => {}
        }
    }
    // stdin closed, which is how oneharness says the run is over. The turn in
    // flight is still this process's to finish reporting on.
    if let Some(handle) = running {
        let _ = handle.join();
    }
    exit(0)
}

/// One turn: hold if the prompt asked it to, then answer.
///
/// `interrupted` is the control channel's flag, and it is what makes a held turn
/// *stoppable*: the hold ends the moment it is set, the turn reports a terminal
/// result having done none of its work, and the flag is cleared so the redirected
/// turn that follows is not aborted by the same request.
fn turn(prompt: &str, interrupted: Option<&AtomicBool>) {
    let session = std::env::var("FAKE_HARNESS_SESSION").unwrap_or_else(|_| "fake-session".into());
    // Written before any wait, so a journey can wait for a turn that is really
    // in flight rather than for a process that has merely started.
    if interrupted.is_some() {
        if let Some(path) = sentinel_path(prompt, "started") {
            let _ = std::fs::write(path, "started");
        }
    }
    let stopped = || interrupted.is_some_and(|flag| flag.load(Ordering::SeqCst));
    if let Some(path) = sentinel_path(prompt, "hold") {
        while !path.exists() && !stopped() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    // A turn with nothing to do but wait to be redirected. Bounded rather than
    // endless for the reason `hang`'s tick is: reaching the bound *is* a journey
    // failing, and a double left parked forever would outlive the suite that
    // failed to interrupt it.
    if interrupted.is_some() && steers(prompt, "park") {
        let deadline = std::time::Instant::now() + PARK_FOR;
        while std::time::Instant::now() < deadline && !stopped() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    emit(&json!({"type": "system", "subtype": "init", "session_id": session}));
    if stopped() {
        if let Some(flag) = interrupted {
            flag.store(false, Ordering::SeqCst);
        }
        emit(&json!({
            "type": "result", "subtype": "success", "is_error": false, "duration_ms": 1,
            "num_turns": 1, "result": "", "session_id": session,
            "usage": {"input_tokens": 1, "output_tokens": 0}, "total_cost_usd": 0.0,
        }));
        return;
    }
    // Past the wait and past the abort, so this line is the proof that *this*
    // turn did its work — a turn an interrupt stopped never reaches it.
    if interrupted.is_some() {
        record(prompt, "did-work", prompt);
    }
    emit(&json!({
        "type": "assistant", "session_id": session,
        "message": {"id": "m1", "type": "message", "role": "assistant", "content": [
            {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "just check"}}]},
    }));
    emit(&json!({
        "type": "result", "subtype": "success", "is_error": false, "duration_ms": 5,
        "num_turns": 1, "result": answer(prompt), "session_id": session,
        "usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 4,
                  "cache_creation_input_tokens": 1},
        "total_cost_usd": 0.002,
    }));
}

/// How often a hanging turn proves it is still alive.
const TICK_EVERY: std::time::Duration = std::time::Duration::from_millis(50);

/// How long a parked turn waits to be redirected before answering anyway.
///
/// Far longer than the journey that drives it needs, and bounded for the same
/// reason [`TICK_FOR`] is: a turn nothing ever interrupted is a journey failing,
/// and one that waited forever would leave a double running on a CI host after
/// the run that started it had gone.
const PARK_FOR: std::time::Duration = std::time::Duration::from_secs(180);

/// How long it keeps doing so before giving up and exiting.
///
/// Bounded rather than endless, and far longer than any journey looks for: a
/// turn that reaches this bound is one nothing reaped, and a leak guard that
/// outlived the run which caused it would go on writing on a CI host until the
/// host went away.
const TICK_FOR: std::time::Duration = std::time::Duration::from_secs(30);

/// Leave a ticker behind that outlives this process.
///
/// This is the descendant nothing *upstream* reaches. The chain a member is —
/// `onejudge`, then `oneharness`, then this — tears itself down when its head is
/// killed: each CLI ends the child it holds. A process whose parent has already
/// exited has nobody left to be ended by, so it is the one a supervisor that
/// kills only the process it holds leaks, and it is what a real harness leaving a
/// background worker behind looks like. Reaching it is the whole reason
/// membership is proven by something the kernel keeps — an environment stamp, or
/// a job object — rather than by walking the tree from the member's own pid.
///
/// Detached from this process's streams as well as its lifetime: a leaked
/// descendant holding a pipe open is a *different* failure, covered by the
/// cancel journeys, and one that would make this journey pass for the wrong
/// reason by hanging the run instead of leaving something running.
fn spawn_ticker(path: &std::path::Path) {
    let Ok(own) = std::env::current_exe() else {
        eprintln!("fake-harness: cannot find this binary to launch a ticker from");
        return;
    };
    let _ = std::process::Command::new(own)
        .arg("-p")
        .arg(format!("{MARK}hang {MARK}tick={}", path.display()))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Never answer, and — when a journey asked for it — leave a tick behind for as
/// long as this process is alive.
///
/// This is how a journey watches a *tree* die rather than a process. A
/// supervisor condemning a member says so in `member-died`, but the event is the
/// decision, not the outcome: what an operator is promised is that the
/// `oneharness` and paid provider underneath are no longer running. This process
/// is that descendant, and asking the platform which processes are still alive
/// is asking the very facility under test — so the descendant answers for
/// itself. The file grows while it lives and stops the instant it does not.
fn hang(tick: Option<&std::path::Path>) -> std::process::ExitCode {
    let Some(tick) = tick else {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    };
    let deadline = std::time::Instant::now() + TICK_FOR;
    while std::time::Instant::now() < deadline {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(tick)
        {
            let _ = writeln!(file, "tick");
        }
        std::thread::sleep(TICK_EVERY);
    }
    // llmlint: ignore-block[changed_behavior_has_e2e] no journey drives this arm
    // because reaching it *is* a journey failing: it runs only when nothing
    // reaped this process for thirty seconds, and the two journeys that use the
    // tick assert the teardown happened within one and a half. A test asserting
    // on this branch would be asserting that the reap did not work. What it is
    // for is the failing run — where it stops a leaked double from outliving the
    // suite that leaked it — and it was observed doing exactly that on the
    // throwaway branch that compiled the Windows layer out.
    eprintln!("fake-harness: a ticking turn outlived its bound, so nothing ever reaped it");
    exit(1)
    // llmlint: ignore-end[changed_behavior_has_e2e]
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
        if steers(prompt, "should-fail") {
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
            "value": !steers(prompt, "should-fail"),
            "reason": "fake judge verdict",
        })
        .to_string();
    }
    if prompt.contains("Assessment request:") {
        return "None".into();
    }
    if steers(prompt, "complete-now") {
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
    let mut selection: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
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
    named_path(&named, &format!("{MARK}{key}"))
}

/// One path this process was handed, once it is one it will act on.
///
/// The same bar wherever the path came from — a prompt sentinel or a variable —
/// because both reach this process from somewhere else: absolute, no parent
/// reference, and a directory that already exists.
fn named_path(named: &str, source: &str) -> Option<std::path::PathBuf> {
    let path = std::path::PathBuf::from(named);
    let usable = path.is_absolute()
        && !path
            .components()
            .any(|part| part == std::path::Component::ParentDir)
        && path.parent().is_some_and(std::path::Path::is_dir);
    if !usable {
        eprintln!(
            "fake-harness: {source} must name an absolute path in an existing directory, got \
             {named:?}"
        );
        return None;
    }
    Some(path)
}

/// Refuse `SIGTERM` when a journey asked this turn to, so that only a `SIGKILL`
/// can stop it.
///
/// This is the descendant a reap has to survive: asking politely is what a
/// process is free to decline, and a sweeper that only ever asks leaves the
/// declining ones running. The escalation past it is what this makes reachable.
#[cfg(unix)]
fn ignore_term() {
    if std::env::var("FAKE_HARNESS_IGNORE_TERM").is_ok_and(|value| value == "1") {
        // SAFETY: setting `SIGTERM` to `SIG_IGN` installs a disposition rather
        // than a handler, so no code runs in signal context and there is no
        // async-signal-safety obligation to meet.
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
        }
    }
}

/// Nothing to refuse: this platform's reap does not signal at all.
#[cfg(not(unix))]
fn ignore_term() {}

/// Record this launch, and fail it when a journey asked for the first `n` to
/// fail before the turn.
///
/// This is the shape a saturated host makes: the provider starts and exits
/// having published nothing, so oneharness records a non-zero result with no
/// tokens and no cost against it — which is exactly what tells a launch that
/// spent nothing apart from a turn somebody was billed for.
fn unavailable_launch() -> Option<std::process::ExitCode> {
    let launches = record_launch();
    let requested = std::env::var("FAKE_HARNESS_UNAVAILABLE_ATTEMPTS").unwrap_or_default();
    if requested.is_empty() {
        return None;
    }
    let Ok(unavailable) = requested.parse::<usize>() else {
        eprintln!(
            "fake-harness: FAKE_HARNESS_UNAVAILABLE_ATTEMPTS must be a count of launches, got \
             {requested:?}"
        );
        return Some(exit(2));
    };
    // Counting is what the refusal is decided on, so a journey that asked for
    // one without a log is asserting against a turn it never configured.
    let Some(launches) = launches else {
        eprintln!(
            "fake-harness: FAKE_HARNESS_UNAVAILABLE_ATTEMPTS needs a usable \
             FAKE_HARNESS_ATTEMPT_LOG to count launches against"
        );
        return Some(exit(2));
    };
    if launches > unavailable {
        return None;
    }
    eprintln!("fake-harness: the provider started and then failed (launch {launches})");
    Some(exit(1))
}

/// Append this launch to the log a journey named, and answer which launch it is.
fn record_launch() -> Option<usize> {
    let named = std::env::var("FAKE_HARNESS_ATTEMPT_LOG").ok()?;
    let path = named_path(&named, "FAKE_HARNESS_ATTEMPT_LOG")?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    writeln!(file, "launch").ok()?;
    drop(file);
    Some(std::fs::read_to_string(&path).ok()?.lines().count())
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
