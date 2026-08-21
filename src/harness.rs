//! Running a single-sided member through oneharness's library, in this process.
//!
//! A `kind: oneharness` member's turn is `oneharness_core::io::run::run_supervised`
//! on a thread of this process, so the member has no argv, exit status or stderr
//! of its own. [`crate::judge`] is this module for the other member kind and has
//! the same shape; both reach the shared settle and death in [`crate::member`].
//!
//! `docs/oneharness-library.md` owns why each seam below is shaped as it is.

use std::path::Path;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use oneharness_core::domain::events::ActionEvent;
use oneharness_core::domain::report::{FallbackReport, RunReport, RunResult};
use oneharness_core::domain::signals::Usage;
use oneharness_core::io::cancel::CancelToken;
use oneharness_core::io::run::{
    run_supervised, EventSink, RunControls, RunOutcome, RunRequest, SinkStep,
};
use oneharness_core::io::runner::ProcessSupervisor;
use serde_json::Value;

use crate::event::{
    bound_text, Cause, Emitter, EventKind, FallbackAdvanced, MemberDied, Party, TurnCompleted,
    TurnStarted, MAX_PAYLOAD_TEXT_BYTES,
};
use crate::invoke::HarnessLaunch;
use crate::member::{
    activity, as_payload, payload, settle_report, unstartable, Bounds, Death, Kind, Outcome, Rule,
    Stall, HEARTBEAT_INTERVAL,
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
    // Opened before the engine runs, because it is what the spawns go into.
    //
    // llmlint: ignore-block[changed_behavior_has_e2e] no input reaches this arm:
    // opening a group fails only on a kernel refusal or an unwritable scratch.
    // The reachable half is tests/e2e/liveness.rs.
    let group = match crate::scratch::Group::open(scratch) {
        Ok(group) => Arc::new(group),
        Err(err) => {
            let reason = err.to_string();
            emitter.emit(EventKind::MemberDied, as_payload(&unstartable(&reason)));
            return Outcome::Unstartable(reason);
        }
    };
    // llmlint: ignore-end[changed_behavior_has_e2e]

    let request = launch.request();
    // Minted before the engine is handed the request: this member's one turn
    // begins here, and its opening and its close have to name the same instant.
    let turn = TheTurn::new(&launch.prompt);
    let activity = Arc::new(AtomicU64::new(0));
    let cancel = CancelToken::new();
    let started = Instant::now();
    let (tx, rx) = mpsc::channel();

    let engine = {
        let (emitter, activity, cancel, group, turn) = (
            emitter.clone(),
            Arc::clone(&activity),
            cancel.clone(),
            Arc::clone(&group),
            turn.clone(),
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
                turn,
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
        emitter.emit(EventKind::MemberDied, as_payload(&unstartable(&reason)));
        return Outcome::Unstartable(reason);
    }
    // llmlint: ignore-end[changed_behavior_has_e2e]

    supervise(
        &rx, &cancel, emitter, bounds, scratch, started, &activity, &turn,
    )
}

/// Watch a member's engine to its end: the two watchdogs, and the answer.
///
/// Split from [`run`] so the containment this module promises can be driven
/// against a real thread that really panics — see
/// [`tests::a_panicking_engine_kills_its_own_member_and_not_the_process`].
// Eight values, none derivable from another: where the answer arrives, the lever
// that stops the engine, where the events go, the bounds, the member's scratch,
// the two halves of the activity clock, and the turn a settle closes.
#[allow(clippy::too_many_arguments)]
fn supervise(
    rx: &mpsc::Receiver<Answer>,
    cancel: &CancelToken,
    emitter: &Emitter,
    bounds: Bounds,
    scratch: &Path,
    started: Instant,
    activity: &Arc<AtomicU64>,
    turn: &TheTurn,
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
            Ok(answer) => return finish(answer, emitter, scratch, turn),
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
    turn: TheTurn,
}

impl EventSink for Events {
    fn event(&mut self, _harness_id: &str, event: &ActionEvent) -> SinkStep {
        self.activity
            .store(elapsed_millis(self.started), Ordering::SeqCst);
        self.turn.open(&self.emitter);
        ingest(event, &self.emitter);
        if self.cancel.is_cancelled() {
            SinkStep::Stop
        } else {
            SinkStep::Continue
        }
    }
}

/// The one turn a single-sided member takes: `oneharness run` is a single turn,
/// so this member's turn *is* its run, and the run's bounds are the turn's.
///
/// Held from before the engine is handed the request, because `started_at` has to
/// be the same instant on the turn's opening and on its close — and the close is
/// read off a report that arrives minutes later.
#[derive(Clone)]
struct TheTurn {
    /// The task prose this turn is answering, head-bounded — a turn's opening is
    /// what an operator watching a dispatch reads it for.
    instruction: String,
    instruction_truncated: bool,
    started_at: String,
    /// Whether this turn has already been announced on the stream. Shared,
    /// because two places announce it and neither knows about the other: the
    /// event sink on the member's first tool event, and [`close`](Self::close)
    /// for a member whose run published none — a config that asks not to stream,
    /// or a harness whose output exposes no machine-readable trace. The turn
    /// happened either way, so a `turn-completed` is never served without the
    /// `turn-started` it answers.
    opened: Arc<AtomicBool>,
}

impl TheTurn {
    fn new(prompt: &str) -> Self {
        // Head-bounded: `bound_text` keeps a tail, so a field that keeps its
        // opening trims to the same constant on a character boundary first and
        // counts that trim as a cut. See `bound_text`'s own doc for why this
        // field is the head-keeping one.
        let mut head = MAX_PAYLOAD_TEXT_BYTES.min(prompt.len());
        while !prompt.is_char_boundary(head) {
            head -= 1;
        }
        let (instruction, cut) = bound_text(&prompt[..head]);
        let instruction_truncated = cut || head < prompt.len();
        Self {
            instruction,
            instruction_truncated,
            started_at: crate::clock::now_rfc3339(),
            opened: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Announce the turn, once however often this is called.
    fn open(&self, emitter: &Emitter) {
        if self.opened.swap(true, Ordering::SeqCst) {
            return;
        }
        emitter.emit(
            EventKind::TurnStarted,
            as_payload(&TurnStarted {
                turn: THE_ONLY_TURN,
                role: THE_ONLY_ROLE.as_str().to_string(),
                instruction: self.instruction.clone(),
                instruction_truncated: self.instruction_truncated,
                started_at: self.started_at.clone(),
            }),
        );
    }

    /// Close the turn on what the report says it consumed.
    fn close(&self, emitter: &Emitter, report: &RunReport) {
        self.open(emitter);
        emitter.emit(
            EventKind::TurnCompleted,
            as_payload(&TurnCompleted {
                turn: THE_ONLY_TURN,
                role: THE_ONLY_ROLE.as_str().to_string(),
                usage: ran(report).map(usage).unwrap_or_default(),
                started_at: self.started_at.clone(),
                finished_at: crate::clock::now_rfc3339(),
            }),
        );
    }
}

/// The turn index every event of a single-sided member's run is attributed to.
///
/// `oneharness run` is one turn and its stream envelope carries no index, so
/// this is the number the NDJSON reader this replaces defaulted to.
const THE_ONLY_TURN: u64 = 1;

/// Who takes that turn. A single-sided member has no supervisor to answer it, so
/// the one party there is is the agent.
const THE_ONLY_ROLE: Party = Party::Assistant;

/// The candidate whose result is this member's turn, when one ran.
///
/// oneharness's own ordering rule, read the way onejudge reads it: a fallback run
/// holds the candidates it stepped past *before* the one that ran, so the last
/// result carrying the named id is the turn.
fn ran(report: &RunReport) -> Option<&RunResult> {
    let Some(fallback) = report.fallback.as_ref() else {
        // Not a fallback run: the results are the candidates that were asked, in
        // order, and the first of them is this member's turn.
        return report.results.first();
    };
    // A chain that fell through every candidate names none, and there is no turn
    // to account for. Matched from the end because a model fan-out repeats a
    // harness id across candidates, so the *last* one carrying it is the one that
    // ran.
    let ran = fallback.ran.as_deref()?;
    report
        .results
        .iter()
        .rev()
        .find(|result| result.harness_id == ran || result.harness == ran)
        .or_else(|| report.results.last())
}

/// One turn's accounting, field for field with oneharness's own.
///
/// Spelled out rather than round-tripped through JSON so a signal added upstream
/// is a compile error here rather than a figure silently dropped.
fn usage(result: &RunResult) -> crate::event::Usage {
    let Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cost_usd,
    } = result.usage;
    crate::event::Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cost_usd,
    }
}

/// Publish one live tool event, through the payload builder both member kinds
/// share — see [`crate::member::activity`].
fn ingest(event: &ActionEvent, emitter: &Emitter) {
    emitter.emit(
        EventKind::TurnActivity,
        as_payload(&activity(
            &event.kind,
            event.name.as_deref(),
            event.input.as_ref(),
            event.output.as_deref(),
            event.tool_call_id.as_deref(),
            event.index,
        )),
    );
}

/// An answered run is not automatically a settled member: a grouping failure or
/// a non-zero exit outranks whatever report came back with it.
fn finish(answer: Answer, emitter: &Emitter, scratch: &Path, turn: &TheTurn) -> Outcome {
    let Answer { outcome, ungrouped } = answer;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        // The *request* could not be honoured, which oneharness decides before
        // anything is spawned — so no process ever existed, exactly as a failed
        // `Command::spawn` meant here before.
        Err(reason) => {
            emitter.emit(EventKind::MemberDied, as_payload(&unstartable(&reason)));
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
            // Four of oneharness's eight `FailureKind`s — `session_not_found`,
            // `tool_deferred`, `untrusted_directory`, `input_too_large` — have no
            // `cause` in `docs/contract.md`'s closed set, so `unclassified` is the
            // honest answer inside it and the summary below names the harness. A
            // partial map would report those four as something they are not.
            // Widening `cause` is a contract proposal, recorded in
            // `docs/oneharness-library.md`.
            Cause::Unclassified,
            &outcome.failure_summary.unwrap_or_else(|| {
                format!("the turn exited {} without a report", outcome.exit_code)
            }),
        );
    }
    // The turn closes before the member settles, carrying what this one turn
    // consumed: a single-sided member's turn *is* its run, so the accounting the
    // report holds is that turn's own rather than a total over several.
    turn.close(emitter, &outcome.report);
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

/// Stop a member a watchdog condemned, then report it.
///
/// Shorter than [`crate::judge`]'s escalation because the engine takes a cancel
/// token: cancelling terminates each harness tree through oneharness's own
/// `Finish::Terminate`, so there is no second ask to make — there is one lever,
/// the reap beside it for anything the stamp still finds, and a bounded wait
/// before the run gives up on the thread rather than on itself. That reap is
/// [`crate::scratch::reap_after_cancel`], which is what leaves the lever a
/// moment to be answered.
fn condemn(
    rx: &mpsc::Receiver<Answer>,
    cancel: &CancelToken,
    emitter: &Emitter,
    rule: Rule,
    scratch: &Path,
) -> Outcome {
    cancel.cancel();
    let deadline = Instant::now() + TEARDOWN_GRACE;
    // llmlint: ignore-block[changed_behavior_has_e2e] the grace this reap leaves
    // the cancel is Windows-only — POSIX already spent it — so no journey on a
    // POSIX runner can be red before this line and green after it. Asserted at
    // the seam it lives in, by `scratch::platform`'s two timing tests; the
    // condemnation around it is `tests/e2e/liveness.rs`.
    let mut reaped = crate::scratch::reap_after_cancel(scratch);
    // llmlint: ignore-end[changed_behavior_has_e2e]
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
    emitter.emit(EventKind::MemberDied, as_payload(&payload));
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

    use crate::event::MAX_PAYLOAD_TEXT_BYTES;

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
            tool_call_id: Some("t1".into()),
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
            turn: TheTurn::new("write the thing"),
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
        // The turn names who takes it, what it was asked, and when it began —
        // the three an operator watching a live dispatch has no other source for.
        assert_eq!(events[0].payload["role"], json!("assistant"));
        assert_eq!(events[0].payload["instruction"], json!("write the thing"));
        assert!(
            events[0].payload["started_at"]
                .as_str()
                .is_some_and(|at| at.ends_with('Z')),
            "the turn opened at no instant: {:?}",
            events[0].payload
        );
        assert_eq!(events[1].payload["name"], json!("bash"));
        assert_eq!(events[1].payload["kind"], json!("tool_call"));
        assert_eq!(events[1].payload["detail"], json!("just check"));
        assert!(
            sink.activity.load(Ordering::SeqCst) < u64::MAX,
            "the watchdog clock was never touched"
        );
    }

    /// A `tool_result` names no tool because it answers one already named, and it
    /// is published all the same: the observation, and the call identity that
    /// joins it back. Discarding it for having no name is what threw away every
    /// observation this member's tools returned.
    #[test]
    fn an_observation_reaches_the_stream_with_the_identity_of_the_call_it_answers() {
        let (emitter, recorder) = recorded();
        ingest(
            &ActionEvent {
                kind: "tool_result".into(),
                output: Some("2 passed; 0 failed".into()),
                input: None,
                index: 4,
                ..call(None)
            },
            &emitter,
        );
        let events = recorder.events();
        assert_eq!(events.len(), 1, "the observation was discarded: {events:?}");
        assert_eq!(events[0].kind, EventKind::TurnActivity);
        assert_eq!(events[0].payload["kind"], json!("tool_result"));
        assert_eq!(events[0].payload["name"], Value::Null);
        assert_eq!(events[0].payload["output"], json!("2 passed; 0 failed"));
        assert_eq!(events[0].payload["tool_call_id"], json!("t1"));
        assert_eq!(events[0].payload["index"], json!(4));
    }

    /// An observation the trace did not expose is absent; one that was genuinely
    /// empty is an empty string. A consumer can tell the two apart, and a call
    /// carries neither.
    #[test]
    fn an_absent_observation_and_an_empty_one_are_different_facts() {
        let (emitter, recorder) = recorded();
        let unexposed = ActionEvent {
            kind: "tool_result".into(),
            output: None,
            input: None,
            ..call(None)
        };
        ingest(&unexposed, &emitter);
        ingest(
            &ActionEvent {
                output: Some(String::new()),
                ..unexposed
            },
            &emitter,
        );
        ingest(&call(Some("bash")), &emitter);

        let events = recorder.events();
        assert_eq!(events.len(), 3);
        assert!(
            !events[0].payload.contains_key("output"),
            "an observation the trace never exposed was published as one: {:?}",
            events[0].payload
        );
        assert_eq!(events[1].payload["output"], json!(""));
        assert!(
            !events[2].payload.contains_key("output"),
            "a call was published carrying an observation: {:?}",
            events[2].payload
        );
    }

    /// A long observation keeps its **tail** at this crate's own payload bound,
    /// and says it was cut — what names a failure is the last of a tool's output.
    #[test]
    fn a_long_observation_keeps_its_tail_at_the_published_payload_bound() {
        let (emitter, recorder) = recorded();
        let long = format!(
            "{}the test suite failed",
            "x".repeat(MAX_PAYLOAD_TEXT_BYTES)
        );
        ingest(
            &ActionEvent {
                kind: "tool_result".into(),
                output: Some(long),
                input: None,
                ..call(None)
            },
            &emitter,
        );
        let events = recorder.events();
        let output = events[0].payload["output"]
            .as_str()
            .expect("an observation");
        assert!(output.ends_with("the test suite failed"), "{output}");
        assert!(output.len() <= MAX_PAYLOAD_TEXT_BYTES);
        assert_eq!(events[0].payload["output_truncated"], json!(true));
        // The summary bound is its own and is untouched by the new field.
        assert!(!events[0].payload.contains_key("truncated"));
    }

    /// The instruction this member's one turn answers keeps its **head** at the
    /// same bound, and says it was cut — a turn's opening is where the model says
    /// what it is about to do.
    ///
    /// This side owns its own head-trim, so it is driven here rather than
    /// inherited from the judge side's. `é` is two bytes behind a fifteen-byte
    /// ASCII opening, so every boundary past that opening is odd and the even
    /// bound falls *inside* a character — the case a naive slice panics on.
    #[test]
    fn a_long_instruction_keeps_its_opening_at_the_published_payload_bound() {
        let (emitter, recorder) = recorded();
        let long = format!("write the thing{}", "é".repeat(MAX_PAYLOAD_TEXT_BYTES));
        TheTurn::new(&long).open(&emitter);

        let events = recorder.events();
        let instruction = events[0].payload["instruction"]
            .as_str()
            .expect("an instruction");
        assert!(instruction.starts_with("write the thing"), "{instruction}");
        assert!(
            instruction.len() < MAX_PAYLOAD_TEXT_BYTES,
            "the cut did not walk back to a boundary: {}",
            instruction.len()
        );
        assert_eq!(events[0].payload["instruction_truncated"], json!(true));

        // And a prompt inside the bound is served whole, saying nothing about a
        // cut that never happened.
        let (emitter, recorder) = recorded();
        TheTurn::new("write the thing").open(&emitter);
        let events = recorder.events();
        assert_eq!(events[0].payload["instruction"], json!("write the thing"));
        assert_eq!(
            events[0].payload.get("instruction_truncated"),
            None,
            "an uncut instruction claimed a cut: {:?}",
            events[0].payload
        );
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
            turn: TheTurn::new("write the thing"),
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
            &TheTurn::new("write the thing"),
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
