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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphConfig {
    /// Schema version.
    pub version: u32,
    /// The graph's name.
    pub name: String,
    /// Exported to every member process. Values may reference `${HOME}`.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The side that supervises.
    pub judge: JudgeSide,
    /// onejudge approval mode.
    pub mode: String,
    /// Turn ceiling for the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
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
    /// Members whose successful settle precedes this member's first run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<String>,
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

/// The judge side of a onejudge member: an oneharness identity chain, or a
/// command provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JudgeSide {
    /// Supervised by a harness selected from an oneharness config.
    Harness(JudgeHarness),
    /// Supervised by a command provider.
    Command(JudgeCommand),
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
}

/// The command-provider judge side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeCommand {
    /// The command and its arguments.
    pub command: Vec<String>,
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

/// The first graph schema version this crate still reads.
pub const FIRST_SCHEMA_VERSION: u32 = 1;

/// The latest graph schema version this crate reads and writes in examples.
pub const SCHEMA_VERSION: u32 = 5;

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

/// The first graph schema version in which a single-sided member may carry its
/// own [`task`](OneharnessMember::task) and [`dir`](OneharnessMember::dir).
///
/// Both are optional and both default to the graph's own, so a version 1 or 2
/// document keeps parsing and running exactly as before; what the gate buys is
/// that a document *using* one says which schema it was written against.
pub const FIRST_MEMBER_JOB_VERSION: u32 = 3;

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
                if let JudgeSide::Command(command) = &member.judge {
                    if command.command.is_empty() {
                        return Err(Error::InvalidConfig(format!(
                            "member {name:?}: a command judge needs a command to run"
                        )));
                    }
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
                // process happened to be; an empty `task` becomes the value of
                // this member's `--prompt`, which is a harness given no
                // instruction at all. Refusing here is what makes either the
                // author's typo rather than a member run on it.
                if member
                    .dir
                    .as_ref()
                    .is_some_and(|dir| dir.as_os_str().is_empty())
                {
                    return Err(Error::InvalidConfig(format!(
                        "member {name:?}: `dir` names no directory — omit it to work in the \
                         graph's own directory"
                    )));
                }
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
                if let Some(schedule) = member.schedule {
                    if schedule.every == 0 {
                        return Err(Error::InvalidConfig(format!(
                            "member {name:?}: a schedule of every 0 seconds never stops firing"
                        )));
                    }
                    // Refused rather than ignored under an older schema: what a
                    // missing `start_after` means is that schema's answer, so a
                    // document declaring version 3 and asking for one would
                    // otherwise be given the t=0 it did not ask for.
                    if schedule.start_after.is_some() && graph.version < FIRST_START_AFTER_VERSION {
                        return Err(Error::InvalidConfig(format!(
                            "member {name:?} uses `schedule.start_after`, which requires graph \
                             schema version {FIRST_START_AFTER_VERSION}"
                        )));
                    }
                    // A span nobody could mean is refused as the typo it is,
                    // rather than becoming a member that waits out the heat death
                    // of the universe while reporting nothing at all.
                    for (field, seconds) in [
                        ("every", schedule.every),
                        ("start_after", schedule.first_turn_after(graph.version)),
                    ] {
                        if seconds > MAX_SCHEDULE_SECONDS {
                            return Err(Error::InvalidConfig(format!(
                                "member {name:?}: `{field}` of {seconds} seconds is longer than \
                                 any run this will ever pace — the ceiling is \
                                 {MAX_SCHEDULE_SECONDS}"
                            )));
                        }
                    }
                }
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
