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
//! the classified `cause`, and a bounded `detail`. `docs/contract.md` scopes
//! `exit_code`, `disposition` and `stderr_tail` to "a member that was a child
//! process", and none is any more, so no member carries them; `cause` and
//! `detail` are how both kinds say the same thing. The classification is the
//! point: provider throttling, quota exhaustion, an OOM kill, and a genuine crash
//! otherwise all reach a supervisor as the same dead member.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::event::{bound_text, Cause, Emitter, EventKind, Labels, MemberDied, MemberStarted, Runner};
use crate::invoke::{Invocation, Launch};
use crate::liveness::{
    DEFAULT_HEARTBEAT_TIMEOUT, DEFAULT_STALL_TIMEOUT, HEARTBEAT_TIMEOUT_ENV, STALL_TIMEOUT_ENV,
};

/// How often this supervisor refreshes a live member's heartbeat.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(500);

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
/// `emitter` is already labelled for this member. There is no environment
/// parameter: both engines run in *this* process and read its environment, which
/// `crate::run`'s `export` has already made the one a member's child used to be
/// launched with — the graph's `env:` block applied over the inherited one, with
/// [`crate::invoke::PROCESS_WIDE_HARNESS_ENV`] removed first.
#[must_use]
pub fn run(
    invocation: &Invocation,
    emitter: &Emitter,
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
    }
}

/// One duration in the milliseconds the watchdogs count in.
fn millis(span: Duration) -> u64 {
    u64::try_from(span.as_millis()).unwrap_or(u64::MAX)
}


// llmlint: ignore-end[boundary_inputs_validated]



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
        assert!(!stall.condemns(millis(stall.started.elapsed()), scratch.path()));
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
        assert!(!stall.condemns(millis(stall.started.elapsed()), scratch.path()));
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
        // Both kinds are `runner: library`, and the `engine` is what tells them
        // apart. Neither carries `cwd`: a member driven in this process has no
        // working directory of its own, and claiming one would name a thing that
        // is not true — the directory it *works* in is `worktree`.
        let harness = MemberStarted {
            runner: runner(&Launch::Harness(Box::new(crate::invoke::HarnessLaunch {
                config: std::path::PathBuf::from("/scratch/oneharness.toml"),
                worktree: std::path::PathBuf::from("/work"),
                prompt: "report".into(),
                stream: true,
            }))),
            start_after: None,
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
            }))),
            start_after: None,
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
