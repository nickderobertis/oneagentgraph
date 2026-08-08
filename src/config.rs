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
