//! Running one member, and turning what it publishes into envelopes.
//!
//! A **single-sided** member is one child process, `oneharness run`, and this
//! module is the whole of running it. A **two-party** member is onejudge's own
//! run driver called in this process; [`crate::judge`] runs that one, and shares
//! the settle, the death, and the two watchdogs below so both kinds reach the
//! stream as the same events.
//!
//! A streaming child member speaks two NDJSON envelopes on stdout (onejudge's
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
//! Whether a member streams at all is **its own oneharness config's** decision —
//! see [`crate::invoke`], which is where the argv is built. A member that does
//! not (one asking for a schema-validated answer, or one that simply said
//! `stream = false`) publishes no envelopes and one report, on one line: the same
//! document, reaching the same settle, with no `turn-activity` before it.
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
//!   published nothing for [`crate::liveness::DEFAULT_STALL_TIMEOUT`] *and has
//!   no live work under it* is not working. Silence alone was the rule once, and
//!   it condemned members that were working: a supervisory member whose turn is
//!   one long child — a whole round — publishes nothing for far longer than the
//!   bound while being entirely healthy, and its teardown took the live worker
//!   underneath it with it. It was style-sensitive, too, which is how it hid: a
//!   supervisor that drove its round by *polling* emitted a tool event every few
//!   seconds and survived, while one that *blocked* on a single call died — same
//!   persona, same graph, opposite verdicts, on a choice the agent makes freely
//!   turn by turn. [`Stall`] is the rule now, and what it adds is the evidence
//!   the old one threw away.
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
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::event::{
    bound_detail, bound_text, Cause, Disposition, Emitter, EventKind, Labels, MemberDied,
    MemberStarted, Runner,
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

/// The activity watchdog, as a clock one member's supervisor keeps.
///
/// The rule it applies is two things rather than one: a member is condemned when
/// it has published nothing for the whole stall bound **and** the process tree
/// stamped for it did nothing in that time either. A member blocked on a child
/// that is doing the work — the shape a supervisory member's turn takes, and the
/// shape that made silence alone the wrong rule — clears the clock on the child's
/// progress and is left alone.
///
/// What counts as progress is a *rate*: the CPU charged to the processes stamped
/// for this member — [`crate::scratch::work`] — against the wall time between two
/// looks at it, held against the share of a core
/// [`crate::scratch::Work::worked`] fixes. Deliberately not "a live child
/// exists" — a wedged member has one of those too, which is precisely why its
/// harness never answers — so the rule that keeps a working member alive is not
/// one that keeps a dead one alive with it. And deliberately not "the reading
/// moved": that asks how finely the platform counts rather than what the member
/// did, and it is why an idle member evaded this watchdog on macOS.
///
/// Two consequences worth stating plainly, because they are the cost of the
/// trade:
///
/// * The bound becomes a **floor** rather than an exact deadline. Establishing
///   that a tree is idle takes two observations, so a condemnation can arrive up
///   to one probe interval late. A backstop measured in minutes does not need
///   the precision, and a member killed for being briefly unobserved is the
///   failure this exists to stop.
/// * A member whose tree spins **forever** is no longer condemned by this rule.
///   That is a real narrowing and it is deliberate: this rule's subject is a
///   member that is idle and silent, which is what a wedged one is. A member
///   burning CPU to no purpose is what `cancel` is for, and the heartbeat rule
///   still answers for a supervisor that cannot confirm its member at all.
#[derive(Debug)]
pub struct Stall {
    /// When the member started, which is the origin of the only clock this rule
    /// counts in.
    ///
    /// Held here rather than passed in beside the member's last event, because
    /// the two were the same bare millisecond count and reversing them at a call
    /// site would invert the rule silently — a member that had just published
    /// would read as one that never had.
    started: Instant,
    /// How long a member may be quiet before this rule condemns it.
    bound: Duration,
    /// How often the tree is examined while a member is quiet enough to be
    /// worth examining.
    probe_every: Duration,
    /// The member's own elapsed milliseconds at the last evidence of live work,
    /// which counts exactly as a published line does.
    cleared: u64,
    /// What the last look at this member's tree established.
    observed: Observed,
}

/// What looking at a member's process tree established, which is a *verdict*
/// rather than a reading.
///
/// One look says what a tree is; the question this rule asks is whether it
/// changed, so an idle verdict cannot exist without the two observations that
/// establish it — which is why the sample and the verdict are one value rather
/// than a sample beside a flag.
#[derive(Debug)]
enum Observed {
    /// Nothing has been looked at: the member is publishing normally, so there
    /// is nothing to explain and nothing worth the cost of a look.
    Nothing,
    /// The tree was doing something — or this is the first look, which is a
    /// baseline and no evidence of idleness at all.
    Moving {
        /// When the sample was taken, which is what the probe cadence counts.
        at: Instant,
        /// The sample itself, to compare the next one against.
        work: crate::scratch::Work,
    },
    /// Two looks agreed: whatever is under this member was charged too little
    /// CPU, over the time between them, to be doing anything.
    Idle {
        /// When the later sample was taken.
        at: Instant,
        /// That sample, which the next look is compared against in turn.
        work: crate::scratch::Work,
    },
}

impl Observed {
    /// Whether it is time to look again.
    fn due(&self, now: Instant, every: Duration) -> bool {
        match self {
            Observed::Nothing => true,
            Observed::Moving { at, .. } | Observed::Idle { at, .. } => {
                now.duration_since(*at) >= every
            }
        }
    }

    /// The sample the next one is compared against, and when it was taken —
    /// which is the window that comparison is a rate over.
    fn taken(&self) -> Option<(Instant, crate::scratch::Work)> {
        match self {
            Observed::Nothing => None,
            Observed::Moving { at, work } | Observed::Idle { at, work } => Some((*at, *work)),
        }
    }
}

impl Stall {
    /// The clock for a member that started at `started`, supervised under
    /// `bound`.
    #[must_use]
    pub fn new(bound: Duration, started: Instant) -> Self {
        Self {
            started,
            bound,
            // A quarter of the window this rule watches in, so a member always
            // gets a baseline *and* a comparison before the bound expires, and
            // bounded above so a production run's probe is not fifteen minutes
            // apart. Nothing is examined at all until a member has been quiet
            // for half the bound, so a member publishing normally never pays for
            // any of this.
            probe_every: (bound / 8).clamp(HEARTBEAT_INTERVAL, MAX_PROBE_INTERVAL),
            cleared: 0,
            observed: Observed::Nothing,
        }
    }

    /// Whether this member is condemned, given that it last published
    /// `published` milliseconds into its life — the count its supervisor keeps.
    ///
    /// `scratch` is the member's own, which is the stamp its tree carries.
    pub fn condemns(&mut self, published: u64, scratch: &Path) -> bool {
        self.judge(published, Instant::now(), || crate::scratch::work(scratch))
    }

    /// The rule itself: a decision over readings, taking the clock and the
    /// observation it judges rather than reaching for either.
    ///
    /// Split out from [`condemns`](Self::condemns) so the decision can be driven
    /// over a sequence of observations — a wedged tree's, a working one's — in a
    /// test, on any platform, without two minutes of real waiting on a real
    /// process to produce them. The journeys in `tests/e2e/liveness.rs` are what
    /// prove the readings this is given are the kernel's; this is what proves
    /// what is concluded from them.
    ///
    /// `observe` is called at most once, and only when a look is actually due.
    fn judge(
        &mut self,
        published: u64,
        now: Instant,
        observe: impl FnOnce() -> crate::scratch::Work,
    ) -> bool {
        let elapsed = millis(now.saturating_duration_since(self.started));
        let quiet = Duration::from_millis(elapsed.saturating_sub(published.max(self.cleared)));
        if quiet < self.bound / 2 {
            // Publishing normally: nothing to explain, and a tree examined
            // before this member's last event is not evidence about the silence
            // that follows it.
            self.observed = Observed::Nothing;
            return false;
        }
        if self.observed.due(now, self.probe_every) {
            let before = self.observed.taken();
            let work = observe();
            self.observed = match before {
                // How fast the tree was charged CPU over the window between the
                // two looks decides which this is — a rate, so that neither
                // verdict rests on how finely a platform happens to count.
                Some((at, before)) => {
                    if before.worked(work, now.saturating_duration_since(at)) {
                        // Which counts exactly as a published line does: the
                        // member has live work under it, and its stall clock
                        // starts again from here.
                        self.cleared = elapsed;
                        Observed::Moving { at: now, work }
                    } else {
                        Observed::Idle { at: now, work }
                    }
                }
                // The first look is a baseline and nothing else. Condemning on
                // one would be condemning on a reading rather than on a rate,
                // and there is no rate until there is a window to divide by.
                None => Observed::Moving { at: now, work },
            };
        }
        matches!(self.observed, Observed::Idle { .. }) && quiet > self.bound
    }
}

/// The longest a quiet member goes unexamined, whatever its stall bound is.
///
/// The contract's default bound is ten minutes and an eighth of it would be over
/// a minute — long enough that a member which did its work early in the window
/// and then wedged would still be holding a stale verdict when the bound
/// expired.
const MAX_PROBE_INTERVAL: Duration = Duration::from_secs(15);

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
    // One place says a member started, whichever runner it has and whether that
    // start is a turn beginning now or a deferred one announced ahead of time —
    // see `crate::run`, which publishes the deferred case from the same type.
    emitter.emit(
        EventKind::MemberStarted,
        started_payload(&MemberStarted {
            runner: runner(&invocation.launch),
            start_after: None,
        }),
    );
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

/// What runs this member, described for the `member-started` it publishes.
pub(crate) fn runner(launch: &Launch) -> Runner {
    match launch {
        Launch::Process { program, args, cwd } => Runner::Process {
            program: program.clone(),
            args: args.to_vec(),
            cwd: cwd.display().to_string(),
        },
        Launch::Library(launch) => Runner::Library {
            engine: ONEJUDGE_ENGINE.to_string(),
            config: launch.config.display().to_string(),
            worktree: launch.worktree.display().to_string(),
        },
    }
}

/// The engine a two-party member is driven by, as its `member-started` names it.
pub(crate) const ONEJUDGE_ENGINE: &str = "onejudge";

/// One `member-started` payload as the field map an [`Emitter`] takes.
pub(crate) fn started_payload(started: &MemberStarted) -> Map<String, Value> {
    match serde_json::to_value(started) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
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
    let mut command = crate::harness_process::command(program);
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
    let mut stall = Stall::new(bounds.stall, started);
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
        if stall.condemns(activity.load(Ordering::SeqCst), scratch) {
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
    millis(since.elapsed())
}

/// One duration in the milliseconds the watchdogs count in.
fn millis(span: Duration) -> u64 {
    u64::try_from(span.as_millis()).unwrap_or(u64::MAX)
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
        // A member whose own config turned streaming off publishes no envelopes
        // at all: `oneharness run --compact` writes its whole report as one
        // line, and that document is the same one a streamed `result` carries.
        //
        // Recognized by the report envelope's own two identifying fields rather
        // than by position, so a line arriving before it cannot be mistaken for
        // the report. This is a trust boundary — another process's stdout — and
        // the same bar the `result` arm above applies: what is accepted must be
        // the document everything downstream reads as one, not merely something
        // shaped a bit like it.
        _ if is_report(&value) => {
            *held(report) = Some(value.clone());
        }
        _ => {}
    }
}

/// Whether one line of a member's stdout is oneharness's own run report.
///
/// The two fields every report carries and nothing else this crate reads does:
/// the `schema_version` that says which report contract it is written to, and
/// the `results` array everything downstream — the settle, the fallback chain,
/// the structured answer — is read out of. Checked by name rather than by
/// deserializing into `oneharness_core`'s `Report`, because a member runs
/// whichever `oneharness` an operator installed: a report from a build whose
/// schema this one cannot parse is still that member's report, and refusing it
/// would report a member that answered as one that died.
fn is_report(value: &Value) -> bool {
    value.get("schema_version").is_some_and(Value::is_string)
        && value.get("results").is_some_and(Value::is_array)
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
    use crate::scratch::Work;

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

    /// The activity rule, on its own terms: a member publishing normally is never
    /// condemned and its tree is never even examined; a silent one with nothing
    /// running under it is condemned, but only once two looks agree.
    ///
    /// The scratch here is an empty directory, which is a member whose tree is
    /// *gone* — the strongest form of "no live work". The member that has one and
    /// is working through it is a real process tree, and so is proven by a real
    /// run in `tests/e2e/liveness.rs` rather than here.
    ///
    /// The bound is a second, and the sleeps are real, because the probe cadence
    /// is real time: the rule's whole subject is what happened *between* two
    /// looks, so a clock this test supplied would be testing arithmetic rather
    /// than the rule.
    #[test]
    fn the_activity_rule_needs_both_silence_and_an_idle_tree() {
        let scratch = tempfile::tempdir().expect("a scratch");
        let bound = Duration::from_secs(1);
        let mut stall = Stall::new(bound, Instant::now());

        // Publishing right now: not condemned, and nothing examined.
        assert!(!stall.condemns(elapsed_millis(stall.started), scratch.path()));
        assert!(
            matches!(stall.observed, Observed::Nothing),
            "a member publishing normally paid for a look at its own process tree"
        );

        // Quiet past half the bound, so the tree is examined — but one look is a
        // reading rather than a change, so it is not condemned on it.
        std::thread::sleep(bound / 2 + Duration::from_millis(100));
        assert!(!stall.condemns(0, scratch.path()));
        assert!(matches!(stall.observed, Observed::Moving { .. }));

        // A second look, agreeing with the first, past the whole bound: nothing
        // is running under this member and it has published nothing.
        std::thread::sleep(bound / 2 + Duration::from_millis(100));
        assert!(stall.condemns(0, scratch.path()));

        // And a member that publishes again is cleared, tree and all — the next
        // silence is judged from scratch rather than on the verdict this one
        // reached.
        assert!(!stall.condemns(elapsed_millis(stall.started), scratch.path()));
        assert!(matches!(stall.observed, Observed::Nothing));
    }

    /// The rule's *decision*, over supplied observations rather than a process:
    /// the rates a wedged tree and a working one are charged, and the boundary
    /// between them.
    ///
    /// The journeys prove the readings are the kernel's. This proves what is
    /// concluded from readings taken on a platform this host is not, which is
    /// where the rule this replaces silently stopped condemning anything.
    #[test]
    fn a_tree_is_judged_by_how_fast_it_is_charged_cpu() {
        let bound = Duration::from_secs(2);
        let core = 1_000_000;

        // Charged nothing: a parked tree, and also exactly what a platform with
        // no CPU accounting at all reports for every process it is asked about.
        assert!(condemned(bound, 0).is_some(), "a wedged member was spared");
        // 121 µs/s, measured: a process waking every half second, which is the
        // noisiest thing a wedged tree holds and the reading macOS is precise
        // enough to see. The member it belongs to is wedged on both platforms.
        assert!(
            condemned(bound, 121).is_some(),
            "a member whose tree only ticked its own bookkeeping was spared"
        );
        // 98.8% of a core, measured: a spin loop.
        assert_eq!(
            condemned(bound, 987_678),
            None,
            "a member whose child was burning a core was condemned anyway"
        );

        // And the boundary itself, which no real process can be held at: a
        // hundredth of a core is idle, and twice that is working.
        assert!(condemned(bound, core / 100).is_some());
        assert_eq!(condemned(bound, core / 50), None);

        // A total that *falls* is work too. A process leaving the tree takes its
        // whole lifetime's CPU out of the sum, so a member whose child just
        // finished a second of work reads as a large negative change — and it was
        // working. What the rule weighs is the size of the change, not its sign.
        let started = Instant::now();
        let mut stall = Stall::new(bound, started);
        let (probe, mut charged) = (stall.probe_every, 4 * core);
        assert!(!stall.judge(0, started + bound, || Work::of_micros(charged)));
        charged -= core;
        assert!(
            !stall.judge(0, started + bound + probe, || Work::of_micros(charged)),
            "a member whose working child exited between two looks was condemned for it"
        );

        // The bound is a floor rather than a deadline — establishing that a tree
        // is idle takes two looks — but a floor within one probe of the bound,
        // not two minutes past it.
        let at = condemned(bound, 0).expect("a wedged member is condemned");
        let probe = Stall::new(bound, Instant::now()).probe_every;
        assert!(
            at > bound && at <= bound + 2 * probe,
            "a wedged member was condemned at {at:?}, which is not just past a {bound:?} bound"
        );
    }

    /// Drive the rule over a member that publishes nothing at all while the tree
    /// under it is charged `micros_per_second` of CPU, and answer how far into
    /// its life it was condemned — or `None` if it outlived four whole bounds.
    ///
    /// No process and no sleeping: the clock and the readings are both supplied,
    /// which is what makes this a test of the decision rather than of a host.
    fn condemned(bound: Duration, micros_per_second: u64) -> Option<Duration> {
        let started = Instant::now();
        let mut stall = Stall::new(bound, started);
        let mut at = Duration::ZERO;
        while at < bound * 4 {
            at += stall.probe_every;
            let charged = micros_per_second.saturating_mul(millis(at)) / 1_000;
            if stall.judge(0, started + at, || Work::of_micros(charged)) {
                return Some(at);
            }
        }
        None
    }

    /// The probe cadence is derived from the bound, and bounded at both ends: a
    /// quiet member is always looked at twice before its bound expires, and a
    /// production run's look is never a minute and a half apart.
    #[test]
    fn the_probe_cadence_fits_inside_the_bound_it_watches() {
        for bound in [
            Duration::from_millis(500),
            Duration::from_secs(2),
            Duration::from_secs(30),
            DEFAULT_STALL_TIMEOUT,
        ] {
            let stall = Stall::new(bound, Instant::now());
            assert!(
                stall.probe_every <= MAX_PROBE_INTERVAL,
                "{bound:?}: a quiet member would go {:?} unexamined",
                stall.probe_every
            );
            assert!(
                stall.probe_every >= HEARTBEAT_INTERVAL,
                "{bound:?}: the tree would be examined faster than the supervisor loops"
            );
        }
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

    /// Each runner describes its own launch, and the payload says so in the
    /// fields a supervisor branches on.
    ///
    /// One type for the member starting a turn now and the one saying so ahead of
    /// a deferred first turn, so what a stream carries never depends on which of
    /// the two it was — asserted on the serialized event, because that is what a
    /// consumer meets.
    #[test]
    fn each_runner_describes_the_launch_it_is_about_to_run() {
        let process = MemberStarted {
            runner: runner(&Launch::Process {
                program: "oneharness".into(),
                args: vec!["run".into(), "--prompt".into(), "report".into()],
                cwd: std::path::PathBuf::from("/work"),
            }),
            start_after: None,
        };
        let published = started_payload(&process);
        assert_eq!(published["runner"], "process");
        assert_eq!(published["program"], "oneharness");
        assert_eq!(published["cwd"], "/work");
        assert_eq!(published["args"][2], "report");
        assert!(
            published.get("start_after").is_none(),
            "a member taking its turn now named a delay: {published:?}"
        );

        // Not `cwd`: a member driven in this process has no working directory of
        // its own, and claiming one would name a thing that is not true.
        let library = started_payload(&MemberStarted {
            runner: runner(&Launch::Library(Box::new(crate::invoke::JudgeLaunch {
                config: std::path::PathBuf::from("/scratch/onejudge.yaml"),
                task: "do the thing".into(),
                worktree: std::path::PathBuf::from("/work"),
                agent_config: std::path::PathBuf::from("/scratch/oneharness.toml"),
                session: "s-worker".into(),
            }))),
            start_after: None,
        });
        assert_eq!(library["runner"], "library");
        assert_eq!(library["engine"], ONEJUDGE_ENGINE);
        assert_eq!(library["worktree"], "/work");
        assert!(library.get("cwd").is_none(), "{library:?}");

        // A deferred member names the delay beside the launch it will run, and
        // the whole payload reads back as what it was.
        let deferred = MemberStarted {
            start_after: Some(1800),
            ..process.clone()
        };
        let published = started_payload(&deferred);
        assert_eq!(published["start_after"], 1800);
        assert_eq!(
            serde_json::from_value::<MemberStarted>(Value::Object(published)).expect("reads back"),
            deferred
        );
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
