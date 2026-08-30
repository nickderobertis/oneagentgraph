//! Running a two-party member through onejudge's library, in this process.
//!
//! `docs/contract.md` says a `kind: onejudge` member is "driven in-process
//! through the onejudge library". This module is that: the effective config
//! [`crate::invoke`] wrote is loaded through onejudge's own
//! [`onejudge::cli::Config`], resolved into its own [`onejudge::cli::Plan`], and
//! driven by its own run driver
//! ([`onejudge::cli::run_plan_observing_reporting_failure`]).
//! Nothing about the conversation is re-implemented here — the loop, the
//! `done_when` re-judgement, the evals, and the report are onejudge's, exactly as
//! they were when this crate spawned its CLI.
//!
//! What is *this* crate's is everything around that call, and it is the same
//! three things a child member gets: the two watchdogs, the merged stream, and
//! the death.
//!
//! # What changes when a member is not a process
//!
//! **The whole of a turn arrives live, typed.** The sink is called with an
//! [`onejudge::Observation`] the instant the engine produces one, without the
//! round-trip through another process's stdout and back through a parser. There
//! is no reconciliation to do: a turn index is a `usize`, not a field that might
//! be missing.
//!
//! That seam is wider than the events the NDJSON hop carried, and every part of
//! it is published: the turn's opening and the instruction it answers
//! ([`crate::event::EventKind::TurnStarted`]), each tool call **and the
//! observation that answered it**
//! ([`crate::event::EventKind::TurnActivity`]), each party's own reply
//! ([`crate::event::EventKind::TurnMessage`]), and that one turn's usage and
//! bounds ([`crate::event::EventKind::TurnCompleted`]). An operator watching a
//! live dispatch reads what the agent did and what it said off the journal,
//! rather than waiting for the settled report.
//!
//! **A failure is typed, not a stderr tail.** onejudge classifies a provider
//! failure with its own `ProviderErrorKind` — which is oneharness's normalized
//! `failure_kind` — so `member-died` carries [`crate::event::Cause`] rather than
//! the last four kilobytes of somebody's standard error. That is the whole reason
//! `member-died` changed shape.
//!
//! **A panic is this member's death, not the graph's.** The engine runs on a
//! thread and answers over an `mpsc::channel`, so a panic drops the sender and
//! the supervision loop reads `RecvTimeoutError::Disconnected` as an engine that
//! ended without answering — a `provider-failure` for this member, in a process
//! that keeps running every other one. Starting that thread cannot panic either:
//! a host that refuses one is a refused *member*. Both halves are
//! [`crate::harness`]'s too, and deliberately identical — since that conversion
//! neither member kind has a child process to crash instead.
//!
//! **A stalled member is stopped by escalation, not by one kill.** A thread
//! cannot be killed the way a process can, so a watchdog here works the way an
//! operator's `cancel --kill` already does, and in the same order:
//!
//! 1. Ask. The abort flag is set, and the sink answers the engine's next event
//!    with [`ControlFlow::Break`], which is onejudge's own documented
//!    short-circuit — it tears down the `oneharness run` it owns and returns.
//! 2. Reap. Every live process still stamped for this member's scratch is
//!    signalled, which is what reaches a member that is stalled *because* its
//!    harness is silent and so will never deliver the event step 1 waits for.
//!    The stamp is the one [`crate::invoke`] writes into each side's oneharness
//!    config, so it is on the harness itself.
//! 3. Give up on the thread, not on the run. If the engine has not answered by
//!    `TEARDOWN_GRACE`, the member is reported dead anyway and its thread is
//!    abandoned. A run that waited would hang on a member it has already
//!    condemned, which is the failure the watchdog exists to prevent.

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use onejudge::cli::{Config, Overrides, RunFailure, RunSummary};
use onejudge::Observation;
use serde_json::Value;

use crate::event::{
    bound_text, Artifact, Cause, Emitter, EventKind, FallbackAdvanced, MemberDied,
    OneharnessSession, Party, Role, TurnCompleted, TurnStarted, MAX_PAYLOAD_TEXT_BYTES,
    ONEHARNESS_SESSION_ARTIFACT,
};
use crate::invoke::JudgeLaunch;
use crate::member::{
    activity, as_payload, payload, settle_report, unstartable, Bounds, Death, Outcome, Rule, Stall,
    HEARTBEAT_INTERVAL,
};

/// How long a condemned member's engine is given to answer the teardown before
/// its thread is abandoned and the member reported dead regardless.
///
/// Long enough for onejudge's own escalation — it closes the harness runner's
/// stdout, signals it, and kills it — to run to its end, and short enough that a
/// watchdog is still a watchdog.
const TEARDOWN_GRACE: Duration = Duration::from_secs(5);

/// How often the teardown re-reaps while it waits.
///
/// Repeated on purpose: a chain that steps to another candidate after the first
/// was reaped starts a new harness process, and one that is not reaped in turn is
/// a paid turn the member was already condemned for.
const TEARDOWN_POLL: Duration = Duration::from_millis(100);

/// Run one two-party member to its end, publishing every envelope it produces.
#[must_use]
pub fn run(launch: &JudgeLaunch, emitter: &Emitter, bounds: Bounds, scratch: &Path) -> Outcome {
    // The group is opened before the plan is driven for the same reason
    // `crate::member` opens one before it spawns: it is what the spawns go
    // *into*. Here they are not this module's spawns to make — onejudge starts
    // `oneharness` for each side of each turn — so the group is handed to it as
    // a hook instead, and the failure direction is identical. A member that
    // could not be grouped is refused rather than started, because a paid
    // harness no cancel can reach is worse than a member that never ran.
    //
    // llmlint: ignore-block[changed_behavior_has_e2e] this arm has no journey
    // for the reason `crate::member`'s twin does not: opening a group takes no
    // input, and the only ways it fails are the kernel refusing a job object or
    // a scratch this run created moments ago having become unwritable. Both are
    // host failures rather than requests. The reachable half — that both sides
    // of a two-party member land in the group, and that a cancel reaps them
    // through it — is `tests/e2e/liveness.rs`.
    let group = match crate::scratch::Group::open(scratch) {
        Ok(group) => Arc::new(group),
        Err(err) => {
            let reason = err.to_string();
            emitter.emit(EventKind::MemberDied, as_payload(&unstartable(&reason)));
            return Outcome::Unstartable(reason);
        }
    };
    // llmlint: ignore-end[changed_behavior_has_e2e]

    // The note endpoint is bound before the plan is built, because the engine's
    // end of the same channel goes *on* that plan. A member that could not bind
    // one runs exactly as it did before notes existed: no inbox, and nothing able
    // to send to it.
    let spool = crate::note::Spool::bind(scratch);
    let (notes, inbox) = match spool {
        Some(_) => {
            let (notes, inbox) = onejudge::note::Notes::channel();
            (Some(notes), Some(inbox))
        }
        None => (None, None),
    };

    let plan = match plan(launch) {
        Ok(plan) => {
            let plan = plan.with_spawn_hook(Arc::new(MemberSpawn {
                group: Arc::clone(&group),
                agent_config: launch.agent_config.clone(),
            }));
            // Which side of this conversation is live is a fact only the engine
            // driving it has, so the routing is onejudge's rather than this
            // crate's: a note arriving during the worker's turn reopens that turn
            // carrying it *before* the supervisor is consulted — which is how the
            // judge receives it together with the worker's response — and one
            // arriving while the supervisor is deciding re-takes that decision
            // with the note in hand. See [`crate::note`].
            match inbox {
                Some(inbox) => plan.with_notes(inbox),
                None => plan,
            }
        }
        Err(reason) => {
            emitter.emit(EventKind::MemberDied, as_payload(&unstartable(&reason)));
            return Outcome::Unstartable(reason);
        }
    };

    // Before the plan is driven, because the turn an operator wants to redirect
    // is the one in flight and onejudge reports `control` only on the *finished*
    // run. Every value here is this crate's own — see [`crate::control`] — and
    // the report replaces it with oneharness's authoritative answer at
    // [`finish`], which is also where an ask that was refused becomes the reason
    // `interrupt` reports.
    let address = crate::control::Address {
        session: agent_session(&launch.session),
        session_dir: None,
        cwd: launch.worktree.clone(),
    };
    // The record names the endpoint bound above, so a caller that reads the
    // record and offers a note has somewhere to offer it into.
    crate::control::write_with_notes(
        scratch,
        &crate::control::Turn::Open {
            address: address.clone(),
        },
        spool.as_ref().map(crate::note::Spool::path),
    );

    let activity = Arc::new(AtomicU64::new(0));
    let abort = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let (tx, rx) = mpsc::channel();

    let engine = {
        let (emitter, activity, abort) =
            (emitter.clone(), Arc::clone(&activity), Arc::clone(&abort));
        // `Builder`, not `thread::spawn`: a host that cannot give this run one
        // more thread is a recoverable refusal, and the plain spawn answers it by
        // panicking — which would take the whole graph down over one member. A
        // run of many members is exactly where that limit is met. The same choice
        // [`crate::harness`] makes, because since that conversion both member
        // kinds put their engine on a thread of this one process.
        std::thread::Builder::new().spawn(move || {
            let mut sink = move |observation: &Observation<'_>| {
                activity.store(elapsed_millis(started), Ordering::SeqCst);
                ingest(observation, &emitter);
                #[cfg(feature = "test-doubles")]
                hold_between_turns(observation);
                if abort.load(Ordering::SeqCst) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            };
            let answer = onejudge::cli::run_plan_observing_reporting_failure(plan, &mut sink)
                .map_err(Box::new);
            // A send that fails means the supervisor already condemned this
            // member and stopped listening; the engine still tore itself down on
            // the way here, which is what that teardown was waiting for.
            let _ = tx.send(answer);
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

    // On a thread of its own, because `Notes::send` blocks until the conversation
    // has disposed of the note — for a supervisor-side delivery, until its
    // re-taken decision comes back. Servicing the spool from the supervision loop
    // below would put a judge invocation between two heartbeats.
    //
    // llmlint: ignore-block[changed_behavior_has_e2e] the `None` arm is a host
    // that refused this run one more thread, the same unreachable refusal the
    // engine thread's own arm above records and for the same reason: no graph,
    // task or config asks for it, and no seam this crate sanctions fakes
    // `pthread_create`. What it decides is that the member runs on without a note
    // courier rather than being killed over one, and every note offered to it is
    // then answered by `submit`'s own deadline. The reachable half — a courier
    // that carries notes into the conversation — is `tests/e2e/note.rs`.
    let ending = match (spool, notes) {
        (Some(spool), Some(notes)) => {
            let (courier, ending) = crate::note::Courier::open(spool, notes, emitter);
            match std::thread::Builder::new().spawn(move || courier.serve()) {
                Ok(_) => Some(ending),
                Err(_) => None,
            }
        }
        _ => None,
    };
    // llmlint: ignore-end[changed_behavior_has_e2e]
    supervise(
        &rx, &abort, emitter, bounds, scratch, started, &activity, ending,
    )
}

/// How long [`hold_between_turns`] waits before giving up on the journey that
/// asked for it.
///
/// Bounded because a fixture that never returns wedges the suite rather than
/// failing it: reaching this bound means the journey never released the gate,
/// which its own assertions then report.
#[cfg(feature = "test-doubles")]
const FIXTURE_HOLD: Duration = Duration::from_secs(30);

/// A test-only pause at the one conversation boundary a journey cannot otherwise
/// hold: after a turn has closed and before the next one opens.
///
/// Behind the non-default `test-doubles` feature, which is where this crate
/// already keeps what only the suite needs, so a consumer's build has no trace of
/// it and no production path can reach it.
///
/// It exists because that boundary is real and unreachable from outside. onejudge
/// answers [`crate::note::Accepted::Queued`] only for a note offered with **no
/// turn live**, and no seam a journey has holds the conversation there: the one
/// process this suite may fake is the harness, and a harness runs *inside* a
/// turn, so it cannot hold the gap between two. The engine's own
/// `NoteInbox::begin` runs before the first turn opens, and every later gap is
/// closed by its `take_notes` a few statements later. This sink is the only code
/// that runs in that window, because the engine publishes the closing turn
/// through it before looking for notes again.
///
/// Held on the **supervisor's** close and no other: it is the gap before the next
/// *worker* turn opens, which is the turn a queued note is delivered into. The
/// agent's own close is followed immediately by a `take_notes` that would take
/// the note as a live-turn delivery instead, which is a different journey.
///
/// `ONEAGENTGRAPH_FIXTURE_HOLD_BETWEEN_TURNS=<path>`: this writes `<path>.entered`
/// as it reaches the boundary and waits for `<path>` to appear. Read from the
/// environment of the process driving the run — a library caller's own — rather
/// than from a graph's `env:`, which is exported to member processes and would
/// never reach this thread.
#[cfg(feature = "test-doubles")]
fn hold_between_turns(observation: &Observation<'_>) {
    let Observation::TurnClosed(closed) = observation else {
        return;
    };
    if !matches!(closed.role, onejudge::Role::User) {
        return;
    }
    let Ok(named) = std::env::var("ONEAGENTGRAPH_FIXTURE_HOLD_BETWEEN_TURNS") else {
        return;
    };
    let gate = PathBuf::from(named);
    let mut entered = gate.clone().into_os_string();
    entered.push(".entered");
    let _ = std::fs::write(PathBuf::from(entered), "entered");
    let deadline = Instant::now() + FIXTURE_HOLD;
    while !gate.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Watch a member's engine to its end: the two watchdogs, and the answer.
///
/// Split from [`run`] for [`crate::harness::run`]'s reason, which is now this
/// module's too: the panic containment both members rely on can only be driven
/// against a real thread that really panics — see
/// [`tests::a_panicking_engine_kills_its_own_member_and_not_the_process`].
// Eight values, none derivable from another: where the answer arrives, the lever
// that stops the engine, where the events go, the bounds, the member's scratch,
// the two halves of the activity clock, and how this member's note seam is closed.
#[allow(clippy::too_many_arguments)]
fn supervise(
    rx: &mpsc::Receiver<Answer>,
    abort: &Arc<AtomicBool>,
    emitter: &Emitter,
    bounds: Bounds,
    scratch: &Path,
    started: Instant,
    activity: &Arc<AtomicU64>,
    ending: Option<crate::note::Ending>,
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
            Ok(answer) => {
                // Before the settle, because the settle is what makes the record
                // say this member is over: a note already in the spool is
                // answered by the conversation that could not take it rather than
                // left for a caller to time out on.
                if let Some(ending) = ending.as_ref() {
                    ending.end(&terminal_refusal(&answer));
                }
                return finish(answer, emitter, scratch);
            }
            // A sender dropped without an answer means the engine thread
            // panicked: this member's failure, not the graph's.
            //
            // llmlint: ignore-block[changed_behavior_has_e2e] nothing a graph,
            // task or config can ask for makes `onejudge` panic, so this arm has
            // no journey; forcing one would mean replacing the engine it
            // protects. Covered by
            // `tests::a_panicking_engine_kills_its_own_member_and_not_the_process`.
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return died(
                    emitter,
                    Rule::ProviderFailure,
                    Cause::Unclassified,
                    "the onejudge engine ended without answering",
                );
            }
            // llmlint: ignore-end[changed_behavior_has_e2e]
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let _ = std::fs::write(&heartbeat_file, elapsed_millis(started).to_string());
        let now = Instant::now();
        // llmlint: ignore-block[changed_behavior_has_e2e] the two `end` calls below
        // add no behaviour of their own: they close the note seam on the paths
        // where a *condemned* member never reaches the answer above, so a note in
        // its spool is refused rather than left for its caller to time out on.
        // What they do is `crate::note::Ending::end`, driven across a real spool
        // by that module's
        // `a_member_that_stops_taking_notes_refuses_them_rather_than_accepting_one`,
        // and the watchdogs that reach them are driven by `tests/e2e/liveness.rs`.
        // A journey joining the two would have to send a note into a member and
        // then wedge it for a whole watchdog bound — thirty minutes by default —
        // to assert a refusal both halves already assert.
        if now.duration_since(last_heartbeat) > bounds.heartbeat {
            if let Some(ending) = ending.as_ref() {
                ending.end(&crate::note::Undelivered::MemberSettled {
                    outcome: "the member was condemned by its heartbeat watchdog".to_string(),
                });
            }
            return condemn(rx, abort, emitter, Rule::Heartbeat, scratch);
        }
        last_heartbeat = now;
        if now.duration_since(published) >= publish_every {
            published = now;
            emitter.emit(EventKind::MemberHeartbeat, payload([]));
        }
        if stall.condemns(activity.load(Ordering::SeqCst), scratch) {
            if let Some(ending) = ending.as_ref() {
                ending.end(&crate::note::Undelivered::MemberSettled {
                    outcome: "the member was condemned by its activity watchdog".to_string(),
                });
            }
            return condemn(rx, abort, emitter, Rule::Activity, scratch);
        }
        // llmlint: ignore-end[changed_behavior_has_e2e]
    }
}

/// The answer the engine thread sends back.
type Answer = Result<RunSummary, Box<RunFailure>>;

/// The refusal every note that arrives after this member is over will get.
///
/// Named for what it answers rather than for the happy path through it: a
/// conversation the supervisor *passed* is only one of the ways to get here, and
/// a failed one and one that merely settled reach it too. Which of them it was is
/// the part a caller acts on — a passed conversation needs no relaunch and the
/// note is a follow-up, while one that ended may well be worth starting again —
/// so the reason carried is the supervisor's own where it gave one: a `done_when`
/// it re-judged, or the settled reason a loop that ended without a completion
/// decision records.
fn terminal_refusal(answer: &Answer) -> crate::note::Undelivered {
    let Ok(summary) = answer else {
        let Err(failure) = answer else {
            unreachable!("the arm above")
        };
        return crate::note::Undelivered::MemberSettled {
            outcome: format!("its conversation failed: {}", failure.error),
        };
    };
    let why = summary
        .done_when
        .as_ref()
        .map(|done| {
            format!(
                "its supervisor judged {:?} {}",
                done.criterion,
                if done.satisfied {
                    "satisfied"
                } else {
                    "unsatisfied"
                }
            )
        })
        .or_else(|| summary.report.settled_reason.clone())
        .unwrap_or_else(|| {
            if summary.completed {
                "its conversation completed".to_string()
            } else {
                "its conversation ended without completing".to_string()
            }
        });
    // The two are not interchangeable to a caller: a conversation the supervisor
    // *passed* needs no relaunch and a note against it is a follow-up, while one
    // that merely ended may well be worth starting again.
    if summary.completed {
        crate::note::Undelivered::ConversationCompleted {
            completion_reason: why,
        }
    } else {
        crate::note::Undelivered::MemberSettled { outcome: why }
    }
}

/// Everything this crate has to do to a process onejudge is about to start:
/// place it in this member's [`crate::scratch::Group`], and — for the agent side
/// only — name the config that side runs under.
///
/// **The group** is what `cancel --kill` and the reap reach a member's tree
/// through, and in-process the `oneharness run` for each side is spawned by this
/// process rather than into a group of its own. One hook for both sides on
/// purpose: onejudge installs it on both backends of a `split`, so one
/// termination ends the pair.
///
/// **The config** is why the graph's `--dir` can reach this member's harness —
/// see [`crate::invoke::JudgeLaunch::agent_config`], including the upstream key
/// that would retire the arm. Appending is safe in both directions: oneharness
/// takes the last `--config` it is given, so a future onejudge that passes one is
/// overridden rather than collided with, and only `respond` is touched — a judge
/// side and a `judge: {command: [...]}` provider stay byte-identical.
struct MemberSpawn {
    /// The group every process onejudge starts for this member joins.
    group: Arc<crate::scratch::Group>,
    /// The stamped config the **agent** side runs under.
    agent_config: PathBuf,
}

impl onejudge::SpawnHook for MemberSpawn {
    fn spawning(
        &self,
        command: &mut std::process::Command,
        context: &onejudge::SpawnContext<'_>,
    ) -> std::io::Result<()> {
        if context.role == onejudge::TelemetryRole::Agent {
            command.arg("--config").arg(&self.agent_config);
        }
        self.group.prepare(command)
    }

    fn spawned(
        &self,
        child: &std::process::Child,
        _context: &onejudge::SpawnContext<'_>,
    ) -> std::io::Result<Option<String>> {
        self.group.adopt(child).map(Some)
    }
}

/// Resolve the member's effective config into onejudge's own plan.
///
/// Every step here is the one `onejudge run` itself takes, in the same order:
/// the file is parsed, a config-file `skill:` is rebased against the config's own
/// directory — through [`crate::anchor`] — the `ONEJUDGE_*` environment is
/// applied, and the run's task beats it. What this adds is the last line, and it
/// is what replaces changing directory: a conversation with no skill directory of
/// its own is anchored to this member's worktree by *name*, and onejudge puts that
/// name on the agent side's `oneharness run --cwd` — so the member works in the
/// directory the graph was given without any member touching the process's own
/// working directory.
///
/// What it no longer has to carry is the agent side's config: that rides
/// [`MemberSpawn`] to the same side's `--config`, which is what let this become
/// the operator's directory rather than the member's scratch.
fn plan(launch: &JudgeLaunch) -> Result<onejudge::cli::Plan, String> {
    let text = std::fs::read_to_string(&launch.config)
        .map_err(|err| format!("cannot read {}: {err}", launch.config.display()))?;
    let mut config = Config::from_yaml(&text).map_err(|err| err.to_string())?;
    if let Some(named) = config.skill.take() {
        config.skill = Some(crate::anchor::anchored_path(launch.config.parent(), &named));
    }
    config
        .apply(Overrides::from_env(|key| std::env::var(key).ok()).map_err(|err| err.to_string())?);
    config.apply(Overrides {
        task: Some(launch.task.clone()),
        ..Overrides::default()
    });
    let mut plan = config.into_plan().map_err(|err| err.to_string())?;
    if plan.conversation.skill.dir == "." {
        plan.conversation.skill.dir = launch.worktree.display().to_string();
    }
    Ok(plan)
}

/// Turn one live observation into the envelope that carries it.
///
/// Total over the seam: every observation onejudge produces is published, and
/// nothing is dropped for being unnamed. The turn index no longer has to be
/// inferred from an event that moved past it — the engine opens and closes each
/// turn itself, so what a consumer reads is the conversation's own structure
/// rather than this crate's reconstruction of it.
fn ingest(observation: &Observation<'_>, emitter: &Emitter) {
    match observation {
        Observation::TurnOpened(opened) => {
            // Head-bounded: `bound_text` keeps a tail, so a field that keeps
            // its opening trims to the same constant on a character boundary
            // first and counts that trim as a cut.
            let mut head = MAX_PAYLOAD_TEXT_BYTES.min(opened.instruction.len());
            while !opened.instruction.is_char_boundary(head) {
                head -= 1;
            }
            let (instruction, cut) = bound_text(&opened.instruction[..head]);
            let instruction_truncated = cut || head < opened.instruction.len();
            emitter.emit(
                EventKind::TurnStarted,
                as_payload(&TurnStarted {
                    turn: opened.turn as u64,
                    role: party(opened.role).as_str().to_string(),
                    instruction,
                    instruction_truncated,
                    started_at: opened.started_at.clone(),
                }),
            );
        }
        Observation::Tool(event) => {
            // Through the payload builder both member kinds share — see
            // [`crate::member::activity`].
            let event = event.event;
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
        Observation::Message(message) => {
            // Head-bounded, on the same terms as the instruction above.
            let mut head = MAX_PAYLOAD_TEXT_BYTES.min(message.text.len());
            while !message.text.is_char_boundary(head) {
                head -= 1;
            }
            let (text, cut) = bound_text(&message.text[..head]);
            let truncated = cut || head < message.text.len();
            emitter.emit(
                EventKind::TurnMessage,
                as_payload(&crate::event::TurnMessage {
                    turn: message.turn as u64,
                    role: party(message.role).as_str().to_string(),
                    text,
                    truncated,
                }),
            );
        }
        Observation::TurnClosed(closed) => {
            emitter.emit(
                EventKind::TurnCompleted,
                as_payload(&TurnCompleted {
                    turn: closed.turn as u64,
                    role: party(closed.role).as_str().to_string(),
                    usage: closed.usage.map(usage).unwrap_or_default(),
                    started_at: closed.started_at.clone(),
                    finished_at: closed.finished_at.clone(),
                }),
            );
        }
    }
}

/// One party of the conversation, as this crate's own closed set.
///
/// Total, so there is no fallback that could put a party on the wire that is not
/// one — and a variant added upstream is a compile error here rather than an
/// event nobody can attribute. The unit test below holds each arm against
/// onejudge's own serialization, so the two spellings cannot drift.
fn party(role: onejudge::Role) -> Party {
    match role {
        onejudge::Role::Assistant => Party::Assistant,
        onejudge::Role::User => Party::User,
        onejudge::Role::System => Party::System,
    }
}

/// One turn's accounting, field for field with onejudge's own.
///
/// Spelled out rather than round-tripped through JSON so a field added upstream
/// is a compile error here rather than a figure silently dropped.
fn usage(usage: &onejudge::Usage) -> crate::event::Usage {
    let onejudge::Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cost_usd,
    } = *usage;
    crate::event::Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cost_usd,
    }
}

/// Settle or condemn a member whose engine answered.
fn finish(answer: Answer, emitter: &Emitter, scratch: &Path) -> Outcome {
    match answer {
        Ok(summary) => {
            // Published before the verdict, not conditionally on it: which
            // subscription to restore is exactly what an operator needs, and a
            // chain records every candidate it stepped past whether or not a
            // later one ran.
            publish_attribution(emitter, summary.report.telemetry.as_ref());
            record_control(&summary.report, scratch);
            let completed = onejudge::cli::exit_code(&summary) == 0;
            let document = serde_json::to_value(&summary.report).unwrap_or(Value::Null);
            settle_report(emitter, &document, completed, scratch)
        }
        Err(failure) => {
            // The one place harness attribution for a *failed* run is reachable
            // at all: a run that produced no report used to reach a supervisor
            // with nothing said about which identity refused.
            publish_attribution(emitter, failure.telemetry.as_ref());
            // The stop files are the supervisor's authoritative cancellation
            // request. Do not infer it from the provider process's exit shape:
            // POSIX reports a signal while Windows job termination reports an
            // ordinary nonzero exit, and onejudge consequently classifies the
            // same operator action differently on the two platforms.
            if cancellation_requested(scratch) {
                return died(
                    emitter,
                    Rule::ProviderFailure,
                    Cause::Cancelled,
                    &failure.error.to_string(),
                );
            }
            // A producer's classification is read against the record oneharness
            // wrote beside it before it becomes the cause of a death — see
            // [`reconcile`].
            let cause = provider_cause(&failure);
            let detail = failure.error.to_string();
            match reconcile(cause, turn_record(failure.telemetry.as_ref())) {
                Reconciled::Completed(record) => carried(
                    emitter,
                    &format!(
                        "the provider classified this turn {}, but {record}, so the turn it \
                         describes is carried rather than published as a death — {detail}",
                        cause.as_str()
                    ),
                    failure.telemetry.as_ref(),
                    scratch,
                ),
                Reconciled::Disputed(record) => died(
                    emitter,
                    Rule::ProviderFailure,
                    cause,
                    &format!("{detail} — classified {}, while {record}", cause.as_str()),
                ),
                Reconciled::Unchallenged => died(emitter, Rule::ProviderFailure, cause, &detail),
            }
        }
    }
}

/// How a producer's classification stands against the harness record beside it.
///
/// A `member-died` is the one event a supervisor destroys finished work on, and
/// this crate publishes one on a *classification* — a single field a producer
/// filled in. oneharness writes the whole record for that turn next to it, and a
/// dispatch was lost to the two disagreeing: a node was killed on a
/// `rate_limit` while the record for the same turn read `status: ok`,
/// `exit_code: 0` and billed usage of twelve dollars. `crate::smoke` already
/// refuses a candidate whose record does not back the reason it names; this is
/// that same judgement, applied where it costs a node rather than a probe.
enum Reconciled {
    /// The record describes a turn that ran to completion and was billed, so
    /// there is no death to publish whatever the classification says. Carries
    /// the record's own words for the settle that replaces it.
    Completed(String),
    /// The record and the classification disagree some other way. The death is
    /// published with its cause unchanged — swallowing one would be the opposite
    /// mistake — and names the record too, so a reader sees the disagreement
    /// rather than only the verdict.
    Disputed(String),
    /// Nothing in the record contradicts the classification: it names the same
    /// failure, or there is no record to read. The death is published as it was
    /// classified.
    Unchallenged,
}

/// Judge one classification against the harness record for the same turn.
fn reconcile(cause: Cause, record: Option<&onejudge::CandidateAttempt>) -> Reconciled {
    let Some(record) = record else {
        return Reconciled::Unchallenged;
    };
    // oneharness's own status token for a candidate that ran to completion, and
    // the exit code of a process that did. Both, plus accounting that says the
    // provider was charged, is a turn somebody paid for and got.
    if record.status == "ok" && record.exit_code == Some(0) && billed(record) {
        return Reconciled::Completed(describe(record));
    }
    let named = record
        .failure_kind
        .as_deref()
        .map(onejudge::ProviderErrorKind::classify)
        .map(Cause::from);
    if named == Some(cause) {
        return Reconciled::Unchallenged;
    }
    Reconciled::Disputed(describe(record))
}

/// The harness record for the invocation a failure was classified on.
///
/// The last attribution that names any candidate is the invocation that failed —
/// telemetry is in invocation order — and within it the candidate that *ran* is
/// the turn, falling back to the last one attempted when a chain reached none.
fn turn_record(telemetry: Option<&onejudge::Telemetry>) -> Option<&onejudge::CandidateAttempt> {
    let attribution = telemetry?
        .attribution
        .iter()
        .rev()
        .find(|attribution| !attribution.candidates.is_empty())?;
    attribution
        .candidates
        .iter()
        .find(|candidate| candidate.ran)
        .or_else(|| attribution.candidates.last())
}

/// Whether this record's accounting says the provider billed real work.
///
/// Judged by oneharness's **own** predicate — the one its quota classifier and
/// its fallback chain share, and the one `crate::smoke` holds a candidate to —
/// rather than a second reading of it here. The mapping is spelled out field by
/// field so a signal added upstream is a compile error rather than a figure
/// silently dropped from the judgement.
fn billed(record: &onejudge::CandidateAttempt) -> bool {
    record.usage.as_ref().is_some_and(|usage| {
        let onejudge::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            cost_usd,
        } = *usage;
        oneharness_core::domain::signals::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            cost_usd,
        }
        .reports_billed_work()
    })
}

/// The record's own three facts, in the words a reader needs beside a
/// classification: what oneharness called the attempt, what the process exited,
/// and what it was billed.
fn describe(record: &onejudge::CandidateAttempt) -> String {
    let usage = record
        .usage
        .as_ref()
        .and_then(|usage| serde_json::to_string(usage).ok())
        .unwrap_or_else(|| "none".to_string());
    format!(
        "the harness's own record for that turn says status {}, exit code {}, usage {usage}",
        record.status,
        record
            .exit_code
            .map_or_else(|| "none".to_string(), |code| code.to_string()),
    )
}

/// Carry a turn the harness record says completed, instead of killing the member
/// over a classification that record contradicts.
///
/// A `member-settled` with `completed: false`, which `docs/contract.md` is
/// explicit is how a member that failed its task is distinguished from one that
/// died. The artifact is a real onejudge report, carrying the reconciliation in
/// onejudge's own `settled_reason` — the field that already exists for a run that
/// ended without a completion decision — and the telemetry the record was read
/// from, so an operator can see the turn that was paid for.
fn carried(
    emitter: &Emitter,
    why: &str,
    telemetry: Option<&onejudge::Telemetry>,
    scratch: &Path,
) -> Outcome {
    let mut report =
        onejudge::Report::new(onejudge::Transcript::default(), Vec::new(), None, false);
    report.settled_reason = Some(why.to_string());
    report.telemetry = telemetry.cloned();
    let document = serde_json::to_value(&report).unwrap_or(Value::Null);
    settle_report(emitter, &document, false, scratch)
}

/// Whether this member's run or the member itself was explicitly stopped.
fn cancellation_requested(scratch: &Path) -> bool {
    let Some(name) = scratch.file_name() else {
        return false;
    };
    let Some(root) = scratch.parent().and_then(Path::parent) else {
        return false;
    };
    let signals = root.join(crate::run::SIGNAL_DIR);
    signals.join("stop").exists()
        || signals
            .join(format!("{}.stop", name.to_string_lossy()))
            .exists()
}

/// The handle the **agent** side's turns are threaded under.
///
/// A member names one session and onejudge's engine gives each party its own
/// under it — `<base>-skill` for the side that does the work and `<base>-user`
/// for the one that supervises it, "always", per `onejudge::cli::Settings`. Only
/// the agent side opens a controllable turn, so only its handle addresses one.
///
/// This is the single place that derivation is written down here, and it is a
/// *provisional* answer with a gate under it: the report replaces the whole
/// address at [`record_control`] with the handle oneharness actually bound, and
/// `tests/e2e/interrupt.rs` asserts the two name the same session — so a rename
/// upstream fails a journey rather than silently addressing nothing.
fn agent_session(member_session: &str) -> String {
    format!("{member_session}-skill")
}

/// Replace this member's provisional control record with what its report says.
///
/// This is the first moment the process learns whether the ask was honored at
/// all: onejudge reports `control` only on the finished run, so a member that
/// spent its whole life on a harness with no lever looked addressable until now.
/// The address is taken *from the report* rather than kept, because that is the
/// one that names oneharness's own store directory — and a `control: null` is the
/// contract's exit-3 case, carrying the reason `control_unavailable` gave.
fn record_control(report: &onejudge::Report, scratch: &Path) {
    let turn = match &report.control {
        Some(address) => crate::control::Turn::Open {
            address: crate::control::Address {
                session: address.session.clone(),
                session_dir: Some(PathBuf::from(&address.session_dir)),
                cwd: PathBuf::from(&address.cwd),
            },
        },
        None => crate::control::Turn::Unavailable {
            reason: report
                .control_unavailable
                .clone()
                .unwrap_or_else(|| "this member's report named no controllable turn".to_string()),
        },
    };
    crate::control::write(scratch, &turn);
}

/// The classified cause of a failed run.
fn provider_cause(failure: &RunFailure) -> Cause {
    match &failure.error {
        onejudge::cli::CliError::Engine(error) => {
            error.kind().map_or(Cause::Unclassified, Cause::from)
        }
        // A config or IO failure named no provider kind, because no provider ran.
        _ => Cause::Unclassified,
    }
}

/// Publish what this member's telemetry attributes: every candidate its chains
/// stepped past, and the oneharness conversation each invocation wrote down.
///
/// A two-party member has one chain per side per turn, and onejudge's telemetry
/// keeps them apart — which is why `role` and `turn` are on both payloads.
/// Nothing like this was reachable while this hop was a subprocess: onejudge's
/// report carries no `fallback` block, so a two-party member published no
/// `fallback-advanced` at all and an operator had to read the identity out of a
/// harness's own history.
///
/// `attribution` is also the only place a *failed* run says either thing, which
/// is why both ride it. The sibling `telemetry.sessions` is not a substitute for
/// the session pointer: a `SessionLink` exists only where the harness exposed a
/// native continuation id, and the agent side of a claude-code dispatch exposes
/// none — so a member's own turns would be exactly the ones missing from it,
/// while `attribution` carries a history id for every one of them.
fn publish_attribution(emitter: &Emitter, telemetry: Option<&onejudge::Telemetry>) {
    let Some(telemetry) = telemetry else {
        return;
    };
    for attribution in &telemetry.attribution {
        for candidate in &attribution.fell_through {
            let advanced = FallbackAdvanced {
                identity: candidate.harness.clone(),
                reason: candidate.reason.clone(),
                role: Some(Role::from(attribution.role)),
                turn: Some(u64::from(attribution.turn_index)),
            };
            emitter.emit(EventKind::FallbackAdvanced, as_payload(&advanced));
        }
        if let Some((session, artifact)) = session_pointer(attribution) {
            emitter.emit_with(
                EventKind::OneharnessSession,
                as_payload(&session),
                vec![artifact],
            );
        }
    }
}

/// Where one invocation's conversation was written down, and how much of it
/// there is.
///
/// [`None`] for an invocation that names no history record: history was off for
/// that side, or no candidate ran at all, and a pointer at a file nobody wrote is
/// worse than the silence an operator can act on. The id comes from the candidate
/// that **ran** — the others' records are attempts, and the artifact id names the
/// record a consumer opens.
///
/// The path is decomposed rather than published whole because the reader takes
/// its three parts: oneharness stores a session at
/// `<history_dir>/<history_project>/<history_session>.jsonl`, and a consumer
/// resolves it through that library rather than by splitting a string this crate
/// chose the shape of. A path outside that layout is refused by
/// [`location::Location::of`], so it publishes nothing.
// llmlint: ignore-block[changed_behavior_has_e2e] two arms below have no journey
// because no input a user can give reaches them, and the one seam this suite
// sanctions does not either: the `history_file` is oneharness's own, written by
// oneharness at a path oneharness chose, and the doubled *harness binary* has no
// say in either. So a path outside that layout, and a file that cannot be read
// where one was named, are upstream faults rather than requests — driven here
// against constructed telemetry, which is the only place they exist. What a run
// really produces is `tests/e2e/session.rs`, which resolves the file through
// oneharness's own reader and so fails if this layout stops being the one.
fn session_pointer(
    attribution: &onejudge::HarnessAttribution,
) -> Option<(OneharnessSession, Artifact)> {
    let file = Path::new(attribution.history_file.as_deref()?);
    let location = location::Location::of(file)?;
    let ran = attribution
        .candidates
        .iter()
        .find(|candidate| candidate.ran)?;
    let history_id = ran.history_id.clone()?;
    let session = OneharnessSession {
        role: Role::from(attribution.role),
        turn: u64::from(attribution.turn_index),
        identity: ran.harness_id.clone(),
        session_id: ran.session_id.clone(),
        history_id: history_id.clone(),
        history_dir: location.dir().to_string(),
        history_project: location.project().to_string(),
        history_session: location.session().to_string(),
    };
    let artifact = Artifact {
        id: history_id,
        kind: ONEHARNESS_SESSION_ARTIFACT.to_string(),
        // The bytes stay in oneharness's store — nothing is copied here — so an
        // unreadable file is a count of zero beside a pointer that still names
        // it, rather than a pointer withheld.
        bytes: std::fs::metadata(file).map_or(0, |file| file.len()),
    };
    Some((session, artifact))
}

/// Where one session file sits in oneharness's history store.
///
/// A module of its own so [`Location`](location::Location)'s fields are
/// unreachable **inside** this file as well as outside it:
/// [`Location::of`](location::Location::of) is the trust boundary on a telemetry
/// path, and a boundary that can be walked around by writing the struct literal
/// a few lines further down is a comment rather than a check.
mod location {
    use std::path::{Component, Path};

    /// The extension oneharness gives a history session file.
    ///
    /// The three values published from a `history_file` are only resolvable for
    /// a path in oneharness's own layout: its reader lists a project directory's
    /// `*.jsonl` and matches a stem against it, so a path ending otherwise
    /// decomposes perfectly well into three fields that open nothing. Stated
    /// here rather than imported because upstream keeps it private; a real run
    /// holds the two together, in `tests/e2e/session.rs`, by asserting the file
    /// oneharness actually wrote carries it.
    const SESSION_EXTENSION: &str = "jsonl";

    /// The three values a consumer resolves a session file with — the store, the
    /// project inside it, and the file's own name — valid by construction.
    pub(super) struct Location {
        dir: String,
        project: String,
        session: String,
    }

    impl Location {
        /// One telemetry `history_file`, checked and taken apart; [`None`] for a
        /// path that is not inside oneharness's own store.
        ///
        /// This is the **trust boundary** on that path, and the check is stricter
        /// than "does it decompose" because of what the three values become:
        /// they are published for someone else to join back together, and
        /// `onepipeline-ui`'s read API joins them and opens the result on its own
        /// host. A component that climbs (`..`) is therefore *refused rather than
        /// normalised* — a normalised path is a path this crate invented, and one
        /// that climbed is an arbitrary file read at the far end rather than a
        /// transcript. Only oneharness's own layout is taken: an absolute path to
        /// a named session file carrying [`SESSION_EXTENSION`], in a project
        /// directory, in a store named by at least one component of its own. A
        /// relative path — including a `.`-prefixed one — is refused with them; a
        /// `.` further along names the same file either way and the parser folds
        /// it out before this sees it. The store must be named because an *empty*
        /// `history_dir` is worse than a wrong one: the consumer's resolver reads
        /// an empty one as unset and answers with the host's own default store,
        /// sending a reader to a stranger's transcripts rather than to none.
        pub(super) fn of(file: &Path) -> Option<Self> {
            // The name without oneharness's extension, which its own reader adds
            // back: the two are checked against each other by a real run, in
            // `tests/e2e/session.rs`. Stripped off the name here rather than read
            // from `Path::file_stem`, because that pair disagrees about a bare
            // `.jsonl` — `extension` calls it extensionless while `file_stem`
            // hands the whole name back — and neither answer is the one a
            // consumer needs.
            let session = file
                .file_name()?
                .to_str()?
                .strip_suffix(SESSION_EXTENSION)
                .and_then(|named| named.strip_suffix('.'))?;
            // An empty name is refused with the paths below rather than
            // published: the consumer joins these three values back into a file
            // name, and an empty session names the extension itself — a dot-file
            // in the project directory rather than the transcript asked for.
            if session.is_empty() {
                return None;
            }
            let mut named: Vec<&str> = Vec::new();
            let mut rooted = false;
            for component in file.components() {
                match component {
                    // A root, or a Windows prefix before one, and only ahead of
                    // every name — never a `..` or a `.` anywhere, at any depth.
                    Component::Prefix(_) | Component::RootDir if named.is_empty() => rooted = true,
                    Component::Normal(part) => named.push(part.to_str()?),
                    _ => return None,
                }
            }
            let [store @ .., project, _session] = named.as_slice() else {
                return None;
            };
            if !rooted || store.is_empty() {
                return None;
            }
            Some(Self {
                // The store and the project as they were written rather than
                // rejoined from the components above, so a path that resolved
                // for oneharness resolves the same way for the reader.
                dir: file.parent()?.parent()?.to_str()?.to_string(),
                project: (*project).to_string(),
                session: session.to_string(),
            })
        }

        /// The store the record was written into, absolute and named.
        pub(super) fn dir(&self) -> &str {
            &self.dir
        }

        /// The project directory inside that store.
        pub(super) fn project(&self) -> &str {
            &self.project
        }

        /// The session file's own name, without its extension.
        pub(super) fn session(&self) -> &str {
            &self.session
        }
    }
}
// llmlint: ignore-end[changed_behavior_has_e2e]

/// Stop a member a watchdog condemned, then report it.
///
/// The escalation this module's own documentation describes: ask, reap, and give
/// up on the thread rather than on the run. The reap is
/// [`crate::scratch::reap_after_cancel`], which is what leaves the ask a moment
/// to be answered.
fn condemn(
    rx: &mpsc::Receiver<Answer>,
    abort: &AtomicBool,
    emitter: &Emitter,
    rule: Rule,
    scratch: &Path,
) -> Outcome {
    abort.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + TEARDOWN_GRACE;
    // llmlint: ignore-block[changed_behavior_has_e2e] [`crate::harness`]'s twin
    // of this line carries the reason: the wait is Windows-only and is asserted
    // at the platform seam.
    let mut reaped = crate::scratch::reap_after_cancel(scratch);
    // llmlint: ignore-end[changed_behavior_has_e2e]
    while Instant::now() < deadline {
        match rx.recv_timeout(TEARDOWN_POLL) {
            // The engine tore itself down and answered. What it says is evidence
            // — the chain it attempted above all — but the member is still dead
            // by the rule that condemned it, not settled by a report it only
            // produced because it was stopped.
            Ok(answer) => {
                let telemetry = match &answer {
                    Ok(summary) => summary.report.telemetry.clone(),
                    Err(failure) => failure.telemetry.clone(),
                };
                publish_attribution(emitter, telemetry.as_ref());
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
}

/// Report a member that died, and say which rule found it and what caused it.
fn died(emitter: &Emitter, rule: Rule, cause: Cause, detail: &str) -> Outcome {
    let (detail, truncated) = bound_text(detail.trim());
    let payload = MemberDied {
        rule: rule.as_str().to_string(),
        cause,
        detail,
        truncated,
        // A member this process ran in-library was never a process of its own, so
        // it has none of the three facts one leaves behind.
        exit_code: None,
        disposition: None,
        stderr_tail: None,
    };
    emitter.emit(EventKind::MemberDied, as_payload(&payload));
    Outcome::Died(Death { rule, payload })
}

/// Milliseconds since the member started, as the watchdogs count them.
fn elapsed_millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;
    use crate::event::{Envelope, MAX_PAYLOAD_TEXT_BYTES};
    use onejudge::StreamEvent;

    /// A sink a test can read its own events back out of.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<u8>>>);

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

    /// One workspace holding an effective onejudge config the member can run.
    fn workspace(body: &str) -> (tempfile::TempDir, JudgeLaunch) {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join(crate::invoke::ONEJUDGE_CONFIG_FILE);
        std::fs::write(&config, body).expect("config");
        let launch = JudgeLaunch {
            config,
            task: "do the thing".to_string(),
            worktree: dir.path().to_path_buf(),
            agent_config: dir.path().join(crate::invoke::AGENT_CONFIG_FILE),
            session: "run-1-worker".to_string(),
        };
        (dir, launch)
    }

    const EFFECTIVE: &str = concat!(
        "provider:\n  kind: oneharness\n  bin: oneharness\n  stream: true\n",
        "system_prompt: preamble\n",
        "user:\n  persona: lead\n  done_when: done\n  max_turns: 4\n",
    );

    /// The conversation is anchored to the member's own worktree by *name*.
    ///
    /// This is what replaces changing directory: the directory onejudge takes
    /// from the conversation is the one it puts on the agent side's
    /// `oneharness run --cwd`, so it is where the member's harness really works.
    /// What no longer rides it is the agent side's config — see [`MemberSpawn`].
    #[test]
    fn a_member_s_conversation_is_anchored_to_its_own_worktree() {
        let (dir, launch) = workspace(EFFECTIVE);
        let plan = plan(&launch).expect("a plan");
        assert_eq!(
            plan.conversation.skill.dir,
            dir.path().display().to_string(),
            "a member that did not name its worktree would inherit the process's own"
        );
        // The run's task reaches the conversation, and the config's own settings
        // survive resolution.
        assert_eq!(plan.conversation.input, "do the thing");
        assert_eq!(plan.done_when.as_deref(), Some("done"));
    }

    /// A config that names its own skill directory keeps it: that is onejudge's
    /// own meaning for the field, and overriding it would run the harness
    /// somewhere its author did not ask for.
    #[test]
    fn a_config_that_names_a_skill_directory_keeps_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let skill = dir.path().join("skills").join("greeter");
        std::fs::create_dir_all(&skill).expect("skill dir");
        std::fs::write(skill.join("SKILL.md"), "Greet warmly.\n").expect("skill");
        let (_held, mut launch) = workspace(&format!("{EFFECTIVE}skill: skills/greeter\n"));
        // The config is rebased against its own directory, as `onejudge run` does,
        // so move it beside the skill.
        let config = dir.path().join(crate::invoke::ONEJUDGE_CONFIG_FILE);
        std::fs::copy(&launch.config, &config).expect("copy");
        launch.config = config;
        launch.worktree = dir.path().to_path_buf();

        let plan = plan(&launch).expect("a plan");
        // Compared as a *path*, not as a string: the claim is that the plan names
        // *this* directory, and canonicalizing both says so while still failing
        // if the plan names one that does not exist at all.
        assert_eq!(
            std::fs::canonicalize(&plan.conversation.skill.dir)
                .expect("the plan named a directory that is not there"),
            std::fs::canonicalize(&skill).expect("the skill directory"),
        );
        // The config's own preamble first, then the skill's body — onejudge's
        // composition, carried through untouched.
        assert_eq!(
            plan.conversation.skill.instructions,
            "preamble\n\nGreet warmly."
        );
    }

    /// A `skill:` that names its own root is loaded from there, whatever directory
    /// the effective config naming it sits in.
    ///
    /// That directory is a real temporary one because only a base carrying a drive
    /// prefix is one a `join` could re-root `/graphs/api` under. onejudge's refusal
    /// is what names the path back, since a skill directory at the filesystem root
    /// is one no run can read; the relative half of the rule is
    /// [`a_config_that_names_a_skill_directory_keeps_it`].
    #[test]
    fn a_skill_that_names_its_own_root_is_not_re_rooted_under_the_config() {
        let (_dir, launch) = workspace(&format!("{EFFECTIVE}skill: /graphs/api\n"));
        let err = plan(&launch).unwrap_err();
        assert!(err.contains("skill `/graphs/api`"), "{err}");
    }

    /// A config this build cannot resolve is a member that never started, named
    /// as that rather than launched into a run nothing can drive.
    #[test]
    fn a_config_that_cannot_resolve_refuses_before_anything_runs() {
        let (_dir, missing) = workspace(EFFECTIVE);
        let mut absent = missing.clone();
        absent.config = absent.config.with_file_name("nowhere.yaml");
        assert!(plan(&absent).unwrap_err().contains("cannot read"));

        let (_dir, malformed) = workspace("provider: [not, a, mapping]\n");
        assert!(!plan(&malformed).unwrap_err().is_empty());

        let (_dir, mut taskless) = workspace(EFFECTIVE);
        taskless.task = String::new();
        assert!(plan(&taskless).unwrap_err().contains("no task"));

        // And the refusal reaches the stream as a death, not as a settle.
        let (emitter, recorder) = recorded();
        let outcome = run(&absent, &emitter, Bounds::default(), _dir.path());
        assert!(matches!(outcome, Outcome::Unstartable(_)), "{outcome:?}");
        let kinds: Vec<_> = recorder
            .events()
            .into_iter()
            .map(|event| event.kind)
            .collect();
        assert_eq!(kinds, vec![EventKind::MemberDied]);
    }

    fn call() -> onejudge::ToolEvent {
        onejudge::ToolEvent {
            kind: "tool_call".into(),
            name: Some("bash".into()),
            input: Some(json!({"command": "just check", "n": 3})),
            output: None,
            index: 0,
            tool_call_id: Some("t1".into()),
        }
    }

    /// The whole of a turn reaches the stream: its opening and what it was asked,
    /// each tool call, the party's own reply, and the turn's own cost and bounds.
    ///
    /// Every one of these is an observation this crate already received and threw
    /// away one line into `ingest`, which is why a view built on the journal could
    /// not show what the agent did or what it said.
    #[test]
    fn a_whole_turn_reaches_the_stream_as_it_happens() {
        let (emitter, recorder) = recorded();
        let call = call();
        let usage = onejudge::Usage {
            input_tokens: Some(900),
            output_tokens: Some(120),
            cache_read_tokens: None,
            cache_write_tokens: None,
            cost_usd: Some(0.42),
        };
        for observation in [
            Observation::TurnOpened(onejudge::TurnOpened {
                turn: 1,
                role: onejudge::Role::Assistant,
                instruction: "write the thing",
                started_at: "2026-08-21T09:15:02.847Z".into(),
            }),
            Observation::Tool(StreamEvent {
                turn: 1,
                event: &call,
            }),
            Observation::Message(onejudge::TurnMessage {
                turn: 1,
                role: onejudge::Role::Assistant,
                text: "done",
            }),
            Observation::TurnClosed(onejudge::TurnClosed {
                turn: 1,
                role: onejudge::Role::Assistant,
                usage: Some(&usage),
                started_at: "2026-08-21T09:15:02.847Z".into(),
                finished_at: "2026-08-21T09:16:11.002Z".into(),
            }),
        ] {
            ingest(&observation, &emitter);
        }

        let events = recorder.events();
        let kinds: Vec<_> = events.iter().map(|event| event.kind).collect();
        assert_eq!(
            kinds,
            vec![
                EventKind::TurnStarted,
                EventKind::TurnActivity,
                EventKind::TurnMessage,
                EventKind::TurnCompleted,
            ]
        );
        assert_eq!(events[0].payload["turn"], json!(1));
        assert_eq!(events[0].payload["role"], json!("assistant"));
        assert_eq!(events[0].payload["instruction"], json!("write the thing"));
        assert_eq!(
            events[0].payload["started_at"],
            json!("2026-08-21T09:15:02.847Z")
        );
        assert_eq!(events[1].payload["name"], json!("bash"));
        assert_eq!(events[1].payload["kind"], json!("tool_call"));
        assert_eq!(events[1].payload["detail"], json!("just check"));
        assert_eq!(events[1].payload["tool_call_id"], json!("t1"));
        assert_eq!(events[2].payload["text"], json!("done"));
        assert_eq!(events[2].payload["role"], json!("assistant"));
        // This one turn's cost, on this one turn — not a run total served once at
        // the end, which is what a journal carried before.
        assert_eq!(
            events[3].payload["usage"],
            json!({"input_tokens": 900, "output_tokens": 120, "cost_usd": 0.42})
        );
        assert_eq!(
            events[3].payload["finished_at"],
            json!("2026-08-21T09:16:11.002Z")
        );
    }

    /// Every party this crate publishes keeps onejudge's own spelling for it, so
    /// the total mapping above cannot drift from the engine it converts.
    #[test]
    fn every_party_keeps_the_spelling_the_engine_gives_it() {
        for role in [
            onejudge::Role::Assistant,
            onejudge::Role::User,
            onejudge::Role::System,
        ] {
            assert_eq!(
                serde_json::to_value(role).expect("serializes"),
                json!(party(role).as_str()),
                "onejudge renamed {role:?}"
            );
        }
    }

    /// A supervisor turn that reported no usage closes with an empty object, not
    /// with zeroes: a turn nobody accounted for is a different fact from one that
    /// cost nothing.
    #[test]
    fn a_turn_whose_provider_reported_nothing_publishes_no_figures_at_all() {
        let (emitter, recorder) = recorded();
        ingest(
            &Observation::TurnClosed(onejudge::TurnClosed {
                turn: 2,
                role: onejudge::Role::User,
                usage: None,
                started_at: "2026-08-21T09:16:11.002Z".into(),
                finished_at: "2026-08-21T09:16:12.500Z".into(),
            }),
            &emitter,
        );
        let events = recorder.events();
        assert_eq!(events[0].payload["role"], json!("user"));
        assert_eq!(events[0].payload["usage"], json!({}));
    }

    /// A `tool_result` names no tool because it answers one already named, and it
    /// is published all the same — carrying the observation and the identity of
    /// the call it answers.
    #[test]
    fn an_observation_reaches_the_stream_with_the_identity_of_the_call_it_answers() {
        let (emitter, recorder) = recorded();
        let result = onejudge::ToolEvent {
            kind: "tool_result".into(),
            name: None,
            input: None,
            output: Some("2 passed; 0 failed".into()),
            index: 1,
            tool_call_id: Some("t1".into()),
        };
        ingest(
            &Observation::Tool(StreamEvent {
                turn: 1,
                event: &result,
            }),
            &emitter,
        );
        let events = recorder.events();
        assert_eq!(events.len(), 1, "the observation was discarded: {events:?}");
        assert_eq!(events[0].payload["kind"], json!("tool_result"));
        assert_eq!(events[0].payload["name"], Value::Null);
        assert_eq!(events[0].payload["output"], json!("2 passed; 0 failed"));
        assert_eq!(events[0].payload["tool_call_id"], json!("t1"));
        assert_eq!(events[0].payload["index"], json!(1));
        assert_eq!(events[0].payload["detail"], json!(""));
    }

    /// The three long fields are bounded from the end each is read from, at this
    /// crate's one published payload bound and each saying whether it was cut:
    /// the instruction a turn answers and the party's own reply keep their
    /// opening, an observation keeps its tail.
    ///
    /// Every text here is multi-byte behind an odd-length ASCII opening, so the
    /// bound lands *inside* a character and the two head-keeping sites have to
    /// walk back to the previous boundary. That is the case a naive slice panics
    /// on, and each site owns its own trim — so each is driven here rather than
    /// one shared helper being trusted for all three.
    #[test]
    fn each_live_text_is_bounded_from_the_end_it_is_read_from() {
        let (emitter, recorder) = recorded();
        // `é` is two bytes and `the plan is` is eleven, so every character
        // boundary past the opening is odd and the even bound falls between two.
        let long = format!("the plan is{}", "é".repeat(MAX_PAYLOAD_TEXT_BYTES));
        let asked = format!("write the thing{}", "é".repeat(MAX_PAYLOAD_TEXT_BYTES));
        let result = onejudge::ToolEvent {
            kind: "tool_result".into(),
            name: None,
            input: None,
            output: Some(format!(
                "{}the test suite failed",
                "é".repeat(MAX_PAYLOAD_TEXT_BYTES)
            )),
            index: 1,
            tool_call_id: None,
        };
        ingest(
            &Observation::TurnOpened(onejudge::TurnOpened {
                turn: 1,
                role: onejudge::Role::Assistant,
                instruction: &asked,
                started_at: "2026-08-21T09:15:02.847Z".into(),
            }),
            &emitter,
        );
        ingest(
            &Observation::Message(onejudge::TurnMessage {
                turn: 1,
                role: onejudge::Role::Assistant,
                text: &long,
            }),
            &emitter,
        );
        ingest(
            &Observation::Tool(StreamEvent {
                turn: 1,
                event: &result,
            }),
            &emitter,
        );

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

        let text = events[1].payload["text"].as_str().expect("a reply");
        assert!(text.starts_with("the plan is"), "{text}");
        assert!(
            text.len() < MAX_PAYLOAD_TEXT_BYTES,
            "the cut did not walk back to a boundary: {}",
            text.len()
        );
        assert_eq!(events[1].payload["truncated"], json!(true));

        let output = events[2].payload["output"]
            .as_str()
            .expect("an observation");
        assert!(output.ends_with("the test suite failed"), "{output}");
        assert!(output.len() <= MAX_PAYLOAD_TEXT_BYTES);
        assert_eq!(events[2].payload["output_truncated"], json!(true));
    }

    /// A text that stops exactly on the bound is served whole and says it was not
    /// cut — the boundary either side of the head-trim's walk-back.
    #[test]
    fn a_text_that_ends_on_the_bound_is_not_reported_as_cut() {
        let (emitter, recorder) = recorded();
        let exact = "é".repeat(MAX_PAYLOAD_TEXT_BYTES / 2);
        assert_eq!(exact.len(), MAX_PAYLOAD_TEXT_BYTES);
        ingest(
            &Observation::Message(onejudge::TurnMessage {
                turn: 1,
                role: onejudge::Role::Assistant,
                text: &exact,
            }),
            &emitter,
        );

        let events = recorder.events();
        assert_eq!(events[0].payload["text"], json!(exact));
        assert_eq!(
            events[0].payload.get("truncated"),
            None,
            "an uncut reply claimed a cut: {:?}",
            events[0].payload
        );
    }

    /// Every candidate each side stepped past is published, carrying the side and
    /// the turn onejudge attributed it to — which is what a two-party member could
    /// not say at all while this hop was a subprocess.
    #[test]
    fn every_candidate_a_side_stepped_past_is_published_with_its_side_and_turn() {
        let telemetry: onejudge::Telemetry = serde_json::from_value(json!({
            "wall_ms": 10, "orchestration_ms": 1,
            "agent": {}, "judge": {}, "sessions": [],
            "attribution": [
                {"role": "agent", "turn_index": 1, "ran": "codex",
                 "fell_through": [{"harness": "claude-code", "reason": "quota"}],
                 "candidates": []},
                {"role": "judge", "turn_index": 2, "ran": "claude-code",
                 "fell_through": [], "candidates": []},
            ],
        }))
        .expect("telemetry");

        let (emitter, recorder) = recorded();
        publish_attribution(&emitter, Some(&telemetry));
        let events = recorder.events();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].kind, EventKind::FallbackAdvanced);
        assert_eq!(events[0].payload["identity"], json!("claude-code"));
        assert_eq!(events[0].payload["reason"], json!("quota"));
        assert_eq!(events[0].payload["role"], json!("agent"));
        assert_eq!(events[0].payload["turn"], json!(1));

        // A run with no telemetry publishes nothing rather than an invented chain.
        let (quiet, unwritten) = recorded();
        publish_attribution(&quiet, None);
        assert!(unwritten.events().is_empty());
    }

    /// One attribution's session file, decomposed into the three values a
    /// consumer resolves it with, beside the artifact that names the record.
    ///
    /// The file here does not exist, which is the *unreadable* case: the pointer
    /// is still published, carrying a count of zero, because where the record is
    /// is what an operator needs and how big it is is not. That the same three
    /// values open a file oneharness really wrote is `tests/e2e/session.rs`.
    #[test]
    fn an_invocation_that_wrote_a_record_publishes_where_to_read_it() {
        let telemetry: onejudge::Telemetry = serde_json::from_value(json!({
            "wall_ms": 10, "orchestration_ms": 1,
            "agent": {}, "judge": {}, "sessions": [],
            "attribution": [
                {"role": "judge", "turn_index": 2, "ran": "claude-code:alternate",
                 "fell_through": [],
                 "candidates": [
                     {"harness": "codex", "harness_id": "codex", "status": "skipped",
                      "available": false, "ran": false, "history_id": "the-attempt"},
                     {"harness": "claude-code", "harness_id": "claude-code:alternate",
                      "status": "ok", "available": true, "ran": true,
                      "session_id": "54e7ad34", "history_id": "01a00d0f"},
                 ],
                 "history_file": "/state/oneharness/history/a-project/worker-skill-1.jsonl"},
            ],
        }))
        .expect("telemetry");

        let (emitter, recorder) = recorded();
        publish_attribution(&emitter, Some(&telemetry));
        let events = recorder.events();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].kind, EventKind::OneharnessSession);
        assert_eq!(
            events[0].payload,
            json!({
                "role": "judge",
                "turn": 2,
                "identity": "claude-code:alternate",
                "session_id": "54e7ad34",
                "history_id": "01a00d0f",
                "history_dir": "/state/oneharness/history",
                "history_project": "a-project",
                "history_session": "worker-skill-1",
            })
            .as_object()
            .cloned()
            .expect("an object"),
            "the pointer is built from the candidate that ran, not from an attempt"
        );
        assert_eq!(
            events[0].artifacts,
            vec![crate::event::Artifact {
                id: "01a00d0f".to_string(),
                kind: ONEHARNESS_SESSION_ARTIFACT.to_string(),
                bytes: 0,
            }]
        );
        // The label a consumer renders a transcript from is not on this event:
        // it names where a conversation is, and is not a turn of one.
        assert_eq!(
            events[0].labels.extra.get(crate::event::SESSION_LABEL),
            None
        );
    }

    /// One attribution as telemetry, for the arms that turn on a single field.
    fn one_attribution(attribution: Value) -> onejudge::Telemetry {
        serde_json::from_value(json!({
            "wall_ms": 1, "orchestration_ms": 0,
            "agent": {}, "judge": {}, "sessions": [],
            "attribution": [attribution],
        }))
        .expect("telemetry")
    }

    /// The candidate that ran and wrote a record, which the arms below keep
    /// fixed so the field under test is the only thing that changed.
    fn a_candidate_that_ran() -> Value {
        json!({"harness": "claude-code", "harness_id": "claude-code", "status": "ok",
               "available": true, "ran": true, "history_id": "01a00d0f"})
    }

    /// An invocation with no record to point at publishes nothing.
    ///
    /// Three ways there is none, all ordinary rather than exceptional: history
    /// was off for that side, no candidate ran at all, and the candidate that ran
    /// wrote no record.
    #[test]
    fn an_invocation_with_no_record_publishes_no_pointer() {
        for telemetry in [
            // History off: the invocation named no file.
            one_attribution(
                json!({"role": "agent", "turn_index": 1, "ran": "claude-code",
                                   "fell_through": [], "candidates": [a_candidate_that_ran()]}),
            ),
            // Nothing ran: every candidate refused, records of their own or not.
            one_attribution(json!({"role": "agent", "turn_index": 1, "fell_through": [],
                                   "candidates": [{"harness": "codex", "harness_id": "codex",
                                                   "status": "skipped", "available": false,
                                                   "ran": false, "history_id": "the-attempt"}],
                                   "history_file": "/state/h/p/s.jsonl"})),
            // It ran, but wrote nothing to point at.
            one_attribution(json!({"role": "agent", "turn_index": 1, "ran": "codex",
                                   "fell_through": [],
                                   "candidates": [{"harness": "codex", "harness_id": "codex",
                                                   "status": "ok", "available": true,
                                                   "ran": true}],
                                   "history_file": "/state/h/p/s.jsonl"})),
        ] {
            let (emitter, recorder) = recorded();
            publish_attribution(&emitter, Some(&telemetry));
            assert!(
                recorder.events().is_empty(),
                "a pointer at nothing was published: {:?}",
                recorder.events()
            );
        }
    }

    /// A `history_file` that is not a path inside oneharness's own store
    /// publishes nothing — refused rather than tidied into three fields.
    ///
    /// This is the trust boundary, and what makes it one is what the three
    /// fields become: a consumer joins them back together and opens the result on
    /// its own host, so a component that climbs out of the store is an arbitrary
    /// file read there rather than a transcript. Every path here decomposes
    /// perfectly well into a store, a project and a session — which is exactly
    /// why decomposing cannot be the check.
    #[test]
    fn a_history_file_that_could_escape_the_store_publishes_nothing() {
        for history_file in [
            // Climbing out of the store, from the middle and from the front.
            "/state/h/p/../../../../etc/shadow.jsonl",
            "/state/h/../../p/s.jsonl",
            "../../../../etc/p/s.jsonl",
            // Relative, so three fields anchored to whatever working directory
            // the consumer happens to have rather than to a store.
            "./p/s.jsonl",
            "p/s.jsonl",
            // Absolute, but with nothing named above the project — which
            // publishes a `history_dir` that is empty or a bare root, and the
            // consumer's resolver reads an empty one as unset and answers with
            // the host's own default store, sending a reader to a stranger's
            // transcripts rather than to none.
            "/p/s.jsonl",
            "/s.jsonl",
            // Not a session file at all: three fields that open nothing, however
            // neatly the path splits into them.
            "/state/h/p/s.txt",
            "/state/h/p/s",
        ] {
            let telemetry = one_attribution(json!({
                "role": "agent", "turn_index": 1, "ran": "claude-code", "fell_through": [],
                "candidates": [a_candidate_that_ran()], "history_file": history_file,
            }));
            let (emitter, recorder) = recorded();
            publish_attribution(&emitter, Some(&telemetry));
            assert!(
                recorder.events().is_empty(),
                "{history_file:?} was published for a consumer to resolve: {:?}",
                recorder.events()
            );
        }
    }

    /// A `history_file` that names no session — the extension and nothing else —
    /// publishes nothing rather than a pointer whose session name is empty.
    ///
    /// Its own arm because it is the one malformed path that decomposes into a
    /// store and a project a consumer can reach: `onepipeline-ui`'s read API
    /// joins the three values back into a file name, and an empty session names
    /// the project directory's `.jsonl` dot-file rather than a transcript. The
    /// second path is the same name a directory down, so what is refused is the
    /// empty name itself and not a path too short to take apart.
    #[test]
    fn a_history_file_naming_no_session_publishes_nothing() {
        for history_file in ["/state/h/p/.jsonl", "/state/oneharness/history/p/.jsonl"] {
            let telemetry = one_attribution(json!({
                "role": "agent", "turn_index": 1, "ran": "claude-code", "fell_through": [],
                "candidates": [a_candidate_that_ran()], "history_file": history_file,
            }));
            let (emitter, recorder) = recorded();
            publish_attribution(&emitter, Some(&telemetry));
            assert!(
                recorder.events().is_empty(),
                "{history_file:?} was published with an empty session name: {:?}",
                recorder.events()
            );
        }
    }

    /// A failure onejudge classified reaches the wire as that class; one it could
    /// not classify says so rather than borrowing a class it does not have.
    #[test]
    fn a_failure_is_classified_by_onejudge_or_named_unclassified() {
        let classified = RunFailure {
            error: onejudge::cli::CliError::Engine(onejudge::Error::provider_classified(
                "respond",
                "the subscription is exhausted",
                onejudge::ProviderErrorKind::Quota,
            )),
            telemetry: None,
            processes: Vec::new(),
        };
        assert_eq!(provider_cause(&classified), Cause::Quota);

        let bare = RunFailure {
            error: onejudge::cli::CliError::Engine(onejudge::Error::provider("respond", "boom")),
            telemetry: None,
            processes: Vec::new(),
        };
        assert_eq!(provider_cause(&bare), Cause::Unclassified);

        // A config failure names no provider kind, because no provider ran.
        let config = RunFailure {
            error: onejudge::cli::CliError::Config("no task".into()),
            telemetry: None,
            processes: Vec::new(),
        };
        assert_eq!(provider_cause(&config), Cause::Unclassified);
    }

    /// One failed run, classified `kind`, carrying the harness record `candidate`
    /// as the telemetry of its single invocation.
    ///
    /// Built from onejudge's own JSON so the shape a real dispatch carries is the
    /// shape under test: telemetry reaches this crate as a deserialized
    /// [`onejudge::Telemetry`], and a literal assembled field by field could
    /// disagree with what the wire really holds.
    fn failed_run(kind: onejudge::ProviderErrorKind, candidate: Value) -> Answer {
        Err(Box::new(RunFailure {
            error: onejudge::cli::CliError::Engine(onejudge::Error::provider_classified(
                "respond",
                "the provider said the call was rate limited",
                kind,
            )),
            telemetry: serde_json::from_value(json!({
                "wall_ms": 1, "orchestration_ms": 0, "agent": {}, "judge": {}, "sessions": [],
                "attribution": [{"role": "agent", "turn_index": 1, "ran": "claude-code",
                                 "fell_through": [], "candidates": [candidate]}],
            }))
            .expect("telemetry"),
            processes: Vec::new(),
        }))
    }

    /// The record oneharness writes for a turn that ran to completion and was
    /// billed — the exact shape a dispatch was killed over.
    fn a_completed_billed_turn() -> Value {
        json!({
            "harness": "claude-code", "harness_id": "claude-code", "status": "ok",
            "available": true, "ran": true, "exit_code": 0, "duration_ms": 812_004,
            "usage": {"input_tokens": 41_233, "output_tokens": 9_812, "cost_usd": 12.11},
        })
    }

    /// A `rate_limit` classification is **not** published as a death when the
    /// harness's own record for that turn says it completed and was billed.
    ///
    /// The loss this exists for: a node settled `failed (task-failed)` on a
    /// `member-died` carrying `{"cause":"rate_limit"}` while the record beside it
    /// read `status: ok`, `exit_code: 0` and $12.11 of billed usage. One field was
    /// trusted over the whole record next to it, and two finished dispatches were
    /// destroyed. Driven through the real `finish`, on the record's real shape.
    #[test]
    fn a_classification_the_harness_record_contradicts_is_not_published_as_a_death() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (emitter, recorder) = recorded();

        let outcome = finish(
            failed_run(
                onejudge::ProviderErrorKind::RateLimit,
                a_completed_billed_turn(),
            ),
            &emitter,
            dir.path(),
        );

        assert_eq!(
            outcome,
            Outcome::Incomplete,
            "a turn the record says completed killed its member"
        );
        let kinds: Vec<_> = recorder
            .events()
            .into_iter()
            .map(|event| event.kind)
            .collect();
        assert!(
            !kinds.contains(&EventKind::MemberDied),
            "a member died over a classification its own record contradicts: {kinds:?}"
        );
        assert!(kinds.contains(&EventKind::MemberSettled), "{kinds:?}");
        // The turn the record describes is carried, and what it was carried over
        // is on the artifact rather than left for nobody to find.
        let stored = std::fs::read_to_string(dir.path().join(crate::member::REPORT_FILE))
            .expect("the settle stored its report");
        let report: Value = serde_json::from_str(&stored).expect("a report");
        let settled = report["settled_reason"].as_str().expect("a settle reason");
        assert!(settled.contains("rate_limit"), "{settled}");
        assert!(settled.contains("status ok"), "{settled}");
        assert!(settled.contains("exit code 0"), "{settled}");
        assert!(settled.contains("12.11"), "{settled}");
    }

    /// A classification the record disagrees with some *other* way still kills
    /// the member — swallowing it would be the opposite mistake — but what is
    /// published names both, so a reader sees the disagreement rather than only
    /// the verdict.
    #[test]
    fn a_disagreement_that_is_not_a_completed_turn_names_the_record_beside_the_cause() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (emitter, recorder) = recorded();

        let outcome = finish(
            failed_run(
                onejudge::ProviderErrorKind::RateLimit,
                // Neither a completed billed turn nor a record that backs the
                // reason: oneharness called it `auth`, having spent nothing.
                json!({
                    "harness": "claude-code", "harness_id": "claude-code", "status": "nonzero",
                    "available": true, "ran": true, "exit_code": 2, "failure_kind": "auth",
                }),
            ),
            &emitter,
            dir.path(),
        );

        let Outcome::Died(death) = outcome else {
            panic!("a disputed classification did not kill its member: {outcome:?}");
        };
        // The cause is the classification, unchanged.
        assert_eq!(death.payload.cause, Cause::RateLimit);
        let detail = &death.payload.detail;
        assert!(detail.contains("rate_limit"), "{detail}");
        assert!(detail.contains("status nonzero"), "{detail}");
        assert!(detail.contains("exit code 2"), "{detail}");
        assert!(detail.contains("usage none"), "{detail}");
        assert_eq!(
            recorder
                .events()
                .into_iter()
                .filter(|event| event.kind == EventKind::MemberDied)
                .count(),
            1
        );
    }

    /// A genuine provider death still publishes its cause and its own words
    /// untouched: the reconciliation refuses a classification the record
    /// contradicts, and swallows nothing else.
    ///
    /// The record here is the one `crate::smoke` refuses to excuse — a rate limit
    /// *after* billed work, which is why it carries usage and still is not a
    /// completed turn: `status: nonzero`, and the process exited 1.
    #[test]
    fn a_provider_death_its_record_backs_is_published_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (emitter, recorder) = recorded();

        let outcome = finish(
            failed_run(
                onejudge::ProviderErrorKind::RateLimit,
                json!({
                    "harness": "claude-code", "harness_id": "claude-code", "status": "nonzero",
                    "available": true, "ran": true, "exit_code": 1,
                    "failure_kind": "rate_limit", "failure_kind_source": "stderr",
                    "usage": {"input_tokens": 900, "cost_usd": 0.42},
                }),
            ),
            &emitter,
            dir.path(),
        );

        let Outcome::Died(death) = outcome else {
            panic!("a real provider death was swallowed: {outcome:?}");
        };
        assert_eq!(death.rule, Rule::ProviderFailure);
        assert_eq!(death.payload.cause, Cause::RateLimit);
        assert_eq!(
            death.payload.detail,
            "run failed: provider error (respond): the provider said the call was rate limited",
            "a death its record backs was rewritten"
        );
        assert!(recorder
            .events()
            .into_iter()
            .any(|event| event.kind == EventKind::MemberDied));
    }

    /// A failure with no harness record at all is published as it was classified:
    /// there is nothing beside it to reconcile against, and inventing a doubt
    /// would be the reconciliation deciding what it cannot see.
    #[test]
    fn a_failure_carrying_no_harness_record_is_published_as_classified() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (emitter, _recorder) = recorded();
        let outcome = finish(
            Err(Box::new(RunFailure {
                error: onejudge::cli::CliError::Config("no task".into()),
                telemetry: None,
                processes: Vec::new(),
            })),
            &emitter,
            dir.path(),
        );
        let Outcome::Died(death) = outcome else {
            panic!("a recordless failure settled: {outcome:?}");
        };
        assert_eq!(death.payload.cause, Cause::Unclassified);
        assert_eq!(death.payload.detail, "config error: no task");
    }

    /// A member whose engine thread **panics** fails that member and leaves the
    /// process it shares with every other one running.
    ///
    /// Driven through the real supervision loop with a real panicking thread —
    /// the panic drops the sender, which is what the loop reads as an engine that
    /// ended without answering. Nothing here stands in for the loop; what is
    /// substituted is the engine, because no config can make onejudge panic on
    /// demand and the containment being proven is this crate's. The twin of
    /// `crate::harness::tests::a_panicking_engine_kills_its_own_member_and_not_the_process`,
    /// because since that conversion both member kinds run their engine on a
    /// thread of this one process and a panic in either is contained the same way.
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

        let outcome = supervise(
            &rx,
            &Arc::new(AtomicBool::new(false)),
            &emitter,
            Bounds::default(),
            dir.path(),
            Instant::now(),
            &Arc::new(AtomicU64::new(0)),
            None,
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
    /// the run is not left waiting on it.
    ///
    /// The rule that condemned it is what the death carries — not the report the
    /// engine might still produce — and the detail says how many processes were
    /// signalled, which is the evidence an operator reads.
    #[test]
    fn a_condemned_member_whose_engine_never_answers_is_still_reported_dead() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (emitter, recorder) = recorded();
        let abort = AtomicBool::new(false);
        // A sender that is dropped immediately: the engine is gone without an
        // answer, which is the shape a thread abandoned mid-turn leaves behind.
        let (tx, rx) = mpsc::channel::<Answer>();
        drop(tx);

        let outcome = condemn(&rx, &abort, &emitter, Rule::Activity, dir.path());
        assert!(abort.load(Ordering::SeqCst), "the engine was never asked");
        let Outcome::Died(death) = outcome else {
            panic!("a condemned member settled: {outcome:?}");
        };
        assert_eq!(death.rule, Rule::Activity);
        assert_eq!(death.payload.cause, Cause::Cancelled);
        assert!(death.payload.detail.contains("activity"), "{death:?}");
        assert!(death.payload.exit_code.is_none());
        assert!(death.payload.disposition.is_none());
        assert!(death.payload.stderr_tail.is_none());

        let events = recorder.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::MemberDied);
    }

    /// A condemned member whose engine *does* tear itself down still dies by the
    /// rule that condemned it — its chain is published as evidence, but a report
    /// it only produced because it was stopped is not a settle.
    #[test]
    fn a_condemned_member_that_answers_still_dies_by_its_rule() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (emitter, recorder) = recorded();
        let abort = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel::<Answer>();
        tx.send(Err(Box::new(RunFailure {
            error: onejudge::cli::CliError::Engine(onejudge::Error::provider_classified(
                "respond",
                "torn down",
                onejudge::ProviderErrorKind::Cancelled,
            )),
            telemetry: serde_json::from_value(json!({
                "wall_ms": 1, "orchestration_ms": 0, "agent": {}, "judge": {}, "sessions": [],
                "attribution": [{"role": "agent", "turn_index": 1, "ran": null,
                                 "fell_through": [{"harness": "codex", "reason": "auth"}],
                                 "candidates": []}],
            }))
            .expect("telemetry"),
            processes: Vec::new(),
        })))
        .expect("send");

        let outcome = condemn(&rx, &abort, &emitter, Rule::Heartbeat, dir.path());
        let Outcome::Died(death) = outcome else {
            panic!("a condemned member settled: {outcome:?}");
        };
        assert_eq!(death.rule, Rule::Heartbeat);
        let kinds: Vec<_> = recorder
            .events()
            .into_iter()
            .map(|event| event.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![EventKind::FallbackAdvanced, EventKind::MemberDied],
            "the chain a condemned member attempted is evidence either way"
        );
    }
}
