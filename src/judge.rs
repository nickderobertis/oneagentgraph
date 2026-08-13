//! Running a two-party member through onejudge's library, in this process.
//!
//! `docs/contract.md` says a `kind: onejudge` member is "driven in-process
//! through the onejudge library". This module is that: the effective config
//! [`crate::invoke`] wrote is loaded through onejudge's own
//! [`onejudge::cli::Config`], resolved into its own [`onejudge::cli::Plan`], and
//! driven by its own run driver
//! ([`onejudge::cli::run_plan_streaming_reporting_failure`]).
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
//! **Events arrive live, typed.** The sink is called with a
//! [`StreamEvent`] the instant a tool event is observed —
//! the same events the NDJSON hop carried, without the round-trip through
//! another process's stdout and back through a parser. There is no reconciliation
//! to do: a turn index is a `usize`, not a field that might be missing.
//!
//! **A failure is typed, not a stderr tail.** onejudge classifies a provider
//! failure with its own `ProviderErrorKind` — which is oneharness's normalized
//! `failure_kind` — so `member-died` carries [`crate::event::Cause`] rather than
//! the last four kilobytes of somebody's standard error. That is the whole reason
//! `member-died` changed shape.
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
use onejudge::StreamEvent;
use serde_json::{Map, Value};

use crate::event::{
    bound_detail, bound_text, Cause, Emitter, EventKind, FallbackAdvanced, MemberDied, Role,
};
use crate::invoke::JudgeLaunch;
use crate::member::{
    died_payload, payload, settle_report, summarize, unstartable, Bounds, Death, Outcome, Rule,
    Stall, HEARTBEAT_INTERVAL,
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
    emitter.emit(
        EventKind::MemberStarted,
        payload([
            ("runner", Value::String("library".into())),
            ("engine", Value::String("onejudge".into())),
            ("config", Value::String(launch.config.display().to_string())),
            // `worktree`, not `cwd`: this member has no working directory of its
            // own, and naming one would claim a thing that is not true.
            (
                "worktree",
                Value::String(launch.worktree.display().to_string()),
            ),
        ]),
    );

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
            emitter.emit(EventKind::MemberDied, died_payload(&unstartable(&reason)));
            return Outcome::Unstartable(reason);
        }
    };
    // llmlint: ignore-end[changed_behavior_has_e2e]

    let plan = match plan(launch) {
        Ok(plan) => plan.with_spawn_hook(Arc::new(Grouped(Arc::clone(&group)))),
        Err(reason) => {
            emitter.emit(EventKind::MemberDied, died_payload(&unstartable(&reason)));
            return Outcome::Unstartable(reason);
        }
    };

    // Before the plan is driven, because the turn an operator wants to redirect
    // is the one in flight and onejudge reports `control` only on the *finished*
    // run. Every value here is this crate's own — see [`crate::control`] — and
    // the report replaces it with oneharness's authoritative answer at
    // [`finish`], which is also where an ask that was refused becomes the reason
    // `interrupt` reports.
    crate::control::write(
        scratch,
        &crate::control::Turn::Open {
            address: crate::control::Address {
                session: agent_session(&launch.session),
                session_dir: None,
                cwd: launch.worktree.clone(),
            },
        },
    );

    let activity = Arc::new(AtomicU64::new(0));
    let abort = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let (tx, rx) = mpsc::channel();

    {
        let (emitter, activity, abort) =
            (emitter.clone(), Arc::clone(&activity), Arc::clone(&abort));
        std::thread::spawn(move || {
            let mut turn = 0usize;
            let mut sink = move |event: &StreamEvent<'_>| {
                activity.store(elapsed_millis(started), Ordering::SeqCst);
                ingest(event, &emitter, &mut turn);
                if abort.load(Ordering::SeqCst) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            };
            let answer = onejudge::cli::run_plan_streaming_reporting_failure(plan, &mut sink);
            // A send that fails means the supervisor already condemned this
            // member and stopped listening; the engine still tore itself down on
            // the way here, which is what that teardown was waiting for.
            let _ = tx.send(answer);
        });
    }

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
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // The engine thread is gone without an answer, which only happens
                // if it panicked. That is a member that produced no report, which
                // is the failure below rather than a run this crate takes down.
                return died(
                    emitter,
                    Rule::ProviderFailure,
                    Cause::Unclassified,
                    "the onejudge engine ended without answering",
                );
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let _ = std::fs::write(&heartbeat_file, elapsed_millis(started).to_string());
        let now = Instant::now();
        if now.duration_since(last_heartbeat) > bounds.heartbeat {
            return condemn(&rx, &abort, emitter, Rule::Heartbeat, scratch);
        }
        last_heartbeat = now;
        if now.duration_since(published) >= publish_every {
            published = now;
            emitter.emit(EventKind::MemberHeartbeat, payload([]));
        }
        if stall.condemns(activity.load(Ordering::SeqCst), scratch) {
            return condemn(&rx, &abort, emitter, Rule::Activity, scratch);
        }
    }
}

/// The answer the engine thread sends back.
type Answer = Result<RunSummary, Box<RunFailure>>;

/// This member's [`crate::scratch::Group`], offered to onejudge as the group
/// every process it spawns belongs to.
///
/// The seam exists because driving onejudge in-process took away grouping that a
/// subprocess hop gave for free: while a member *was* `onejudge run`, this crate
/// spawned one process into a group and everything below it joined by
/// inheritance. In-process, the `oneharness run` for each side is spawned by this
/// process — so without this, a two-party member's harnesses sit in whatever
/// group the supervisor is in, and `cancel --kill` has no tree to name.
///
/// One hook for both sides on purpose: onejudge installs it on both backends of a
/// `split`, so the worker's harness and the judge's land in the *same* group, and
/// one `TerminateJobObject` ends the pair.
struct Grouped(Arc<crate::scratch::Group>);

impl onejudge::SpawnHook for Grouped {
    fn spawning(
        &self,
        command: &mut std::process::Command,
        _context: &onejudge::SpawnContext<'_>,
    ) -> std::io::Result<()> {
        self.0.prepare(command)
    }

    fn spawned(
        &self,
        child: &std::process::Child,
        _context: &onejudge::SpawnContext<'_>,
    ) -> std::io::Result<Option<String>> {
        self.0.adopt(child).map(Some)
    }
}

/// Resolve the member's effective config into onejudge's own plan.
///
/// Every step here is the one `onejudge run` itself takes, in the same order:
/// the file is parsed, a config-file `skill:` is rebased against the config's own
/// directory, the `ONEJUDGE_*` environment is applied, and the run's task beats
/// it. What this adds is the last line, and it is what replaces changing
/// directory: a conversation with no skill directory of its own is anchored to
/// this member's worktree by *name*, so oneharness discovers the agent side's
/// `oneharness.toml` there without any member touching the process's own
/// working directory.
fn plan(launch: &JudgeLaunch) -> Result<onejudge::cli::Plan, String> {
    let text = std::fs::read_to_string(&launch.config)
        .map_err(|err| format!("cannot read {}: {err}", launch.config.display()))?;
    let mut config = Config::from_yaml(&text).map_err(|err| err.to_string())?;
    if let Some(base) = launch
        .config
        .parent()
        .filter(|base| !base.as_os_str().is_empty())
    {
        if let Some(named) = config.skill.take() {
            config.skill = Some(if named.is_relative() {
                base.join(named)
            } else {
                named
            });
        }
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

/// Turn one live tool event into envelopes.
///
/// The same two the NDJSON reader in [`crate::member`] produces, and on the same
/// terms: a turn index that moved opens a turn, and an event this crate cannot
/// name is skipped rather than published with a hole in it — a `tool_result`
/// carries no tool name, and reporting one as an unnamed action would say the
/// member did something it cannot name.
fn ingest(event: &StreamEvent<'_>, emitter: &Emitter, turn: &mut usize) {
    let Some(name) = event.event.name.as_deref() else {
        return;
    };
    if event.turn != *turn {
        *turn = event.turn;
        emitter.emit(
            EventKind::TurnStarted,
            payload([("turn", Value::from(event.turn as u64))]),
        );
    }
    let (detail, truncated) = bound_detail(&summarize(event.event.input.as_ref()));
    emitter.emit(
        EventKind::TurnActivity,
        payload([
            ("kind", Value::String(event.event.kind.clone())),
            ("name", Value::String(name.to_string())),
            ("detail", Value::String(detail)),
            ("truncated", Value::Bool(truncated)),
        ]),
    );
}

/// Settle or condemn a member whose engine answered.
fn finish(answer: Answer, emitter: &Emitter, scratch: &Path) -> Outcome {
    match answer {
        Ok(summary) => {
            // Published before the verdict, not conditionally on it: which
            // subscription to restore is exactly what an operator needs, and a
            // chain records every candidate it stepped past whether or not a
            // later one ran.
            advance(emitter, summary.report.telemetry.as_ref());
            record_control(&summary.report, scratch);
            let completed = onejudge::cli::exit_code(&summary) == 0;
            let document = serde_json::to_value(&summary.report).unwrap_or(Value::Null);
            settle_report(emitter, &document, completed, scratch)
        }
        Err(failure) => {
            // The one place harness attribution for a *failed* run is reachable
            // at all: a run that produced no report used to reach a supervisor
            // with nothing said about which identity refused.
            advance(emitter, failure.telemetry.as_ref());
            // The stop files are the supervisor's authoritative cancellation
            // request. Do not infer it from the provider process's exit shape:
            // POSIX reports a signal while Windows job termination reports an
            // ordinary nonzero exit, and onejudge consequently classifies the
            // same operator action differently on the two platforms.
            let cause = if cancellation_requested(scratch) {
                Cause::Cancelled
            } else {
                provider_cause(&failure)
            };
            died(
                emitter,
                Rule::ProviderFailure,
                cause,
                &failure.error.to_string(),
            )
        }
    }
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

/// Publish every candidate this member's chains stepped past.
///
/// A two-party member has one chain per side per turn, and onejudge's telemetry
/// keeps them apart — which is why `role` and `turn` are on the payload. Nothing
/// like this was reachable while this hop was a subprocess: onejudge's report
/// carries no `fallback` block, so a two-party member published no
/// `fallback-advanced` at all and an operator had to read the identity out of a
/// harness's own history.
fn advance(emitter: &Emitter, telemetry: Option<&onejudge::Telemetry>) {
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
    }
}

/// One payload struct as the map the wire carries.
fn as_payload(advanced: &FallbackAdvanced) -> Map<String, Value> {
    match serde_json::to_value(advanced) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// Stop a member a watchdog condemned, then report it.
///
/// The escalation this module's own documentation describes: ask, reap, and give
/// up on the thread rather than on the run.
fn condemn(
    rx: &mpsc::Receiver<Answer>,
    abort: &AtomicBool,
    emitter: &Emitter,
    rule: Rule,
    scratch: &Path,
) -> Outcome {
    abort.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + TEARDOWN_GRACE;
    let mut reaped = crate::scratch::reap(scratch);
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
                advance(emitter, telemetry.as_ref());
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
    emitter.emit(EventKind::MemberDied, died_payload(&payload));
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
    use crate::event::Envelope;

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
    /// This is what replaces changing directory, and it is the whole of how the
    /// agent side's `oneharness.toml` is still found: oneharness discovers a
    /// project config upward from the directory the harness runs in, and that
    /// directory is what onejudge takes from the conversation.
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
        // Compared as a *path*, not as a string. A config names its skill the way
        // its author writes one — `skills/greeter` — and rebasing that is a join,
        // so on Windows the result carries both separators
        // (`C:\…\skills/greeter`). That is a real directory there and onejudge
        // opens it without complaint; only the spelling differs from a
        // `join("skills").join("greeter")`. Canonicalizing both is what asserts
        // the thing that matters — the plan names *this* directory — and it
        // fails if the plan names one that does not exist at all.
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
        assert_eq!(kinds, vec![EventKind::MemberStarted, EventKind::MemberDied]);
    }

    /// One live tool event opens its turn and reports what it acted on; a turn
    /// that has already been opened is not opened twice.
    #[test]
    fn a_live_event_opens_its_turn_once_and_summarizes_what_it_acted_on() {
        let (emitter, recorder) = recorded();
        let mut turn = 0usize;
        let call = onejudge::ToolEvent {
            kind: "tool_call".into(),
            name: Some("bash".into()),
            input: Some(json!({"command": "just check", "n": 3})),
            output: None,
            index: 0,
        };
        ingest(
            &StreamEvent {
                turn: 1,
                event: &call,
            },
            &emitter,
            &mut turn,
        );
        ingest(
            &StreamEvent {
                turn: 1,
                event: &call,
            },
            &emitter,
            &mut turn,
        );
        ingest(
            &StreamEvent {
                turn: 2,
                event: &call,
            },
            &emitter,
            &mut turn,
        );

        let events = recorder.events();
        let kinds: Vec<_> = events.iter().map(|event| event.kind).collect();
        assert_eq!(
            kinds,
            vec![
                EventKind::TurnStarted,
                EventKind::TurnActivity,
                EventKind::TurnActivity,
                EventKind::TurnStarted,
                EventKind::TurnActivity,
            ]
        );
        assert_eq!(events[1].payload["name"], json!("bash"));
        assert_eq!(events[1].payload["kind"], json!("tool_call"));
        assert_eq!(events[1].payload["detail"], json!("just check"));
        assert_eq!(events[3].payload["turn"], json!(2));
    }

    /// An event this crate cannot name is skipped rather than published with a
    /// hole in it — a `tool_result` carries no tool name, and reporting one as an
    /// unnamed action would say the member did something it cannot name.
    #[test]
    fn a_live_event_with_no_tool_name_is_skipped() {
        let (emitter, recorder) = recorded();
        let mut turn = 0usize;
        let result = onejudge::ToolEvent {
            kind: "tool_result".into(),
            name: None,
            input: None,
            output: Some("done".into()),
            index: 1,
        };
        ingest(
            &StreamEvent {
                turn: 1,
                event: &result,
            },
            &emitter,
            &mut turn,
        );
        assert!(recorder.events().is_empty(), "an unnamed event published");
        assert_eq!(turn, 0, "an unnamed event opened a turn");

        // An event with a name but no input still publishes: what it acted on is
        // simply empty.
        let bare = onejudge::ToolEvent {
            name: Some("read".into()),
            ..result
        };
        ingest(
            &StreamEvent {
                turn: 1,
                event: &bare,
            },
            &emitter,
            &mut turn,
        );
        let events = recorder.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].payload["detail"], json!(""));
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
        advance(&emitter, Some(&telemetry));
        let events = recorder.events();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].kind, EventKind::FallbackAdvanced);
        assert_eq!(events[0].payload["identity"], json!("claude-code"));
        assert_eq!(events[0].payload["reason"], json!("quota"));
        assert_eq!(events[0].payload["role"], json!("agent"));
        assert_eq!(events[0].payload["turn"], json!(1));

        // A run with no telemetry publishes nothing rather than an invented chain.
        let (quiet, unwritten) = recorded();
        advance(&quiet, None);
        assert!(unwritten.events().is_empty());
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
