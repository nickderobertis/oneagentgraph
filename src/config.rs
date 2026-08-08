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
    /// Present on a cron member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    /// Members whose settle precedes this member's first run.
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
    /// Whether `reset-timer` may restart this schedule's clock.
    #[serde(default)]
    pub resettable: bool,
}

/// serde default for [`AgentSide::stream`]: streaming is on unless a graph turns
/// it off.
fn default_stream() -> bool {
    true
}

/// The schema version this crate reads.
pub const SCHEMA_VERSION: u32 = 1;

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
    if graph.version != SCHEMA_VERSION {
        return Err(Error::InvalidConfig(format!(
            "version {} is not a graph schema this build reads; it reads version {SCHEMA_VERSION}",
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
    for (name, member) in &graph.members {
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
                if let Some(schedule) = member.schedule {
                    if schedule.every == 0 {
                        return Err(Error::InvalidConfig(format!(
                            "member {name:?}: a schedule of every 0 seconds never stops firing"
                        )));
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
        let err = validate(&parse(&ONE_MEMBER.replace("version: 1", "version: 2"))).unwrap_err();
        assert!(err.to_string().contains("it reads version 1"), "{err}");
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
