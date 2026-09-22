//! Running one member, and turning what it publishes into envelopes.
//!
//! **No member is a child process.** Each of the contract's two kinds is its own
//! engine's run driver, called on a thread of this process — [`crate::judge`] for
//! a two-party member, [`crate::harness`] for a single-sided one — and this
//! module is what they share: the dispatch below, the settle, the death payload,
//! and the two watchdogs each of them runs.
//!
//! A member's events arrive typed, at the moment they occur, and both modules
//! turn them into the same pair of envelopes: the first event of a turn is
//! preceded by a [`EventKind::TurnStarted`] and each becomes a
//! [`EventKind::TurnActivity`]; the engine's own report becomes a
//! [`EventKind::TurnCompleted`] and a [`EventKind::MemberSettled`].
//!
//! Whether a member publishes events at all is **its own config's** decision —
//! see [`crate::invoke`], which is where the run is built. A single-sided member
//! that does not (one asking for a schema-validated answer, or one that simply
//! said `stream = false`) publishes no `turn-activity` and one report, reaching
//! the same settle.
//!
//! Two watchdogs run alongside, both ported from ai-orchestrator with their
//! defaults and their environment overrides intact:
//!
//! * The **heartbeat** is refreshed every [`HEARTBEAT_INTERVAL`] by a thread of
//!   the member's own — [`Heartbeat`] — while the member is alive, and the turn
//!   loop that supervises the member condemns it when that beat is older than
//!   the bound. Its deadline is therefore not a latency budget — it is the
//!   margin by which a *live* member's supervision may be starved of CPU before
//!   the member is declared dead. This crate runs many members at once, and a
//!   threshold near the refresh cadence reaps healthy ones under exactly the
//!   load it creates; so does a rule that reads the turn loop's own lateness as
//!   the beat, which is why the beat is not kept there — see [`Heartbeat`].
//! * The **activity watchdog** is the slow-stall backstop: a member that
//!   published nothing for [`crate::liveness::DEFAULT_STALL_TIMEOUT`] *while a
//!   tree that can be found under it did nothing* is not working. Silence alone
//!   was the rule once, and it condemned members that were working: a supervisory
//!   member whose turn is one long child — a whole round — publishes nothing for
//!   far longer than the bound while being entirely healthy, and its teardown
//!   took the live worker underneath it with it. It was style-sensitive, too,
//!   which is how it hid: a supervisor that drove its round by *polling* emitted
//!   a tool event every few seconds and survived, while one that *blocked* on a
//!   single call died — same persona, same graph, opposite verdicts, on a choice
//!   the agent makes freely turn by turn. [`Stall`] is the rule now, and what it
//!   adds is the evidence the old one threw away. Its bound is half an hour and
//!   its clock is cleared by published events and by live work, never by
//!   streamed provider output — [`Stall`] records why that signal is not
//!   available to count, and against which engine versions that was read.
//!
//! Either firing is a [`EventKind::MemberDied`], carrying the `rule` that fired,
//! the classified `cause`, and a bounded `detail`. `docs/contract.md` scopes
//! `exit_code`, `disposition` and `stderr_tail` to "a member that was a child
//! process", and none is any more, so no member carries them; `cause` and
//! `detail` are how both kinds say the same thing. The classification is the
//! point: provider throttling, quota exhaustion, an OOM kill, and a genuine crash
//! otherwise all reach a supervisor as the same dead member.

use std::collections::BTreeMap;
use std::path::Path;
#[cfg(feature = "test-doubles")]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::event::{
    bound_detail, bound_text, Cause, Emitter, EventKind, Labels, MemberDied, MemberStarted, Runner,
    TurnActivity,
};
use crate::invoke::{Invocation, Launch};
use crate::liveness::{
    DEFAULT_HEARTBEAT_TIMEOUT, DEFAULT_STALL_TIMEOUT, HEARTBEAT_TIMEOUT_ENV, STALL_TIMEOUT_ENV,
};

/// How often a member's [`Heartbeat`] thread refreshes its beat.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(500);

/// A live member's heartbeat, kept by a thread of its own.
///
/// The beat is what the heartbeat rule condemns on, and it is deliberately not
/// kept by the turn loop that applies the rule. That loop is where a member's
/// events are published and its tree examined, and both of those wait on things
/// outside this process — a stream whose reader is behind, a loaded host walking
/// a process table — so it was delayed past the bound during a live tool call on
/// exactly the host that running many members produces, and each time it came
/// back it read its own lateness as a dead member and tore down a healthy
/// worker's tree. This thread does nothing that waits: it stores one counter,
/// writes one small file, and sleeps. A turn loop that comes back late reads a
/// beat that kept going and moves on.
///
/// What the rule still catches is the case it exists for: a supervisor that
/// cannot confirm its member. This thread dead — it could not be started, or it
/// stopped — or starved past the bound leaves a beat that stops advancing, and
/// the turn loop condemns on that by the same bound it always applied. The bound
/// is therefore still the margin by which a *live* member's supervision may be
/// starved of CPU, and no shorter.
///
/// `member-heartbeat` is still published from the turn loop, on its own cadence,
/// for two reasons: it is a stream write, which is one of the waits this thread
/// exists to be free of; and it is what an operator-facing view reads `alive N
/// ago` from, so it should say when the *supervision* last got round to the
/// member rather than promise more than the loop can vouch for.
pub struct Heartbeat {
    /// Milliseconds since `started` at the thread's last refresh — the same clock
    /// the watchdogs count in, so it needs no `Instant` across threads.
    beat: Arc<AtomicU64>,
    /// The origin the beat is counted from.
    started: Instant,
    /// Dropping this ends the thread's wait, so a member that is over does not
    /// keep a thread beating for it.
    stop: Option<mpsc::Sender<()>>,
    /// The thread, for a bounded join once it has been told to stop. `None` when
    /// the host refused the thread, in which case the beat never advances and
    /// the rule condemns the member for it — which is the honest reading: a
    /// member nobody can confirm alive.
    thread: Option<std::thread::JoinHandle<()>>,
    /// The gate a journey asked this member's turn loop to hold at — see
    /// [`hold_turn_loop`](Self::hold_turn_loop). Taken on the first pass, so
    /// the hold happens once.
    #[cfg(feature = "test-doubles")]
    hold: Option<FixtureGate>,
}

impl Heartbeat {
    /// Start beating for the member whose scratch is `scratch` and whose clock
    /// began at `started`, for the turn loop that will supervise its `task`.
    ///
    /// The task is read for the two fixtures a journey may have asked for and
    /// nothing else; without the `test-doubles` feature it is not read at all.
    #[must_use]
    pub fn start(scratch: &Path, started: Instant, task: &str) -> Self {
        #[cfg(feature = "test-doubles")]
        let stop_at = FixtureGate::parse(task, STOP_HEARTBEAT);
        #[cfg(feature = "test-doubles")]
        let hold = FixtureGate::parse(task, HOLD_TURN_LOOP);
        #[cfg(not(feature = "test-doubles"))]
        let _ = task;

        let beat = Arc::new(AtomicU64::new(0));
        let (stop, stopped) = mpsc::channel::<()>();
        let file = scratch.join("member.heartbeat");
        let thread = {
            let beat = Arc::clone(&beat);
            // `Builder`, not `thread::spawn`, for the reason the engine threads
            // give: a host that will not give this run one more thread is a
            // refusal to answer, not a panic to take the graph down with.
            std::thread::Builder::new()
                .spawn(move || loop {
                    beat.store(millis(started.elapsed()), Ordering::SeqCst);
                    let _ = std::fs::write(&file, beat.load(Ordering::SeqCst).to_string());
                    #[cfg(feature = "test-doubles")]
                    if let Some(gate) = stop_at.as_ref().filter(|gate| gate.gate.exists()) {
                        let _ = std::fs::write(&gate.entered, "stopped");
                        return;
                    }
                    // A disconnect is the member ending; a message is never sent.
                    if !matches!(
                        stopped.recv_timeout(HEARTBEAT_INTERVAL),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ) {
                        return;
                    }
                })
                .ok()
        };
        Self {
            beat,
            started,
            stop: Some(stop),
            thread,
            #[cfg(feature = "test-doubles")]
            hold,
        }
    }

    /// How long ago this member's supervision last confirmed it alive.
    ///
    /// Counted from the member's own clock, so a thread that never ran at all
    /// reads as the whole of the member's life.
    #[must_use]
    pub fn since_last(&self) -> Duration {
        Duration::from_millis(
            millis(self.started.elapsed()).saturating_sub(self.beat.load(Ordering::SeqCst)),
        )
    }

    /// Hold the calling turn loop at the gate the task named, once — the
    /// journey's lever for a turn loop delayed past the heartbeat bound.
    ///
    /// Behind `test-doubles`, and a no-op without it. A journey cannot delay the
    /// real loop from outside — the delays it stands in for are a stream reader
    /// that is behind and a process table that is slow to walk, neither of which
    /// a test can arrange deterministically — so this pauses the real loop at the
    /// one point the delays it replaces would have, and writes the gate's
    /// `.entered` sibling so the journey can wait for the hold rather than sleep
    /// at it. Bounded by `FIXTURE_HOLD` for the same reason every fixture is.
    pub fn hold_turn_loop(&mut self) {
        #[cfg(feature = "test-doubles")]
        if let Some(gate) = self.hold.take() {
            let _ = std::fs::write(&gate.entered, "entered");
            let deadline = Instant::now() + FIXTURE_HOLD;
            while !gate.gate.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// How long a fixture waits before giving up on the journey that asked for it.
///
/// Bounded because a fixture that never returns wedges the suite rather than
/// failing it: reaching this bound means the journey never released the gate,
/// which its own assertions then report.
#[cfg(feature = "test-doubles")]
pub(crate) const FIXTURE_HOLD: Duration = Duration::from_secs(30);

/// The marker a journey puts in its **task** to have this member's turn loop
/// held at the gate it names, once, on the loop's next pass — see
/// [`Heartbeat::hold_turn_loop`].
///
/// In the task rather than in this process's environment, so the lever is the
/// one every other journey already uses: a member is steered by the prose it is
/// given, and `tests/e2e/note.rs` holds a live turn open with the harness
/// double's own `fake:` sentinels in exactly the same way. Its own prefix, so no
/// sentinel of the double's can collide with it.
#[cfg(feature = "test-doubles")]
pub(crate) const HOLD_TURN_LOOP: &str = "oneagentgraph-fixture:hold-turn-loop=";

/// The marker a journey puts in its **task** to have this member's heartbeat
/// thread stop beating once the gate it names exists — a supervisor thread that
/// is dead, which the rule has to condemn as it always did.
///
/// The thread writes the gate's `.entered` sibling as it stops, so a journey can
/// tell a condemnation caused by the stop from one caused by anything else.
#[cfg(feature = "test-doubles")]
pub(crate) const STOP_HEARTBEAT: &str = "oneagentgraph-fixture:stop-heartbeat=";

/// Both files a fixture touches, checked before either is named.
///
/// A type rather than a `PathBuf`, because [`Self::parse`] is the only way to
/// hold one: a path cut out of task text is text until something checks it, and a
/// newtype is what makes "checked" a property of the value instead of a habit of
/// each caller. Both members are derived here for the same reason — the sibling
/// a fixture writes used to be rebuilt at the point of use, which is a second
/// place the checks would have to be remembered.
///
/// Shared by the three fixtures — [`crate::judge`]'s hold between turns, and the
/// two above — because each is the same lever: a marker in the task naming a
/// file the journey will create.
#[cfg(feature = "test-doubles")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FixtureGate {
    /// The file the journey creates to release the hold. Only ever read.
    pub(crate) gate: PathBuf,
    /// The sibling a fixture writes on reaching its boundary, so a journey can
    /// wait for the hold rather than sleep at it.
    pub(crate) entered: PathBuf,
}

#[cfg(feature = "test-doubles")]
impl FixtureGate {
    /// The gate `instruction` names after `marker`, if it names one this fixture
    /// will act on.
    ///
    /// `None` covers an instruction that names no gate *and* one whose path is
    /// refused, which are one answer to the caller: the member runs with no
    /// hold, exactly as it does for every task that never asked for one. A
    /// fixture that refused louder than that would fail runs over prose.
    pub(crate) fn parse(instruction: &str, marker: &str) -> Option<Self> {
        let at = instruction.find(marker)? + marker.len();
        let rest = &instruction[at..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        Self::at(Path::new(&rest[..end]))
    }

    /// The checks, over the path [`Self::parse`] cut out of the text.
    ///
    /// What is refused, and why each:
    ///
    /// * **a relative path**, because this runs on the member's thread of a
    ///   process whose working directory is deliberately not the journey's — see
    ///   [`crate::judge::MemberSpawn`], and the journey that pins it — so a
    ///   relative gate resolves somewhere neither end named;
    /// * **a `..` component**, which is the traversal a path assembled from text
    ///   should never carry however it came to be assembled;
    /// * **a gate with no file name**, which has no sibling to write beside it;
    /// * **a parent directory that does not already exist**, because a journey
    ///   names a file in a workspace it has already made, so an absent parent
    ///   means the text was not the path anything meant. Nothing is created to
    ///   make it exist: this writes one file beside the gate and no directory.
    fn at(gate: &Path) -> Option<Self> {
        if !gate.is_absolute()
            || gate
                .components()
                .any(|part| part == std::path::Component::ParentDir)
        {
            return None;
        }
        let name = gate.file_name()?;
        if !gate.parent().is_some_and(Path::is_dir) {
            return None;
        }
        let mut entered = name.to_os_string();
        entered.push(".entered");
        Some(Self {
            // Named against the gate's own parent, which the checks above cleared,
            // so the sibling cannot land anywhere the gate could not.
            entered: gate.with_file_name(entered),
            gate: gate.to_path_buf(),
        })
    }
}

/// Which engine a member is driven by, because the two read their exit codes
/// differently.
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
/// *Did nothing*, and that is not the same fact as *was not there*:
/// [`crate::scratch::work`] answers `None` for a tree it cannot find, and states
/// there when a member has one to find. The policy here is what to do with that
/// answer, and it is to decline it in both directions — an unfindable tree
/// neither condemns this member nor clears its silence — and to let no sample
/// outlive the tree it was taken from.
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
/// * A member with **no tree to ask** is outside this rule entirely, for the
///   same reason and at the same cost. What is given up is small and the hole it
///   would otherwise open is not: a wedged member is one whose harness is alive
///   and will never answer, so it has a tree by construction — the journeys in
///   `tests/e2e/liveness.rs` condemn exactly that member, with a live idle child
///   under it, on every platform. What is bought is every member that is
///   *between* trees while it is silent — one still starting its first, and one
///   whose round is a succession of children with a gap between two of them.
///   Both are ordinary; neither is a member doing nothing.
///
/// # What clears this clock, and what deliberately does not
///
/// Two things clear it, and the second is the paragraphs above: live work under
/// the member. The first is the member's own published events — every tool call
/// and every tool result, and on a two-party member each turn boundary as well,
/// because [`crate::judge`]'s sink stamps the clock for every `Observation` it
/// is handed while [`crate::harness`]'s stamps it for every `ActionEvent`.
///
/// **Streamed provider output does not clear it, because at this crate's pins
/// nothing delivers it here.** Re-read on 2026-09-22 against the two engines
/// this crate links — both bullets survived the bump unchanged: the core's
/// `EventSink` and `ActionEvent` are the same tokens at 0.18.0 as at 0.14.0,
/// and the `onejudge` engine loop is byte-identical from 0.13.1 to 0.14.0,
/// panel included — 0.14.0 changed `note.rs` and `sdk_schema.rs` and nothing
/// else, taking the note contract's declaration back from the retired bus
/// profile without touching what publishes an `Observation`. Neither provider
/// call publishes one while it is in flight. The stamp is a date rather than a release of this crate on
/// purpose: it records when the upstream channels were last actually read,
/// which a version bumped by the release automation would silently claim on
/// its behalf. The two versions
/// named below are not stamps but live claims about what is linked, so this
/// module's `the_engines_this_rule_was_read_against_are_the_ones_the_manifest_links`
/// holds each against `Cargo.toml`. It is named in prose rather than linked
/// because it is a `#[cfg(test)]` item, which rustdoc cannot resolve:
///
/// * `oneharness_core` 0.18.0 delivers a streaming run's events to an
///   `EventSink` as `ActionEvent`s, whose `kind` is `tool_call` or
///   `tool_result`. A turn's prose is not on that channel at all, so a
///   single-sided member spending ten minutes generating a report hands this
///   clock nothing to stamp.
/// * `onejudge` 0.14.0 publishes `Observation::Message` **after**
///   `respond_streaming` has returned — the turn's finished text, as it is
///   appended to the transcript. That is a turn boundary rather than progress
///   within a turn, so it clears the clock only once the report it would have
///   vouched for already exists. The panel's `Observation::JudgeDecided` is the
///   same shape on the other side: one per judge, delivered only once the whole
///   supervisor call has returned, so a stacked panel deliberating is as silent
///   here as a single judge is.
/// * The `alive N ago` an operator-facing view prints is `member-heartbeat`,
///   which [`crate::harness`] and [`crate::judge`] emit on their supervisor's
///   own timer whatever the member is doing. It is this process saying it is
///   running, not the member; counting it as activity would switch this rule off
///   rather than sharpen it, and it is itself an instance of the failure this
///   rule exists inside — what the supervisor reports and what is true coming
///   apart.
///
/// That per-kind asymmetry is worth keeping in view, because it is what makes
/// the same silence read two ways: a two-party member whose round is several
/// turns is stamped at each of them and survives a long quiet stretch, while a
/// single-sided member inside one long turn is stamped by tool events alone and
/// its report is invisible until it lands. A member observed surviving ten
/// minutes of quiet is therefore no evidence that a silent member is safe.
///
/// So for a member that is genuinely quiet, the bound is the whole of the
/// judgement, which is why it is set where
/// [`crate::liveness::DEFAULT_STALL_TIMEOUT`] sets it. The narrower fix stays
/// available and is the one to take the moment either engine grows an
/// incremental text observation: stamp the clock at the sink that receives it,
/// and this bound goes back to being a backstop rather than the judgement.
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
    /// A look found no tree to ask: nothing at all is stamped for this member,
    /// so there is no process whose CPU could answer for the silence.
    ///
    /// A state of its own rather than a reading of zero, because it is not a
    /// sample: the look that follows it is a fresh baseline, exactly as the
    /// first one is. It replaces whatever the last look left behind, so no
    /// verdict about one tree is ever carried across to the next.
    Unfound {
        /// When the look was taken, which is what the probe cadence counts.
        at: Instant,
    },
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
            Observed::Unfound { at } | Observed::Moving { at, .. } | Observed::Idle { at, .. } => {
                now.duration_since(*at) >= every
            }
        }
    }

    /// The sample the next one is compared against, and when it was taken —
    /// which is the window that comparison is a rate over.
    fn taken(&self) -> Option<(Instant, crate::scratch::Work)> {
        match self {
            Observed::Nothing | Observed::Unfound { .. } => None,
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
    /// It answers `None` for a member with no tree to ask at all, which is a
    /// look that establishes nothing rather than one that found no work — see
    /// [`crate::scratch::work`].
    fn judge(
        &mut self,
        published: u64,
        now: Instant,
        observe: impl FnOnce() -> Option<crate::scratch::Work>,
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
            self.observed = match (observe(), before) {
                // Nothing is stamped for this member, so there is no tree whose
                // CPU could answer for the silence. Not a reading of zero: a
                // member is silent while it is still starting the processes that
                // would be its tree, and reading that as an idle one condemned
                // it before its first turn.
                //
                // Whatever the last look left behind goes with it, including a
                // reading of a tree that has since exited. A reading is the CPU
                // charged to the processes stamped *now*, so one taken either
                // side of a gap is of two different populations — the successor
                // starts its own accounting at zero — and their difference is a
                // rate of nothing. Discarding is what makes the first look at
                // the next tree a baseline instead.
                (None, _) => Observed::Unfound { at: now },
                // How fast the tree was charged CPU over the window between the
                // two looks decides which this is — a rate, so that neither
                // verdict rests on how finely a platform happens to count.
                (Some(work), Some((at, before))) => {
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
                // and there is no rate until there is a window to divide by. A
                // tree that has just appeared is in exactly that position, which
                // is why an unfound look leaves no sample behind.
                (Some(work), None) => Observed::Moving { at: now, work },
            };
        }
        matches!(self.observed, Observed::Idle { .. }) && quiet > self.bound
    }
}

/// Whether the activity rule condemns a member that publishes nothing at all
/// while `scratch` holds whatever it really holds.
///
/// The clock is supplied and the observation is the platform's own, which is what
/// lets [`crate::scratch`]'s two platform modules judge the answer their own
/// enumeration gives for a member nobody has launched anything for — without
/// holding a test still for a whole bound to reach the verdict.
///
/// The window walked is every look this rule would take in a member's first two
/// bounds: it examines nothing until half a bound of silence has passed, and any
/// verdict it can reach it has reached inside the second.
#[cfg(test)]
pub(crate) fn condemns_a_silent_member(scratch: &Path) -> bool {
    let bound = Duration::from_secs(60);
    let started = Instant::now();
    let mut stall = Stall::new(bound, started);
    let mut at = bound / 2;
    while at <= bound * 2 {
        if stall.judge(0, started + at, || crate::scratch::work(scratch)) {
            return true;
        }
        at += stall.probe_every;
    }
    false
}

/// The longest a quiet member goes unexamined, whatever its stall bound is.
///
/// The contract's default bound is half an hour and an eighth of it would be
/// nearly four minutes — long enough that a member which did its work early in
/// the window and then wedged would still be holding a stale verdict when the
/// bound expired.
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
/// `emitter` is already labelled for this member. There is no environment
/// parameter: both engines run in *this* process and read its environment, which
/// `crate::run`'s `export` has already made the one a member's child used to be
/// launched with — the graph's `env:` block applied over the inherited one, with
/// [`crate::invoke::PROCESS_WIDE_HARNESS_ENV`] removed first.
#[must_use]
pub fn run(invocation: &Invocation, emitter: &Emitter, bounds: Bounds, scratch: &Path) -> Outcome {
    // One place says a member started, whichever runner it has and whether that
    // start is a turn beginning now or a deferred one announced ahead of time —
    // see `crate::run`, which publishes the deferred case from the same type.
    emitter.emit(
        EventKind::MemberStarted,
        started_payload(&MemberStarted {
            runner: runner(&invocation.launch),
            start_after: None,
            truncated: false,
        }),
    );
    match &invocation.launch {
        Launch::Judge(judge) => crate::judge::run(judge, emitter, bounds, scratch),
        Launch::Harness(harness) => crate::harness::run(harness, emitter, bounds, scratch),
    }
}

/// What runs this member, described for the `member-started` it publishes.
///
/// Both kinds are `runner: library` now, and what tells them apart is the
/// `engine` — which is the field the contract put there for exactly this, and the
/// reason the conversion needed nothing of the wire schema.
pub(crate) fn runner(launch: &Launch) -> Runner {
    let (engine, config, worktree) = match launch {
        Launch::Judge(launch) => (ONEJUDGE_ENGINE, &launch.config, &launch.worktree),
        Launch::Harness(launch) => (
            crate::harness::ONEHARNESS_ENGINE,
            &launch.config,
            &launch.worktree,
        ),
    };
    Runner::Library {
        engine: engine.to_string(),
        config: config.display().to_string(),
        worktree: worktree.display().to_string(),
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
        candidates: Vec::new(),
    }
}

/// One duration in the milliseconds the watchdogs count in.
fn millis(span: Duration) -> u64 {
    u64::try_from(span.as_millis()).unwrap_or(u64::MAX)
}

/// One live tool event as the contract's activity payload, whichever engine
/// produced it.
///
/// Takes the six fields rather than the event around them, because each engine
/// has its own event type — onejudge's `ToolEvent` and oneharness's
/// `ActionEvent` — carrying exactly these six under the same names. What a
/// consumer reads must not depend on which member kind it came from, and neither
/// must the bounds: the summary is bounded in characters and the observation in
/// bytes, from opposite ends, and both decisions live here rather than once per
/// member kind.
///
/// A `tool_result` is published like any other event. It names no tool because it
/// *answers* a call already named, and `tool_call_id` is what joins it back —
/// skipping it for having no name is what discarded every observation a member's
/// tools returned.
// llmlint: ignore-block[invalid_states_unrepresentable] `kind` stays the open
// string both upstreams deliberately publish — oneharness's `ActionEvent` says so
// at the field ("Left open for future kinds rather than an enum, so a new shape
// never breaks the field") and onejudge's `ToolEvent` mirrors it. This crate
// *forwards* that value; an enum here would re-decide a taxonomy it does not own,
// and the first new kind either engine normalizes would be dropped on the floor
// or turned into a member death rather than reaching the operator who needs to
// see it. Scoped to this one function, which is the only place it is read.
// llmlint: ignore-block[boundary_inputs_validated] and it is not untrusted input:
// it arrives as a field of a linked library's typed struct, in this process, from
// the same engine whose report this crate settles on — the same trust level as
// the `Observation` beside it. There is no parse to validate.
pub(crate) fn activity(
    kind: &str,
    name: Option<&str>,
    input: Option<&Value>,
    output: Option<&str>,
    tool_call_id: Option<&str>,
    index: usize,
) -> TurnActivity {
    let (detail, truncated) = bound_detail(&summarize(input));
    // `bound_text`, so an observation keeps its **tail**: what names a failure is
    // the last of a tool's output, not its startup chatter.
    let bounded = output.map(bound_text);
    TurnActivity {
        kind: kind.to_string(),
        name: name.map(str::to_string),
        detail,
        truncated,
        output: bounded.as_ref().map(|(text, _)| text.clone()),
        output_truncated: bounded.is_some_and(|(_, cut)| cut),
        tool_call_id: tool_call_id.map(str::to_string),
        index: index as u64,
    }
}
// llmlint: ignore-end[boundary_inputs_validated]
// llmlint: ignore-end[invalid_states_unrepresentable]

/// One tool event's structured input as the contract's bounded summary: what it
/// acted on.
fn summarize(input: Option<&Value>) -> String {
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

/// One payload, built from the declared type that describes it.
///
/// Every kind whose payload has a type in [`crate::event`] reaches the wire
/// through here rather than through a field list assembled beside it: a type
/// nothing serializes is free to drift from what is published, which is how
/// `Usage` came to declare six fields the wire had never carried.
pub(crate) fn as_payload<T: serde::Serialize>(value: &T) -> Map<String, Value> {
    match serde_json::to_value(value) {
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

    /// This module's own source, so the rule's prose is held against the
    /// manifest by the same file that states it.
    const SOURCE: &str = include_str!("member.rs");
    /// The manifest, which owns which release of each engine is actually linked.
    const MANIFEST: &str = include_str!("../Cargo.toml");

    /// The version [`Stall`]'s doc bullet names for `engine`, written as a Rust
    /// path (`oneharness_core`) the way the prose refers to the crate.
    ///
    /// The needle is assembled at run time rather than written as one literal,
    /// so this function cannot match its own source and report itself.
    fn version_the_rule_names(engine: &str) -> &str {
        let needle = format!("/// * `{engine}` ");
        let (_, rest) = SOURCE
            .split_once(&needle)
            .unwrap_or_else(|| panic!("the rule still names `{engine}` in a bullet of its own"));
        rest.split_whitespace()
            .next()
            .unwrap_or_else(|| panic!("the `{engine}` bullet still opens with a version"))
    }

    /// The version the manifest takes for `krate`, spelled as the package name.
    ///
    /// Both requirement shapes in this manifest — a bare `"0.13.0"` and a
    /// `{ version = "0.8.1", .. }` table — put the version in the first quoted
    /// string after the key, so one reader covers each.
    fn version_the_manifest_takes(krate: &str) -> &str {
        let (_, rest) = MANIFEST
            .split_once(&format!("\n{krate} = "))
            .unwrap_or_else(|| panic!("the manifest still takes `{krate}`"));
        let (_, quoted) = rest
            .split_once('"')
            .unwrap_or_else(|| panic!("the `{krate}` requirement is quoted"));
        quoted
            .split_once('"')
            .unwrap_or_else(|| panic!("the `{krate}` requirement's quote closes"))
            .0
    }

    /// The stall rule rests on what each engine does *not* deliver, and the
    /// manifest is what decides which release of each is linked.
    ///
    /// Without this the two bullets are a claim about the linked engines that
    /// only a reader could falsify: a bump that left them naming the releases
    /// they were written against would describe channels nobody had re-read,
    /// and the rule they justify is the one that decides whether a live member
    /// gets condemned. `tests/inventory.rs` guards `docs/oneharness-library.md`
    /// the same way and for the same reason.
    #[test]
    fn the_engines_this_rule_was_read_against_are_the_ones_the_manifest_links() {
        for (engine, krate) in [
            ("oneharness_core", "oneharness-core"),
            ("onejudge", "onejudge"),
        ] {
            let named = version_the_rule_names(engine);
            let linked = version_the_manifest_takes(krate);
            assert_eq!(
                named, linked,
                "the stall rule was read against `{engine}` {named}, but the manifest links \
                 {linked} — re-read the channel and restate the bullet"
            );
        }
    }

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
        assert_eq!(Bounds::default().stall, Duration::from_secs(1800));

        let moved = Bounds::from_env(&env(&[
            (HEARTBEAT_TIMEOUT_ENV, "1.5"),
            (STALL_TIMEOUT_ENV, "30"),
        ]))
        .expect("both parse");
        assert_eq!(moved.heartbeat, Duration::from_millis(1500));
        assert_eq!(moved.stall, Duration::from_secs(30));
    }

    /// The activity rule against a real scratch directory: a member publishing
    /// normally is never condemned and its tree is never even examined, and a
    /// member whose tree cannot be *found* is not condemned however long it is
    /// silent.
    ///
    /// The scratch here is an empty directory — a member nobody has launched
    /// anything for, which is what every member is while it starts. That is the
    /// look this rule has no evidence in, and reading it as an idle tree is what
    /// condemned a member on Windows before it had said anything. The member
    /// whose tree *is* found and does nothing is the condemned one, and it is
    /// judged over supplied readings below and driven end to end, with a live
    /// idle child under it, in `tests/e2e/liveness.rs`.
    ///
    /// The bound is a second, and the sleeps are real, because the probe cadence
    /// is real time: the rule's whole subject is what happened *between* two
    /// looks, so a clock this test supplied would be testing arithmetic rather
    /// than the rule.
    #[test]
    fn the_activity_rule_declines_a_member_whose_tree_cannot_be_found() {
        let scratch = tempfile::tempdir().expect("a scratch");
        let bound = Duration::from_secs(1);
        let mut stall = Stall::new(bound, Instant::now());

        // Publishing right now: not condemned, and nothing examined.
        assert!(!stall.condemns(millis(stall.started.elapsed()), scratch.path()));
        assert!(
            matches!(stall.observed, Observed::Nothing),
            "a member publishing normally paid for a look at its own process tree"
        );

        // Quiet past half the bound, so the tree is looked for — and there is
        // none, which is a look that establishes nothing rather than a baseline.
        std::thread::sleep(bound / 2 + Duration::from_millis(100));
        assert!(!stall.condemns(0, scratch.path()));
        assert!(matches!(stall.observed, Observed::Unfound { .. }));

        // A second look, past the whole bound, agreeing with the first: still
        // nothing to ask, so still nothing concluded.
        std::thread::sleep(bound / 2 + Duration::from_millis(100));
        assert!(
            !stall.condemns(0, scratch.path()),
            "a member with no tree to ask was condemned for the answer nobody gave"
        );
        assert!(matches!(stall.observed, Observed::Unfound { .. }));

        // And a member that publishes again is cleared, tree and all — the next
        // silence is judged from scratch rather than on the verdict this one
        // reached.
        assert!(!stall.condemns(millis(stall.started.elapsed()), scratch.path()));
        assert!(matches!(stall.observed, Observed::Nothing));
    }

    /// A tree that appears while a member is already silent is a **baseline**,
    /// not a comparison — so the first look that finds one cannot condemn on it,
    /// and the member is judged on what happens after it.
    ///
    /// The startup window, as the rule sees it: nothing, nothing, then a tree.
    /// A rule that compared the first real reading against the unfound looks
    /// before it would be comparing a number against no number at all.
    #[test]
    fn a_tree_that_appears_mid_silence_is_a_baseline_rather_than_a_verdict() {
        let bound = Duration::from_secs(2);
        let started = Instant::now();
        let mut stall = Stall::new(bound, started);
        let probe = stall.probe_every;

        // The whole bound with nothing launched yet.
        let mut at = bound / 2;
        while at <= bound + probe {
            assert!(!stall.judge(0, started + at, || None), "at {at:?}");
            at += probe;
        }

        // The tree arrives, charged for the work of starting: a baseline.
        at += probe;
        assert!(!stall.judge(0, started + at, || Some(Work::of_micros(2_000))));
        assert!(matches!(stall.observed, Observed::Moving { .. }));

        // And from there the rule is what it always was: one comparison against
        // that baseline, and a tree charged nothing over it is condemned.
        at += probe;
        assert!(
            stall.judge(0, started + at, || Some(Work::of_micros(2_000))),
            "a member whose tree was found and did nothing outlived the rule"
        );
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
        assert!(!stall.judge(0, started + bound, || Some(Work::of_micros(charged))));
        charged -= core;
        assert!(
            !stall.judge(0, started + bound + probe, || Some(Work::of_micros(
                charged
            ))),
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

    /// The rule at its **default** bound: a working tree is never condemned, and
    /// an idle one only long past the ten-minute bound this replaces — which is
    /// where members were being killed while their reports were still in flight.
    ///
    /// Driven at [`DEFAULT_STALL_TIMEOUT`] itself, because here the number is
    /// the subject rather than the rule. Both halves are asserted: the sparing
    /// alone would pass just as well against a watchdog switched off.
    #[test]
    fn the_default_bound_condemns_an_idle_tree_long_after_the_bound_it_replaces() {
        /// The bound this one replaces, and the moment both halves must get past.
        const REPLACED: Duration = Duration::from_secs(600);
        /// Two hundredths of a core: a tree doing work, on the rate the rule
        /// reads everywhere else.
        const WORKING: u64 = 1_000_000 / 50;

        assert_eq!(
            condemned(DEFAULT_STALL_TIMEOUT, WORKING),
            None,
            "a member with live work under it was condemned under the default bound"
        );

        let at = condemned(DEFAULT_STALL_TIMEOUT, 0)
            .expect("a member doing nothing at all is still condemned");
        assert!(
            at > REPLACED + MAX_PROBE_INTERVAL,
            "a member was condemned at {at:?}, back inside the window that killed five of them \
             while they were writing their reports"
        );
        assert!(
            at > DEFAULT_STALL_TIMEOUT && at <= DEFAULT_STALL_TIMEOUT + 2 * MAX_PROBE_INTERVAL,
            "a wedged member was condemned at {at:?} rather than just past its own bound"
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
            if stall.judge(0, started + at, || Some(Work::of_micros(charged))) {
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
        // Both kinds are `runner: library`, and the `engine` is what tells them
        // apart. Neither carries `cwd`: a member driven in this process has no
        // working directory of its own, and claiming one would name a thing that
        // is not true — the directory it *works* in is `worktree`.
        let harness = MemberStarted {
            runner: runner(&Launch::Harness(Box::new(crate::invoke::HarnessLaunch {
                config: std::path::PathBuf::from("/scratch/oneharness.toml"),
                worktree: std::path::PathBuf::from("/work"),
                prompt: "report".into(),
                reporting: crate::invoke::Reporting::Streamed,
                views: Vec::new(),
            }))),
            start_after: None,
            truncated: false,
        };
        let published = started_payload(&harness);
        assert_eq!(published["runner"], "library");
        assert_eq!(published["engine"], crate::harness::ONEHARNESS_ENGINE);
        assert_eq!(published["config"], "/scratch/oneharness.toml");
        assert_eq!(published["worktree"], "/work");
        assert!(published.get("cwd").is_none(), "{published:?}");
        assert!(published.get("program").is_none(), "{published:?}");
        assert!(
            published.get("start_after").is_none(),
            "a member taking its turn now named a delay: {published:?}"
        );

        let judge = started_payload(&MemberStarted {
            runner: runner(&Launch::Judge(Box::new(crate::invoke::JudgeLaunch {
                config: std::path::PathBuf::from("/scratch/onejudge.yaml"),
                task: "do the thing".into(),
                worktree: std::path::PathBuf::from("/work"),
                agent_config: std::path::PathBuf::from("/scratch/oneharness.toml"),
                session: "s-worker".into(),
                pace: None,
            }))),
            start_after: None,
            truncated: false,
        });
        assert_eq!(judge["runner"], "library");
        assert_eq!(judge["engine"], ONEJUDGE_ENGINE);
        assert_eq!(judge["worktree"], "/work");
        assert!(judge.get("cwd").is_none(), "{judge:?}");
        assert_ne!(
            judge["engine"], published["engine"],
            "the two kinds are indistinguishable on the wire"
        );

        // A deferred member names the delay beside the launch it will run, and
        // the whole payload reads back as what it was.
        let deferred = MemberStarted {
            start_after: Some(1800),
            ..harness.clone()
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

    /// A member that never started has none of a process's three facts — there
    /// was no process — and says so as `spawn`, with the reason as its detail.
    #[test]
    fn a_member_that_never_started_carries_a_typed_cause_and_no_process_facts() {
        let payload = as_payload(&unstartable("cannot start oneharness: No such file"));
        assert_eq!(payload["rule"], Value::from("unstartable"));
        assert_eq!(payload["cause"], Value::from("spawn"));
        assert!(payload["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("No such file")));
        for absent in ["exit_code", "disposition", "stderr_tail"] {
            assert!(!payload.contains_key(absent), "{absent}: {payload:?}");
        }
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

    /// The beat keeps going however late the loop that reads it is: a reader
    /// that does nothing for several refresh intervals finds the beat fresh,
    /// which is what makes a delayed turn loop a delayed loop rather than a dead
    /// member. The wrapper file the rule was ported with is written beside it.
    #[test]
    fn the_beat_stays_fresh_while_its_reader_is_away() {
        let dir = tempfile::tempdir().expect("tempdir");
        let started = Instant::now();
        let heartbeat = Heartbeat::start(dir.path(), started, "write the thing");
        std::thread::sleep(HEARTBEAT_INTERVAL * 3);
        let since = heartbeat.since_last();
        assert!(
            since < HEARTBEAT_INTERVAL * 2,
            "the beat went stale while nothing read it: {since:?}"
        );
        let written = std::fs::read_to_string(dir.path().join("member.heartbeat"))
            .expect("the heartbeat file");
        let at: u64 = written.trim().parse().expect("a millisecond count");
        assert!(at > 0, "the file never saw a refresh: {written:?}");
        // Dropping the member's supervision ends the thread rather than leaving
        // one beating for a member that is over — `Drop` joins it, so a thread
        // that did not stop would hang this test rather than pass it.
        drop(heartbeat);
    }

    /// A thread that stops leaves a beat that stops advancing, so the reader
    /// sees the whole of the time since as unconfirmed — the same reading a
    /// thread the host refused gives, and what the rule condemns on.
    #[cfg(feature = "test-doubles")]
    #[test]
    fn a_stopped_thread_leaves_a_beat_that_stops_advancing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gate = dir.path().join("stop");
        std::fs::write(&gate, "stop").expect("the gate");
        let started = Instant::now();
        let heartbeat = Heartbeat::start(
            dir.path(),
            started,
            &format!("fake:hang {STOP_HEARTBEAT}{}", gate.display()),
        );
        std::thread::sleep(HEARTBEAT_INTERVAL * 3);
        let since = heartbeat.since_last();
        assert!(
            since >= HEARTBEAT_INTERVAL * 2,
            "a stopped thread's beat kept advancing: {since:?}"
        );
        assert!(
            dir.path().join("stop.entered").is_file(),
            "the thread did not say it stopped"
        );
    }

    /// The turn loop's hold is the journey's lever and nothing else's: a task
    /// naming no gate holds nothing, and one naming a gate holds until the gate
    /// exists and then never again.
    #[cfg(feature = "test-doubles")]
    #[test]
    fn the_turn_loop_holds_at_the_gate_the_task_named_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let started = Instant::now();
        let mut plain = Heartbeat::start(dir.path(), started, "fake:complete-now: ordinary work");
        let before = Instant::now();
        plain.hold_turn_loop();
        assert!(
            before.elapsed() < HEARTBEAT_INTERVAL,
            "an unasked hold held"
        );

        let gate = dir.path().join("loop");
        let mut held = Heartbeat::start(
            dir.path(),
            started,
            &format!("fake:complete-now {HOLD_TURN_LOOP}{} go", gate.display()),
        );
        let releaser = {
            let gate = gate.clone();
            std::thread::spawn(move || {
                std::thread::sleep(HEARTBEAT_INTERVAL);
                std::fs::write(&gate, "go").expect("release");
            })
        };
        let before = Instant::now();
        held.hold_turn_loop();
        assert!(
            before.elapsed() >= HEARTBEAT_INTERVAL,
            "the loop was not held until the gate appeared"
        );
        assert!(dir.path().join("loop.entered").is_file());
        releaser.join().expect("releaser");
        let before = Instant::now();
        held.hold_turn_loop();
        assert!(before.elapsed() < HEARTBEAT_INTERVAL, "the loop held twice");
    }
}
