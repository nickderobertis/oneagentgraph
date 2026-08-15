//! The shared event envelope.
//!
//! Every process in the stack emits NDJSON in one envelope shape, defined by
//! `docs/contract.md`. These types are deliberately duplicated per repository —
//! there is no shared util crate — so each producer owns its own copy and a
//! cross-repo contract test holds them together.
//!
//! Nothing here emits, orders, truncates, or redacts anything: this is the wire
//! shape and its documented bounds, not the machinery that honours them.

// llmlint: ignore-file[invalid_states_unrepresentable] two of the shapes below are fixed
// by `docs/contract.md` rather than chosen: `v` is the wire integer a consumer reads
// before it knows whether it can decode the rest, and `member-died` is specified as
// sibling fields (`rule`, `cause`, `detail`, and the three a child process adds), so
// folding the exit code into an `exited` variant would change what this stack emits.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::clock::now_rfc3339;

/// The envelope version this crate produces and understands.
pub const ENVELOPE_VERSION: u8 = 1;

/// The byte bound on a payload text field, past which it is truncated and the
/// payload carries `truncated: true`.
pub const MAX_PAYLOAD_TEXT_BYTES: usize = 4096;

/// The character bound on a [`TurnActivity::detail`] summary.
pub const MAX_ACTIVITY_DETAIL_CHARS: usize = 160;

/// One NDJSON event.
///
/// Merge order across streams is `(ts, stream, seq)`. A consumer detects loss
/// through per-stream [`seq`](Self::seq) gaps; there are no cross-stream
/// ordering promises beyond the timestamps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    /// Envelope version; [`ENVELOPE_VERSION`] for anything this crate writes.
    pub v: u8,
    /// RFC 3339 timestamp, millisecond precision, UTC.
    pub ts: String,
    /// Unique id of the producing process.
    pub stream: String,
    /// Monotonic per [`stream`](Self::stream).
    pub seq: u64,
    /// Which library in the stack produced the event.
    pub source: Source,
    /// What happened.
    pub kind: EventKind,
    /// Reserved keys plus free-form extras. Producers stamp what they know;
    /// enrichers never rewrite what is already there.
    #[serde(default)]
    pub labels: Labels,
    /// Kind-specific detail. Text fields are bounded by
    /// [`MAX_PAYLOAD_TEXT_BYTES`]; large evidence is an [`Artifact`] instead.
    #[serde(default)]
    pub payload: Map<String, Value>,
    /// Evidence stored by the producing library and referenced by id.
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
}

/// The library that produced an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// This crate.
    Agentgraph,
    /// `onevcs`.
    Vcs,
    /// `onepipeline`.
    Pipeline,
}

/// What an [`Envelope`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    /// The graph began running.
    GraphStarted,
    /// A member's first process started.
    MemberStarted,
    /// A turn began on one side of a member's conversation.
    TurnStarted,
    /// A bounded tool summary from inside a turn; see [`TurnActivity`].
    TurnActivity,
    /// A turn finished; see [`TurnCompleted`].
    TurnCompleted,
    /// An operator asked a member's in-flight turn to do something else; see
    /// [`TurnInterrupted`].
    TurnInterrupted,
    /// A member is alive but has produced nothing since the last heartbeat.
    MemberHeartbeat,
    /// An identity chain moved past a candidate; see [`FallbackAdvanced`].
    FallbackAdvanced,
    /// A member's process died; see [`MemberDied`].
    MemberDied,
    /// A scheduled member fired.
    CronFired,
    /// A resettable schedule's clock restarted.
    CronReset,
    /// A member settled: the full onejudge report is an artifact and the verdict
    /// is inline in the payload.
    MemberSettled,
    /// Every member settled.
    GraphSettled,
}

impl EventKind {
    /// This kind's kebab-case spelling on the wire, which is what an
    /// [`EventFilter`] globs against and what a rendering names.
    ///
    /// Stated rather than derived through serde, because a filter consults it
    /// once per envelope and the derivation allocates a `Value` to read one
    /// string out of. The test below walks every variant against serde's own
    /// spelling, so the two cannot drift.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::GraphStarted => "graph-started",
            EventKind::MemberStarted => "member-started",
            EventKind::TurnStarted => "turn-started",
            EventKind::TurnActivity => "turn-activity",
            EventKind::TurnCompleted => "turn-completed",
            EventKind::TurnInterrupted => "turn-interrupted",
            EventKind::MemberHeartbeat => "member-heartbeat",
            EventKind::FallbackAdvanced => "fallback-advanced",
            EventKind::MemberDied => "member-died",
            EventKind::CronFired => "cron-fired",
            EventKind::CronReset => "cron-reset",
            EventKind::MemberSettled => "member-settled",
            EventKind::GraphSettled => "graph-settled",
        }
    }
}

/// The reserved label keys, plus whatever else a producer stamped.
///
/// Reserved keys are absent rather than empty when unknown, so an enricher can
/// tell "not stamped" from "stamped empty".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Labels {
    /// The run this event belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The round within the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u64>,
    /// The graph node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The step within the node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// The graph member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// The persona the member runs under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// Free-form extras, carried through untouched.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Evidence too large for a payload: stored by the producing library, referenced
/// by id, and fetched through that library's CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    /// Identifier, unique within the producing library's store.
    pub id: String,
    /// What the artifact is — a gate log, a check log, a transcript, a report.
    pub kind: String,
    /// Size of the stored artifact.
    pub bytes: u64,
}

/// Which envelopes a run puts on its merged stream.
///
/// The grammar is shared across the stack — `onevcs`, `oneagentgraph`, and
/// `onepipeline` read the same document — so a consumer with a narrow attention
/// budget states its interest **once**, to the producer that owns the stream,
/// rather than re-filtering everything downstream. Like the envelope above it,
/// this type is duplicated per repository by design.
///
/// [`EventFilter::default`] — no matcher on either list — admits everything, so
/// a run naming no filter streams exactly what it always did.
///
/// A filter decides what is *emitted*, never what the run acts on: the
/// settlements, heartbeats, and deaths this crate's own liveness, scheduling,
/// and settle detection read are the values [`Emitter::emit`] returns, which it
/// returns whether or not the envelope reached the stream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventFilter {
    /// Matchers an envelope satisfies one of to pass. Absent or empty admits
    /// every envelope, so a filter that only rejects need name nothing here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<Matcher>,
    /// Matchers that reject. A match here rejects whatever
    /// [`include`](Self::include) said, so a broad include beside a narrow
    /// exclude is how "all of this except that" is written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<Matcher>,
}

/// One matcher: every field it names must hold of an envelope, and a field it
/// does not name is not consulted.
///
/// Deliberately absent: `stream`, which identifies a producing process rather
/// than anything a consumer means by an event; and payload fields — a turn's
/// `role` lives in a payload rather than in the labels, and this stays a matcher
/// over the envelope's addressing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Matcher {
    /// The producing library, by exact equality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// A glob over the kind's kebab-case wire string, where `*` stands for any
    /// run of characters including none and every other character is itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The [`Labels::run_id`] the envelope was stamped with, by exact equality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The [`Labels::node`] the envelope was stamped with, by exact equality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The [`Labels::step`] the envelope was stamped with, by exact equality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// The [`Labels::member`] the envelope was stamped with, by exact equality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// The [`Labels::persona`] the envelope was stamped with, by exact equality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
}

impl EventFilter {
    /// Whether an envelope reaches the merged stream.
    ///
    /// `exclude` wins: a matcher there rejects whatever `include` admitted, and
    /// an empty `include` admits everything.
    ///
    /// The kind arrives as its wire string rather than as an [`EventKind`],
    /// because the merged stream carries what a sibling library relayed as well
    /// as what this one produced. `onevcs` and `onepipeline` name kinds this
    /// enum does not have, so a filter typed on the enum would have to either
    /// silence every relayed event or refuse a spec for naming one. Everything
    /// else a matcher reads — the [`Source`] and the reserved [`Labels`] — is
    /// shared across the stack and stays typed.
    #[must_use]
    pub fn allows(&self, source: Source, kind: &str, labels: &Labels) -> bool {
        if self
            .exclude
            .iter()
            .any(|matcher| matcher.matches(source, kind, labels))
        {
            return false;
        }
        self.include.is_empty()
            || self
                .include
                .iter()
                .any(|matcher| matcher.matches(source, kind, labels))
    }

    /// Whether every matcher in this filter could match anything.
    ///
    /// A spec is external input — a graph document's `events.filter`, or the
    /// `--event-filter` an operator typed — so this is its trust boundary, and a
    /// run checks it before it starts rather than after a paid turn has been
    /// spent streaming the wrong thing.
    ///
    /// # Errors
    ///
    /// A message naming the offending matcher — which list it is in, where in
    /// that list, and what it says — for a matcher that names no field at all
    /// (it matches *every* envelope, so one in `exclude` silences the stream
    /// entirely), or one whose field is empty (nothing on the stream carries an
    /// empty kind or an empty label, so it matches nothing).
    pub fn validate(&self) -> Result<(), String> {
        for (list, matchers) in [("include", &self.include), ("exclude", &self.exclude)] {
            for (at, matcher) in matchers.iter().enumerate() {
                matcher.check().map_err(|why| {
                    format!(
                        "{list}[{at}] {}: {why}",
                        serde_json::to_string(matcher).unwrap_or_else(|_| "{}".to_string())
                    )
                })?;
            }
        }
        Ok(())
    }
}

impl Matcher {
    /// What this matcher asks of the reserved labels, in the order the grammar
    /// lists them.
    ///
    /// One list rather than two, because `matches` and `check` must read exactly
    /// the same keys: a key added to the grammar and to only one of them is
    /// either unchecked or unmatched, and both are silent.
    fn labels_asked(&self) -> [(&'static str, Option<&str>); 5] {
        [
            ("run_id", self.run_id.as_deref()),
            ("node", self.node.as_deref()),
            ("step", self.step.as_deref()),
            ("member", self.member.as_deref()),
            ("persona", self.persona.as_deref()),
        ]
    }

    /// Whether every field this matcher names holds of the envelope.
    fn matches(&self, source: Source, kind: &str, labels: &Labels) -> bool {
        if self.source.is_some_and(|named| named != source) {
            return false;
        }
        if self
            .kind
            .as_deref()
            .is_some_and(|pattern| !glob(pattern, kind))
        {
            return false;
        }
        let typed = [
            labels.run_id.as_deref(),
            labels.node.as_deref(),
            labels.step.as_deref(),
            labels.member.as_deref(),
            labels.persona.as_deref(),
        ];
        // A label the envelope never stamped is `None`, which no asked-for value
        // equals — "a matcher naming a label the envelope did not stamp does not
        // match it".
        self.labels_asked()
            .iter()
            .zip(typed)
            .all(|((key, asked), typed)| match asked {
                None => true,
                Some(asked) => stamped(labels, key, typed) == Some(*asked),
            })
    }

    /// Whether this matcher could match anything; see [`EventFilter::validate`].
    fn check(&self) -> Result<(), String> {
        let mut named = usize::from(self.source.is_some());
        for (field, asked) in
            std::iter::once(("kind", self.kind.as_deref())).chain(self.labels_asked())
        {
            let Some(asked) = asked else { continue };
            named += 1;
            if asked.trim().is_empty() {
                return Err(format!(
                    "`{field}` is empty, and nothing on the stream carries an empty {field} — \
                     omit the field to leave it unasked"
                ));
            }
        }
        if named == 0 {
            return Err(
                "a matcher naming no field matches every event — name at least one of `source`, \
                 `kind`, `run_id`, `node`, `step`, `member`, or `persona`"
                    .to_string(),
            );
        }
        Ok(())
    }
}

/// What an envelope carries under one reserved label key.
///
/// The typed slot, or — where that is unset — the same key among the extras,
/// because a matcher asks about the key *as the envelope carries it*. `Labels`
/// flattens its extras beside the reserved fields, so a stamp an operator added
/// by hand (`--label node=service`, which is not one this run stamps itself)
/// reaches the wire under exactly the name the grammar names, and a filter that
/// consulted only the typed slot would refuse to see a label its own consumer
/// can plainly read. A non-string extra is not a label value and matches
/// nothing.
fn stamped<'a>(labels: &'a Labels, key: &str, typed: Option<&'a str>) -> Option<&'a str> {
    typed.or_else(|| labels.extra.get(key).and_then(Value::as_str))
}

/// Whether `pattern` matches `text`, where `*` stands for any run of characters
/// including none and every other character is itself.
///
/// The whole dialect, stated rather than inherited: this is a cross-repo grammar
/// with no shared implementation, so a `?` or a `[a-z]` supported here and
/// nowhere else would be a spec that filters differently depending on which
/// producer read it. Kebab-case wire strings need neither.
fn glob(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0, 0);
    // Where to resume from if the run this `*` is currently standing for turns
    // out to be one character too short.
    let (mut star, mut resume) = (None, 0);
    while t < text.len() {
        if pattern.get(p) == Some(&'*') {
            star = Some(p);
            resume = t;
            p += 1;
        } else if pattern.get(p) == Some(&text[t]) {
            p += 1;
            t += 1;
        } else if let Some(at) = star {
            p = at + 1;
            resume += 1;
            t = resume;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|character| *character == '*')
}

/// The payload of an [`EventKind::MemberStarted`] event.
///
/// A member says what it is about to run before it runs it, and the two runners
/// describe different things — so the runner is a typed alternation carrying its
/// own facts rather than a string beside a bag of optional ones, and no consumer
/// can meet a `library` member with an argv or a `process` one with a worktree.
///
/// [`start_after`](Self::start_after) is the one field orthogonal to both: it is
/// present when the member came up *without* taking a turn, and says how many
/// seconds until the first one. Only a schedule defers a turn, so today only a
/// single-sided member carries it — but that is the graph schema's rule rather
/// than this payload's, and a consumer reads the field the same way whichever
/// runner it arrives on.
// `deny_unknown_fields` sits on [`Runner`] rather than here, because serde cannot
// carry it across a `flatten`: the flattened field is what the remaining keys are
// handed to, so it is the one that refuses a key nobody declared — including a
// field belonging to the *other* runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberStarted {
    /// What runs this member, and what that runner is.
    #[serde(flatten)]
    pub runner: Runner,
    /// Seconds until this member's first turn, on the event a member publishes
    /// when it comes up without taking one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_after: Option<u64>,
}

/// What runs one member, and the facts that runner has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "runner", rename_all = "lowercase", deny_unknown_fields)]
pub enum Runner {
    /// Driven in this process through a library.
    Library {
        /// The engine driving it.
        engine: String,
        /// The effective config it was given.
        config: String,
        /// The directory the harness works in. Not `cwd`: this member has no
        /// working directory of its own, and naming one would claim a thing that
        /// is not true.
        worktree: String,
    },
    /// A child process of its own.
    Process {
        /// The program spawned.
        program: String,
        /// Its arguments.
        args: Vec<String>,
        /// The directory the child runs in.
        cwd: String,
    },
}

/// The payload of an [`EventKind::TurnActivity`] event: a bounded tool summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnActivity {
    /// The kind of tool call.
    pub kind: String,
    /// The tool's name.
    pub name: String,
    /// A summary bounded by [`MAX_ACTIVITY_DETAIL_CHARS`].
    pub detail: String,
    /// Whether a text field above was cut to its documented bound.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

/// The payload of an [`EventKind::TurnCompleted`] event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnCompleted {
    /// What the turn consumed.
    pub usage: Usage,
}

/// What one turn consumed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    /// Input tokens.
    pub tokens_in: u64,
    /// Output tokens.
    pub tokens_out: u64,
    /// Tokens read from the prompt cache.
    pub cache_read: u64,
    /// Tokens written to the prompt cache.
    pub cache_write: u64,
    /// What the turn cost.
    pub cost: f64,
    /// How long the turn took.
    pub duration: f64,
}

/// The payload of an [`EventKind::TurnInterrupted`] event.
///
/// Published for every `interrupt`, delivered or not, because "the lever was
/// pulled and nothing happened" is exactly what an operator watching a run needs
/// to see — a verb that stayed silent unless it worked would leave the three
/// not-delivered cases visible only in an exit code somebody has to be watching
/// for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnInterrupted {
    /// The member whose turn was addressed. On the payload as well as in the
    /// labels, because that is what `docs/contract.md` names for this event.
    pub member: String,
    /// Whether the run took ownership of the redirection.
    pub delivered: bool,
    /// How many bytes of redirection were offered; `0` for an interrupt that
    /// only asks the turn to stop.
    pub input_bytes: u64,
    /// Why the delivery did not land — present exactly when
    /// [`delivered`](Self::delivered) is false, and never otherwise, so a
    /// consumer can never read a served interrupt as having had a reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The payload of an [`EventKind::FallbackAdvanced`] event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FallbackAdvanced {
    /// The identity the chain moved past.
    pub identity: String,
    /// oneharness's classification of why it could not run.
    pub reason: String,
    /// Which side of a two-party conversation the chain belonged to. Absent for
    /// a single-sided member, which has one side and so nothing to distinguish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// The turn the chain advanced on, for the same reason: a two-party member
    /// runs one chain per side per turn, and an operator restoring a
    /// subscription needs to know which of them refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
}

/// Which side of a two-party conversation an event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The side that does the work.
    Agent,
    /// The side that supervises it.
    Judge,
}

impl From<onejudge::TelemetryRole> for Role {
    fn from(role: onejudge::TelemetryRole) -> Self {
        match role {
            onejudge::TelemetryRole::Agent => Role::Agent,
            onejudge::TelemetryRole::Judge => Role::Judge,
        }
    }
}

/// The payload of an [`EventKind::MemberDied`] event.
///
/// The first three fields answer for every member. The last three are a *child
/// process's* facts, and a two-party member no longer has them: `docs/contract.md`
/// runs onejudge through its library, in this process, so there is no exit status
/// to read and no stderr to tail. What that member has instead is a typed error,
/// which is what [`cause`](Self::cause) and [`detail`](Self::detail) carry — for
/// both kinds, so a consumer branches on one field rather than on which shape
/// arrived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberDied {
    /// The liveness rule that fired.
    pub rule: String,
    /// What killed the member, classified.
    pub cause: Cause,
    /// What that cause said, bounded by [`MAX_PAYLOAD_TEXT_BYTES`]: the engine's
    /// own error for an in-process member, the tail of standard error for a child
    /// process.
    pub detail: String,
    /// Whether [`detail`](Self::detail) was cut to its documented bound.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    /// The process's exit code, when the member *was* a process and it exited
    /// rather than being signalled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// How the process ended; absent for a member this process ran in-library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<Disposition>,
    /// The tail of the process's standard error; absent for a member this process
    /// ran in-library, which has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
}

/// How a member's process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Disposition {
    /// The process exited with a code.
    Exited,
    /// The process was terminated by a signal.
    Signaled,
}

/// The classified cause of a [`MemberDied`].
///
/// A closed set, for the same reason [`crate::member::Rule`] is one: this is what
/// a supervisor branches on, and a cause spelled two ways is a branch that
/// silently stops matching. The ten classified kinds are onejudge's own
/// `ProviderErrorKind` — which is in turn oneharness's normalized `failure_kind` —
/// mapped **totally**, so a category added upstream is a compile error here rather
/// than a new bare string on the wire. The last three are the causes that exist
/// only outside that taxonomy: a child process's two dispositions, and an engine
/// failure that named no kind at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// Credentials were missing or rejected.
    Auth,
    /// The provider rate-limited the call.
    RateLimit,
    /// The harness did not recognize the requested model.
    ModelNotFound,
    /// The account's quota or billing limit is exhausted.
    Quota,
    /// A transient server-side overload.
    Overloaded,
    /// The call exceeded its deadline.
    Timeout,
    /// The turn was torn down because it was cancelled.
    Cancelled,
    /// The provider process could not be started.
    Spawn,
    /// The provider ran but violated its protocol.
    Protocol,
    /// A classified failure with no more specific category.
    Other,
    /// The member's own process exited with a status of its own.
    Exited,
    /// The member's own process was terminated by a signal.
    Signaled,
    /// The member failed without any classification at all.
    Unclassified,
}

impl Cause {
    /// This cause's spelling on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::Auth => "auth",
            Cause::RateLimit => "rate_limit",
            Cause::ModelNotFound => "model_not_found",
            Cause::Quota => "quota",
            Cause::Overloaded => "overloaded",
            Cause::Timeout => "timeout",
            Cause::Cancelled => "cancelled",
            Cause::Spawn => "spawn",
            Cause::Protocol => "protocol",
            Cause::Other => "other",
            Cause::Exited => "exited",
            Cause::Signaled => "signaled",
            Cause::Unclassified => "unclassified",
        }
    }
}

impl From<Disposition> for Cause {
    fn from(disposition: Disposition) -> Self {
        match disposition {
            Disposition::Exited => Cause::Exited,
            Disposition::Signaled => Cause::Signaled,
        }
    }
}

impl From<onejudge::ProviderErrorKind> for Cause {
    /// Total, so a category onejudge adds later fails this build instead of
    /// reaching the wire as an unhandled string.
    fn from(kind: onejudge::ProviderErrorKind) -> Self {
        use onejudge::ProviderErrorKind as Kind;
        match kind {
            Kind::Auth => Cause::Auth,
            Kind::RateLimit => Cause::RateLimit,
            Kind::ModelNotFound => Cause::ModelNotFound,
            Kind::Quota => Cause::Quota,
            Kind::Overloaded => Cause::Overloaded,
            Kind::Timeout => Cause::Timeout,
            Kind::Cancelled => Cause::Cancelled,
            Kind::Spawn => Cause::Spawn,
            Kind::Protocol => Cause::Protocol,
            Kind::Other => Cause::Other,
        }
    }
}

/// `skip_serializing_if` helper: an unset truncation flag is omitted rather than
/// written as `false`, so a payload that was never near its bound stays quiet.
fn is_false(value: &bool) -> bool {
    !*value
}

/// Bound one payload text field to [`MAX_PAYLOAD_TEXT_BYTES`], reporting whether
/// it had to be cut.
///
/// The cut lands on a character boundary, and it takes the **tail**: what names a
/// failure is the last of a harness's output, not its startup chatter. A caller
/// that wants the head says so by trimming before it gets here.
#[must_use]
pub fn bound_text(text: &str) -> (String, bool) {
    if text.len() <= MAX_PAYLOAD_TEXT_BYTES {
        return (text.to_string(), false);
    }
    let mut start = text.len() - MAX_PAYLOAD_TEXT_BYTES;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    (text[start..].to_string(), true)
}

/// Bound one [`TurnActivity::detail`] to [`MAX_ACTIVITY_DETAIL_CHARS`],
/// reporting whether it had to be cut.
///
/// Characters, not bytes — the contract says "160-char detail", and a tool
/// summary is prose a person reads.
#[must_use]
pub fn bound_detail(detail: &str) -> (String, bool) {
    let collapsed: String = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_ACTIVITY_DETAIL_CHARS {
        return (collapsed, false);
    }
    (
        collapsed.chars().take(MAX_ACTIVITY_DETAIL_CHARS).collect(),
        true,
    )
}

/// Stamps and numbers this process's events, and writes them where the run said.
///
/// One emitter per producing process, because `seq` is "monotonic per `stream`"
/// and `stream` is "a unique id per producing process" — the two are the same
/// fact, so they are the same object. It is cheap to clone and safe to share
/// across the reader threads a graph runs one per member: the counter is atomic
/// and the sink is behind one lock, so a line is written whole.
#[derive(Clone)]
pub struct Emitter {
    stream: String,
    seq: Arc<AtomicU64>,
    sink: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    labels: Labels,
    filter: Arc<EventFilter>,
}

impl std::fmt::Debug for Emitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Emitter")
            .field("stream", &self.stream)
            .field("seq", &self.seq.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Emitter {
    /// An emitter for `stream`, writing to `sink`.
    #[must_use]
    pub fn new(stream: impl Into<String>, sink: Box<dyn std::io::Write + Send>) -> Self {
        Self {
            stream: stream.into(),
            seq: Arc::new(AtomicU64::new(0)),
            sink: Arc::new(Mutex::new(sink)),
            labels: Labels::default(),
            filter: Arc::new(EventFilter::default()),
        }
    }

    /// The same emitter, putting only what `filter` admits on the stream.
    ///
    /// Set once, at the run's own emitter, and inherited by every emitter
    /// derived from it: one graph run is one merged stream, and a member whose
    /// events escaped the run's filter would be exactly the noise the filter was
    /// given to remove.
    #[must_use]
    pub fn with_filter(&self, filter: EventFilter) -> Self {
        Self {
            filter: Arc::new(filter),
            ..self.clone()
        }
    }

    /// The same emitter with `labels` stamped on everything it writes next.
    ///
    /// Enrichers never rewrite: a label already on the derived emitter stays, so
    /// a member's own `member`/`persona` cannot be overwritten by a graph-level
    /// stamp added later.
    #[must_use]
    pub fn with_labels(&self, labels: Labels) -> Self {
        let mut merged = labels;
        merged.run_id = merged.run_id.or_else(|| self.labels.run_id.clone());
        merged.round = merged.round.or(self.labels.round);
        merged.node = merged.node.or_else(|| self.labels.node.clone());
        merged.step = merged.step.or_else(|| self.labels.step.clone());
        merged.member = merged.member.or_else(|| self.labels.member.clone());
        merged.persona = merged.persona.or_else(|| self.labels.persona.clone());
        for (key, value) in &self.labels.extra {
            merged
                .extra
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        Self {
            labels: merged,
            ..self.clone()
        }
    }

    /// The stream id every envelope this emitter writes carries.
    #[must_use]
    pub fn stream(&self) -> &str {
        &self.stream
    }

    /// Write one event, returning the envelope as it was written.
    ///
    /// A sink that cannot be written to is not a run failure — the events are
    /// also on disk — so the write is best-effort and the envelope is still
    /// returned to whatever else the run does with it. An envelope this
    /// emitter's [`EventFilter`] does not admit is returned on the same terms
    /// and never written: filtering decides what a consumer *sees*, and the
    /// run's own liveness, scheduling, and settle detection go on reading
    /// everything.
    pub fn emit(&self, kind: EventKind, payload: Map<String, Value>) -> Envelope {
        self.emit_with(kind, payload, Vec::new())
    }

    /// [`Emitter::emit`], with artifacts attached.
    pub fn emit_with(
        &self,
        kind: EventKind,
        payload: Map<String, Value>,
        artifacts: Vec<Artifact>,
    ) -> Envelope {
        let admitted = self
            .filter
            .allows(Source::Agentgraph, kind.as_str(), &self.labels);
        let envelope = Envelope {
            v: ENVELOPE_VERSION,
            ts: now_rfc3339(),
            stream: self.stream.clone(),
            // `seq` numbers what the stream *carries*: the contract has a
            // consumer detect loss through per-stream gaps, and a filtered event
            // that took a number with it would report every deliberate omission
            // as a dropped event. A suppressed envelope is returned carrying the
            // number the next admitted one will take, which is the only honest
            // answer for a line that was never on the wire.
            seq: if admitted {
                self.seq.fetch_add(1, Ordering::SeqCst)
            } else {
                self.seq.load(Ordering::SeqCst)
            },
            source: Source::Agentgraph,
            kind,
            labels: self.labels.clone(),
            payload,
            artifacts,
        };
        if !admitted {
            return envelope;
        }
        if let Ok(mut sink) = self.sink.lock() {
            let line = serde_json::to_string(&envelope).unwrap_or_default();
            let _ = writeln!(sink, "{line}");
            let _ = sink.flush();
        }
        envelope
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shared sink a test can read back, standing in for stdout or the run's
    /// events file.
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

    fn lines(recorder: &Recorder) -> Vec<Envelope> {
        let raw = recorder.0.lock().expect("recorder").clone();
        String::from_utf8(raw)
            .expect("utf-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("envelope"))
            .collect()
    }

    /// `seq` is monotonic per stream, and it stays so across the derived
    /// emitters a graph hands to its members — they are one producing process.
    #[test]
    fn seq_is_monotonic_across_every_derived_emitter() {
        let recorder = Recorder::default();
        let emitter = Emitter::new("run-1", Box::new(recorder.clone()));
        let member = emitter.with_labels(Labels {
            member: Some("worker".into()),
            ..Labels::default()
        });
        emitter.emit(EventKind::GraphStarted, Map::new());
        member.emit(EventKind::MemberStarted, Map::new());
        emitter.emit(EventKind::GraphSettled, Map::new());

        let written = lines(&recorder);
        assert_eq!(
            written.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(written
            .iter()
            .all(|e| e.stream == "run-1" && e.v == ENVELOPE_VERSION));
        assert_eq!(written[1].labels.member.as_deref(), Some("worker"));
        assert_eq!(written[0].labels.member, None);
        assert_eq!(emitter.stream(), "run-1");
    }

    /// An enricher never rewrites: a member's own label survives a later stamp,
    /// and a graph-level label reaches a member that did not set one.
    #[test]
    fn labels_are_stamped_but_never_rewritten() {
        let recorder = Recorder::default();
        let base = Emitter::new("s", Box::new(recorder.clone())).with_labels(Labels {
            run_id: Some("R".into()),
            member: Some("graph".into()),
            extra: [("tier".to_string(), Value::from("gate"))]
                .into_iter()
                .collect(),
            ..Labels::default()
        });
        let member = base.with_labels(Labels {
            member: Some("worker".into()),
            round: Some(2),
            extra: [("tier".to_string(), Value::from("member"))]
                .into_iter()
                .collect(),
            ..Labels::default()
        });
        member.emit(EventKind::MemberStarted, Map::new());
        let written = lines(&recorder);
        assert_eq!(written[0].labels.run_id.as_deref(), Some("R"));
        assert_eq!(written[0].labels.member.as_deref(), Some("worker"));
        assert_eq!(written[0].labels.round, Some(2));
        assert_eq!(written[0].labels.extra["tier"], Value::from("member"));
    }

    /// Text fields are bounded at the documented byte count, keeping the tail and
    /// never splitting a character.
    #[test]
    fn payload_text_is_bounded_at_its_documented_size() {
        let (short, cut) = bound_text("brief");
        assert_eq!((short.as_str(), cut), ("brief", false));

        // `é` is two bytes, so a five-byte ASCII tail puts the cut at an odd
        // offset — *inside* a character — and it has to walk forward to the next
        // boundary. That is the case a naive slice panics on.
        let long = format!("{}TAILS", "é".repeat(MAX_PAYLOAD_TEXT_BYTES));
        let (bounded, cut) = bound_text(&long);
        assert!(cut);
        assert!(
            bounded.len() < MAX_PAYLOAD_TEXT_BYTES,
            "the cut did not move to a boundary"
        );
        assert!(bounded.ends_with("TAILS"));
        assert!(bounded.chars().all(|c| c == 'é' || c.is_ascii_uppercase()));

        // And an even offset needs no walk, so the whole bound is used.
        let aligned = format!("{}TAIL", "é".repeat(MAX_PAYLOAD_TEXT_BYTES));
        let (bounded, cut) = bound_text(&aligned);
        assert!(cut);
        assert_eq!(bounded.len(), MAX_PAYLOAD_TEXT_BYTES);
    }

    /// A tool summary is bounded in characters and collapsed to one line, so a
    /// multi-line command cannot break the rendering it lands in.
    #[test]
    fn an_activity_detail_is_collapsed_and_bounded_in_characters() {
        let (detail, cut) = bound_detail("just   check\n  --all");
        assert_eq!((detail.as_str(), cut), ("just check --all", false));

        let (detail, cut) = bound_detail(&"é".repeat(MAX_ACTIVITY_DETAIL_CHARS + 10));
        assert!(cut);
        assert_eq!(detail.chars().count(), MAX_ACTIVITY_DETAIL_CHARS);
    }

    /// Every kind onejudge classifies a provider failure as has a cause of its
    /// own here, and each keeps onejudge's own spelling.
    ///
    /// The spelling is the point: `cause` is what a supervisor branches on, and
    /// oneharness names the same categories in its `failure_kind`, so a consumer
    /// joining one against the other must not have to translate. This walks
    /// onejudge's own `classify`, so a kind renamed upstream fails here.
    #[test]
    fn every_provider_error_kind_maps_to_a_cause_with_the_same_name() {
        for name in [
            "auth",
            "rate_limit",
            "model_not_found",
            "quota",
            "overloaded",
            "timeout",
            "cancelled",
            "spawn",
            "protocol",
            "other",
        ] {
            let kind = onejudge::ProviderErrorKind::classify(name);
            assert_eq!(kind.as_str(), name, "onejudge renamed {name:?}");
            assert_eq!(Cause::from(kind).as_str(), name);
        }
        // The three causes that exist outside that taxonomy.
        assert_eq!(Cause::from(Disposition::Exited), Cause::Exited);
        assert_eq!(Cause::from(Disposition::Signaled), Cause::Signaled);
        assert_eq!(Cause::Unclassified.as_str(), "unclassified");
    }

    /// Every kind, for a filter test that must not miss one.
    const ALL_KINDS: &[EventKind] = &[
        EventKind::GraphStarted,
        EventKind::MemberStarted,
        EventKind::TurnStarted,
        EventKind::TurnActivity,
        EventKind::TurnCompleted,
        EventKind::TurnInterrupted,
        EventKind::MemberHeartbeat,
        EventKind::FallbackAdvanced,
        EventKind::MemberDied,
        EventKind::CronFired,
        EventKind::CronReset,
        EventKind::MemberSettled,
        EventKind::GraphSettled,
    ];

    /// Every kind spells itself on the wire the way serde does, which is what a
    /// glob is matched against.
    #[test]
    fn every_event_kind_spells_itself_the_way_the_wire_does() {
        for kind in ALL_KINDS {
            assert_eq!(
                Value::from(kind.as_str()),
                serde_json::to_value(kind).expect("serializes"),
                "{kind:?} spells itself two ways"
            );
        }
    }

    /// The filter every run has until one is named: every kind passes, and the
    /// grammar's own empty lists mean the same thing.
    #[test]
    fn a_filter_naming_nothing_admits_everything() {
        let empty: EventFilter = serde_json::from_str("{}").expect("an empty filter");
        assert_eq!(empty, EventFilter::default());
        let spelled: EventFilter =
            serde_json::from_str(r#"{"include": [], "exclude": []}"#).expect("empty lists");
        assert_eq!(spelled, EventFilter::default());
        for kind in ALL_KINDS {
            assert!(empty.allows(Source::Agentgraph, kind.as_str(), &Labels::default()));
        }
        // And it serializes back to nothing, so a filter that says nothing is
        // not a document naming empty lists at every consumer downstream.
        assert_eq!(
            serde_json::to_string(&EventFilter::default()).expect("serializes"),
            "{}"
        );
    }

    /// A spec is external input: a key nobody declared is a typo, not something
    /// to drop silently.
    #[test]
    fn a_filter_with_an_unknown_field_is_rejected() {
        let error = serde_json::from_str::<EventFilter>(r#"{"includes": []}"#)
            .expect_err("`includes` is not a list this grammar has");
        assert!(error.to_string().contains("includes"), "{error}");
        let error = serde_json::from_str::<Matcher>(r#"{"role": "agent"}"#)
            .expect_err("a turn's role lives in a payload, not in this grammar");
        assert!(error.to_string().contains("role"), "{error}");
    }

    /// Include names what passes, exclude names what does not, and a match in
    /// exclude wins over anything include said.
    #[test]
    fn exclude_beats_include_and_an_empty_include_admits_everything() {
        let kinds = |filter: &EventFilter| -> Vec<&str> {
            ALL_KINDS
                .iter()
                .map(|kind| kind.as_str())
                .filter(|kind| filter.allows(Source::Agentgraph, kind, &Labels::default()))
                .collect()
        };
        let matcher = |kind: &str| Matcher {
            kind: Some(kind.to_string()),
            ..Matcher::default()
        };

        let include_only = EventFilter {
            include: vec![matcher("graph-*")],
            exclude: Vec::new(),
        };
        assert_eq!(kinds(&include_only), vec!["graph-started", "graph-settled"]);

        let exclude_only = EventFilter {
            include: Vec::new(),
            exclude: vec![matcher("turn-*"), matcher("member-heartbeat")],
        };
        assert!(!kinds(&exclude_only)
            .iter()
            .any(|kind| kind.starts_with("turn-") || *kind == "member-heartbeat"));
        assert!(kinds(&exclude_only).contains(&"member-started"));

        // The precedence: a matcher on both lists is rejected, and the rest of
        // a broad include survives.
        let both = EventFilter {
            include: vec![matcher("turn-*")],
            exclude: vec![matcher("turn-activity")],
        };
        assert_eq!(
            kinds(&both),
            vec!["turn-started", "turn-completed", "turn-interrupted"]
        );
    }

    /// A glob spans the wire strings it names and nothing else.
    #[test]
    fn a_kind_glob_spans_the_wire_strings_it_names() {
        for (pattern, wanted) in [
            ("turn-*", vec!["turn-started", "turn-activity"]),
            ("*", vec!["turn-started", "graph-settled", ""]),
            ("*-started", vec!["turn-started", "member-started"]),
            ("member-*ed", vec!["member-started", "member-settled"]),
            ("turn-activity", vec!["turn-activity"]),
        ] {
            for kind in wanted {
                assert!(glob(pattern, kind), "{pattern:?} should match {kind:?}");
            }
        }
        for (pattern, refused) in [
            ("turn-*", vec!["member-started", "turn", "a-turn-started"]),
            ("*-started", vec!["started-turn", "turn-startedly"]),
            ("turn-activity", vec!["turn-activit", "turn-activityy"]),
            // A glob that runs out of pattern before it runs out of text, which
            // is the case a naive prefix walk gets wrong.
            ("*-fired", vec!["cron-fired-again"]),
            ("", vec!["cron-fired"]),
        ] {
            for kind in refused {
                assert!(
                    !glob(pattern, kind),
                    "{pattern:?} should not match {kind:?}"
                );
            }
        }
        // Backtracking: the first run `*` stands for is too short, and it has to
        // give characters back until the tail lines up.
        assert!(glob("*-c", "a-b-c"));
        assert!(glob("", ""));
    }

    /// Every field a matcher names must hold, and a label the envelope never
    /// stamped is not one a matcher can name its way into.
    #[test]
    fn a_matcher_conjoins_its_fields_and_never_matches_a_label_that_was_not_stamped() {
        let worker = Labels {
            run_id: Some("R".into()),
            member: Some("worker".into()),
            persona: Some("engineer".into()),
            ..Labels::default()
        };
        let filter = |matcher: Matcher| EventFilter {
            include: vec![matcher],
            exclude: Vec::new(),
        };

        let both = filter(Matcher {
            member: Some("worker".into()),
            persona: Some("engineer".into()),
            ..Matcher::default()
        });
        assert!(both.allows(Source::Agentgraph, "turn-started", &worker));
        // One field of the pair wrong is the whole matcher wrong.
        let mismatched = filter(Matcher {
            member: Some("worker".into()),
            persona: Some("reviewer".into()),
            ..Matcher::default()
        });
        assert!(!mismatched.allows(Source::Agentgraph, "turn-started", &worker));
        // The graph's own events carry no member, and a matcher naming one does
        // not reach them.
        assert!(!both.allows(Source::Agentgraph, "graph-started", &Labels::default()));
        // Nor does a matcher reach a *node* or *step* nothing here stamps.
        let by_node = filter(Matcher {
            node: Some("service".into()),
            ..Matcher::default()
        });
        assert!(!by_node.allows(Source::Agentgraph, "turn-started", &worker));
        assert!(by_node.allows(
            Source::Agentgraph,
            "turn-started",
            &Labels {
                node: Some("service".into()),
                step: Some("implement".into()),
                ..Labels::default()
            }
        ));
        // A reserved key an operator stamped by hand lands among the extras and
        // reaches the wire under that name, so a matcher naming it sees it.
        let by_hand = Labels {
            extra: [("node".to_string(), Value::from("service"))]
                .into_iter()
                .collect(),
            ..worker.clone()
        };
        assert!(by_node.allows(Source::Agentgraph, "turn-started", &by_hand));
        // But only a string is a label value: a number under that key is not one
        // this grammar can be equal to.
        let numeric = Labels {
            extra: [("node".to_string(), Value::from(7))].into_iter().collect(),
            ..worker.clone()
        };
        assert!(!by_node.allows(Source::Agentgraph, "turn-started", &numeric));

        // And `source` is exact equality, not a family a sibling falls into.
        let by_source = filter(Matcher {
            source: Some(Source::Vcs),
            ..Matcher::default()
        });
        assert!(by_source.allows(Source::Vcs, "turn-started", &worker));
        assert!(!by_source.allows(Source::Agentgraph, "turn-started", &worker));
    }

    /// A relayed sibling's kind is matched as the wire string it arrived as: it
    /// is not one of this crate's kinds, and naming one is not an error.
    #[test]
    fn a_relayed_siblings_kind_is_matched_as_a_wire_string() {
        let spec = r#"{"include": [{"source": "vcs", "kind": "commit-*"}]}"#;
        let filter: EventFilter = serde_json::from_str(spec).expect("a filter may name any kind");
        filter.validate().expect("a kind this crate lacks is legal");

        let relayed = Labels {
            run_id: Some("R".into()),
            member: Some("worker".into()),
            ..Labels::default()
        };
        assert!(filter.allows(Source::Vcs, "commit-created", &relayed));
        assert!(filter.allows(Source::Vcs, "commit-amended", &relayed));
        // The same kind from another library is another event.
        assert!(!filter.allows(Source::Pipeline, "commit-created", &relayed));
        // And this crate's own kinds are outside a spec that named none of them.
        for kind in ALL_KINDS {
            assert!(!filter.allows(Source::Agentgraph, kind.as_str(), &relayed));
        }
    }

    /// A matcher that could match nothing — or everything — is refused, naming
    /// which list it is in, where, and what it says.
    #[test]
    fn a_matcher_that_names_nothing_usable_is_refused_by_name() {
        let refused = |spec: &str| -> String {
            serde_json::from_str::<EventFilter>(spec)
                .expect("the spec parses")
                .validate()
                .expect_err("the spec is not usable")
        };

        let empty = refused(r#"{"exclude": [{"kind": "turn-*"}, {}]}"#);
        assert!(empty.contains("exclude[1] {}"), "{empty}");
        assert!(empty.contains("matches every event"), "{empty}");

        let blank = refused(r#"{"include": [{"member": "  "}]}"#);
        assert!(blank.contains(r#"include[0] {"member":"  "}"#), "{blank}");
        assert!(blank.contains("`member` is empty"), "{blank}");

        let no_kind = refused(r#"{"include": [{"source": "vcs"}, {"kind": ""}]}"#);
        assert!(no_kind.contains(r#"include[1] {"kind":""}"#), "{no_kind}");
        assert!(no_kind.contains("`kind` is empty"), "{no_kind}");

        // A matcher naming one field is enough, whichever field it is.
        for spec in [
            r#"{"include": [{"source": "agentgraph"}]}"#,
            r#"{"include": [{"kind": "*"}]}"#,
            r#"{"exclude": [{"run_id": "R"}]}"#,
            r#"{"exclude": [{"step": "implement"}]}"#,
        ] {
            serde_json::from_str::<EventFilter>(spec)
                .expect("parses")
                .validate()
                .unwrap_or_else(|why| panic!("{spec}: {why}"));
        }
    }

    /// An emitter writes only what its filter admits, numbers only what it
    /// wrote, and hands every envelope back to the run either way.
    ///
    /// The `seq` half is the point: the contract has a consumer read per-stream
    /// gaps as loss, so a filtered stream that skipped numbers would report
    /// every deliberate omission as a dropped event.
    #[test]
    fn a_filter_decides_what_is_written_without_deciding_what_the_run_sees() {
        let recorder = Recorder::default();
        let emitter = Emitter::new("run-1", Box::new(recorder.clone())).with_filter(EventFilter {
            include: Vec::new(),
            exclude: vec![Matcher {
                kind: Some("turn-*".to_string()),
                ..Matcher::default()
            }],
        });
        // A derived emitter inherits it: one run is one merged stream.
        let member = emitter.with_labels(Labels {
            member: Some("worker".into()),
            ..Labels::default()
        });

        emitter.emit(EventKind::GraphStarted, Map::new());
        let suppressed = member.emit(EventKind::TurnActivity, Map::new());
        member.emit(EventKind::MemberSettled, Map::new());
        emitter.emit(EventKind::GraphSettled, Map::new());

        let written = lines(&recorder);
        assert_eq!(
            written.iter().map(|e| e.kind).collect::<Vec<_>>(),
            vec![
                EventKind::GraphStarted,
                EventKind::MemberSettled,
                EventKind::GraphSettled
            ]
        );
        assert_eq!(
            written.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        // The suppressed envelope still came back, fully formed, for the
        // liveness and settle detection that read what `emit` returns.
        assert_eq!(suppressed.kind, EventKind::TurnActivity);
        assert_eq!(suppressed.labels.member.as_deref(), Some("worker"));
        assert_eq!(suppressed.v, ENVELOPE_VERSION);
    }

    /// An emitter whose sink is gone still numbers and returns its events: the
    /// stream is a view, and the run does not fail because nobody is reading.
    #[test]
    fn a_broken_sink_does_not_stop_the_run() {
        struct Broken;
        impl std::io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("gone"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("gone"))
            }
        }
        let emitter = Emitter::new("s", Box::new(Broken));
        assert_eq!(emitter.emit(EventKind::GraphStarted, Map::new()).seq, 0);
        assert_eq!(emitter.emit(EventKind::GraphSettled, Map::new()).seq, 1);
        assert!(format!("{emitter:?}").contains("seq: 2"));
    }
}
