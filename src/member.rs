//! Running one member, and turning what it publishes into envelopes.
//!
//! A **single-sided** member is one child process, `oneharness run --stream`, and
//! this module is the whole of running it. A **two-party** member is onejudge's
//! own run driver called in this process; [`crate::judge`] runs that one, and
//! shares the settle, the death, and the two watchdogs below so both kinds reach
//! the stream as the same events.
//!
//! A child member speaks two NDJSON envelopes on stdout (onejudge's
//! `docs/streaming.md`):
//!
//! ```text
//! {"type":"event","turn":1,"event":{"kind":"tool_call","name":"bash",…}}
//! {"type":"result","report":{…}}
//! ```
//!
//! Each `event` line becomes a [`EventKind::TurnActivity`]; the first of a turn
//! is preceded by a [`EventKind::TurnStarted`]; the terminal `result` line
//! becomes a [`EventKind::TurnCompleted`] and a [`EventKind::MemberSettled`].
//!
//! Two watchdogs run alongside, both ported from ai-orchestrator with their
//! defaults and their environment overrides intact:
//!
//! * The **heartbeat** is refreshed by this supervisor every
//!   [`HEARTBEAT_INTERVAL`] while the member is alive. Its deadline is therefore
//!   not a latency budget — it is the margin by which a *live* member may be
//!   starved of CPU before it is declared dead. This crate runs many members at
//!   once, and a threshold near the write cadence reaps healthy ones under
//!   exactly the load it creates.
//! * The **activity watchdog** is the slow-stall backstop: a member that
//!   published nothing for [`crate::liveness::DEFAULT_STALL_TIMEOUT`] is not
//!   working, whatever its process tree looks like.
//!
//! Either firing is a [`EventKind::MemberDied`], carrying the `rule` that fired,
//! the classified `cause`, and a bounded `detail` — plus, for a member that was a
//! child process, that process's `exit_code`, `disposition`, and `stderr_tail`.
//! The classification is the point: provider throttling, quota exhaustion, an OOM
//! kill, and a genuine crash otherwise all reach a supervisor as the same dead
//! member.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::event::{
    bound_detail, bound_text, Cause, Disposition, Emitter, EventKind, Labels, MemberDied,
};
use crate::invoke::{Invocation, Launch};
use crate::liveness::{
    DEFAULT_HEARTBEAT_TIMEOUT, DEFAULT_STALL_TIMEOUT, HEARTBEAT_TIMEOUT_ENV, STALL_TIMEOUT_ENV,
};
use crate::scratch::Group;

/// How often this supervisor refreshes a live member's heartbeat.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(500);

/// How much of a dead member's stderr is kept as evidence, before the payload's
/// own [`crate::event::MAX_PAYLOAD_TEXT_BYTES`] bound is applied to the tail.
const STDERR_KEEP_BYTES: usize = 64 * 1024;

/// Which program a member is, because the two read their exit codes differently.
///
/// This is not a detail: `onejudge` exits `1` for a task it drove but did not
/// complete — the member's own verdict, and a settle. `oneharness` exits
/// non-zero when it could not run the turn at all, which is a death. Reading one
/// by the other's contract turns a chain that reached nothing into a member that
/// settled incomplete, and that is the failure a supervisor most needs told
/// apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `onejudge run`: `0` completed, `1` incomplete, `2` a config or provider
    /// failure.
    Onejudge,
    /// `oneharness run`: `0` ran the turn, anything else did not.
    Oneharness,
}

impl Kind {
    /// Whether `code` is a verdict this program's member settled on, rather than
    /// a failure to run at all.
    #[must_use]
    pub fn settled(self, code: i32) -> bool {
        match self {
            Kind::Onejudge => code == 0 || code == 1,
            Kind::Oneharness => code == 0,
        }
    }
}

/// The liveness rules a member can die by.
///
/// A closed set, because `member-died`'s `rule` is what a supervisor branches
/// on: provider throttling, an OOM kill, a watchdog, and a harness that never
/// started otherwise all reach it as the same dead process, and a rule spelled
/// two ways is a branch that silently stops matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rule {
    /// The member could not be started at all.
    Unstartable,
    /// The member was terminated by a signal.
    Signalled,
    /// The member exited without a report it could settle on.
    ProviderFailure,
    /// This supervisor could not confirm the member alive inside its deadline.
    Heartbeat,
    /// The member published nothing for the whole stall bound.
    Activity,
}

impl Rule {
    /// The rule a name spells, or `None` for one this build does not know.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        [
            Rule::Unstartable,
            Rule::Signalled,
            Rule::ProviderFailure,
            Rule::Heartbeat,
            Rule::Activity,
        ]
        .into_iter()
        .find(|rule| rule.as_str() == name)
    }

    /// The rule's name on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Rule::Unstartable => "unstartable",
            Rule::Signalled => "signalled",
            Rule::ProviderFailure => "provider-failure",
            Rule::Heartbeat => "heartbeat",
            Rule::Activity => "activity",
        }
    }
}

/// What became of one member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The member settled having reached its completion bar — onejudge's own
    /// verdict, its exit 0.
    Settled,
    /// The member settled without reaching it — onejudge's exit 1. A settle
    /// either way, and the graph's exit 1 either way, but not the same outcome:
    /// [`crate::run::MemberOutcome`] spells the two differently in the record,
    /// and a single variant carrying a flag is how one gets read as the other.
    Incomplete,
    /// The member died. The payload says which rule fired and what the process
    /// left behind.
    Died(Death),
    /// The member could not be started at all.
    Unstartable(String),
}

impl Outcome {
    /// Whether this outcome lets the graph exit `0`.
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, Outcome::Settled)
    }
}

/// One member's death: the rule that found it, and what the process left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Death {
    /// The liveness rule that fired.
    pub rule: Rule,
    /// The payload the stream carried.
    pub payload: MemberDied,
}

/// The liveness bounds one run supervises its members under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    /// How long a member may go without a heartbeat before it is declared dead.
    pub heartbeat: Duration,
    /// How long a member may publish nothing before the activity watchdog fires.
    pub stall: Duration,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            heartbeat: DEFAULT_HEARTBEAT_TIMEOUT,
            stall: DEFAULT_STALL_TIMEOUT,
        }
    }
}

impl Bounds {
    /// The bounds `env` asks for, falling back to the contract's defaults.
    ///
    /// # Errors
    ///
    /// The variable's name and the value it carried, when that value is not a
    /// positive number of seconds. A member supervised under a bound nobody meant
    /// is worse than a run that refuses to start.
    pub fn from_env(env: &BTreeMap<String, String>) -> Result<Self, String> {
        Ok(Self {
            heartbeat: seconds(env, HEARTBEAT_TIMEOUT_ENV, DEFAULT_HEARTBEAT_TIMEOUT)?,
            stall: seconds(env, STALL_TIMEOUT_ENV, DEFAULT_STALL_TIMEOUT)?,
        })
    }
}

/// One duration read out of the environment.
fn seconds(
    env: &BTreeMap<String, String>,
    name: &str,
    fallback: Duration,
) -> Result<Duration, String> {
    let Some(raw) = env.get(name) else {
        return Ok(fallback);
    };
    match raw.parse::<f64>() {
        Ok(value) if value.is_finite() && value > 0.0 => Ok(Duration::from_secs_f64(value)),
        _ => Err(format!(
            "{name} must be a positive number of seconds, got {raw:?}"
        )),
    }
}

/// Run one member to its end, publishing every envelope it produces.
///
/// `emitter` is already labelled for this member. `env` is the whole environment
/// the child is launched with, over the inherited one.
#[must_use]
pub fn run(
    invocation: &Invocation,
    emitter: &Emitter,
    env: &BTreeMap<String, String>,
    bounds: Bounds,
    scratch: &Path,
) -> Outcome {
    match &invocation.launch {
        Launch::Library(judge) => crate::judge::run(judge, emitter, bounds, scratch),
        Launch::Process { program, args, cwd } => spawned(
            invocation.kind,
            program,
            args,
            cwd,
            &invocation.env,
            emitter,
            env,
            bounds,
            scratch,
        ),
    }
}

/// Run a member that is a child process of its own.
// Nine values, none derivable from another: which contract it settles under,
// the three parts of the command, what this member adds to the environment,
// where its events go, the graph's environment and bounds, and its scratch.
// Bundling them into a struct would name the same values twice.
#[allow(clippy::too_many_arguments)]
fn spawned(
    kind: Kind,
    program: &str,
    args: &[String],
    cwd: &Path,
    member_env: &[(String, String)],
    emitter: &Emitter,
    env: &BTreeMap<String, String>,
    bounds: Bounds,
    scratch: &Path,
) -> Outcome {
    emitter.emit(
        EventKind::MemberStarted,
        payload([
            ("runner", Value::String("process".into())),
            ("program", Value::String(program.to_string())),
            (
                "args",
                Value::Array(args.iter().cloned().map(Value::String).collect()),
            ),
            ("cwd", Value::String(cwd.display().to_string())),
        ]),
    );

    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        // Dropping it from `env` is not enough: a child *inherits* this
        // process's environment, and `envs` only adds to it. So the one variable
        // that beats the per-side config a graph named has to be unset here, on
        // the command itself — and re-added by `envs` below when the graph's own
        // block asked for it. See `invoke::PROCESS_WIDE_HARNESS_ENV`.
        .env_remove(crate::invoke::PROCESS_WIDE_HARNESS_ENV)
        .envs(env)
        .envs(member_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        // The ownership stamp is deliberately *not* set here: `Group::spawn`
        // applies it as it spawns, so the one place a command joins a group is
        // the group itself — which is also the only way the commands this
        // process does not spawn, the ones onejudge starts for a two-party
        // member, could be stamped the same way.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // The group is opened before the spawn because it is what the spawn goes
    // *into*: a member is `onejudge`, which starts `oneharness`, which starts the
    // paid provider, and the child this supervisor holds is only the first of
    // the three. Everything a cancel or a watchdog has to reach is reached
    // through the group, not through the child.
    //
    // llmlint: ignore-block[changed_behavior_has_e2e] this arm has no journey
    // because no input a user can give reaches it. Opening a group is a no-op on
    // POSIX — the stamp is applied by the `Command` below — so the arm cannot be
    // taken there at all, and on Windows it is taken only when the kernel
    // refuses a job object or the scratch this run created moments earlier has
    // become unwritable underneath it. Both are host failures, not requests, and
    // the journeys that *can* be driven — grouping, cancel, the killed launcher,
    // the forged record — are in tests/e2e/liveness.rs. What matters about this
    // arm is the direction it fails in, and that is decided here rather than
    // observed: a member that could not be grouped is a member no cancel could
    // ever reach, so it is refused rather than started.
    let group = match Group::open(scratch) {
        Ok(group) => group,
        Err(err) => {
            let reason = err.to_string();
            emitter.emit(EventKind::MemberDied, died_payload(&unstartable(&reason)));
            return Outcome::Unstartable(reason);
        }
    };
    // llmlint: ignore-end[changed_behavior_has_e2e]
    let child = match group.spawn(&mut command) {
        Ok(child) => child,
        Err(err) => {
            let reason = format!("cannot start {program}: {err}");
            // No process ever existed, so it has no exit code, no disposition, and
            // no standard error — only the reason it could not be started, which is
            // exactly what `spawn` classifies.
            emitter.emit(EventKind::MemberDied, died_payload(&unstartable(&reason)));
            return Outcome::Unstartable(reason);
        }
    };
    supervise(child, &group, kind, emitter, bounds, scratch)
}

/// The death of a member that could not be started at all.
pub(crate) fn unstartable(reason: &str) -> MemberDied {
    let (detail, truncated) = bound_text(reason);
    MemberDied {
        rule: Rule::Unstartable.as_str().into(),
        cause: Cause::Spawn,
        detail,
        truncated,
        exit_code: None,
        disposition: None,
        stderr_tail: None,
    }
}

/// Supervise a spawned member: read what it publishes, watch it live, and settle.
fn supervise(
    mut child: Child,
    group: &Group,
    kind: Kind,
    emitter: &Emitter,
    bounds: Bounds,
    scratch: &Path,
) -> Outcome {
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");

    let activity = Arc::new(AtomicU64::new(0));
    let report: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let tail = Arc::new(Mutex::new(String::new()));
    let started = Instant::now();

    let reader = {
        let (emitter, activity, report) =
            (emitter.clone(), Arc::clone(&activity), Arc::clone(&report));
        std::thread::spawn(move || {
            let mut turn = 0u64;
            for line in readable(BufReader::new(stdout)) {
                activity.store(elapsed_millis(started), Ordering::SeqCst);
                ingest(&line, &emitter, &mut turn, &report);
            }
        })
    };
    let stderr_reader = {
        let tail = Arc::clone(&tail);
        std::thread::spawn(move || {
            for line in readable(BufReader::new(stderr)) {
                let mut kept = held(&tail);
                kept.push_str(&line);
                kept.push('\n');
                if kept.len() > STDERR_KEEP_BYTES {
                    let cut = kept.len() - STDERR_KEEP_BYTES;
                    let cut = (cut..kept.len())
                        .find(|at| kept.is_char_boundary(*at))
                        .unwrap_or(0);
                    *kept = kept[cut..].to_string();
                }
            }
        })
    };

    let heartbeat_file = scratch.join("member.heartbeat");
    let mut last_heartbeat = Instant::now();
    // The heartbeat is *refreshed* every [`HEARTBEAT_INTERVAL`], because that
    // cadence is what makes the deadline a starvation margin rather than a
    // latency budget. It is *published* far more rarely: a consumer watching a
    // 600-second turn wants to know the member is alive, not to read two
    // envelopes a second, and a stream that is mostly heartbeats buries the
    // events it exists to carry. A quarter of the deadline is frequent enough
    // that a reader notices the silence before the watchdog does.
    let publish_every = (bounds.heartbeat / 4).max(HEARTBEAT_INTERVAL * 2);
    let mut published = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(err) => break Err(format!("cannot wait for the member: {err}")),
        }
        std::thread::sleep(HEARTBEAT_INTERVAL);
        let _ = std::fs::write(&heartbeat_file, elapsed_millis(started).to_string());
        let now = Instant::now();
        if now.duration_since(last_heartbeat) > bounds.heartbeat {
            return kill_and_report(
                &mut child,
                group,
                emitter,
                Rule::Heartbeat,
                &tail,
                reader,
                stderr_reader,
            );
        }
        last_heartbeat = now;
        if now.duration_since(published) >= publish_every {
            published = now;
            emitter.emit(EventKind::MemberHeartbeat, payload([]));
        }
        let quiet = elapsed_millis(started).saturating_sub(activity.load(Ordering::SeqCst));
        if Duration::from_millis(quiet) > bounds.stall {
            return kill_and_report(
                &mut child,
                group,
                emitter,
                Rule::Activity,
                &tail,
                reader,
                stderr_reader,
            );
        }
    };

    let _ = reader.join();
    let _ = stderr_reader.join();
    let settled = held(&report).take();

    match status {
        Ok(status) => settle(emitter, kind, status, settled, &tail, scratch),
        Err(reason) => Outcome::Unstartable(reason),
    }
}

/// Milliseconds since the member started, as the watchdogs count them.
fn elapsed_millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Turn one published line into envelopes.
fn ingest(line: &str, emitter: &Emitter, turn: &mut u64, report: &Arc<Mutex<Option<Value>>>) {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return;
    };
    match value.get("type").and_then(Value::as_str) {
        Some("event") => {
            // A trust boundary: this is a child process's stdout. An `event`
            // line with no tool event in it is not an event, and publishing a
            // `turn-activity` invented from its absence would report the member
            // as having done something it never did. Skip it instead — the
            // stream is a view, and the turn is still read to its end.
            let Some(event) = value.get("event").filter(|event| event.is_object()) else {
                return;
            };
            let Some(name) = event.get("name").and_then(Value::as_str) else {
                return;
            };
            let Some(kind) = event.get("kind").and_then(Value::as_str) else {
                return;
            };
            // A turn index this build cannot read is one it does not renumber
            // on: the events still publish, under the turn already open.
            let observed = value
                .get("turn")
                .and_then(Value::as_u64)
                .unwrap_or((*turn).max(1));
            if observed != *turn {
                *turn = observed;
                emitter.emit(
                    EventKind::TurnStarted,
                    payload([("turn", Value::from(observed))]),
                );
            }
            let (detail, truncated) = bound_detail(&summarize(event.get("input")));
            emitter.emit(
                EventKind::TurnActivity,
                payload([
                    ("kind", Value::String(kind.into())),
                    ("name", Value::String(name.into())),
                    ("detail", Value::String(detail)),
                    ("truncated", Value::Bool(truncated)),
                ]),
            );
        }
        Some("result") => {
            // Only a report *document* counts. Everything downstream reads this
            // as a mapping — the verdict, the usage, the fallback chain — and a
            // `result` carrying anything else is a member that produced nothing
            // it can settle on, which is the failure already spelled below
            // rather than a settle on a document with no fields in it.
            if let Some(document) = value.get("report").filter(|value| value.is_object()) {
                *held(report) = Some(document.clone());
            }
        }
        _ => {}
    }
}

/// What a member's shared state holds, whether or not a reader thread died
/// holding it.
///
/// These mutexes guard a member's stderr tail and its report, and the threads
/// sharing them are this file's own readers. A poisoned lock means one of those
/// panicked — and the evidence behind the lock is exactly what a supervisor
/// needs at that moment to say what became of the member. Panicking here in turn
/// would take the whole run down over a member that merely failed, and lose the
/// stderr tail that says why.
fn held<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Every line a child stream yields, past the ones that are not text.
///
/// A member's stdout is another process's output, and the two ways reading it
/// fails are not the same failure. A line that is not UTF-8 is *this* line being
/// unreadable: skipping it and carrying on is what the rest of the reader
/// already does with a line it cannot model, and stopping there would drop the
/// `result` report that follows and condemn a healthy member for producing no
/// report. A genuine read failure is the stream itself ending, and is where the
/// loop stops — `map_while` treated both as the second.
fn readable(stream: impl BufRead) -> impl Iterator<Item = String> {
    let mut lines = stream.lines();
    std::iter::from_fn(move || loop {
        match lines.next() {
            Some(Ok(line)) => return Some(line),
            Some(Err(err)) if err.kind() == std::io::ErrorKind::InvalidData => continue,
            Some(Err(_)) | None => return None,
        }
    })
}

/// One tool event's structured input as the contract's bounded summary: what it
/// acted on.
///
/// Takes the input itself rather than the event around it, because the two kinds
/// of member reach it differently — a child's event is a JSON document this crate
/// parsed, and an in-process one is a typed `ToolEvent` whose `input` is already
/// in hand — and the summary must be the same either way.
pub(crate) fn summarize(input: Option<&Value>) -> String {
    match input {
        Some(Value::Object(fields)) => fields
            .values()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        Some(Value::String(text)) => text.clone(),
        _ => String::new(),
    }
}

/// Settle a member that exited on its own.
fn settle(
    emitter: &Emitter,
    kind: Kind,
    status: std::process::ExitStatus,
    report: Option<Value>,
    tail: &Arc<Mutex<String>>,
    scratch: &Path,
) -> Outcome {
    // A signalled member never chose to stop, whatever it managed to publish
    // first — that is a death, and the disposition is what distinguishes it from
    // an exit status the member itself returned.
    if disposition(status) == Disposition::Signaled {
        return died(emitter, Rule::Signalled, status, tail);
    }
    // A chain names every candidate it stepped past, and it does so whether or
    // not a later one ran. Those records are evidence either way — which
    // subscription to restore is exactly what an operator needs from a member
    // that reached nothing — so they are published before the verdict, not
    // conditionally on it.
    for advance in fallback_advances(report.as_ref()) {
        emitter.emit(EventKind::FallbackAdvanced, advance);
    }
    let code = status.code().unwrap_or(-1);
    if report.is_none() || !kind.settled(code) {
        return died(emitter, Rule::ProviderFailure, status, tail);
    }
    settle_report(emitter, &report.unwrap_or(Value::Null), code == 0, scratch)
}

/// The file a member's full report is stored as, which its `member-settled`
/// artifact names.
pub const REPORT_FILE: &str = "report.json";

/// Publish one member's settle, whichever kind of member produced the report.
///
/// The two kinds reach this with the same document — a child's terminal `result`
/// line, or onejudge's own `Report` serialized — so the payload a consumer reads
/// does not depend on how the member was run.
///
/// The report is *stored* here, in the member's own scratch, and its path is on
/// the payload. The contract has always called it "an artifact, referenced by id,
/// fetched via that library's CLI", and until it was written down there was
/// nothing behind the id to fetch: a `bytes` count of a document that had already
/// been dropped.
pub(crate) fn settle_report(
    emitter: &Emitter,
    document: &Value,
    completed: bool,
    scratch: &Path,
) -> Outcome {
    let rendered = serde_json::to_string(document).unwrap_or_default();
    let path = scratch.join(REPORT_FILE);
    // Best-effort: a member that settled is settled, and losing the stored copy
    // of its report is not a reason to report it as anything else. The payload
    // says where it went either way, which is what an operator needs to look.
    let stored = std::fs::write(&path, &rendered).is_ok();
    emitter.emit_with(
        EventKind::TurnCompleted,
        payload([(
            "usage",
            document.get("usage").cloned().unwrap_or(Value::Null),
        )]),
        Vec::new(),
    );
    emitter.emit_with(
        EventKind::MemberSettled,
        payload([
            ("completed", Value::Bool(completed)),
            (
                "verdict",
                document
                    .get("verdicts")
                    .cloned()
                    .unwrap_or(Value::Array(Vec::new())),
            ),
            (
                "completion_reason",
                document
                    .get("completion_reason")
                    .cloned()
                    .unwrap_or(Value::Null),
            ),
            (
                "report_path",
                if stored {
                    Value::String(path.display().to_string())
                } else {
                    Value::Null
                },
            ),
        ]),
        vec![crate::event::Artifact {
            id: format!("report-{}", emitter.stream()),
            kind: "report".into(),
            bytes: rendered.len() as u64,
        }],
    );
    if completed {
        Outcome::Settled
    } else {
        Outcome::Incomplete
    }
}

/// Every candidate a fallback chain stepped past, as the contract's event.
///
/// A two-party member's report carries no chain of its own, so this is the
/// single-sided shape: oneharness's own `fallback.fell_through`, which names the
/// identity and its classified reason.
/// The same trust boundary as the stream reader above: this document is a child
/// process's stdout, and the contract's payload is an identity and a classified
/// reason — both text a consumer displays and joins on. A candidate carrying
/// something else for either is one this build cannot read, and publishing it
/// with a `null` identity would report a chain as having stepped past a harness
/// nobody can name. It is dropped instead, exactly as an unreadable tool event
/// is, and the candidates around it still publish.
fn fallback_advances(report: Option<&Value>) -> Vec<Map<String, Value>> {
    report
        .and_then(|report| report.get("fallback"))
        .and_then(|fallback| fallback.get("fell_through"))
        .and_then(Value::as_array)
        .map(|candidates| {
            candidates
                .iter()
                // Read into oneharness's **own** declaration of a fallen-through
                // candidate rather than into two `get`s by field name, so the
                // shape this crate expects is the shape oneharness publishes.
                // Per candidate, not per chain: a document that fails to parse
                // as a whole would take the readable candidates beside it down.
                .filter_map(|candidate| {
                    serde_json::from_value::<oneharness_core::domain::report::FallThrough>(
                        candidate.clone(),
                    )
                    .ok()
                })
                .map(|candidate| {
                    payload([
                        ("identity", Value::String(candidate.harness)),
                        ("reason", Value::String(candidate.reason)),
                    ])
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Report a member that died, and say which rule found it.
fn died(
    emitter: &Emitter,
    rule: Rule,
    status: std::process::ExitStatus,
    tail: &Arc<Mutex<String>>,
) -> Outcome {
    let payload = process_died(rule, status.code(), disposition(status), &held(tail));
    emitter.emit(EventKind::MemberDied, died_payload(&payload));
    Outcome::Died(Death { rule, payload })
}

/// The death of a member that was a child process.
///
/// `cause` is that process's own disposition, because an exit status and a signal
/// are what a process failure *is*: the classified provider kinds belong to a
/// member whose engine returned a typed error, and inventing one from an exit
/// code would be this crate guessing at a classification oneharness already owns.
fn process_died(
    rule: Rule,
    exit_code: Option<i32>,
    disposition: Disposition,
    tail: &str,
) -> MemberDied {
    let (detail, truncated) = bound_text(tail.trim());
    MemberDied {
        rule: rule.as_str().to_string(),
        cause: Cause::from(disposition),
        detail: detail.clone(),
        truncated,
        exit_code,
        disposition: Some(disposition),
        stderr_tail: Some(detail),
    }
}

/// Kill a member a watchdog condemned, then report it.
fn kill_and_report(
    child: &mut Child,
    group: &Group,
    emitter: &Emitter,
    rule: Rule,
    tail: &Arc<Mutex<String>>,
    reader: std::thread::JoinHandle<()>,
    stderr_reader: std::thread::JoinHandle<()>,
) -> Outcome {
    // Whether the member was still running when the watchdog reached it, asked
    // *before* the kill because the exit status afterwards cannot answer it.
    // Windows has no signal disposition at all, so a process this supervisor
    // terminated reports an ordinary exit code there; and on either platform a
    // member that finished a moment before the kill landed reports one too. What
    // `member-died` is for is which of those happened, so it is recorded rather
    // than inferred from a status that spells both the same way.
    let settled_first = child.try_wait().ok().flatten();
    // The whole tree, and the child only after it. A condemned member is
    // `onejudge` with `oneharness` and a paid provider still running under it,
    // and those two are the ones holding the pipes the two readers below are
    // waiting on — killing the child alone leaves this supervisor blocked on a
    // stream a process it just condemned is still keeping open, which is the
    // condemnation never being reported at all.
    let _ = group.terminate();
    let _ = child.kill();
    let status = child.wait().ok();
    let _ = reader.join();
    let _ = stderr_reader.join();
    let payload = process_died(
        rule,
        status.and_then(|status| status.code()),
        settled_first.map_or(Disposition::Signaled, disposition),
        &held(tail),
    );
    emitter.emit(EventKind::MemberDied, died_payload(&payload));
    Outcome::Died(Death { rule, payload })
}

/// How a process ended.
fn disposition(status: std::process::ExitStatus) -> Disposition {
    if status.code().is_some() {
        Disposition::Exited
    } else {
        Disposition::Signaled
    }
}

/// A `member-died` payload as the wire carries it.
pub(crate) fn died_payload(died: &MemberDied) -> Map<String, Value> {
    match serde_json::to_value(died) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// One payload, spelled as the field list it is.
pub(crate) fn payload<const N: usize>(fields: [(&str, Value); N]) -> Map<String, Value> {
    fields
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

/// The labels a member's own emitter carries.
#[must_use]
pub fn labels(run_id: &str, member: &str, persona: Option<&str>) -> Labels {
    Labels {
        run_id: Some(run_id.to_string()),
        member: Some(member.to_string()),
        persona: persona.map(str::to_string),
        ..Labels::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    /// The contract's own defaults, and the two variables that move them.
    #[test]
    fn the_bounds_are_the_contract_s_defaults_until_the_environment_moves_them() {
        assert_eq!(Bounds::from_env(&env(&[])), Ok(Bounds::default()));
        assert_eq!(Bounds::default().heartbeat, Duration::from_secs(60));
        assert_eq!(Bounds::default().stall, Duration::from_secs(600));

        let moved = Bounds::from_env(&env(&[
            (HEARTBEAT_TIMEOUT_ENV, "1.5"),
            (STALL_TIMEOUT_ENV, "30"),
        ]))
        .expect("both parse");
        assert_eq!(moved.heartbeat, Duration::from_millis(1500));
        assert_eq!(moved.stall, Duration::from_secs(30));
    }

    /// A bound nobody meant refuses the run rather than supervising under it.
    #[test]
    fn an_unusable_bound_is_refused_by_name() {
        for bad in ["0", "-1", "nan", "inf", "soon"] {
            let err = Bounds::from_env(&env(&[(STALL_TIMEOUT_ENV, bad)])).unwrap_err();
            assert!(err.starts_with(STALL_TIMEOUT_ENV), "{bad}: {err}");
            assert!(err.contains("positive number of seconds"), "{bad}: {err}");
        }
    }

    /// A tool event's summary is what it acted on, whatever shape the input took.
    #[test]
    fn a_tool_event_summarizes_what_it_acted_on() {
        let event = serde_json::json!({"kind": "tool_call", "name": "bash",
                                       "input": {"command": "just check", "n": 3}});
        assert_eq!(summarize(event.get("input")), "just check");
        assert_eq!(summarize(Some(&serde_json::json!("raw"))), "raw");
        assert_eq!(summarize(Some(&serde_json::json!(7))), "");
        assert_eq!(summarize(None), "");
    }

    /// A published line this crate cannot model is skipped rather than crashing
    /// the reader: the stream is a view, and a member that wrote one odd line is
    /// still a member whose turn has to be read to its end.
    #[test]
    fn a_line_the_reader_cannot_model_is_skipped_rather_than_fatal() {
        let recorder = crate::event::Emitter::new("s", Box::new(std::io::sink()));
        let report = Arc::new(Mutex::new(None));
        let mut turn = 0;
        for line in [
            "not json at all",
            "[]",
            "{\"type\":\"unknown\"}",
            "{\"type\":\"result\"}",
            // An `event` line carrying nothing this build can model: no event,
            // an event that is not an object, and one missing either half of the
            // summary a `turn-activity` is.
            "{\"type\":\"event\"}",
            "{\"type\":\"event\",\"event\":7}",
            "{\"type\":\"event\",\"event\":{\"kind\":\"tool_call\"}}",
            "{\"type\":\"event\",\"event\":{\"name\":\"bash\"}}",
            // A `result` whose report is not a document. Everything downstream
            // reads it as a mapping, so accepting one would settle the member on
            // a report with no fields rather than failing for the absence.
            "{\"type\":\"result\",\"report\":\"done\"}",
            "{\"type\":\"result\",\"report\":7}",
            "{\"type\":\"result\",\"report\":null}",
            "{\"type\":\"result\",\"report\":[]}",
        ] {
            ingest(line, &recorder, &mut turn, &report);
        }
        assert_eq!(turn, 0, "a line the reader cannot model started a turn");
        assert!(held(&report).is_none());

        ingest(
            "{\"type\":\"result\",\"report\":{\"usage\":{}}}",
            &recorder,
            &mut turn,
            &report,
        );
        assert!(held(&report).is_some());
    }

    /// A child member's death reaches the wire as every field the contract names
    /// for one, with the truncation flag omitted when nothing was cut.
    #[test]
    fn a_child_member_s_death_reaches_the_wire_as_its_documented_fields() {
        let payload = died_payload(&process_died(
            Rule::Activity,
            Some(1),
            Disposition::Exited,
            "  harness failed (quota)\n",
        ));
        assert_eq!(payload["rule"], Value::from("activity"));
        assert_eq!(payload["cause"], Value::from("exited"));
        assert_eq!(payload["detail"], Value::from("harness failed (quota)"));
        assert_eq!(payload["exit_code"], Value::from(1));
        assert_eq!(payload["disposition"], Value::from("exited"));
        assert_eq!(
            payload["stderr_tail"],
            Value::from("harness failed (quota)")
        );
        assert!(!payload.contains_key("truncated"));

        // A signalled one classifies as that, so a consumer branches on `cause`
        // whichever kind of member produced the death.
        let signalled = died_payload(&process_died(
            Rule::Signalled,
            None,
            Disposition::Signaled,
            "",
        ));
        assert_eq!(signalled["cause"], Value::from("signaled"));
        assert!(!signalled.contains_key("exit_code"));
    }

    /// A member that never started has none of a process's three facts — there
    /// was no process — and says so as `spawn`, with the reason as its detail.
    #[test]
    fn a_member_that_never_started_carries_a_typed_cause_and_no_process_facts() {
        let payload = died_payload(&unstartable("cannot start oneharness: No such file"));
        assert_eq!(payload["rule"], Value::from("unstartable"));
        assert_eq!(payload["cause"], Value::from("spawn"));
        assert!(payload["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("No such file")));
        for absent in ["exit_code", "disposition", "stderr_tail"] {
            assert!(!payload.contains_key(absent), "{absent}: {payload:?}");
        }
    }

    /// Only oneharness's own classification becomes a `fallback-advanced`; a
    /// report with no chain produces none rather than an invented one.
    #[test]
    fn fallback_advances_come_from_the_chain_s_own_record() {
        let report = serde_json::json!({
            "fallback": {"ran": "codex", "fell_through": [{"harness": "claude-code", "reason": "auth"}]}
        });
        let advances = fallback_advances(Some(&report));
        assert_eq!(advances.len(), 1);
        assert_eq!(advances[0]["identity"], Value::from("claude-code"));
        assert_eq!(advances[0]["reason"], Value::from("auth"));

        assert!(fallback_advances(None).is_empty());
        assert!(fallback_advances(Some(&serde_json::json!({}))).is_empty());
    }

    /// A candidate this build cannot name is dropped rather than published with a
    /// hole in it, and it does not take the readable candidates beside it down.
    ///
    /// The document is another process's stdout, so every shape here is one a
    /// future oneharness could emit: a missing field, and a field that is JSON but
    /// not the text the contract says a consumer displays.
    #[test]
    fn a_fallback_candidate_that_is_not_two_strings_is_not_published() {
        let report = serde_json::json!({
            "fallback": {"fell_through": [
                {"reason": "auth"},
                {"harness": "codex"},
                {"harness": 7, "reason": "quota"},
                {"harness": "codex", "reason": {"code": 429}},
                {"harness": "claude-code", "reason": "quota"},
            ]}
        });
        let advances = fallback_advances(Some(&report));
        assert_eq!(advances.len(), 1, "{advances:?}");
        assert_eq!(advances[0]["identity"], Value::from("claude-code"));
        assert_eq!(advances[0]["reason"], Value::from("quota"));
    }

    /// The two programs' exit codes are read by their own contracts. `onejudge`
    /// exits `1` for a task it drove but did not complete, which is a settle;
    /// `oneharness` exits non-zero when it could not run the turn at all, which
    /// is a death. Reading one by the other turns a chain that reached nothing
    /// into a member that settled incomplete.
    #[test]
    fn each_program_s_exit_code_is_read_by_its_own_contract() {
        assert!(Kind::Onejudge.settled(0) && Kind::Onejudge.settled(1));
        assert!(!Kind::Onejudge.settled(2) && !Kind::Onejudge.settled(-1));
        assert!(Kind::Oneharness.settled(0));
        assert!(!Kind::Oneharness.settled(1) && !Kind::Oneharness.settled(2));
    }

    /// Only a completed settle is a success; a death and an unstartable member
    /// are both the graph's exit `1`.
    #[test]
    fn only_a_completed_settle_is_a_success() {
        assert!(Outcome::Settled.is_success());
        assert!(!Outcome::Incomplete.is_success());
        assert!(!Outcome::Unstartable("no".into()).is_success());
    }
}
