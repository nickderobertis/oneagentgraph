//! The graph config schema.
//!
//! A graph is one YAML document, given to the CLI by path or URL, defined by
//! `docs/contract.md`. These structs are the schema and nothing else: no ref is
//! resolved, no remote is fetched or checksummed, no `model`/chain pairing is
//! validated, and no member is launched.
//!
//! Where the contract states a field's default (`stream: true`) or shows it as
//! `null`/`[]`, that reading is encoded here. Where it neither states a default
//! nor marks a field optional, the field is required.

// llmlint: ignore-file[invalid_states_unrepresentable] `mode` is stringly typed because
// the approval modes belong to onejudge and `docs/contract.md` names exactly one of them
// (`bypass`). An enum here would either invent the rest — the interface-only stage forbids
// adding a public item the contract does not name — or reject a mode onejudge accepts.
// Narrow it when the contract enumerates them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::event::EventFilter;

/// A reference to another config file: a filesystem path, or an `https` URL that
/// is fetched, checksummed, and recorded content-addressed in the run record so
/// replay never depends on the URL staying stable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConfigRef(pub String);

/// One graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphConfig {
    /// Schema version.
    pub version: u32,
    /// The graph's name.
    pub name: String,
    /// Exported to every member process. Values may reference `${HOME}`.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// The directory this graph's own persona catalog sits in.
    ///
    /// A member's `persona:` that is a bare or slash-qualified *name* — rather
    /// than a path or a URL — is looked up here as `<name>.yaml` before it is
    /// looked up in the personas this crate ships. Without it an operator's own
    /// catalog is unreachable by name, which is most of what a catalog is for.
    /// A relative path resolves against the graph document, like every other ref
    /// a graph names. Requires graph schema version
    /// [`FIRST_PERSONA_CATALOG_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personas: Option<PathBuf>,
    /// What this graph's run puts on its merged event stream.
    ///
    /// Absent is every envelope, which is what every graph written before the
    /// block existed says and goes on meaning. Requires graph schema version
    /// [`FIRST_EVENT_FILTER_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Events>,
    /// The members, by name.
    pub members: BTreeMap<String, Member>,
}

/// A graph's own say over its merged event stream.
///
/// A block rather than a bare `filter:` key, because what a run publishes is a
/// subject of its own — this is where a second decision about the stream goes,
/// rather than beside the members that happen to feed it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Events {
    /// Which envelopes reach the stream; absent is all of them.
    ///
    /// `oneagentgraph run --event-filter` names one instead, and wins over this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<EventFilter>,
}

/// A graph member: either a two-party onejudge conversation, or a single-sided
/// oneharness agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Member {
    /// Two-party: an agent and a judge.
    Onejudge(OnejudgeMember),
    /// Single-sided: one agent, no judge.
    Oneharness(OneharnessMember),
}

impl Member {
    /// Members whose successful settle precedes this member's first run.
    #[must_use]
    pub fn deps(&self) -> &[String] {
        match self {
            Member::Onejudge(member) => &member.deps,
            Member::Oneharness(member) => &member.deps,
        }
    }

    /// This member's schedule, when it carries one.
    ///
    /// The same field on both kinds, and the same shape; what a firing *is*
    /// differs. A single-sided member's is one turn, so its clock fires it again
    /// and again. A two-party member's is one long-lived conversation, so the
    /// schedule paces that conversation's turns and never starts a second one —
    /// see [`OnejudgeMember::schedule`].
    #[must_use]
    pub fn schedule(&self) -> Option<Schedule> {
        match self {
            Member::Onejudge(member) => member.schedule,
            Member::Oneharness(member) => member.schedule,
        }
    }

    /// The directory this member works in, when it named one of its own.
    #[must_use]
    pub fn dir(&self) -> Option<&std::path::Path> {
        match self {
            Member::Onejudge(member) => member.dir.as_deref(),
            Member::Oneharness(member) => member.dir.as_deref(),
        }
    }

    /// What this member's document says about holding the run open, if it says
    /// anything. What an omission means is [`GraphConfig::is_background`]'s.
    #[must_use]
    pub fn declared_background(&self) -> Option<bool> {
        match self {
            Member::Onejudge(member) => member.background,
            Member::Oneharness(member) => member.background,
        }
    }
}

impl GraphConfig {
    /// Whether member `name` is **background** — work the run does not stay open
    /// for — rather than **foreground**, which holds the run open while it is
    /// unfinished.
    ///
    /// The one reading of `background`, and it takes the whole graph because
    /// what an omission means depends on the schema this document declares.
    /// From [`FIRST_BACKGROUND_VERSION`] a member's own `background:` is the
    /// answer when it names one, and otherwise its schedule decides: a scheduled
    /// member is a pacemaker unless its author says otherwise, and an unscheduled
    /// one is the work being paced. Under every older schema the answer is the
    /// inference those documents have always run under — a member with a
    /// schedule, or whose every dependency (transitively) is such a member, is
    /// background, and every other member is foreground — so a document written
    /// before the field existed runs exactly as it did.
    ///
    /// A name this graph has no member called is answered `false`: nothing
    /// declared it, and every caller reaches this through a graph whose members
    /// and `deps` [`validate`] and `run::ready_order` have already checked.
    #[must_use]
    pub fn is_background(&self, name: &str) -> bool {
        self.background_memoized(name, &mut BTreeMap::new())
    }

    /// [`is_background`](Self::is_background), sharing one memo across the
    /// pre-[`FIRST_BACKGROUND_VERSION`] descent so a diamond of `deps` is walked
    /// once per member rather than once per path.
    fn background_memoized(&self, name: &str, memo: &mut BTreeMap<String, bool>) -> bool {
        if let Some(answer) = memo.get(name) {
            return *answer;
        }
        let Some(member) = self.members.get(name) else {
            return false;
        };
        let answer = if self.version >= FIRST_BACKGROUND_VERSION {
            member
                .declared_background()
                .unwrap_or_else(|| member.schedule().is_some())
        } else {
            member.schedule().is_some()
                || (!member.deps().is_empty()
                    && member
                        .deps()
                        .iter()
                        .all(|dep| self.background_memoized(dep, memo)))
        };
        memo.insert(name.to_string(), answer);
        answer
    }
}

/// A `kind: onejudge` member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnejudgeMember {
    /// The onejudge base config, by path or URL.
    pub base_config: ConfigRef,
    /// The persona delta, by path or URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<ConfigRef>,
    /// The task prose. Usually supplied by `--task` instead.
    ///
    /// `{task}` anywhere in it expands to the run's own `--task`, and `{{task}}`
    /// is the literal text `{task}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The side that does the work.
    pub agent: AgentSide,
    /// The sides that supervise — the judge panel.
    ///
    /// One side in the document is the one-element shorthand for a list of
    /// sides, and both spellings read into this one list, so there is exactly
    /// one composition path over it (`crate::invoke`). A one-element list is
    /// written back as the single mapping it reads from, so a document written
    /// before the list existed reads back to the same graph and is written in
    /// the shape it was written in — which is what keeps the checked-in graph
    /// goldens, serialized from this build, unchanged.
    #[serde(with = "judge_sides")]
    pub judge: Vec<JudgeSide>,
    /// onejudge approval mode.
    pub mode: String,
    /// Turn ceiling for the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// The conversation's worktree, when this member's job is not the graph's.
    ///
    /// The same field, on the same terms, as [`OneharnessMember::dir`]: the
    /// directory the agent side's harness runs in (`oneharness run --cwd`) and
    /// the one onejudge takes as the conversation's skill directory, resolved
    /// exactly as a single-sided member's is — relative against the run's
    /// `--dir`, absolute used as written — and defaulting to the run's. The agent
    /// side stays pinned to its stamped config whatever this says. Requires graph
    /// schema version [`FIRST_TWO_PARTY_JOB_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// The pace this member's **one** conversation runs at, when it is paced.
    ///
    /// The same [`Schedule`] a single-sided member carries, under the same
    /// rules, with one difference in what a firing *is*: a `kind: onejudge`
    /// member is one long-lived conversation, and a firing of it is one turn of
    /// that conversation rather than a conversation run to settlement. So
    /// `start_after` defers the first turn, `every` is a hold between turns —
    /// counted from the moment the judge side closes a turn with a next
    /// instruction, and skipped when it closed with none — and when the
    /// conversation settles the schedule starts no second one. The hold is
    /// [`crate::judge::Pace`]'s. Requires graph schema version
    /// [`FIRST_TWO_PARTY_JOB_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    /// Whether this member holds the run open. Absent is decided by the schedule.
    ///
    /// Read through [`GraphConfig::is_background`], because what its absence
    /// means depends on the schema the document declares. Requires graph schema
    /// version [`FIRST_BACKGROUND_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    /// Members whose successful settle precedes this member's first run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<String>,
}

/// A `kind: oneharness` member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OneharnessMember {
    /// The oneharness config, by path or URL.
    pub oneharness_config: ConfigRef,
    /// The persona delta, by path or URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<ConfigRef>,
    /// The task prose. Usually supplied by `--task` instead — the same field, on
    /// the same terms, as [`OnejudgeMember::task`].
    ///
    /// A member whose job is not the graph's needs its own prose, and without
    /// this a single-sided member had no way to hold any: it received the
    /// graph-wide `--task` verbatim, and a scheduled member whose whole job is to
    /// write one status update was handed the orchestrator's instructions to
    /// drive the run instead. Requires graph schema version 3.
    ///
    /// `{task}` anywhere in it expands to the run's own `--task`, which is how two
    /// members share one run's context and differ only in what they are told to do
    /// with it. `{{task}}` is the literal text `{task}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The directory this member works in, when its job is not the graph's.
    ///
    /// Named to oneharness as `run --cwd`, exactly as the graph-wide `--dir` is,
    /// and defaulting to it. A relative path is resolved against that graph-wide
    /// directory, so `dir: ./api` is the member working one level inside the
    /// graph's own. Requires graph schema version 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// Present on a cron member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    /// Whether this member holds the run open. Absent is decided by the schedule.
    ///
    /// The same field, on the same terms, as [`OnejudgeMember::background`]:
    /// read through [`GraphConfig::is_background`], and requiring graph schema
    /// version [`FIRST_BACKGROUND_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    /// Commands run immediately before each of this member's turns, whose output
    /// is prepended to the instruction that turn receives.
    ///
    /// A supervisory member opens each turn by going and looking — spending tool
    /// calls to rediscover state that already exists and is already labelled, and
    /// reporting on whichever half of it the turn got to. This is how such a
    /// member is *handed* that state instead: its first act becomes reading a
    /// prepared view, and it investigates only what looks strange.
    ///
    /// Scoped to this member kind because here a member's turn **is** its run, so
    /// "immediately before the turn" is an exact moment rather than an
    /// approximation of one. A two-party member's turns are onejudge's to open,
    /// and this crate has no seam inside them. Requires graph schema version
    /// [`FIRST_PRE_TURN_VERSION`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_turn: Vec<PreTurn>,
    /// Members whose successful settle precedes this member's first run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<String>,
}

/// One command run immediately before a member's turn, whose output becomes part
/// of that turn's context.
///
/// An argv, never a command line: what a member declares here is spawned
/// directly, so nothing in it is a shell's to expand, split, or interpret. That
/// is also why it is checked here rather than where it is spawned — see
/// [`validate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreTurn {
    /// The program and its arguments.
    pub command: Vec<String>,
    /// What this view is called, in the turn's context and on the stream.
    ///
    /// A name the *member's own prose* can refer to — "read the queue view" — so
    /// it is the operator's word rather than the program's. Absent is the program
    /// itself, which is a name too, just a less useful one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// How many seconds this command may take before the turn goes on without
    /// it. [`DEFAULT_PRE_TURN_SECONDS`] when it names none.
    ///
    /// Its own bound, deliberately unrelated to the member's turn: a member with
    /// no per-turn deadline at all — a single-sided member is one — would
    /// otherwise wait on a hung view forever.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

impl PreTurn {
    /// How long this command may run, which is what it named or the default.
    #[must_use]
    pub fn seconds(&self) -> u64 {
        self.timeout.unwrap_or(DEFAULT_PRE_TURN_SECONDS)
    }

    /// What this view is called: the label its author gave it, else the program.
    ///
    /// Never empty for a command [`validate`] accepted, because that refuses both
    /// an empty label and an empty program.
    #[must_use]
    pub fn view(&self) -> &str {
        match &self.label {
            Some(label) => label,
            None => self.command.first().map_or("", String::as_str),
        }
    }
}

/// The agent side of a onejudge member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSide {
    /// The oneharness config for this side, by path or URL.
    pub oneharness_config: ConfigRef,
    /// Optional model override, forwarded to the harness unchecked. It must be
    /// paired with a config whose declared chain is one harness family, which is
    /// checked before launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Whether the side's turns stream as they happen. `false` is report-only.
    #[serde(default = "default_stream")]
    pub stream: bool,
}

/// One judge of a onejudge member's panel: an oneharness identity chain, an
/// `llmlint` run, or a command provider.
///
/// Told apart by the one field each shape requires — `oneharness_config`,
/// `kind: llmlint`, `command` — rather than by serde's untagged fallback, so a
/// mapping that is none of the three is refused naming what it carried instead
/// of with "did not match any variant". The harness and command shapes are
/// spelled exactly as they were before a member could carry a list, so every
/// graph written against the single spelling — and every
/// `members.<name>.judge.oneharness_config=…` override — still reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum JudgeSide {
    /// Supervised by a harness selected from an oneharness config.
    Harness(JudgeHarness),
    /// Supervised by one `llmlint` run over the worker's tree.
    Llmlint(JudgeLlmlint),
    /// Supervised by a command provider.
    Command(JudgeCommand),
}

impl JudgeSide {
    /// The label this judge asked to be known by, whatever its shape.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        match self {
            JudgeSide::Harness(side) => side.label.as_deref(),
            JudgeSide::Llmlint(side) => side.label.as_deref(),
            JudgeSide::Command(side) => side.label.as_deref(),
        }
    }

    /// The provider kind onejudge knows this judge as, which is also what it
    /// defaults an unlabelled judge's label from.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            JudgeSide::Harness(_) => "oneharness",
            JudgeSide::Llmlint(_) => "llmlint",
            JudgeSide::Command(_) => "command",
        }
    }

    /// Read one judge off the mapping a graph wrote, or say which of the three
    /// shapes it is not.
    fn classify(entry: serde_json::Value) -> Result<Self, String> {
        let Some(map) = entry.as_object() else {
            return Err(format!("{entry} is not a mapping"));
        };
        let shape = if map.contains_key("oneharness_config") {
            "a harness side"
        } else if let Some(kind) = map.get("kind") {
            if kind != "llmlint" {
                return Err(format!(
                    "`kind: {}` names no judge shape: the graph spells a harness side by its \
                     `oneharness_config` and a command side by its `command`, and `kind:` only \
                     ever says `llmlint`",
                    kind.as_str()
                        .map_or_else(|| kind.to_string(), str::to_string)
                ));
            }
            "an llmlint side"
        } else if map.contains_key("command") {
            "a command side"
        } else {
            let keys: Vec<&str> = map.keys().map(String::as_str).collect();
            return Err(format!(
                "{{{}}} is none of the three judge shapes: a harness side names an \
                 `oneharness_config`, an llmlint side says `kind: llmlint`, and a command side \
                 names a `command`",
                keys.join(", ")
            ));
        };
        let read = match shape {
            "a harness side" => serde_json::from_value(entry).map(JudgeSide::Harness),
            "an llmlint side" => serde_json::from_value(entry).map(JudgeSide::Llmlint),
            _ => serde_json::from_value(entry).map(JudgeSide::Command),
        };
        read.map_err(|err| format!("{shape}: {err}"))
    }
}

impl<'de> Deserialize<'de> for JudgeSide {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let entry = serde_json::Value::deserialize(deserializer)?;
        JudgeSide::classify(entry).map_err(serde::de::Error::custom)
    }
}

/// The harness-backed judge side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeHarness {
    /// The oneharness config for this side, by path or URL.
    pub oneharness_config: ConfigRef,
    /// Optional model override, under the same pairing rule as
    /// [`AgentSide::model`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// This judge's name on every surface onejudge attributes to it; absent,
    /// onejudge defaults it from the kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// The `llmlint` judge side: one lint run over the worker's tree per decision,
/// recognized by its `kind: llmlint`.
///
/// Every field but `config` is handed to onejudge as written. `config` is a
/// path resolved against the graph document's directory, like every other path
/// a graph names, and handed over absolute — never copied into the member's
/// scratch, because an llmlint config resolves its own plugins relative to
/// itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeLlmlint {
    /// The shape's own tag, which is what tells it from the other two.
    pub kind: LlmlintKind,
    /// The llmlint config file, relative to the graph document. Absent, llmlint
    /// discovers its own from the worker's tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
    /// The llmlint executable. Absent, onejudge runs `llmlint` from `PATH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin: Option<String>,
    /// The git revision to review the worker's changes against. Absent, llmlint
    /// judges the whole tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_base: Option<String>,
    /// Extra arguments appended to every `llmlint lint` run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// This judge's name, as on [`JudgeHarness::label`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// The one value [`JudgeLlmlint::kind`] takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmlintKind {
    /// `kind: llmlint`.
    Llmlint,
}

/// The command-provider judge side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeCommand {
    /// The command and its arguments.
    pub command: Vec<String>,
    /// This judge's name, as on [`JudgeHarness::label`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// The two spellings of [`OnejudgeMember::judge`], read into one list and
/// written back as the shorter one.
mod judge_sides {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::JudgeSide;

    /// One mapping or a list of them. A shape that is neither — a scalar, a
    /// list holding a scalar — is refused naming the entry, as one mapping that
    /// is none of the three judge shapes is.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<JudgeSide>, D::Error> {
        let written = serde_json::Value::deserialize(deserializer)?;
        match written {
            serde_json::Value::Array(entries) => entries
                .into_iter()
                .enumerate()
                .map(|(index, entry)| {
                    JudgeSide::classify(entry).map_err(|why| {
                        D::Error::custom(format!("judge entry {}: {why}", index + 1))
                    })
                })
                .collect(),
            entry @ serde_json::Value::Object(_) => JudgeSide::classify(entry)
                .map(|side| vec![side])
                .map_err(|why| D::Error::custom(format!("judge: {why}"))),
            other => Err(D::Error::custom(format!(
                "judge: {other} is neither one side nor a list of sides"
            ))),
        }
    }

    /// A one-element list as the single mapping it reads from, and any other
    /// length as the list.
    pub fn serialize<S: Serializer>(sides: &[JudgeSide], serializer: S) -> Result<S::Ok, S::Error> {
        match sides {
            [one] => one.serialize(serializer),
            many => many.serialize(serializer),
        }
    }
}

/// A cron member's schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    /// Interval in seconds.
    pub every: u64,
    /// Seconds before this member's **first** turn, read through
    /// [`first_turn_after`](Self::first_turn_after) because what its absence
    /// means depends on the schema the document declares.
    ///
    /// `0` is the member taking a turn the moment the graph starts, which is what
    /// every schedule did before this field existed. Requires graph schema
    /// version [`FIRST_START_AFTER_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_after: Option<u64>,
    /// Whether `reset-timer` may restart this schedule's clock.
    #[serde(default)]
    pub resettable: bool,
}

impl Schedule {
    /// Seconds between this member coming up and its first turn, under the schema
    /// `schema` declares.
    ///
    /// A schedule naming no `start_after` waits one whole interval from
    /// [`FIRST_START_AFTER_VERSION`] on, because "every 1800 seconds" reads as
    /// *from now on* rather than *now, and then every 1800 seconds* — and a member
    /// whose job is to report progress has nothing to report at t=0. Under an
    /// older schema it waits none, which is what every schedule written against
    /// those versions has always done, so the default moves only for a document
    /// that says which schema it was written against.
    ///
    /// The schema is a parameter rather than a field because it belongs to the
    /// document, not to the schedule: one graph has one version, and a `Schedule`
    /// carrying its own copy could disagree with the graph holding it.
    #[must_use]
    pub fn first_turn_after(&self, schema: u32) -> u64 {
        match self.start_after {
            Some(seconds) => seconds,
            None if schema >= FIRST_START_AFTER_VERSION => self.every,
            None => 0,
        }
    }
}

/// serde default for [`AgentSide::stream`]: streaming is on unless a graph turns
/// it off.
fn default_stream() -> bool {
    true
}

/// The longest span a [`Schedule`] may name, in seconds — a shade over 136 years.
///
/// A typo guard rather than a policy about cadence. A schedule's seconds are a
/// `u64` an external document supplies, and `u64::MAX` of them is some four
/// hundred billion years: a member whose clock names that is one that never fires
/// and never says why, which is indistinguishable from the member being broken.
/// `u32::MAX` is past every run anyone will make and short of every platform's
/// own clock range, so it refuses the typo without refusing a cadence.
pub const MAX_SCHEDULE_SECONDS: u64 = u32::MAX as u64;

/// How long a [`PreTurn`] command that names no `timeout` may run.
///
/// A view is a thing a turn *waits on*, so this is the span past which waiting
/// costs more than the context is worth. Half a minute is generous for reading
/// prepared state off a disk or a socket and short enough that a member whose
/// every view is wedged still opens its turn promptly.
pub const DEFAULT_PRE_TURN_SECONDS: u64 = 30;

/// The longest a [`PreTurn`] command may be given, whatever it names.
///
/// Not a preference: the ceiling is what keeps this feature from becoming a way
/// to wedge a member. Every declared view timing out costs
/// [`MAX_PRE_TURN_COMMANDS`] × this — twenty minutes — which is inside
/// [`crate::liveness::DEFAULT_STALL_TIMEOUT`], so a member cannot be held past
/// its own supervision by the views it declared.
pub const MAX_PRE_TURN_SECONDS: u64 = 300;

/// How many [`PreTurn`] commands one member may declare.
///
/// Each view is bounded on its own
/// ([`crate::preturn::MAX_PRE_TURN_OUTPUT_BYTES`]), and a list nobody bounded
/// would make the *total* injected context unbounded again — which is the cost
/// the per-view bound exists to stop. Four is a prepared view, a queue, a
/// timeline, and one more; a member wanting a fifth wants one command that
/// assembles them.
pub const MAX_PRE_TURN_COMMANDS: usize = 4;

/// The first graph schema version this crate still reads.
pub const FIRST_SCHEMA_VERSION: u32 = 1;

/// The latest graph schema version this crate reads and writes in examples.
pub const SCHEMA_VERSION: u32 = 9;

/// How a member's own `task` is read: as the prose it has always been, or as a
/// template naming the run's task.
///
/// The document's schema decides, and this is that decision rather than the
/// version it came from — so nothing downstream carries a version number it would
/// have to know the meaning of, and no unsupported one can reach a member's
/// launch at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskText {
    /// Every character of it is what the member is given.
    Literal,
    /// `{task}` in it expands to the run's own task, and `{{task}}` is the
    /// literal text `{task}`.
    Template,
}

impl TaskText {
    /// What a document declaring `schema` means by a member's `task`.
    ///
    /// Total over every `u32`, including versions this build does not read: a
    /// graph is refused for its version by [`validate`] long before a member is
    /// built, so the only thing this has to be is unambiguous.
    #[must_use]
    pub fn under(schema: u32) -> Self {
        if schema >= FIRST_TASK_TOKEN_VERSION {
            TaskText::Template
        } else {
            TaskText::Literal
        }
    }
}

/// The first graph schema version in which a [`Schedule`] may name a
/// [`start_after`](Schedule::start_after) — and, from which, one that names none
/// waits a whole interval before its first turn rather than taking it at t=0.
///
/// The version exists for the *default* rather than for the field. A gate on the
/// field alone would be the pattern [`FIRST_MEMBER_JOB_VERSION`] follows, where a
/// document omitting it is unaffected; this one changes what an omission means, so
/// a document written against an older schema has to keep the meaning it was
/// written under. That is also why the field is refused there rather than merely
/// ignored: a version 3 document asking for `start_after: 30` would otherwise be
/// silently given `start_after: 0`, which is the opposite of what it asked for.
pub const FIRST_START_AFTER_VERSION: u32 = 4;

/// The first graph schema version in which `{task}` in a member's own
/// [`task`](OneharnessMember::task) expands to the run's, rather than standing for
/// itself.
///
/// A gate for the same reason [`FIRST_START_AFTER_VERSION`] is one, and it is the
/// same version: this changes what an existing field's *text* means, and a member
/// task that happens to contain those six characters said them literally under
/// every schema before this one. Unlike `start_after` there is nothing to refuse
/// in an older document — the text is valid prose there, and prose is exactly what
/// it stays.
pub const FIRST_TASK_TOKEN_VERSION: u32 = 4;

/// The first graph schema version in which a graph may name an
/// [`events`](GraphConfig::events) block, and so a filter over its own stream.
///
/// The gate is on the *reading*, the way [`FIRST_START_AFTER_VERSION`] is: a
/// document declaring an older schema and naming this block is refused by the
/// block's name rather than run under a filter that schema never had. Omitting
/// it is unaffected under every version — a graph with no `events` streams every
/// envelope, which is what all of them did before this existed.
pub const FIRST_EVENT_FILTER_VERSION: u32 = 5;

/// The first graph schema version in which a graph may name its own
/// [`personas`](GraphConfig::personas) catalog.
///
/// The gate is on the *reading*, like [`FIRST_EVENT_FILTER_VERSION`]: naming a
/// catalog changes what a member's bare `persona: NAME` means — it may now name
/// the operator's own file, and a name that is in both catalogs is refused
/// rather than resolved — so a document declaring an older schema is refused by
/// the key's name rather than run with a resolution rule that schema never had.
/// Omitting it is unaffected under every version: a name resolves to a shipped
/// persona and anything else to a path or URL, exactly as before.
pub const FIRST_PERSONA_CATALOG_VERSION: u32 = 6;

/// The first graph schema version in which a single-sided member may declare
/// [`pre_turn`](OneharnessMember::pre_turn) commands.
///
/// A gate on the *field*, the way [`FIRST_MEMBER_JOB_VERSION`] is one: a member
/// that declares none behaves exactly as it always did — no command is run and
/// the instruction its turn receives is untouched — so a document written before
/// this existed is unaffected under every version. What the gate buys is that a
/// document *using* it says which schema it was written against, rather than
/// being handed to a build that would silently run no view at all.
pub const FIRST_PRE_TURN_VERSION: u32 = 7;

/// The first graph schema version in which a member of either kind may declare
/// [`background`](OnejudgeMember::background) — and, from which, one that
/// declares none is background exactly when it carries a schedule.
///
/// The version exists for the *default* rather than for the field, the way
/// [`FIRST_START_AFTER_VERSION`] does: before it, whether a member held the run
/// open was inferred from the graph's shape — a scheduled member, or one whose
/// every dependency descends from schedules, did not — and from it that is a
/// declaration the document makes per member, with the schedule deciding only
/// what an omission means. The two readings agree on every graph that has no
/// unscheduled member downstream of nothing but schedules, and differ on the
/// rest, so a document keeps the reading it was written under and is refused the
/// field rather than run under a rule its schema never had.
pub const FIRST_BACKGROUND_VERSION: u32 = 8;

/// The first graph schema version in which a single-sided member may carry its
/// own [`task`](OneharnessMember::task) and [`dir`](OneharnessMember::dir).
///
/// Both are optional and both default to the graph's own, so a version 1 or 2
/// document keeps parsing and running exactly as before; what the gate buys is
/// that a document *using* one says which schema it was written against.
pub const FIRST_MEMBER_JOB_VERSION: u32 = 3;

/// The first graph schema version in which a two-party member may carry its
/// own [`dir`](OnejudgeMember::dir) and [`schedule`](OnejudgeMember::schedule).
///
/// A gate on the *fields*, the way [`FIRST_MEMBER_JOB_VERSION`] is one for the
/// same two on a single-sided member: both are optional and both default to what
/// a two-party member has always had — the run's directory, and one conversation
/// opened in its wave and never held — so a document that names neither runs
/// exactly as it did under every older schema. What the gate buys is that a
/// document *using* one says which schema it was written against, rather than
/// being handed to a build that would refuse the key outright.
pub const FIRST_TWO_PARTY_JOB_VERSION: u32 = 9;

/// Whether `name` is one a member may have.
///
/// A member's name is a path component in the run's own directory, so this is
/// the shape of one that stays there.
#[must_use]
pub fn is_member_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Whether `label` is one a judge may carry: onejudge's own `[A-Za-z0-9_-]+`,
/// which is also the shape [`is_member_name`] holds a member to, and for the
/// same reason here — a harness judge's label names the file its resolved
/// config is written to in the member's scratch.
#[must_use]
pub fn is_judge_label(label: &str) -> bool {
    is_member_name(label)
}

/// Why `command` could not be spawned as a command judge, when it could not.
///
/// The same two refusals a `pre_turn` view's argv earns, for the same reason:
/// this is an argv a graph supplies and onejudge's command provider hands
/// straight to a process. An empty list names no program and a blank one is a
/// spawn of the current directory on POSIX and of nothing at all on Windows;
/// a NUL cannot cross into a process on either platform, so a word carrying
/// one is refused here rather than becoming a spawn error on every turn.
/// Shared by the graph's validation and by [`crate::invoke`]'s composition so
/// the two cannot drift.
pub(crate) fn command_judge_refusal(command: &[String]) -> Option<String> {
    if command
        .first()
        .is_none_or(|program| program.trim().is_empty())
    {
        return Some(
            "a command judge needs a command to run — its `command` is an argv spawned \
             directly, so the program is its first element"
                .to_string(),
        );
    }
    command
        .iter()
        .find(|word| word.contains('\0'))
        .map(|word| {
            format!("{word:?} carries a NUL, which no argument can — this is an argv handed straight to a process")
        })
}

/// The shape refusals a judge list earns before anything is resolved: no judge
/// at all, a command judge with nothing to run, and a label that could not be a
/// file name. Whether a side's config can be read is decided when the member is
/// built, and whether its `bin` answers is onejudge's probe at plan time —
/// neither is claimed here.
///
/// What a label may be beyond that — unique within the list, once onejudge has
/// defaulted the absent ones — is onejudge's rule, applied by it when the
/// member's plan is built, and not restated here.
fn judge_sides_are_well_formed(
    name: &str,
    judges: &[JudgeSide],
) -> Result<(), crate::error::Error> {
    use crate::error::Error;
    if judges.is_empty() {
        return Err(Error::InvalidConfig(format!(
            "member {name:?}: `judge` names no side — a two-party member needs at least one \
             judge"
        )));
    }
    for (index, judge) in judges.iter().enumerate() {
        let entry = index + 1;
        if let JudgeSide::Command(command) = judge {
            if let Some(why) = command_judge_refusal(&command.command) {
                return Err(Error::InvalidConfig(format!(
                    "member {name:?}: judge entry {entry}: {why}"
                )));
            }
        }
        if let Some(label) = judge.label() {
            if !is_judge_label(label) {
                return Err(Error::InvalidConfig(format!(
                    "member {name:?}: judge entry {entry}: label {label:?} — use letters, digits, \
                     hyphens, and underscores; a label names this judge on every surface and, for \
                     a harness judge, the config file this run writes for it"
                )));
            }
        }
    }
    Ok(())
}

/// Everything about a graph that can be checked without launching it.
///
/// The schema itself is checked by serde — `deny_unknown_fields` is the trust
/// boundary, so a typo fails loudly rather than being dropped. What is left is
/// what serde cannot say: the version this crate reads, and the shapes that are
/// legal YAML but not a runnable graph.
///
/// # Errors
///
/// [`crate::error::Error::InvalidConfig`] naming what is wrong, in the terms the
/// graph's author wrote it.
pub fn validate(graph: &GraphConfig) -> Result<(), crate::error::Error> {
    use crate::error::Error;
    if !(FIRST_SCHEMA_VERSION..=SCHEMA_VERSION).contains(&graph.version) {
        return Err(Error::InvalidConfig(format!(
            "version {} is not a graph schema this build reads; it reads versions \
             {FIRST_SCHEMA_VERSION} through {SCHEMA_VERSION}",
            graph.version
        )));
    }
    if graph.name.trim().is_empty() {
        return Err(Error::InvalidConfig("a graph needs a name".into()));
    }
    if graph.members.is_empty() {
        return Err(Error::InvalidConfig(format!(
            "graph {:?} has no members, so there is nothing for it to run",
            graph.name
        )));
    }
    for (key, value) in &graph.env {
        // An `env:` key is exported to every member process, and the platform —
        // not this crate — decides what a variable may be called. An empty name,
        // or one carrying `=` or a NUL, is not a variable at all: it is refused
        // by the spawn, silently dropped, or splits into something nobody wrote.
        // Refusing here is what makes it `validate`'s answer rather than a
        // confusing failure once every member is already launching.
        if key.is_empty() || key.contains('=') || key.contains('\0') {
            return Err(Error::InvalidConfig(format!(
                "env key {key:?}: an environment variable name cannot be empty or contain '=' or \
                 a NUL, and this one is exported to every member"
            )));
        }
        // A NUL in the *value* is the same class of thing and now matters more:
        // the block is put into this process's own environment before any member
        // starts, and `std::env::set_var` answers a value it cannot represent by
        // panicking. A graph is external input, so that would be a document
        // taking the run down instead of being refused.
        if value.contains('\0') {
            return Err(Error::InvalidConfig(format!(
                "env value for {key:?}: an environment value cannot contain a NUL, and this one \
                 is exported to every member"
            )));
        }
    }
    // A graph's own persona catalog, gated for the reason
    // `FIRST_PERSONA_CATALOG_VERSION` records.
    if let Some(root) = &graph.personas {
        if graph.version < FIRST_PERSONA_CATALOG_VERSION {
            return Err(Error::InvalidConfig(format!(
                "this graph uses `personas`, which requires graph schema version \
                 {FIRST_PERSONA_CATALOG_VERSION}"
            )));
        }
        // Present and empty, refused for the reason a member's empty `dir` is:
        // it names wherever the launching process happened to be, which is a
        // catalog nobody chose rather than the one the author meant.
        if root.as_os_str().is_empty() {
            return Err(Error::InvalidConfig(
                "`personas` names no directory — omit it to resolve a member's `persona` name \
                 against the personas this crate ships"
                    .into(),
            ));
        }
    }
    // A graph's say over its own stream, gated the way `schedule.start_after` is
    // and refused for the same reason: a document declaring an older schema and
    // naming this block would otherwise be given the unfiltered stream it did
    // not ask for, silently publishing everything it meant to hold back.
    if let Some(events) = &graph.events {
        if graph.version < FIRST_EVENT_FILTER_VERSION {
            return Err(Error::InvalidConfig(format!(
                "this graph uses `events`, which requires graph schema version \
                 {FIRST_EVENT_FILTER_VERSION}"
            )));
        }
        if let Some(filter) = &events.filter {
            filter
                .validate()
                .map_err(|why| Error::InvalidConfig(format!("`events.filter`: {why}")))?;
        }
    }
    for (name, member) in &graph.members {
        if graph.version == 1
            && matches!(member, Member::Onejudge(member) if !member.deps.is_empty())
        {
            return Err(Error::InvalidConfig(format!(
                "member {name:?} uses onejudge `deps`, which requires graph schema version 2"
            )));
        }
        // A member's name becomes a *path component* — its scratch directory,
        // and the file `trigger` and `reset-timer` leave for it — so a name
        // carrying a separator or a parent reference would put a member's
        // generated configs, and an operator's signal, outside the run's own
        // directory.
        if !is_member_name(name) {
            return Err(Error::InvalidConfig(format!(
                "member name {name:?}: use letters, digits, hyphens, and underscores — a name \
                 is a directory this run creates and a signal file an operator writes"
            )));
        }
        // Whether a member holds the run open, gated the way `schedule.start_after`
        // is and refused for the same reason: what its absence means is the
        // schema's answer, so a document declaring an older schema and naming it
        // would otherwise be run under the inference it was written against,
        // with the declaration silently dropped.
        if member.declared_background().is_some() && graph.version < FIRST_BACKGROUND_VERSION {
            return Err(Error::InvalidConfig(format!(
                "member {name:?} uses `background`, which requires graph schema version \
                 {FIRST_BACKGROUND_VERSION}"
            )));
        }
        match member {
            Member::Onejudge(member) => {
                if member.mode.trim().is_empty() {
                    return Err(Error::InvalidConfig(format!(
                        "member {name:?}: `mode` is the approval posture every member runs \
                         under, and an empty one names none"
                    )));
                }
                if member.max_turns == Some(0) {
                    return Err(Error::InvalidConfig(format!(
                        "member {name:?}: `max_turns` of 0 lets the member take no turn at all"
                    )));
                }
                judge_sides_are_well_formed(name, &member.judge)?;
                // A conversation's own worktree and pace, gated the way a
                // single-sided member's `dir` is and refused for the same reason:
                // a document declaring an older schema and naming either would
                // otherwise run in the graph's directory, or unpaced, without a
                // word about the field it asked for.
                for (field, given) in [
                    ("dir", member.dir.is_some()),
                    ("schedule", member.schedule.is_some()),
                ] {
                    if given && graph.version < FIRST_TWO_PARTY_JOB_VERSION {
                        return Err(Error::InvalidConfig(format!(
                            "member {name:?} uses onejudge `{field}`, which requires graph \
                             schema version {FIRST_TWO_PARTY_JOB_VERSION}"
                        )));
                    }
                }
                own_dir(name, member.dir.as_deref())?;
                if let Some(schedule) = member.schedule {
                    schedule_spans(name, &schedule, graph.version)?;
                }
            }
            Member::Oneharness(member) => {
                // A member's own job, gated the way `deps` is: a document that
                // declares an older schema and then uses a field that schema
                // never had is refused by the field's name, rather than running
                // under a graph-wide task or directory the author did not mean.
                for (field, given) in [
                    ("task", member.task.is_some()),
                    ("dir", member.dir.is_some()),
                ] {
                    if given && graph.version < FIRST_MEMBER_JOB_VERSION {
                        return Err(Error::InvalidConfig(format!(
                            "member {name:?} uses oneharness `{field}`, which requires graph \
                             schema version {FIRST_MEMBER_JOB_VERSION}"
                        )));
                    }
                }
                // Neither field may be present and empty, and for the same
                // reason: each one *replaces* what the graph supplies, so an
                // empty one is a member asking for nothing rather than for the
                // graph's. An empty `dir` would name wherever the launching
                // process happened to be — `own_dir`, shared with the two-party
                // arm; an empty `task` becomes the value of this member's
                // `--prompt`, which is a harness given no instruction at all.
                // Refusing here is what makes either the author's typo rather
                // than a member run on it.
                own_dir(name, member.dir.as_deref())?;
                if member
                    .task
                    .as_ref()
                    .is_some_and(|task| task.trim().is_empty())
                {
                    return Err(Error::InvalidConfig(format!(
                        "member {name:?}: `task` is the job this member runs, and an empty one \
                         is no job — omit it to run the task the graph was given"
                    )));
                }
                pre_turn(name, member, graph.version)?;
                if let Some(schedule) = member.schedule {
                    schedule_spans(name, &schedule, graph.version)?;
                }
            }
        }
    }
    Ok(())
}

/// A member's own `dir`, present and empty, refused.
///
/// The field *replaces* the run's directory, so an empty one is a member asking
/// for nothing rather than for the run's: unrefused, it would name wherever the
/// launching process happened to be. One check for both kinds, because both
/// resolve the field the same way.
///
/// # Errors
///
/// [`crate::error::Error::InvalidConfig`] naming the member.
fn own_dir(name: &str, dir: Option<&std::path::Path>) -> Result<(), crate::error::Error> {
    if dir.is_some_and(|dir| dir.as_os_str().is_empty()) {
        return Err(crate::error::Error::InvalidConfig(format!(
            "member {name:?}: `dir` names no directory — omit it to work in the graph's own \
             directory"
        )));
    }
    Ok(())
}

/// Everything a [`Schedule`] has to be, whichever kind of member carries it.
///
/// One function for both kinds rather than a copy per arm, because the rules
/// are the schedule's and not the member's: a span is a span whether it paces a
/// single-sided member's firings or a two-party member's turns, and a check
/// that lived in one arm would be the drift `tests/contract.rs` gates the field
/// lists against, one level down.
///
/// # Errors
///
/// [`crate::error::Error::InvalidConfig`] naming the member and the field.
fn schedule_spans(name: &str, schedule: &Schedule, schema: u32) -> Result<(), crate::error::Error> {
    use crate::error::Error;
    if schedule.every == 0 {
        return Err(Error::InvalidConfig(format!(
            "member {name:?}: a schedule of every 0 seconds never stops firing"
        )));
    }
    // Refused rather than ignored under an older schema: what a missing
    // `start_after` means is that schema's answer, so a document declaring
    // version 3 and asking for one would otherwise be given the t=0 it did not
    // ask for.
    if schedule.start_after.is_some() && schema < FIRST_START_AFTER_VERSION {
        return Err(Error::InvalidConfig(format!(
            "member {name:?} uses `schedule.start_after`, which requires graph schema version \
             {FIRST_START_AFTER_VERSION}"
        )));
    }
    // A span nobody could mean is refused as the typo it is, rather than
    // becoming a member that waits out the heat death of the universe while
    // reporting nothing at all.
    for (field, seconds) in [
        ("every", schedule.every),
        ("start_after", schedule.first_turn_after(schema)),
    ] {
        if seconds > MAX_SCHEDULE_SECONDS {
            return Err(Error::InvalidConfig(format!(
                "member {name:?}: `{field}` of {seconds} seconds is longer than any run this \
                 will ever pace — the ceiling is {MAX_SCHEDULE_SECONDS}"
            )));
        }
    }
    Ok(())
}

/// Everything a member's [`pre_turn`](OneharnessMember::pre_turn) list has to be
/// before any of it reaches a process.
///
/// This is the trust boundary for a **declared command**, and it is the same one
/// a graph's `env:` block and a command judge already sit behind: a value that
/// arrives from a YAML document and ends up at `Command::spawn` is checked where
/// the document is read, so a typo is `validate`'s one-sentence answer rather
/// than a spawn refusal a member dies on, once per turn, forever.
///
/// # Errors
///
/// [`crate::error::Error::InvalidConfig`] naming the member, which view, and
/// what is wrong with it.
fn pre_turn(name: &str, member: &OneharnessMember, schema: u32) -> Result<(), crate::error::Error> {
    use crate::error::Error;
    if member.pre_turn.is_empty() {
        return Ok(());
    }
    if schema < FIRST_PRE_TURN_VERSION {
        return Err(Error::InvalidConfig(format!(
            "member {name:?} uses oneharness `pre_turn`, which requires graph schema version \
             {FIRST_PRE_TURN_VERSION}"
        )));
    }
    if member.pre_turn.len() > MAX_PRE_TURN_COMMANDS {
        return Err(Error::InvalidConfig(format!(
            "member {name:?}: `pre_turn` declares {} commands, and every turn waits on all of \
             them and carries all of their output — the ceiling is {MAX_PRE_TURN_COMMANDS}",
            member.pre_turn.len()
        )));
    }
    for (at, view) in member.pre_turn.iter().enumerate() {
        let at = format!("member {name:?}: `pre_turn[{at}]`");
        // A program, and a program that is a name rather than whitespace. An
        // empty list is a view that runs nothing; a blank program is a spawn of
        // the current directory on POSIX and of nothing at all on Windows.
        let Some(program) = view
            .command
            .first()
            .filter(|program| !program.trim().is_empty())
        else {
            return Err(Error::InvalidConfig(format!(
                "{at}: `command` is the program this view runs, and an empty one names none — it \
                 is an argv this run spawns directly, so the program is its first element"
            )));
        };
        // A NUL cannot cross into a process on either platform: POSIX arguments
        // are NUL-terminated and Windows refuses one outright, so a word carrying
        // one is refused here rather than becoming a spawn error every turn.
        if let Some(word) = view.command.iter().find(|word| word.contains('\0')) {
            return Err(Error::InvalidConfig(format!(
                "{at}: {word:?} carries a NUL, which no argument can — this is an argv handed \
                 straight to a process"
            )));
        }
        if let Some(label) = &view.label {
            // The label names the view inside the turn's own context, so a blank
            // one names nothing and a control character forges the framing around
            // it — see `crate::preturn`, which is what renders it.
            if label.trim().is_empty() || label.chars().any(char::is_control) {
                return Err(Error::InvalidConfig(format!(
                    "{at}: `label` {label:?} is what this view is called in the turn's context, \
                     so it cannot be blank or carry a control character — omit it to be called \
                     {program:?}"
                )));
            }
        }
        if let Some(seconds) = view.timeout {
            if seconds == 0 || seconds > MAX_PRE_TURN_SECONDS {
                return Err(Error::InvalidConfig(format!(
                    "{at}: `timeout` of {seconds} seconds is not a bound this view can run under \
                     — it is between 1 and {MAX_PRE_TURN_SECONDS}, and omitting it is \
                     {DEFAULT_PRE_TURN_SECONDS}"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(document: &str) -> GraphConfig {
        serde_norway::from_str(document).expect("a graph")
    }

    const ONE_MEMBER: &str = concat!(
        "version: 1\nname: g\nmembers:\n",
        "  build:\n    kind: oneharness\n    oneharness_config: ./a.toml\n",
    );

    /// A graph this build can read passes; one from another schema version says
    /// which version it is, rather than failing on whichever field moved.
    #[test]
    fn a_graph_of_another_version_is_refused_by_version() {
        assert!(validate(&parse(ONE_MEMBER)).is_ok());
        for readable in FIRST_SCHEMA_VERSION..=SCHEMA_VERSION {
            let document = ONE_MEMBER.replace("version: 1", &format!("version: {readable}"));
            assert!(validate(&parse(&document)).is_ok(), "{document}");
        }
        let ahead = format!("version: {}", SCHEMA_VERSION + 1);
        let err = validate(&parse(&ONE_MEMBER.replace("version: 1", &ahead))).unwrap_err();
        assert!(
            err.to_string().contains(&format!(
                "versions {FIRST_SCHEMA_VERSION} through {SCHEMA_VERSION}"
            )),
            "{err}"
        );
    }

    #[test]
    fn onejudge_dependencies_require_version_two_without_breaking_version_one_graphs() {
        let version_one = concat!(
            "version: 1\nname: g\nmembers:\n",
            "  build:\n    kind: oneharness\n    oneharness_config: ./a.toml\n",
            "  w:\n    kind: onejudge\n",
            "    base_config: ./b.yaml\n    mode: bypass\n",
            "    agent: {oneharness_config: ./a.toml}\n",
            "    judge: {oneharness_config: ./j.toml}\n",
        );
        assert!(validate(&parse(version_one)).is_ok());

        let with_deps = format!("{version_one}    deps: [build]\n");
        let error = validate(&parse(&with_deps)).expect_err("version 1 predates onejudge deps");
        assert!(error
            .to_string()
            .contains("requires graph schema version 2"));
        assert!(validate(&parse(&with_deps.replace("version: 1", "version: 2"))).is_ok());
    }

    /// A single-sided member may carry its own job, and a document that declares
    /// an older schema is refused by the field's name rather than running under
    /// the graph's task and directory instead.
    #[test]
    fn a_single_sided_members_own_job_requires_the_schema_that_has_it() {
        let base = concat!(
            "version: 3\nname: g\nmembers:\n",
            "  check_in:\n    kind: oneharness\n    oneharness_config: ./a.toml\n",
        );
        for own in ["    task: send one update\n", "    dir: ./api\n"] {
            let document = format!("{base}{own}");
            assert!(validate(&parse(&document)).is_ok(), "{document}");
            for older in ["version: 1", "version: 2"] {
                let older = document.replace("version: 3", older);
                let err = validate(&parse(&older)).expect_err("the field postdates this schema");
                assert!(
                    err.to_string().contains("requires graph schema version 3"),
                    "{older}: {err}"
                );
            }
        }
        // And a member with neither keeps parsing and validating under every
        // schema this build reads, which is what "an existing graph document is
        // unaffected" means.
        for version in FIRST_SCHEMA_VERSION..=SCHEMA_VERSION {
            let unchanged = base.replace("version: 3", &format!("version: {version}"));
            assert!(validate(&parse(&unchanged)).is_ok(), "{unchanged}");
        }

        // An empty field of either kind is a typo, not a request for the
        // graph's: unrefused, one names wherever the launching process happened
        // to be and the other becomes a harness given no instruction at all.
        for (given, expected) in [
            ("    dir: ''\n", "names no directory"),
            ("    task: ''\n", "an empty one is no job"),
            ("    task: '   '\n", "an empty one is no job"),
        ] {
            let err = validate(&parse(&format!("{base}{given}"))).unwrap_err();
            assert!(err.to_string().contains(expected), "{given}: {err}");
        }
    }

    /// From the schema that has `start_after`, a schedule naming none waits one
    /// whole interval; under an older one it waits none, exactly as it always did.
    ///
    /// The default is what a schedule *already written* means, so both halves are
    /// asserted through parsed documents rather than struct literals: the field is
    /// absent from every graph in existence, and what its absence means under each
    /// schema is the whole of this change.
    #[test]
    fn a_schedules_first_turn_waits_an_interval_only_from_the_schema_that_has_it() {
        let scheduled = |version: u32, schedule: &str| -> Schedule {
            let document = format!(
                concat!(
                    "version: {}\nname: g\nmembers:\n  ticker:\n    kind: oneharness\n",
                    "    oneharness_config: ./a.toml\n    schedule: {}\n",
                ),
                version, schedule
            );
            let graph = parse(&document);
            validate(&graph).unwrap_or_else(|err| panic!("{document}: {err}"));
            let Member::Oneharness(member) = &graph.members["ticker"] else {
                panic!("a scheduled member is single-sided")
            };
            member.schedule.expect("the member is scheduled")
        };

        // Every schema that predates the field: a schedule naming none takes its
        // first turn at t=0, which is what those documents have always done.
        for older in FIRST_SCHEMA_VERSION..FIRST_START_AFTER_VERSION {
            let inherited = scheduled(older, "{every: 1800}");
            assert_eq!(inherited.start_after, None);
            assert_eq!(
                inherited.first_turn_after(older),
                0,
                "version {older} moved under a document that never asked it to"
            );
        }

        let current = SCHEMA_VERSION;
        let inherited = scheduled(current, "{every: 1800}");
        assert_eq!(inherited.start_after, None);
        assert_eq!(inherited.first_turn_after(current), 1800);
        assert_eq!(
            scheduled(current, "{every: 1800, start_after: 0}").first_turn_after(current),
            0
        );
        assert_eq!(
            scheduled(current, "{every: 1800, start_after: 5}").first_turn_after(current),
            5
        );
        // Longer than the cadence is a legal thing to ask for — "settle in, then
        // report often" — so it is carried rather than clamped.
        assert_eq!(
            scheduled(current, "{every: 60, start_after: 600}").first_turn_after(current),
            600
        );
        // And a schedule that named none serializes without one, so a document
        // written before the field existed round-trips unchanged.
        let rendered = serde_norway::to_string(&inherited).expect("a schedule serializes");
        assert!(!rendered.contains("start_after"), "{rendered}");
    }

    /// A document declaring a schema that predates `start_after` is refused by the
    /// field's name rather than run with the delay it did not ask for.
    #[test]
    fn start_after_requires_the_schema_that_has_it() {
        let document = |version: u32| {
            format!(
                concat!(
                    "version: {}\nname: g\nmembers:\n  ticker:\n    kind: oneharness\n",
                    "    oneharness_config: ./a.toml\n",
                    "    schedule: {{every: 1800, start_after: 30}}\n",
                ),
                version
            )
        };
        assert!(validate(&parse(&document(SCHEMA_VERSION))).is_ok());
        for older in FIRST_SCHEMA_VERSION..FIRST_START_AFTER_VERSION {
            let err =
                validate(&parse(&document(older))).expect_err("the field postdates this schema");
            assert!(
                err.to_string().contains(&format!(
                    "requires graph schema version {FIRST_START_AFTER_VERSION}"
                )),
                "version {older}: {err}"
            );
            assert!(err.to_string().contains("start_after"), "{err}");
        }
    }

    /// A span longer than any run is refused by name, on both of a schedule's
    /// fields and however it was arrived at.
    ///
    /// Not a policy about cadence: a `u64` of seconds is four hundred billion
    /// years, and a member whose clock names that never fires and never says why
    /// — which from outside is indistinguishable from the member being broken.
    #[test]
    fn a_schedule_longer_than_any_run_is_refused() {
        let document = |schedule: String| {
            format!(
                concat!(
                    "version: {}\nname: g\nmembers:\n  ticker:\n    kind: oneharness\n",
                    "    oneharness_config: ./a.toml\n    schedule: {}\n",
                ),
                SCHEMA_VERSION, schedule
            )
        };
        let ceiling = MAX_SCHEDULE_SECONDS;
        assert!(validate(&parse(&document(format!("{{every: {ceiling}}}")))).is_ok());
        for (schedule, field) in [
            (format!("{{every: {}}}", ceiling + 1), "every"),
            // Named directly...
            (
                format!("{{every: 60, start_after: {}}}", u64::MAX),
                "start_after",
            ),
            // ...and inherited from `every`, which is the same span by another
            // route and must be refused by the field the author would look at.
            (format!("{{every: {}}}", u64::MAX), "every"),
        ] {
            let err = validate(&parse(&document(schedule.clone()))).unwrap_err();
            assert!(err.to_string().contains(field), "{schedule}: {err}");
            assert!(
                err.to_string().contains("longer than any run"),
                "{schedule}: {err}"
            );
        }
    }

    /// A member's name is a directory this run creates and a signal file an
    /// operator writes, so a name that would leave the run's own directory is
    /// refused before either exists.
    #[test]
    fn a_member_name_that_would_leave_the_run_directory_is_refused() {
        for escape in ["../elsewhere", "a/b", "a\\b", "a b"] {
            let document = format!(
                concat!(
                    "version: 1\nname: g\nmembers:\n",
                    "  \"{escape}\":\n",
                    "    kind: oneharness\n",
                    "    oneharness_config: ./a.toml\n",
                ),
                escape = escape,
            );
            let err = validate(&parse(&document)).unwrap_err();
            assert!(err.to_string().contains("member name"), "{escape:?}: {err}");
        }
        assert!(is_member_name("worker_2-a"));
        assert!(!is_member_name(""));
    }

    /// The shapes that are legal YAML but not a runnable graph.
    #[test]
    fn a_graph_that_could_never_run_is_refused_with_the_reason() {
        for (document, expected) in [
            (
                "version: 1\nname: ' '\nmembers: {}\n",
                "a graph needs a name",
            ),
            ("version: 1\nname: g\nmembers: {}\n", "has no members"),
            (
                concat!(
                    "version: 1\nname: g\nmembers:\n  a:\n    kind: oneharness\n",
                    "    oneharness_config: ./a.toml\n    schedule: {every: 0}\n",
                ),
                "never stops firing",
            ),
        ] {
            let err = validate(&parse(document)).unwrap_err();
            assert!(err.to_string().contains(expected), "{document}: {err}");
        }
    }

    /// A two-party member's own refusals: no posture, no turn, no judge command.
    #[test]
    fn a_two_party_member_that_could_never_run_is_refused() {
        let base = concat!(
            "version: 1\nname: g\nmembers:\n  w:\n    kind: onejudge\n",
            "    base_config: ./b.yaml\n    mode: bypass\n",
            "    agent: {oneharness_config: ./a.toml}\n",
            "    judge: {oneharness_config: ./j.toml}\n",
        );
        assert!(validate(&parse(base)).is_ok());
        for (document, expected) in [
            (base.replace("mode: bypass", "mode: ' '"), "names none"),
            (format!("{base}    max_turns: 0\n"), "no turn at all"),
            (
                base.replace(
                    "judge: {oneharness_config: ./j.toml}",
                    "judge: {command: []}",
                ),
                "needs a command to run",
            ),
        ] {
            let err = validate(&parse(&document)).unwrap_err();
            assert!(err.to_string().contains(expected), "{document}: {err}");
        }
    }

    /// A two-party member's `judge:` reads as one list whether it was written
    /// as one side or as several, each entry told apart by the one field its
    /// shape requires; a one-element list writes back as the mapping it came
    /// from and a longer one as the list, so both spellings round-trip.
    #[test]
    fn a_judge_list_reads_into_one_representation_and_writes_back_as_it_came() {
        let single = parse(concat!(
            "version: 1\nname: g\nmembers:\n  w:\n    kind: onejudge\n",
            "    base_config: ./b.yaml\n    mode: bypass\n",
            "    agent: {oneharness_config: ./a.toml}\n",
            "    judge: {oneharness_config: ./j.toml}\n",
        ));
        let Member::Onejudge(member) = &single.members["w"] else {
            panic!("w is onejudge")
        };
        assert_eq!(
            member.judge,
            vec![JudgeSide::Harness(JudgeHarness {
                oneharness_config: ConfigRef("./j.toml".into()),
                model: None,
                label: None,
            })]
        );
        let written = serde_norway::to_string(&single).expect("serializes");
        assert!(
            written.contains("    judge:\n      oneharness_config: ./j.toml\n"),
            "{written}"
        );

        let stacked = parse(concat!(
            "version: 1\nname: g\nmembers:\n  w:\n    kind: onejudge\n",
            "    base_config: ./b.yaml\n    mode: bypass\n",
            "    agent: {oneharness_config: ./a.toml}\n",
            "    judge:\n",
            "      - oneharness_config: ./j.toml\n        model: opus\n        label: reviewer\n",
            "      - kind: llmlint\n        config: ./llmlint.yml\n        bin: /opt/llmlint\n",
            "        diff_base: origin/main\n        args: [--rule, no_todo]\n        label: lint\n",
            "      - command: [my-judge, --flag]\n",
        ));
        validate(&stacked).expect("a stacked panel validates");
        let Member::Onejudge(member) = &stacked.members["w"] else {
            panic!("w is onejudge")
        };
        assert_eq!(
            member.judge,
            vec![
                JudgeSide::Harness(JudgeHarness {
                    oneharness_config: ConfigRef("./j.toml".into()),
                    model: Some("opus".into()),
                    label: Some("reviewer".into()),
                }),
                JudgeSide::Llmlint(JudgeLlmlint {
                    kind: LlmlintKind::Llmlint,
                    config: Some(PathBuf::from("./llmlint.yml")),
                    bin: Some("/opt/llmlint".into()),
                    diff_base: Some("origin/main".into()),
                    args: vec!["--rule".into(), "no_todo".into()],
                    label: Some("lint".into()),
                }),
                JudgeSide::Command(JudgeCommand {
                    command: vec!["my-judge".into(), "--flag".into()],
                    label: None,
                }),
            ]
        );
        assert_eq!(
            member.judge.iter().map(JudgeSide::kind).collect::<Vec<_>>(),
            vec!["oneharness", "llmlint", "command"]
        );
        let written = serde_norway::to_string(&stacked).expect("serializes");
        assert!(
            written.contains("    judge:\n    - oneharness_config: ./j.toml\n"),
            "{written}"
        );
        assert!(written.contains("kind: llmlint\n"), "{written}");
        let reread = parse(&written);
        assert_eq!(reread, stacked, "a stacked panel must round-trip");
    }

    /// A judge entry that is none of the three shapes is refused naming the
    /// entry — by its position in a list, and by what it carried instead —
    /// and a shape's own unknown field is refused naming that shape.
    #[test]
    fn a_judge_entry_that_is_none_of_the_three_shapes_is_refused_naming_it() {
        let document = |judge: &str| {
            format!(
                concat!(
                    "version: 1\nname: g\nmembers:\n  w:\n    kind: onejudge\n",
                    "    base_config: ./b.yaml\n    mode: bypass\n",
                    "    agent: {{oneharness_config: ./a.toml}}\n",
                    "    judge: {}\n",
                ),
                judge
            )
        };
        for (judge, expected) in [
            (
                "[{oneharness_config: ./j.toml}, {foo: 1, bar: 2}]",
                "judge entry 2: {foo, bar} is none of the three judge shapes",
            ),
            (
                "[{kind: oneharness, judge_config: ./j.toml}]",
                "judge entry 1: `kind: oneharness` names no judge shape",
            ),
            (
                "[{kind: llmlint, command: [x]}]",
                "judge entry 1: an llmlint side: unknown field `command`",
            ),
            (
                "{oneharness_config: ./j.toml, command: [x]}",
                "judge: a harness side: unknown field `command`",
            ),
            (
                "{label: reviewer}",
                "judge: {label} is none of the three judge shapes",
            ),
            ("3", "judge: 3 is neither one side nor a list of sides"),
            ("[1]", "judge entry 1: 1 is not a mapping"),
        ] {
            let err = serde_norway::from_str::<GraphConfig>(&document(judge))
                .expect_err(&format!("{judge} must be refused"));
            assert!(err.to_string().contains(expected), "{judge}: {err}");
        }
    }

    /// A panel's own pre-launch refusals: no judge at all, a command judge with
    /// nothing to run — no program, a blank one, or a word no process can be
    /// handed — and a label that could not be a file name — each naming the
    /// member and, in a list, the entry.
    #[test]
    fn a_panel_that_could_never_run_is_refused_naming_the_entry() {
        let document = |judge: &str| {
            format!(
                concat!(
                    "version: 1\nname: g\nmembers:\n  w:\n    kind: onejudge\n",
                    "    base_config: ./b.yaml\n    mode: bypass\n",
                    "    agent: {{oneharness_config: ./a.toml}}\n",
                    "    judge: {}\n",
                ),
                judge
            )
        };
        validate(&parse(&document(
            "[{oneharness_config: ./j.toml, label: a-B_1}, {kind: llmlint}, {command: [x]}]",
        )))
        .expect("a labelled stack validates");
        for (judge, expected) in [
            ("[]", "member \"w\": `judge` names no side"),
            (
                "[{command: [x]}, {command: []}]",
                "member \"w\": judge entry 2: a command judge needs a command to run",
            ),
            (
                "[{command: [x]}, {command: ['  ']}]",
                "member \"w\": judge entry 2: a command judge needs a command to run",
            ),
            (
                "[{command: [x, \"a\\0b\"]}]",
                "member \"w\": judge entry 1: \"a\\0b\" carries a NUL",
            ),
            (
                "[{command: [x]}, {kind: llmlint, label: 'lint run'}]",
                "member \"w\": judge entry 2: label \"lint run\"",
            ),
            (
                "{oneharness_config: ./j.toml, label: ''}",
                "member \"w\": judge entry 1: label \"\"",
            ),
        ] {
            let err = validate(&parse(&document(judge))).unwrap_err();
            assert!(err.to_string().contains(expected), "{judge}: {err}");
        }
    }

    /// A graph may name a filter over its own stream from the schema that has
    /// one, and a document declaring an older schema is refused by the block's
    /// name rather than run with the unfiltered stream it did not ask for.
    #[test]
    fn an_events_block_requires_the_schema_that_has_it() {
        let document = |version: u32| {
            format!(
                concat!(
                    "version: {}\nname: g\n",
                    "events:\n  filter:\n    exclude: [{{kind: turn-activity}}]\n",
                    "members:\n  build:\n    kind: oneharness\n",
                    "    oneharness_config: ./a.toml\n",
                ),
                version
            )
        };
        let graph = parse(&document(FIRST_EVENT_FILTER_VERSION));
        validate(&graph).expect("the schema that has the block accepts it");
        assert_eq!(
            graph
                .events
                .and_then(|events| events.filter)
                .expect("the block carries a filter")
                .exclude
                .len(),
            1
        );
        for older in FIRST_SCHEMA_VERSION..FIRST_EVENT_FILTER_VERSION {
            let err = validate(&parse(&document(older))).expect_err("the block postdates it");
            assert!(
                err.to_string().contains("`events`"),
                "version {older}: {err}"
            );
            assert!(
                err.to_string().contains(&format!(
                    "requires graph schema version {FIRST_EVENT_FILTER_VERSION}"
                )),
                "version {older}: {err}"
            );
        }
        // And a graph naming no block validates under every schema this build
        // reads, and serializes without one — an existing document is untouched.
        for version in FIRST_SCHEMA_VERSION..=SCHEMA_VERSION {
            let unchanged = ONE_MEMBER.replace("version: 1", &format!("version: {version}"));
            let graph = parse(&unchanged);
            assert_eq!(graph.events, None);
            assert!(validate(&graph).is_ok(), "{unchanged}");
            let rendered = serde_norway::to_string(&graph).expect("a graph serializes");
            assert!(!rendered.contains("events"), "{rendered}");
        }
    }

    /// A graph may name its own persona catalog from the schema that has one,
    /// and a document declaring an older schema is refused by the key's name
    /// rather than run under a name-resolution rule that schema never had.
    #[test]
    fn a_persona_catalog_requires_the_schema_that_has_it() {
        let document = |version: u32, root: &str| {
            format!(
                concat!(
                    "version: {}\nname: g\npersonas: {}\nmembers:\n",
                    "  build:\n    kind: oneharness\n    oneharness_config: ./a.toml\n",
                    "    persona: crozier/crozier-corpus\n",
                ),
                version, root
            )
        };
        let graph = parse(&document(FIRST_PERSONA_CATALOG_VERSION, "./personas"));
        validate(&graph).expect("the schema that has the key accepts it");
        assert_eq!(
            graph.personas.as_deref(),
            Some(std::path::Path::new("./personas"))
        );

        for older in FIRST_SCHEMA_VERSION..FIRST_PERSONA_CATALOG_VERSION {
            let err = validate(&parse(&document(older, "./personas")))
                .expect_err("the key postdates this schema");
            assert!(err.to_string().contains("`personas`"), "{older}: {err}");
            assert!(
                err.to_string().contains(&format!(
                    "requires graph schema version {FIRST_PERSONA_CATALOG_VERSION}"
                )),
                "{older}: {err}"
            );
        }

        // Present and empty is a typo, not a request for the shipped set: it
        // names wherever the launching process happened to be.
        let err = validate(&parse(&document(SCHEMA_VERSION, "''"))).unwrap_err();
        assert!(err.to_string().contains("names no directory"), "{err}");

        // And a graph naming no catalog validates under every schema this build
        // reads, and serializes without the key — an existing document is
        // untouched, and stays readable by a consumer that predates this.
        for version in FIRST_SCHEMA_VERSION..=SCHEMA_VERSION {
            let unchanged = ONE_MEMBER.replace("version: 1", &format!("version: {version}"));
            let graph = parse(&unchanged);
            assert_eq!(graph.personas, None);
            assert!(validate(&graph).is_ok(), "{unchanged}");
            let rendered = serde_norway::to_string(&graph).expect("a graph serializes");
            assert!(!rendered.contains("personas"), "{rendered}");
        }
    }

    /// A filter a run could not honour is refused with the offending matcher
    /// named, before anything is launched.
    #[test]
    fn a_filter_that_could_match_nothing_is_refused_with_the_matcher_named() {
        let document = |filter: &str| {
            format!(
                concat!(
                    "version: {}\nname: g\nevents:\n  filter:\n{}",
                    "members:\n  build:\n    kind: oneharness\n",
                    "    oneharness_config: ./a.toml\n",
                ),
                SCHEMA_VERSION, filter
            )
        };
        for (filter, expected) in [
            ("    exclude: [{}]\n", "exclude[0] {}"),
            ("    include: [{kind: ''}]\n", r#"include[0] {"kind":""}"#),
            (
                "    include: [{kind: 'turn-*'}, {member: ' '}]\n",
                r#"include[1] {"member":" "}"#,
            ),
        ] {
            let err = validate(&parse(&document(filter))).expect_err("the matcher is unusable");
            assert!(err.to_string().contains("`events.filter`"), "{err}");
            assert!(err.to_string().contains(expected), "{filter}: {err}");
        }
        // An `events` block naming no filter is the stream every graph already
        // has, and is not a refusal.
        assert!(validate(&parse(&document("    include: [{kind: '*'}]\n"))).is_ok());
    }

    /// A member's pre-turn views are a schema-gated field, and a member that
    /// declares none serializes without the key — so a document written before
    /// this existed round-trips unchanged and keeps running exactly as it did.
    #[test]
    fn pre_turn_views_require_the_schema_that_has_them() {
        let document = |version: u32| {
            format!(
                concat!(
                    "version: {}\nname: g\nmembers:\n  watcher:\n    kind: oneharness\n",
                    "    oneharness_config: ./a.toml\n",
                    "    pre_turn:\n      - {{command: [queue-depth, --json], label: queue}}\n",
                ),
                version
            )
        };
        let graph = parse(&document(FIRST_PRE_TURN_VERSION));
        validate(&graph).expect("the schema that has the field accepts it");
        let Member::Oneharness(watcher) = &graph.members["watcher"] else {
            panic!("a single-sided member declares the views")
        };
        assert_eq!(
            watcher.pre_turn,
            vec![PreTurn {
                command: vec!["queue-depth".into(), "--json".into()],
                label: Some("queue".into()),
                timeout: None,
            }]
        );
        // What an omitted `timeout` and an omitted `label` mean, read through the
        // accessors every caller uses rather than through the fields.
        assert_eq!(watcher.pre_turn[0].seconds(), DEFAULT_PRE_TURN_SECONDS);
        assert_eq!(watcher.pre_turn[0].view(), "queue");
        assert_eq!(
            PreTurn {
                command: vec!["queue-depth".into()],
                label: None,
                timeout: Some(5),
            }
            .view(),
            "queue-depth"
        );

        for older in FIRST_SCHEMA_VERSION..FIRST_PRE_TURN_VERSION {
            let err =
                validate(&parse(&document(older))).expect_err("the field postdates this schema");
            assert!(err.to_string().contains("`pre_turn`"), "{older}: {err}");
            assert!(
                err.to_string().contains(&format!(
                    "requires graph schema version {FIRST_PRE_TURN_VERSION}"
                )),
                "{older}: {err}"
            );
        }

        // And a member declaring none validates under every schema this build
        // reads, and serializes without the key.
        for version in FIRST_SCHEMA_VERSION..=SCHEMA_VERSION {
            let unchanged = ONE_MEMBER.replace("version: 1", &format!("version: {version}"));
            let graph = parse(&unchanged);
            let Member::Oneharness(build) = &graph.members["build"] else {
                panic!("the member is single-sided")
            };
            assert!(build.pre_turn.is_empty());
            assert!(validate(&graph).is_ok(), "{unchanged}");
            let rendered = serde_norway::to_string(&graph).expect("a graph serializes");
            assert!(!rendered.contains("pre_turn"), "{rendered}");
        }
    }

    /// A view that could never run, or could never be waited on, is refused with
    /// the reason — before any of it reaches a process.
    #[test]
    fn a_pre_turn_view_a_run_could_not_honour_is_refused() {
        let document = |views: &str| {
            format!(
                concat!(
                    "version: {}\nname: g\nmembers:\n  watcher:\n    kind: oneharness\n",
                    "    oneharness_config: ./a.toml\n    pre_turn:\n{}",
                ),
                SCHEMA_VERSION, views
            )
        };
        for (views, expected) in [
            ("      - {command: []}\n", "an empty one names none"),
            ("      - {command: ['  ']}\n", "an empty one names none"),
            ("      - {command: [\"q\\0z\"]}\n", "carries a NUL"),
            (
                "      - {command: [q], label: ' '}\n",
                "cannot be blank or carry a control character",
            ),
            (
                "      - {command: [q], label: \"a\\nb\"}\n",
                "cannot be blank or carry a control character",
            ),
            (
                "      - {command: [q], timeout: 0}\n",
                "is not a bound this view can run under",
            ),
            (
                &format!(
                    "      - {{command: [q], timeout: {}}}\n",
                    MAX_PRE_TURN_SECONDS + 1
                ),
                "is not a bound this view can run under",
            ),
            (
                &"      - {command: [q]}\n".repeat(MAX_PRE_TURN_COMMANDS + 1),
                "the ceiling is",
            ),
        ] {
            let err = validate(&parse(&document(views))).unwrap_err();
            assert!(err.to_string().contains(expected), "{views}: {err}");
            assert!(err.to_string().contains("watcher"), "{views}: {err}");
        }
        // The ceiling itself is a list a member may have, and a timeout at the
        // ceiling is one it may name.
        assert!(validate(&parse(&document(
            &"      - {command: [q]}\n".repeat(MAX_PRE_TURN_COMMANDS)
        )))
        .is_ok());
        assert!(validate(&parse(&document(&format!(
            "      - {{command: [q], timeout: {MAX_PRE_TURN_SECONDS}}}\n"
        ))))
        .is_ok());
    }

    /// The views belong to a single-sided member, and a two-party one naming them
    /// is refused by the field's name — its turns are onejudge's to open, so a
    /// key accepted there would be one nothing ever ran.
    #[test]
    fn a_two_party_member_cannot_declare_pre_turn_views() {
        let document = concat!(
            "version: 7\nname: g\nmembers:\n  w:\n    kind: onejudge\n",
            "    base_config: ./b.yaml\n    mode: bypass\n",
            "    agent: {oneharness_config: ./a.toml}\n",
            "    judge: {oneharness_config: ./j.toml}\n",
            "    pre_turn:\n      - {command: [queue-depth]}\n",
        );
        let err = serde_norway::from_str::<GraphConfig>(document)
            .expect_err("a two-party member has no pre_turn");
        assert!(err.to_string().contains("pre_turn"), "{err}");
    }

    /// A two-party member may carry its own `dir` and `schedule` from the
    /// schema that has them, both omitted when unset, and a document declaring
    /// an older schema is refused by the field's name rather than run in the
    /// graph's directory or unpaced.
    #[test]
    fn a_two_party_members_own_job_requires_the_schema_that_has_it() {
        let document = |version: u32, own: &str| {
            format!(
                concat!(
                    "version: {}\nname: g\nmembers:\n  monitor:\n    kind: onejudge\n",
                    "    base_config: ./b.yaml\n    mode: bypass\n",
                    "    agent: {{oneharness_config: ./a.toml}}\n",
                    "    judge: {{oneharness_config: ./j.toml}}\n{}",
                ),
                version, own
            )
        };
        let graph = parse(&document(
            FIRST_TWO_PARTY_JOB_VERSION,
            "    dir: ./api\n    schedule: {every: 300, start_after: 0, resettable: true}\n",
        ));
        validate(&graph).expect("the schema that has the fields accepts them");
        let Member::Onejudge(monitor) = &graph.members["monitor"] else {
            panic!("the member is two-party")
        };
        assert_eq!(monitor.dir.as_deref(), Some(std::path::Path::new("./api")));
        assert_eq!(
            monitor.schedule,
            Some(Schedule {
                every: 300,
                start_after: Some(0),
                resettable: true,
            })
        );
        // Through the accessors every caller reads them by, so a two-party
        // member's answer is the same one a single-sided member gives.
        assert_eq!(graph.members["monitor"].schedule(), monitor.schedule);
        assert_eq!(graph.members["monitor"].dir(), monitor.dir.as_deref());
        let reparsed: GraphConfig =
            serde_norway::from_str(&serde_norway::to_string(&graph).expect("serializes"))
                .expect("reparses");
        assert_eq!(reparsed, graph);

        for (field, own) in [
            ("dir", "    dir: ./api\n"),
            ("schedule", "    schedule: {every: 300}\n"),
        ] {
            for older in FIRST_SCHEMA_VERSION..FIRST_TWO_PARTY_JOB_VERSION {
                let err = validate(&parse(&document(older, own)))
                    .expect_err("the field postdates this schema");
                assert!(
                    err.to_string().contains(&format!("onejudge `{field}`")),
                    "version {older}: {err}"
                );
                assert!(
                    err.to_string().contains(&format!(
                        "requires graph schema version {FIRST_TWO_PARTY_JOB_VERSION}"
                    )),
                    "version {older}: {err}"
                );
            }
        }

        // Present and empty is a typo, not a request for the run's directory.
        let err = validate(&parse(&document(SCHEMA_VERSION, "    dir: ''\n"))).unwrap_err();
        assert!(err.to_string().contains("names no directory"), "{err}");
        assert!(err.to_string().contains("monitor"), "{err}");

        // And a member naming neither validates under every schema this build
        // reads, and serializes without either key.
        for version in FIRST_SCHEMA_VERSION..=SCHEMA_VERSION {
            let unchanged = parse(&document(version, ""));
            assert!(validate(&unchanged).is_ok(), "version {version}");
            assert_eq!(unchanged.members["monitor"].schedule(), None);
            assert_eq!(unchanged.members["monitor"].dir(), None);
            let rendered = serde_norway::to_string(&unchanged).expect("a graph serializes");
            assert!(!rendered.contains("dir"), "{rendered}");
            assert!(!rendered.contains("schedule"), "{rendered}");
        }
    }

    /// A two-party member's schedule is held to exactly the rules a single-sided
    /// one is — each proven on a two-party member rather than inferred from the
    /// single-sided tests above.
    #[test]
    fn a_two_party_schedule_is_held_to_the_rules_a_single_sided_one_is() {
        let document = |schedule: &str| {
            format!(
                concat!(
                    "version: {}\nname: g\nmembers:\n  monitor:\n    kind: onejudge\n",
                    "    base_config: ./b.yaml\n    mode: bypass\n",
                    "    agent: {{oneharness_config: ./a.toml}}\n",
                    "    judge: {{oneharness_config: ./j.toml}}\n",
                    "    schedule: {}\n",
                ),
                SCHEMA_VERSION, schedule
            )
        };
        let scheduled = |schedule: &str| -> Schedule {
            let graph = parse(&document(schedule));
            validate(&graph).unwrap_or_else(|err| panic!("{schedule}: {err}"));
            graph.members["monitor"]
                .schedule()
                .expect("the member is scheduled")
        };
        // `every: 0` never stops firing.
        let err = validate(&parse(&document("{every: 0}"))).unwrap_err();
        assert!(err.to_string().contains("never stops firing"), "{err}");
        assert!(err.to_string().contains("monitor"), "{err}");
        // A span longer than any run is refused naming the field, on both.
        for (schedule, field) in [
            (format!("{{every: {}}}", MAX_SCHEDULE_SECONDS + 1), "every"),
            (
                format!("{{every: 60, start_after: {}}}", MAX_SCHEDULE_SECONDS + 1),
                "start_after",
            ),
        ] {
            let err = validate(&parse(&document(&schedule))).unwrap_err();
            assert!(err.to_string().contains(field), "{schedule}: {err}");
            assert!(
                err.to_string().contains("longer than any run"),
                "{schedule}: {err}"
            );
        }
        // A schedule naming no `start_after` waits one `every` before its first
        // turn under the current schema, and `resettable` defaults off.
        let inherited = scheduled("{every: 300}");
        assert_eq!(inherited.start_after, None);
        assert_eq!(inherited.first_turn_after(SCHEMA_VERSION), 300);
        assert!(!inherited.resettable);
        assert_eq!(
            scheduled("{every: 300, start_after: 0}").first_turn_after(SCHEMA_VERSION),
            0
        );
        assert!(scheduled("{every: 300, resettable: true}").resettable);
    }

    /// `background` is a member's own say over whether it holds the run open,
    /// on both kinds, from the schema that has it — and what an omission means
    /// is the schema's: the schedule from that version, the graph's shape before.
    ///
    /// Both halves are asserted through parsed documents rather than struct
    /// literals: the field is absent from every graph in existence, and what its
    /// absence means under each schema is most of this change.
    #[test]
    fn background_is_declared_from_the_schema_that_has_it_and_inferred_before() {
        // A schedule, its descendant, and a member outside both — the three
        // shapes the two readings answer differently. Nothing else in it
        // postdates version 2, so every schema from there reads it as written.
        let document = |version: u32, ticker: &str, worker: &str| {
            format!(
                concat!(
                    "version: {}\nname: g\nmembers:\n",
                    "  ticker:\n    kind: oneharness\n    oneharness_config: ./a.toml\n",
                    "    schedule: {{every: 1800}}\n{}",
                    "  report:\n    kind: oneharness\n    oneharness_config: ./a.toml\n",
                    "    deps: [ticker]\n",
                    "  worker:\n    kind: onejudge\n    base_config: ./b.yaml\n",
                    "    mode: bypass\n",
                    "    agent: {{oneharness_config: ./a.toml}}\n",
                    "    judge: {{oneharness_config: ./j.toml}}\n",
                    "    deps: [ticker]\n{}",
                ),
                version, ticker, worker
            )
        };
        let read = |version: u32, ticker: &str, worker: &str| -> GraphConfig {
            let document = document(version, ticker, worker);
            let graph = parse(&document);
            validate(&graph).unwrap_or_else(|err| panic!("{document}: {err}"));
            graph
        };

        // Stating nothing: from the schema that has the field the schedule
        // decides, so the two members descending from it are foreground.
        let current = read(FIRST_BACKGROUND_VERSION, "", "");
        assert!(current.is_background("ticker"));
        assert!(!current.is_background("report"));
        assert!(!current.is_background("worker"));
        assert!(current
            .members
            .values()
            .all(|member| member.declared_background().is_none()));
        let rendered = serde_norway::to_string(&current).expect("a graph serializes");
        assert!(!rendered.contains("background"), "{rendered}");

        // Under every older schema the same document is the inference those
        // documents have always run under: descending from nothing but a
        // schedule is background too — through a diamond as much as a chain.
        for older in 2..FIRST_BACKGROUND_VERSION {
            let graph = read(older, "", "");
            assert!(graph.is_background("ticker"), "version {older}");
            assert!(graph.is_background("report"), "version {older}");
            assert!(graph.is_background("worker"), "version {older}");
            let diamond = parse(&format!(
                "{}  summary:\n    kind: oneharness\n    oneharness_config: ./a.toml\n    \
                 deps: [ticker, report]\n",
                document(older, "", "")
            ));
            validate(&diamond).expect("a diamond of deps is a legal graph");
            assert!(diamond.is_background("summary"), "version {older}");
        }

        // Declared: the value wins over the default in both directions, on
        // either kind, and survives a round trip.
        let declared = read(
            FIRST_BACKGROUND_VERSION,
            "    background: false\n",
            "    background: true\n",
        );
        assert!(!declared.is_background("ticker"));
        assert!(!declared.is_background("report"));
        assert!(declared.is_background("worker"));
        assert_eq!(
            declared.members["ticker"].declared_background(),
            Some(false)
        );
        assert_eq!(declared.members["worker"].declared_background(), Some(true));
        let reparsed: GraphConfig =
            serde_norway::from_str(&serde_norway::to_string(&declared).expect("serializes"))
                .expect("reparses");
        assert_eq!(reparsed, declared);

        // And under every older schema, naming it on either kind is refused by
        // the field's name rather than run under the inference.
        for older in 2..FIRST_BACKGROUND_VERSION {
            for (member, named) in [
                ("ticker", document(older, "    background: false\n", "")),
                ("worker", document(older, "", "    background: true\n")),
            ] {
                let err = validate(&parse(&named)).expect_err("the field postdates this schema");
                assert!(err.to_string().contains("`background`"), "{older}: {err}");
                assert!(err.to_string().contains(member), "{older}: {err}");
                assert!(
                    err.to_string().contains(&format!(
                        "requires graph schema version {FIRST_BACKGROUND_VERSION}"
                    )),
                    "{older}: {err}"
                );
            }
        }

        // A name the graph has no member called holds nothing and is nothing.
        assert!(!current.is_background("ghost"));
    }

    /// The contract's own default: a side streams unless a graph turns it off.
    #[test]
    fn an_agent_side_streams_unless_it_is_turned_off() {
        let side: AgentSide = serde_norway::from_str("oneharness_config: ./a.toml\n").unwrap();
        assert!(side.stream);
        let quiet: AgentSide =
            serde_norway::from_str("oneharness_config: ./a.toml\nstream: false\n").unwrap();
        assert!(!quiet.stream);
    }
}
