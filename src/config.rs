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
    /// The members, by name.
    pub members: BTreeMap<String, Member>,
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
    /// Seconds before this member's **first** turn, defaulting to
    /// [`every`](Self::every) and read through [`first_turn_after`].
    ///
    /// `0` is the member taking a turn the moment the graph starts, which is what
    /// every schedule did before this field existed.
    ///
    /// Deliberately **not** gated on a schema version, unlike
    /// [`OneharnessMember::task`] and [`OneharnessMember::dir`]. Those changed
    /// nothing for a document that omits them, so a gate only asked such a
    /// document to say which schema it was written against; this one moves the
    /// default for every schedule already written, and a version 1 or 2 document
    /// has to be able to ask for the old behaviour back.
    ///
    /// [`first_turn_after`]: Self::first_turn_after
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_after: Option<u64>,
    /// Whether `reset-timer` may restart this schedule's clock.
    #[serde(default)]
    pub resettable: bool,
}

impl Schedule {
    /// Seconds between the graph starting and this member's first turn.
    ///
    /// A schedule that names no `start_after` waits one whole interval, because
    /// "every 1800 seconds" reads as *from now on* rather than *now, and then
    /// every 1800 seconds* — and a member whose job is to report progress has
    /// nothing to report at t=0. A schedule that wants the old behaviour asks for
    /// it by name with `start_after: 0`.
    #[must_use]
    pub fn first_turn_after(&self) -> u64 {
        self.start_after.unwrap_or(self.every)
    }
}

/// serde default for [`AgentSide::stream`]: streaming is on unless a graph turns
/// it off.
fn default_stream() -> bool {
    true
}

/// The longest span a [`Schedule`] may name, in seconds — a shade over 136 years.
///
/// Not a policy about cadence: it is the bound that keeps a document from
/// crashing the run. Both of a schedule's spans are added to a monotonic clock,
/// which panics rather than saturating when the sum is not representable, and
/// `u64::MAX` seconds is some four hundred billion years. `u32::MAX` is far past
/// any run and far short of any platform's ceiling, and it is one number rather
/// than a per-platform probe.
pub const MAX_SCHEDULE_SECONDS: u64 = u32::MAX as u64;

/// The first graph schema version this crate still reads.
pub const FIRST_SCHEMA_VERSION: u32 = 1;

/// The latest graph schema version this crate reads and writes in examples.
pub const SCHEMA_VERSION: u32 = 3;

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
                    // Both spans are added to a monotonic clock to get the moment
                    // the member is next due, and that addition *panics* on a sum
                    // the platform cannot represent. A schedule's seconds are a
                    // `u64` an external document supplies, so without this a
                    // number nobody could mean would take the whole run down
                    // rather than being refused as the typo it is.
                    for (field, seconds) in [
                        ("every", schedule.every),
                        ("start_after", schedule.first_turn_after()),
                    ] {
                        if seconds > MAX_SCHEDULE_SECONDS {
                            return Err(Error::InvalidConfig(format!(
                                "member {name:?}: `{field}` of {seconds} seconds is longer than \
                                 any run, and past what a clock can count to — the ceiling is \
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

    /// A schedule's first turn waits one whole interval unless it says otherwise,
    /// and `0` is the spelling that asks for a turn at t=0.
    ///
    /// The default is what a schedule *already written* now means, so it is
    /// asserted through parsing a document rather than through a struct literal:
    /// the field is absent from every graph in existence, and its absence is the
    /// case that matters.
    #[test]
    fn a_schedules_first_turn_waits_an_interval_unless_it_names_another() {
        let scheduled = |schedule: &str| -> Schedule {
            let document = format!(
                concat!(
                    "version: 1\nname: g\nmembers:\n  ticker:\n    kind: oneharness\n",
                    "    oneharness_config: ./a.toml\n    schedule: {}\n",
                ),
                schedule
            );
            let graph = parse(&document);
            validate(&graph).unwrap_or_else(|err| panic!("{document}: {err}"));
            let Member::Oneharness(member) = &graph.members["ticker"] else {
                panic!("a scheduled member is single-sided")
            };
            member.schedule.expect("the member is scheduled")
        };

        let inherited = scheduled("{every: 1800}");
        assert_eq!(inherited.start_after, None);
        assert_eq!(inherited.first_turn_after(), 1800);
        assert_eq!(
            scheduled("{every: 1800, start_after: 0}").first_turn_after(),
            0
        );
        assert_eq!(
            scheduled("{every: 1800, start_after: 5}").first_turn_after(),
            5
        );
        // Longer than the cadence is a legal thing to ask for — "settle in, then
        // report often" — so it is carried rather than clamped.
        assert_eq!(
            scheduled("{every: 60, start_after: 600}").first_turn_after(),
            600
        );
        // And a schedule that named none serializes without one, so a document
        // written before the field existed round-trips unchanged.
        let rendered = serde_norway::to_string(&inherited).expect("a schedule serializes");
        assert!(!rendered.contains("start_after"), "{rendered}");
    }

    /// A span longer than a clock can count to is refused by name, on both of a
    /// schedule's fields and however it was arrived at.
    ///
    /// Not a policy about cadence. Both spans are added to a monotonic clock,
    /// which *panics* rather than saturating on a sum it cannot represent, so
    /// without this a number in a document takes the run down instead of being
    /// refused as the typo it is.
    #[test]
    fn a_schedule_longer_than_a_clock_can_count_to_is_refused() {
        let document = |schedule: String| {
            format!(
                concat!(
                    "version: 1\nname: g\nmembers:\n  ticker:\n    kind: oneharness\n",
                    "    oneharness_config: ./a.toml\n    schedule: {}\n",
                ),
                schedule
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
                err.to_string().contains("what a clock can count to"),
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
