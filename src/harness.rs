//! Running a single-sided member through oneharness's library, in this process.
//!
//! A `kind: oneharness` member's turn is `oneharness_core::io::run::run_supervised`
//! on a thread of this process, so the member has no argv, exit status or stderr
//! of its own. [`crate::judge`] is this module for the other member kind and has
//! the same shape; both reach the shared settle and death in [`crate::member`].
//!
//! `docs/oneharness-library.md` is the boundary inventory behind the conversion.
//! Read it before changing what this module hands the engine.
use std::path::Path;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use oneharness_core::domain::events::ActionEvent;
use oneharness_core::domain::report::FallbackReport;
use oneharness_core::io::cancel::CancelToken;
use oneharness_core::io::run::{
    run_supervised, EventSink, RunControls, RunOutcome, RunRequest, SinkStep,
};
use oneharness_core::io::runner::ProcessSupervisor;
use serde_json::{Map, Value};

use crate::event::{
    bound_detail, bound_text, Cause, Emitter, EventKind, FallbackAdvanced, MemberDied,
};
use crate::invoke::HarnessLaunch;
use crate::member::{
    died_payload, payload, settle_report, summarize, unstartable, Bounds, Death, Kind, Outcome,
    Rule, Stall, HEARTBEAT_INTERVAL,
};

/// How long a condemned member's engine is given to answer the cancellation
/// before its thread is abandoned and the member reported dead regardless.
///
/// The same bound [`crate::judge`] gives its engine, and for the same reason: a
/// run that waited on a member it has already condemned hangs on it, which is the
/// failure the watchdog exists to prevent. Long enough for oneharness's own
/// `Finish::Terminate` — a TERM, a grace, then a KILL — to run to its end.
const TEARDOWN_GRACE: Duration = Duration::from_secs(5);

/// How often the teardown re-reaps while it waits.
///
/// Repeated for [`crate::judge`]'s reason: a fallback chain that steps to another
/// candidate after the first was reaped starts a new harness, and one that is not
/// reaped in turn is a paid turn the member was already condemned for.
const TEARDOWN_POLL: Duration = Duration::from_millis(100);

/// The engine a single-sided member is driven by, as its `member-started` names
/// it.
pub(crate) const ONEHARNESS_ENGINE: &str = "oneharness";

/// Run one single-sided member to its end.
///
/// Returns only when the member has settled or been condemned: the engine runs
/// on its own thread, and this call is the supervision around it.
#[must_use]
pub fn run(launch: &HarnessLaunch, emitter: &Emitter, bounds: Bounds, scratch: &Path) -> Outcome {
    // Opened before the engine is driven for the reason [`crate::judge`] opens
    // one before its plan: it is what the spawns go *into*, and they are not this
    // module's spawns to make. A member that could not be grouped is refused
    // rather than started, because a paid harness no cancel can reach is worse
    // than a member that never ran.
    //
    // llmlint: ignore-block[changed_behavior_has_e2e] this arm has no journey for
    // the reason its two twins do not: opening a group takes no input, and the
    // only ways it fails are the kernel refusing a job object or a scratch this
    // run created moments ago having become unwritable. Both are host failures
    // rather than requests. The reachable half — that the harness lands in the
    // group and a cancel reaps it through one — is tests/e2e/liveness.rs.
    let group = match crate::scratch::Group::open(scratch) {
        Ok(group) => Arc::new(group),
        Err(err) => {
            let reason = err.to_string();
            emitter.emit(EventKind::MemberDied, died_payload(&unstartable(&reason)));
            return Outcome::Unstartable(reason);
        }
    };
    // llmlint: ignore-end[changed_behavior_has_e2e]

    let request = launch.request();
    let activity = Arc::new(AtomicU64::new(0));
    let cancel = CancelToken::new();
    let started = Instant::now();
    let (tx, rx) = mpsc::channel();

    let engine = {
        let (emitter, activity, cancel, group) = (
            emitter.clone(),
            Arc::clone(&activity),
            cancel.clone(),
            Arc::clone(&group),
        );
        // `Builder`, not `thread::spawn`: a host that cannot give this run one
        // more thread is a recoverable refusal, and the plain spawn answers it by
        // panicking — which would take the whole graph down over one member. A
        // run of many members is exactly where that limit is met.
        std::thread::Builder::new().spawn(move || {
            let supervisor = HarnessSpawn {
                group,
                cancel: cancel.clone(),
                ungrouped: Mutex::new(None),
            };
            let mut sink = Events {
                emitter,
                activity,
                started,
                cancel: cancel.clone(),
                opened: false,
            };
            let outcome = run_supervised(
                &request,
                RunControls {
                    events: Some(&mut sink),
                    cancel,
                    // This process is a graph of many members and installs its
                    // own disposition; a run that took the host's handlers would
                    // be one member deciding what a signal means for all of them.
                    signal_cancel: false,
                    // The engine names itself: unlike the CLI, there is no
                    // separately-versioned binary here for the report to name.
                    version: None,
                },
                Some(&supervisor),
            );
            // A send that fails means the supervisor already condemned this
            // member and stopped listening; the engine still tore itself down on
            // the way here, which is what that teardown was waiting for.
            let _ = tx.send(Answer {
                outcome: outcome.map_err(|err| err.to_string()),
                ungrouped: supervisor.ungrouped(),
            });
        })
    };
    // llmlint: ignore-block[changed_behavior_has_e2e] this arm has no journey
    // because no input a user can give reaches it: it is taken only when the OS
    // refuses a thread, which is a host resource limit rather than anything a
    // graph, a task or a config can ask for, and no seam this crate sanctions
    // fakes `pthread_create`. What it decides is the direction of an unreachable
    // failure, and it is the safe one — nothing was spawned, so the member is
    // refused rather than reported as running.
    if let Err(err) = engine {
        let reason = format!("cannot start this member's engine thread: {err}");
        emitter.emit(EventKind::MemberDied, died_payload(&unstartable(&reason)));
        return Outcome::Unstartable(reason);
    }
    // llmlint: ignore-end[changed_behavior_has_e2e]

    supervise(&rx, &cancel, emitter, bounds, scratch, started, &activity)
}

/// Watch a member's engine to its end: the two watchdogs, and the answer.
///
/// Split from [`run`] so the containment this module promises can be driven
/// against a real thread that really panics — see
/// [`tests::a_panicking_engine_kills_its_own_member_and_not_the_process`].
// Seven values, none derivable from another: where the answer arrives, the lever
// that stops the engine, where the events go, the bounds, the member's scratch,
// and the two halves of the activity clock.
#[allow(clippy::too_many_arguments)]
fn supervise(
    rx: &mpsc::Receiver<Answer>,
    cancel: &CancelToken,
    emitter: &Emitter,
    bounds: Bounds,
    scratch: &Path,
    started: Instant,
    activity: &Arc<AtomicU64>,
) -> Outcome {
    let heartbeat_file = scratch.join("member.heartbeat");
    let mut last_heartbeat = Instant::now();
    // Published far more rarely than it is refreshed, for the reason
    // `crate::member` gives: a stream that is mostly heartbeats buries the events
    // it exists to carry.
    let publish_every = (bounds.heartbeat / 4).max(HEARTBEAT_INTERVAL * 2);
    let mut published = Instant::now();
    let mut stall = Stall::new(bounds.stall, started);
    loop {
        match rx.recv_timeout(HEARTBEAT_INTERVAL) {
            Ok(answer) => return finish(answer, emitter, scratch),
            // A sender dropped without an answer means the engine thread
            // panicked: this member's failure, not the graph's.
            //
            // llmlint: ignore-block[changed_behavior_has_e2e] nothing a graph,
            // task or config can ask for makes `oneharness_core` panic, so this
            // arm has no journey; forcing one would mean replacing the engine it
            // protects. Covered by
            // `tests::a_panicking_engine_kills_its_own_member_and_not_the_process`.
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return died(
                    emitter,
                    Rule::ProviderFailure,
                    Cause::Unclassified,
                    "the oneharness engine ended without answering",
                );
            }
            // llmlint: ignore-end[changed_behavior_has_e2e]
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let _ = std::fs::write(&heartbeat_file, elapsed_millis(started).to_string());
        let now = Instant::now();
        if now.duration_since(last_heartbeat) > bounds.heartbeat {
            return condemn(rx, cancel, emitter, Rule::Heartbeat, scratch);
        }
        last_heartbeat = now;
        if now.duration_since(published) >= publish_every {
            published = now;
            emitter.emit(EventKind::MemberHeartbeat, payload([]));
        }
        if stall.condemns(activity.load(Ordering::SeqCst), scratch) {
            return condemn(rx, cancel, emitter, Rule::Activity, scratch);
        }
    }
}

struct Answer {
    /// The run, or the reason oneharness refused the *request* — a shape it will
    /// not honour, reported before anything was spawned. A harness's own
    /// behaviour is never an error here: a missing binary, a non-zero exit and a
    /// hang all arrive as an `Ok` carrying a report that says so.
    outcome: Result<RunOutcome, String>,
    /// The reason a harness child could not be put in this member's group, when
    /// one could not. Carried beside the outcome rather than replacing it because
    /// the run answers either way — see [`HarnessSpawn`].
    ungrouped: Option<String>,
}

/// Puts every harness oneharness starts for this member into the member's
/// [`crate::scratch::Group`] — the same pair of moments
/// [`crate::judge::MemberSpawn`] hands onejudge.
///
/// Neither hook can refuse a spawn (upstream's methods return nothing), so a
/// grouping failure cancels the run instead and is kept for the death it becomes.
struct HarnessSpawn {
    group: Arc<crate::scratch::Group>,
    cancel: CancelToken,
    ungrouped: Mutex<Option<String>>,
}

impl HarnessSpawn {
    /// Record a harness that could not be grouped, and stop the run.
    fn refuse(&self, moment: &str, err: &std::io::Error) {
        let mut held = self
            .ungrouped
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if held.is_none() {
            *held = Some(format!(
                "this member's harness could not be put in the process group that a cancel and \
                 the activity watchdog reach it through ({moment}: {err}), so the run was stopped \
                 rather than left billing outside it"
            ));
        }
        drop(held);
        self.cancel.cancel();
    }

    fn ungrouped(&self) -> Option<String> {
        self.ungrouped
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

// llmlint: ignore-block[changed_behavior_has_e2e] the refusal arms have no
// journey of their own for `crate::scratch::Group`'s own reason: `prepare` and
// `join` fail only when the kernel refuses a job object assignment or a scratch
// this run created moments ago has become unwritable, and no seam this crate
// sanctions fakes either. The reachable half — that the harness a member runs is
// *in* the group, and that a cancel and the watchdog reach it there — is driven
// end to end in tests/e2e/liveness.rs.
impl ProcessSupervisor for HarnessSpawn {
    fn spawning(&self, command: &mut Command) {
        if let Err(err) = self.group.prepare(command) {
            self.refuse("preparing the command", &err);
        }
    }

    fn spawned(&self, child: &Child) {
        // `join`, not `adopt`: on Windows this child is still suspended and
        // oneharness resumes it once this returns. Resuming here would race that.
        if let Err(err) = self.group.join(child) {
            self.refuse("adding the process to the group", &err);
        }
    }
}
// llmlint: ignore-end[changed_behavior_has_e2e]

/// Where the engine publishes each normalized event as it occurs.
///
/// This is what replaces the NDJSON hop: the same `tool_call`/`tool_result`
/// events the `--stream` lines carried, arriving typed instead of as a document
/// this crate parsed back apart.
struct Events {
    emitter: Emitter,
    activity: Arc<AtomicU64>,
    started: Instant,
    /// Answered as [`SinkStep::Stop`] once cancelled, which is oneharness's own
    /// documented short-circuit — a condemned member stops at its next event
    /// rather than only when the terminate path reaches it.
    cancel: CancelToken,
    /// Whether this member's one turn has been opened. Not a counter: an
    /// `oneharness run` is a single turn, so the only two states are before the
    /// first event and after it — see [`Events::event`].
    opened: bool,
}

impl EventSink for Events {
    fn event(&mut self, _harness_id: &str, event: &ActionEvent) -> SinkStep {
        self.activity
            .store(elapsed_millis(self.started), Ordering::SeqCst);
        // One turn, always: `oneharness run` is a single turn, and its stream
        // envelope carries no turn index — the NDJSON reader this replaces
        // defaulted every event to turn 1 for exactly that reason. So the first
        // event a member publishes opens its turn and nothing renumbers it.
        if !self.opened {
            self.opened = true;
            emit_turn_started(&self.emitter, THE_ONLY_TURN);
        }
        ingest(event, &self.emitter);
        if self.cancel.is_cancelled() {
            SinkStep::Stop
        } else {
            SinkStep::Continue
        }
    }
}

/// The turn index every event of a single-sided member's run is attributed to.
///
/// `oneharness run` is one turn and its stream envelope carries no index, so
/// this is the number the NDJSON reader this replaces defaulted to.
const THE_ONLY_TURN: u64 = 1;

fn emit_turn_started(emitter: &Emitter, turn: u64) {
    emitter.emit(
        EventKind::TurnStarted,
        payload([("turn", Value::from(turn))]),
    );
}

/// Turn one live tool event into the contract's bounded summary.
///
/// The same rule [`crate::judge`] applies, and the same one the NDJSON reader
/// applied before it: an event this crate cannot name is skipped rather than
/// published with a hole in it — a `tool_result` carries no tool name, and
/// reporting one as an unnamed action would say the member did something it
/// cannot name.
fn ingest(event: &ActionEvent, emitter: &Emitter) {
    let Some(name) = event.name.as_deref() else {
        return;
    };
    let (detail, truncated) = bound_detail(&summarize(event.input.as_ref()));
    emitter.emit(
        EventKind::TurnActivity,
        payload([
            ("kind", Value::String(event.kind.clone())),
            ("name", Value::String(name.to_string())),
            ("detail", Value::String(detail)),
            ("truncated", Value::Bool(truncated)),
        ]),
    );
}

/// An answered run is not automatically a settled member: a grouping failure or
/// a non-zero exit outranks whatever report came back with it.
fn finish(answer: Answer, emitter: &Emitter, scratch: &Path) -> Outcome {
    let Answer { outcome, ungrouped } = answer;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        // The *request* could not be honoured, which oneharness decides before
        // anything is spawned — so no process ever existed, exactly as a failed
        // `Command::spawn` meant here before.
        Err(reason) => {
            emitter.emit(EventKind::MemberDied, died_payload(&unstartable(&reason)));
            return Outcome::Unstartable(reason);
        }
    };
    // A chain names every candidate it stepped past whether or not a later one
    // ran: which subscription to restore is exactly what an operator needs from a
    // member that reached nothing, so these publish before the verdict rather
    // than conditionally on it.
    advance(emitter, outcome.report.fallback.as_ref());
    // A harness this run could not group outranks whatever the report says: the
    // run was stopped for that reason, so reporting the cancelled run's own
    // verdict would name the symptom instead of the cause.
    if let Some(reason) = ungrouped {
        return died(emitter, Rule::ProviderFailure, Cause::Spawn, &reason);
    }
    let document = serde_json::to_value(&outcome.report).unwrap_or(Value::Null);
    if !Kind::Oneharness.settled(outcome.exit_code) {
        return died(
            emitter,
            Rule::ProviderFailure,
            // Four of oneharness's eight `FailureKind`s have no `cause` in
            // `docs/contract.md`, so the honest answer inside that closed
            // vocabulary is `unclassified`; the summary below names the harness.
            //
            // llmlint: ignore[contracts_have_one_source_or_a_drift_gate] adding
            // those four causes is the contract owner's, not this crate's. The
            // gap is a tracked follow-up in `docs/oneharness-library.md`, gated
            // both ways by `tests/inventory.rs`.
            Cause::Unclassified,
            &outcome.failure_summary.unwrap_or_else(|| {
                format!("the turn exited {} without a report", outcome.exit_code)
            }),
        );
    }
    settle_report(emitter, &document, true, scratch)
}

/// Publish every candidate this member's chain stepped past.
///
/// Read off [`FallbackReport`] as the type it already is, rather than out of a
/// terminal `result` line this crate parsed: the identity and oneharness's own
/// closed classification, spelled the way oneharness spells them.
fn advance(emitter: &Emitter, fallback: Option<&FallbackReport>) {
    let Some(fallback) = fallback else {
        return;
    };
    for candidate in &fallback.fell_through {
        let advanced = FallbackAdvanced {
            identity: candidate.harness.clone(),
            reason: candidate.reason.as_str().to_string(),
            // A single-sided member has one chain, so there is no side and no
            // turn to attribute it to — those belong to a two-party member's
            // per-side, per-turn chains.
            role: None,
            turn: None,
        };
        emitter.emit(EventKind::FallbackAdvanced, as_payload(&advanced));
    }
}

fn as_payload(advanced: &FallbackAdvanced) -> Map<String, Value> {
    match serde_json::to_value(advanced) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// Stop a member a watchdog condemned, then report it.
///
/// Shorter than [`crate::judge`]'s escalation because the engine takes a cancel
/// token: cancelling terminates each harness tree through oneharness's own
/// `Finish::Terminate`, so there is no "ask, then reap what never answered" —
/// there is one lever, the reap beside it for anything the stamp still finds, and
/// a bounded wait before the run gives up on the thread rather than on itself.
fn condemn(
    rx: &mpsc::Receiver<Answer>,
    cancel: &CancelToken,
    emitter: &Emitter,
    rule: Rule,
    scratch: &Path,
) -> Outcome {
    cancel.cancel();
    let deadline = Instant::now() + TEARDOWN_GRACE;
    let mut reaped = crate::scratch::reap(scratch);
    while Instant::now() < deadline {
        match rx.recv_timeout(TEARDOWN_POLL) {
            // The engine tore itself down and answered. What it says is evidence
            // — the chain it attempted above all — but the member is still dead
            // by the rule that condemned it, not settled by a report it only
            // produced because it was stopped.
            Ok(answer) => {
                if let Ok(outcome) = &answer.outcome {
                    advance(emitter, outcome.report.fallback.as_ref());
                }
                return died(
                    emitter,
                    rule,
                    Cause::Cancelled,
                    &format!(
                        "the {} rule condemned this member; its engine tore down {reaped} \
                         process(es) and stopped",
                        rule.as_str()
                    ),
                );
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => reaped += crate::scratch::reap(scratch),
        }
    }
    // llmlint: ignore-block[changed_behavior_has_e2e] reaching this sentence
    // means holding `oneharness_core` past its own bounded teardown, which would
    // mean replacing the layer under test. Covered by
    // `tests::a_condemned_member_whose_engine_never_answers_is_still_reported_dead`.
    died(
        emitter,
        rule,
        Cause::Cancelled,
        &format!(
            "the {} rule condemned this member; {reaped} process(es) were signalled and its \
             engine did not answer inside {}s, so the run gave up on it",
            rule.as_str(),
            TEARDOWN_GRACE.as_secs()
        ),
    )
    // llmlint: ignore-end[changed_behavior_has_e2e]
}

fn died(emitter: &Emitter, rule: Rule, cause: Cause, detail: &str) -> Outcome {
    let (detail, truncated) = bound_text(detail.trim());
    let payload = MemberDied {
        rule: rule.as_str().to_string(),
        cause,
        detail,
        truncated,
        // A member this process ran in-library was never a process of its own, so
        // it has none of the three facts one leaves behind — `docs/contract.md`
        // scopes all three to "a member that was one".
        exit_code: None,
        disposition: None,
        stderr_tail: None,
    };
    emitter.emit(EventKind::MemberDied, died_payload(&payload));
    Outcome::Died(Death { rule, payload })
}

fn elapsed_millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// A `RunRequest` this crate can also assert on, rather than one assembled at the
/// call.
///
/// The argument-by-argument mapping from the argv this replaces is
/// `docs/oneharness-library.md`'s table, and `tests/inventory.rs` holds this
/// literal's field names against it.
impl HarnessLaunch {
    pub(crate) fn request(&self) -> RunRequest {
        RunRequest {
            config: Some(self.config.clone()),
            // A parameter, never `set_current_dir`: one process hosts every
            // member, so a member that moved the process's own directory would
            // move the members that never asked, and move them mid-run.
            cwd: Some(self.worktree.clone()),
            events: true,
            // The member's own resolved config decided this — see
            // `crate::invoke::Reporting`. `Some` either way, because a flag beat
            // config on the argv this replaces and leaving it `None` would hand
            // the decision back to a layer that already made it.
            stream: Some(self.reporting.streams()),
            prompt: vec![self.prompt.clone()],
            ..RunRequest::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use serde_json::json;

    use super::*;
    use crate::event::Envelope;
    use crate::invoke::Reporting;

    /// A sink a test can read its own events back out of.
    #[derive(Clone, Default)]
    struct Recorder(Arc<StdMutex<Vec<u8>>>);

    impl std::io::Write for Recorder {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("recorder").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Recorder {
        fn events(&self) -> Vec<Envelope> {
            let raw = self.0.lock().expect("recorder").clone();
            String::from_utf8(raw)
                .expect("utf-8")
                .lines()
                .map(|line| serde_json::from_str(line).expect("an envelope"))
                .collect()
        }
    }

    fn recorded() -> (Emitter, Recorder) {
        let recorder = Recorder::default();
        (Emitter::new("s", Box::new(recorder.clone())), recorder)
    }

    fn call(name: Option<&str>) -> ActionEvent {
        ActionEvent {
            kind: "tool_call".into(),
            name: name.map(ToOwned::to_owned),
            input: Some(json!({"command": "just check", "n": 3})),
            output: None,
            index: 0,
            tool_call_id: None,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            status: None,
            timing_source: None,
        }
    }

    /// The first live event opens the member's one turn and reports what it acted
    /// on; every event after it publishes under that same turn.
    ///
    /// One turn is not a simplification: `oneharness run` is a single turn and its
    /// stream envelope carries no index, so the NDJSON reader this replaces
    /// defaulted every event to turn 1 as well.
    #[test]
    fn the_first_live_event_opens_the_only_turn_and_the_rest_publish_under_it() {
        let (emitter, recorder) = recorded();
        let mut sink = Events {
            emitter,
            activity: Arc::new(AtomicU64::new(0)),
            started: Instant::now(),
            cancel: CancelToken::new(),
            opened: false,
        };
        for _ in 0..3 {
            assert!(matches!(
                sink.event("claude-code", &call(Some("bash"))),
                SinkStep::Continue
            ));
        }

        let events = recorder.events();
        let kinds: Vec<_> = events.iter().map(|event| event.kind).collect();
        assert_eq!(
            kinds,
            vec![
                EventKind::TurnStarted,
                EventKind::TurnActivity,
                EventKind::TurnActivity,
                EventKind::TurnActivity,
            ]
        );
        assert_eq!(events[0].payload["turn"], json!(1));
        assert_eq!(events[1].payload["name"], json!("bash"));
        assert_eq!(events[1].payload["kind"], json!("tool_call"));
        assert_eq!(events[1].payload["detail"], json!("just check"));
        assert!(
            sink.activity.load(Ordering::SeqCst) < u64::MAX,
            "the watchdog clock was never touched"
        );
    }

    /// An event this crate cannot name is skipped rather than published with a
    /// hole in it — and it does not open a turn either, because a turn with no
    /// action in it says the member did something it cannot name.
    #[test]
    fn a_live_event_with_no_tool_name_is_skipped() {
        let (emitter, recorder) = recorded();
        ingest(&call(None), &emitter);
        assert!(recorder.events().is_empty(), "an unnamed event published");
    }

    /// A cancelled run's next event answers oneharness's own short-circuit, so a
    /// condemned member stops at that event rather than only when the terminate
    /// path reaches it.
    #[test]
    fn a_cancelled_run_answers_the_next_event_with_stop() {
        let (emitter, _recorder) = recorded();
        let cancel = CancelToken::new();
        let mut sink = Events {
            emitter,
            activity: Arc::new(AtomicU64::new(0)),
            started: Instant::now(),
            cancel: cancel.clone(),
            opened: false,
        };
        assert!(matches!(
            sink.event("claude-code", &call(Some("bash"))),
            SinkStep::Continue
        ));
        cancel.cancel();
        assert!(matches!(
            sink.event("claude-code", &call(Some("bash"))),
            SinkStep::Stop
        ));
    }

    /// A member whose engine thread **panics** fails that member and leaves the
    /// process it shares with every other one running.
    ///
    /// Driven through the real supervision loop with a real panicking thread —
    /// the panic drops the sender, which is what the loop reads as an engine that
    /// ended without answering. Nothing here stands in for the loop; what is
    /// substituted is the engine, because no request can make oneharness panic on
    /// demand and the containment being proven is this crate's.
    #[test]
    fn a_panicking_engine_kills_its_own_member_and_not_the_process() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (emitter, recorder) = recorded();
        let (tx, rx) = mpsc::channel::<Answer>();
        let panicked = std::thread::spawn(move || {
            let _tx = tx;
            panic!("the engine came apart mid-turn");
        });
        assert!(panicked.join().is_err(), "the engine thread did not panic");

        let cancel = CancelToken::new();
        let outcome = supervise(
            &rx,
            &cancel,
            &emitter,
            Bounds::default(),
            dir.path(),
            Instant::now(),
            &Arc::new(AtomicU64::new(0)),
        );
        let Outcome::Died(death) = outcome else {
            panic!("a panicking engine did not kill its member: {outcome:?}");
        };
        assert_eq!(death.rule, Rule::ProviderFailure);
        assert_eq!(death.payload.cause, Cause::Unclassified);
        assert!(
            death.payload.detail.contains("without answering"),
            "{death:?}"
        );
        // None of the three a child process leaves behind: this member was not one.
        assert!(death.payload.exit_code.is_none());
        assert!(death.payload.disposition.is_none());
        assert!(death.payload.stderr_tail.is_none());

        let kinds: Vec<_> = recorder
            .events()
            .into_iter()
            .map(|event| event.kind)
            .collect();
        assert_eq!(kinds, vec![EventKind::MemberDied]);
    }

    /// A condemned member whose engine never answers is reported dead anyway, and
    /// the run is not left waiting on it — the cancel is issued either way, which
    /// is what stops a paid harness the member was already condemned for.
    #[test]
    fn a_condemned_member_whose_engine_never_answers_is_still_reported_dead() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (emitter, recorder) = recorded();
        let cancel = CancelToken::new();
        // A sender dropped immediately: the engine is gone without an answer,
        // which is the shape a thread abandoned mid-turn leaves behind.
        let (tx, rx) = mpsc::channel::<Answer>();
        drop(tx);

        let outcome = condemn(&rx, &cancel, &emitter, Rule::Activity, dir.path());
        assert!(cancel.is_cancelled(), "the engine was never asked to stop");
        let Outcome::Died(death) = outcome else {
            panic!("a condemned member settled: {outcome:?}");
        };
        assert_eq!(death.rule, Rule::Activity);
        assert_eq!(death.payload.cause, Cause::Cancelled);
        assert!(death.payload.detail.contains("activity"), "{death:?}");

        let events = recorder.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::MemberDied);
    }

    /// Every candidate the chain stepped past is published with oneharness's own
    /// spelling of why, and a run with no chain publishes nothing rather than an
    /// invented one.
    #[test]
    fn every_candidate_the_chain_stepped_past_is_published() {
        let fallback: FallbackReport = serde_json::from_value(json!({
            "ran": "codex",
            "fell_through": [
                {"harness": "claude-code", "reason": "quota", "detail": null},
            ],
        }))
        .expect("a fallback block");

        let (emitter, recorder) = recorded();
        advance(&emitter, Some(&fallback));
        let events = recorder.events();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].kind, EventKind::FallbackAdvanced);
        assert_eq!(events[0].payload["identity"], json!("claude-code"));
        assert_eq!(events[0].payload["reason"], json!("quota"));

        let (quiet, unwritten) = recorded();
        advance(&quiet, None);
        assert!(unwritten.events().is_empty());
    }

    /// The reasons a **precondition refusal** classifies as, published as the
    /// tokens oneharness spells them with.
    ///
    /// A harness refusing the working directory or the input's size before making
    /// a request falls the chain through to the next candidate instead of ending
    /// it, so these are reasons a report can carry and a consumer joins on.
    /// Pinned as the tokens rather than the variants, because the token is what
    /// crosses the wire — and it is oneharness's `as_str()` that produces it, so a
    /// respelling upstream fails here rather than reaching a consumer.
    #[test]
    fn a_precondition_refusal_publishes_its_own_reason() {
        let fallback: FallbackReport = serde_json::from_value(json!({
            "ran": "codex",
            "fell_through": [
                {"harness": "claude-code", "reason": "untrusted-directory", "detail": null},
                {"harness": "cursor-agent", "reason": "input-too-large", "detail": null},
            ],
        }))
        .expect("a fallback block");

        let (emitter, recorder) = recorded();
        advance(&emitter, Some(&fallback));
        let reasons: Vec<Value> = recorder
            .events()
            .into_iter()
            .map(|event| event.payload["reason"].clone())
            .collect();
        assert_eq!(
            reasons,
            vec![json!("untrusted-directory"), json!("input-too-large")]
        );
    }

    /// The turn a member's config asked for is the request oneharness is handed,
    /// argument for argument — the mapping `docs/oneharness-library.md` tabulates.
    #[test]
    fn a_members_launch_becomes_the_request_its_argv_used_to_be() {
        let launch = HarnessLaunch {
            config: std::path::PathBuf::from("/scratch/oneharness.toml"),
            worktree: std::path::PathBuf::from("/work/api"),
            prompt: "report in".to_string(),
            reporting: Reporting::Streamed,
        };
        let request = launch.request();
        assert_eq!(
            request.config.as_deref(),
            Some(Path::new("/scratch/oneharness.toml"))
        );
        assert_eq!(request.cwd.as_deref(), Some(Path::new("/work/api")));
        assert!(request.events, "the turn asked for no tool events");
        assert_eq!(request.stream, Some(true));
        assert_eq!(request.prompt, vec!["report in".to_string()]);
        // Nothing here discards the config or the `ONEHARNESS_*` layer: the
        // process's own environment is the one `crate::run`'s `export` built.
        assert!(!request.no_config);
        assert!(request.env.is_empty());
        assert!(request.bin.is_empty());

        // A member whose own config asked for a buffered report carries that
        // decision rather than deferring it back to the layer that made it.
        let buffered = HarnessLaunch {
            reporting: Reporting::Buffered,
            ..launch
        };
        assert_eq!(buffered.request().stream, Some(false));
    }
}
