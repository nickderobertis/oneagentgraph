//! Running a whole graph: dependency order, cron members, and one merged stream.
//!
//! `docs/contract.md` fixes what a run is worth at the end: "Exit 0 = every
//! member settled successfully; 1 = a member failed or died (the stream says
//! which and why); 2 = invalid config." So a run's job is to produce the stream
//! and reduce it to one of those three.
//!
//! Ordering is the graph's own: `deps` names the members whose settle precedes
//! this member's first run, and everything with no unsettled dependency runs
//! concurrently. A cron member is one whose schedule fires it again after each
//! settle, and `trigger` / `reset-timer` are how an operator moves that clock
//! from outside the run.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::clock::unix_millis;
use crate::config::{GraphConfig, Member};
use crate::error::{Error, EXIT_INVALID_CONFIG, EXIT_MEMBER_FAILED, EXIT_SUCCESS};
use crate::event::{Emitter, EventKind};
use crate::invoke::{self, Context, Invocation};
use crate::member::{self, Bounds, Outcome};
use crate::resolve::{ResolvedRef, Resolver};
use crate::scratch::Owned;

/// The control file a run watches for `trigger` and `reset-timer`.
///
/// A signal is a file rather than a socket because the two commands are the
/// contract's whole out-of-band surface, a run may be detached from any
/// terminal, and a file in the run's own directory is something an operator can
/// see, script, and clean up.
pub const SIGNAL_DIR: &str = "signals";

/// Where a run's merged NDJSON is always written, whatever `--output` renders.
pub const EVENTS_FILE: &str = "events.jsonl";

/// The run record `history` reads back.
pub const RECORD_FILE: &str = "record.json";

/// The schema version this build writes into every [`Record`].
///
/// A run record outlives the binary that wrote it — `history` is expected to read
/// runs from months ago — so the shape needs a number a reader can branch on
/// rather than a guess from which keys happen to be present.
///
/// * **1** — the original shape, written without this field at all.
/// * **2** — adds `declared_members`, the graph's member list, so `trigger`,
///   `reset-timer`, and `cancel` can tell a member of the run from a typo while
///   it is still in flight.
/// * **3** — records `skipped (...)` when unsuccessful dependencies prevent a
///   member from starting.
///
/// A record with no version reads as 1, and every field added since is optional
/// and omitted when empty, so a 1 still reads exactly as it did. A version
/// *above* this one is refused by name instead of parsed: those records were
/// written by a build that knew something this one does not, and `deny_unknown_fields`
/// would otherwise reject them with a message about a key rather than a version.
pub const RECORD_SCHEMA_VERSION: u32 = 3;

/// The version a record that names none was written under.
fn unversioned_record() -> u32 {
    1
}

/// How often a run notices a `trigger` / `reset-timer` signal, and re-checks a
/// schedule's clock.
const TICK: Duration = Duration::from_millis(100);

/// One run, as `history` keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// The shape this record was written under — see [`RECORD_SCHEMA_VERSION`].
    ///
    /// Defaulted rather than required, because a record from before the field
    /// existed is a version 1 record and still has to read.
    #[serde(default = "unversioned_record")]
    pub schema_version: u32,
    /// The run's id.
    pub run_id: RunId,
    /// The graph it ran.
    pub graph: String,
    /// The graph's own name.
    pub name: String,
    /// When it started, as milliseconds since the Unix epoch.
    pub started_ms: u64,
    /// When it finished, absent while it is still running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_ms: Option<u64>,
    /// The process exit code the contract assigns this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// What became of each member, by name.
    #[serde(default)]
    pub members: BTreeMap<String, MemberOutcome>,
    /// Every member the graph declared, written before any of them runs.
    ///
    /// `members` above fills in as members *settle*, so for the whole of a live
    /// run it is empty — and that is exactly when `trigger`, `reset-timer`, and
    /// `cancel` are used. Without this, those verbs had nothing to check a member
    /// name against while the run was in flight, and answered a typo with a
    /// signal file nothing would ever read and an exit 0.
    ///
    /// Optional and omitted when empty, so a record written before this field
    /// existed still reads, and the verbs fall back to `members` for one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declared_members: Vec<String>,
    /// Every config ref the run read, content-addressed, so replay and audit
    /// never depend on a URL staying stable.
    #[serde(default)]
    pub refs: Vec<ResolvedRef>,
    /// Where the run's merged NDJSON is.
    ///
    /// Reported, never followed. A run always writes its stream to
    /// [`EVENTS_FILE`] inside its own directory, so every reader in this crate
    /// derives that location from the run's id instead — this field is here so a
    /// consumer of the record, or of the `--detach` answer, is told the path
    /// without having to know how one is composed. Nothing branches on it, and
    /// no path this process reads or writes is built from it, so it stays a
    /// plain `String`: a validated path type would promise a guarantee about a
    /// value this crate never acts on.
    pub events_path: String,
}

/// What became of one member, as the run record keeps it.
///
/// A closed set rather than free-form prose: `history` and anything reading
/// `record.json` branch on it, and "incomplete" and "died" are the distinction a
/// supervisor most needs. It is written as the same one-line string it always
/// was, so the record's shape is unchanged and an existing reader still parses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum MemberOutcome {
    /// The member reached its completion bar.
    Settled,
    /// The member drove the task but did not complete it.
    Incomplete,
    /// The member died; the liveness rule that found it is named.
    Died(crate::member::Rule),
    /// The member could not be started at all.
    Unstartable(String),
    /// The member was not started because these dependencies did not succeed.
    Skipped(SkippedDeps),
}

/// The non-empty, validated dependency list carried by a skipped outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedDeps(Vec<String>);

impl SkippedDeps {
    fn new(deps: Vec<String>) -> Result<Self, String> {
        if deps.is_empty() || deps.iter().any(|dep| !crate::config::is_member_name(dep)) {
            return Err("a skipped outcome needs one or more valid dependency names".into());
        }
        Ok(Self(deps))
    }
}

impl From<MemberOutcome> for String {
    fn from(outcome: MemberOutcome) -> Self {
        match outcome {
            MemberOutcome::Settled => "settled".into(),
            MemberOutcome::Incomplete => "incomplete".into(),
            MemberOutcome::Died(rule) => format!("died ({})", rule.as_str()),
            MemberOutcome::Unstartable(reason) => format!("unstartable ({reason})"),
            MemberOutcome::Skipped(deps) => format!("skipped ({})", deps.0.join(", ")),
        }
    }
}

impl TryFrom<String> for MemberOutcome {
    type Error = String;

    fn try_from(recorded: String) -> Result<Self, String> {
        if recorded == "settled" {
            return Ok(MemberOutcome::Settled);
        }
        if recorded == "incomplete" {
            return Ok(MemberOutcome::Incomplete);
        }
        if let Some(rule) = recorded
            .strip_prefix("died (")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            return crate::member::Rule::named(rule)
                .map(MemberOutcome::Died)
                .ok_or_else(|| format!("{rule:?} is not a liveness rule this build knows"));
        }
        if let Some(reason) = recorded
            .strip_prefix("unstartable (")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            return Ok(MemberOutcome::Unstartable(reason.to_string()));
        }
        if let Some(deps) = recorded
            .strip_prefix("skipped (")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            return SkippedDeps::new(deps.split(", ").map(str::to_string).collect())
                .map(MemberOutcome::Skipped);
        }
        Err(format!("{recorded:?} is not an outcome this build records"))
    }
}

/// What a run was asked to do.
#[derive(Debug, Clone)]
pub struct Request {
    /// The graph config, by path or URL.
    pub graph: crate::config::ConfigRef,
    /// The task prose every member that takes one is given.
    pub task: Option<String>,
    /// The directory members work in.
    pub dir: PathBuf,
    /// Extra labels stamped on every event.
    pub labels: Vec<Label>,
    /// `members.worker.agent.model=NAME`-style overrides, already parsed.
    pub overrides: Vec<Override>,
    /// Where run state lives.
    pub state_dir: PathBuf,
    /// The `oneharness` binary.
    pub oneharness_bin: String,
}

/// A run that has started: what a caller needs to watch or cancel it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Started {
    /// The run's id.
    pub run_id: RunId,
    /// Where its merged NDJSON is being written.
    pub events_path: String,
    /// The process producing it.
    pub pid: u32,
}

/// A run id: sortable, unique on one host, and readable in a directory listing.
///
/// A type rather than a `String` because a run id is *joined onto the state
/// directory* by every verb that reaches a run — and it arrives not only on the
/// argv but out of `record.json`, which is a file on disk this crate re-reads
/// and therefore external input like any other. Left as a `String`, a record
/// carrying `../..` in this field sends `cancel` on to create a directory and
/// write a stop signal outside the run store entirely. Parsing at the boundary
/// is what makes that unrepresentable: a value of this type is one this crate
/// could have minted, so the joins downstream need no guard of their own.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RunId(String);

impl RunId {
    /// Mint one for `name`, sortable by `at` and unique on this host.
    #[must_use]
    pub fn mint(name: &str, at: SystemTime) -> Self {
        let slug: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        Self(format!(
            "{}-{}-{}",
            slug.trim_matches('-'),
            unix_millis(at),
            std::process::id()
        ))
    }

    /// One run id, parsed: nonempty, and drawn only from the alphabet
    /// [`RunId::mint`] writes — lowercase ASCII, digits, and `-`.
    ///
    /// That alphabet, and deliberately not `mint`'s *structure*
    /// (`<slug>-<millis>-<pid>`). A run directory outlives the version that made
    /// it, and `history` is expected to still read it; a parse tied to today's
    /// layout would make every earlier run unreadable the first time that layout
    /// gains a field, which is a worse failure than the one it would prevent.
    /// What the joins downstream actually depend on is that the value is a
    /// single path component that cannot traverse out of the run store, and the
    /// alphabet is exactly that guarantee — no separator, no `.`, so no `..`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidConfig`] when `raw` is outside that alphabet, and so
    /// could name a path outside the run store.
    pub fn parse(raw: &str) -> Result<Self, Error> {
        let path_component = !raw.is_empty()
            && raw
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !path_component {
            return Err(Error::InvalidConfig(format!(
                "{raw:?} is not a run id: a run id is lowercase letters, digits, and hyphens, and \
                 this one would name a path outside the run store"
            )));
        }
        Ok(Self(raw.to_string()))
    }

    /// This id as it is written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<Path> for RunId {
    fn as_ref(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl<'de> Deserialize<'de> for RunId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// One `--set PATH=VALUE`, parsed.
///
/// A type rather than a pair, because the parse is what makes the path usable:
/// it names a field of the graph document, and one that names nothing refuses
/// the run. A `(String, String)` in the public request would let a caller hand
/// in an unparsed pair and reach the same code by another door.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Override {
    /// The dotted path into the graph document.
    path: String,
    /// What to set it to.
    value: String,
}

/// One `--label k=v`, parsed.
///
/// A type for the same reason: the key lands in an envelope's flattened labels,
/// and which keys are usable there is decided once, here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    /// The label key.
    key: String,
    /// Its value.
    value: String,
}

impl Label {
    /// The key, as an envelope carries it.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The value.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// Parse one `--set members.worker.agent.model=NAME` override.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when the argument carries no `=`, or an empty path.
pub fn parse_set(raw: &str) -> Result<Override, Error> {
    let (path, value) = raw.split_once('=').ok_or_else(|| {
        Error::InvalidConfig(format!(
            "--set {raw:?}: expected PATH=VALUE, as in members.worker.agent.model=NAME"
        ))
    })?;
    if path.is_empty() {
        return Err(Error::InvalidConfig(format!(
            "--set {raw:?}: the path before `=` is empty"
        )));
    }
    Ok(Override {
        path: path.to_string(),
        value: value.to_string(),
    })
}

/// Parse one `--label k=v`.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when the argument carries no `=`, or an empty key.
pub fn parse_label(raw: &str) -> Result<Label, Error> {
    let (key, value) = raw
        .split_once('=')
        .ok_or_else(|| Error::InvalidConfig(format!("--label {raw:?}: expected k=v")))?;
    // The key lands in an envelope's flattened `labels`, beside the reserved
    // ones. A key that is not an identifier would produce an envelope a consumer
    // cannot address by field, and one that *is* a reserved key would look like
    // the run's own stamp while carrying whatever an operator typed — the
    // contract's "enrichers never rewrite" read from the wrong direction.
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return Err(Error::InvalidConfig(format!(
            "--label {raw:?}: a label key is letters, digits, underscores, and dots"
        )));
    }
    if RESERVED_LABELS.contains(&key) {
        return Err(Error::InvalidConfig(format!(
            "--label {raw:?}: {key:?} is a reserved label this run stamps itself; pick another \
             name so a consumer can tell the two apart"
        )));
    }
    Ok(Label {
        key: key.to_string(),
        value: value.to_string(),
    })
}

/// The label keys `docs/contract.md` reserves, which a run stamps itself.
const RESERVED_LABELS: &[&str] = &["run_id", "member", "persona"];

/// Apply the run's `--set` overrides to a parsed graph document.
///
/// The paths are the config's own field names, so an override is checked against
/// the same schema the file is: a path naming nothing fails here rather than
/// being silently dropped, which is the difference between a member on the model
/// an operator asked for and one on the model they thought they asked for.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when a path names nothing in the schema, an
/// intermediate parent is absent, or the value does not have the field's type.
pub fn apply_overrides(document: &mut Value, overrides: &[Override]) -> Result<(), Error> {
    for Override { path, value } in overrides {
        let before = document.clone();
        let mut cursor = &mut *document;
        let segments: Vec<&str> = path.split('.').collect();
        let (last, parents) = segments.split_last().ok_or_else(|| {
            Error::InvalidConfig(format!("--set {path}=…: the path before `=` is empty"))
        })?;
        for segment in parents {
            cursor = cursor.get_mut(segment).ok_or_else(|| {
                Error::InvalidConfig(format!("--set {path}=…: this graph has no {segment}"))
            })?;
        }
        let object = cursor.as_object_mut().ok_or_else(|| {
            Error::InvalidConfig(format!("--set {path}=…: this graph has no {last}"))
        })?;
        if let Some(slot) = object.get_mut(*last) {
            *slot = match slot {
                Value::Number(_) => value.parse::<u64>().map(Value::from).map_err(|_| {
                    Error::InvalidConfig(format!("--set {path}={value:?}: not a number"))
                })?,
                Value::Bool(_) => value.parse::<bool>().map(Value::from).map_err(|_| {
                    Error::InvalidConfig(format!("--set {path}={value:?}: not a boolean"))
                })?,
                _ => Value::String(value.clone()),
            };
        } else {
            // An override may add an optional leaf, but it may not repair an
            // otherwise invalid graph by supplying a required field.
            graph_from_value(&before).map_err(|err| {
                Error::InvalidConfig(format!(
                    "--set {path}=…: the graph must satisfy the schema before an absent field can be populated: {err}"
                ))
            })?;
            // The document cannot tell an absent optional leaf's type. Let the
            // same deny-unknown-fields schema that reads graph files decide
            // both whether the field exists and which textual shape it accepts.
            // String comes first so a string field keeps CLI text such as
            // `true`; YAML parsing supplies numbers, booleans, lists, and maps.
            let parsed = serde_norway::from_str::<Value>(value).ok();
            let mut candidates = vec![Value::String(value.clone())];
            if let Some(parsed) = parsed.filter(|parsed| parsed != &candidates[0]) {
                candidates.push(parsed);
            }
            let mut accepted = None;
            for candidate in candidates {
                let mut candidate_document = before.clone();
                insert_leaf(&mut candidate_document, parents, last, candidate);
                if graph_from_value(&candidate_document).is_ok() {
                    accepted = Some(candidate_document);
                    break;
                }
            }
            let candidate_document = accepted.ok_or_else(|| {
                Error::InvalidConfig(format!(
                    "--set {path}={value:?}: the schema has no field at this path, or the value does not parse as that field's type"
                ))
            })?;
            *document = candidate_document;
        }
    }
    Ok(())
}

fn graph_from_value(document: &Value) -> Result<GraphConfig, serde_norway::Error> {
    serde_norway::from_value(serde_norway::to_value(document)?)
}

fn insert_leaf(document: &mut Value, parents: &[&str], last: &str, value: Value) {
    let mut cursor = document;
    for segment in parents {
        cursor = cursor
            .get_mut(segment)
            .expect("parents were found in the source document");
    }
    cursor
        .as_object_mut()
        .expect("the leaf's parent was an object in the source document")
        .insert(last.to_string(), value);
}

/// The order members may start in, or the reason there is none.
///
/// # Errors
///
/// [`Error::InvalidConfig`] naming a dependency that is not a member, or the
/// members left in a cycle. A graph whose `deps` cannot be satisfied would
/// otherwise start nothing and settle as if it had.
pub fn ready_order(graph: &GraphConfig) -> Result<Vec<Vec<String>>, Error> {
    let mut pending: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (name, member) in &graph.members {
        let deps: &[String] = match member {
            Member::Oneharness(member) => &member.deps,
            Member::Onejudge(member) => &member.deps,
        };
        for dep in deps {
            if !graph.members.contains_key(dep) {
                return Err(Error::InvalidConfig(format!(
                    "member {name:?} depends on {dep:?}, which this graph has no member called"
                )));
            }
        }
        pending.insert(name, deps.iter().map(String::as_str).collect());
    }
    let mut waves = Vec::new();
    while !pending.is_empty() {
        let ready: Vec<String> = pending
            .iter()
            .filter(|(_, deps)| deps.is_empty())
            .map(|(name, _)| (*name).to_string())
            .collect();
        if ready.is_empty() {
            let stuck: Vec<&str> = pending.keys().copied().collect();
            return Err(Error::InvalidConfig(format!(
                "the `deps` of {} form a cycle, so none of them can ever start",
                stuck.join(", ")
            )));
        }
        for name in &ready {
            pending.remove(name.as_str());
        }
        for deps in pending.values_mut() {
            deps.retain(|dep| !ready.iter().any(|name| name == dep));
        }
        waves.push(ready);
    }
    Ok(waves)
}

/// Run a graph to its end, writing envelopes to `sink`.
///
/// # Errors
///
/// [`Error::InvalidConfig`] for anything that stops the graph starting; a member
/// that starts and then fails is reported through the stream and the exit code,
/// not as an error.
pub fn run(
    request: &Request,
    sink: Box<dyn std::io::Write + Send>,
    env: &BTreeMap<String, String>,
) -> Result<i32, Error> {
    let mut resolver = Resolver::new();
    let graph_document = resolver.resolve(&request.graph, None)?.clone();
    let mut parsed: Value = serde_norway::from_str(&graph_document.content)
        .map_err(|err| Error::InvalidConfig(format!("{}: {err}", request.graph.0)))?;
    apply_overrides(&mut parsed, &request.overrides)?;
    let graph: GraphConfig = serde_norway::from_value(
        serde_norway::to_value(&parsed)
            .map_err(|err| Error::InvalidConfig(format!("{}: {err}", request.graph.0)))?,
    )
    .map_err(|err| Error::InvalidConfig(format!("{}: {err}", request.graph.0)))?;
    crate::config::validate(&graph)?;
    let waves = ready_order(&graph)?;

    let run_id = RunId::mint(&graph.name, SystemTime::now());
    let root = request.state_dir.join(&run_id);
    let mut owned = Owned::claim(&root)?;
    owned.keep();
    std::fs::create_dir_all(root.join(SIGNAL_DIR)).map_err(|err| {
        Error::InvalidConfig(format!(
            "cannot create {}: {err}",
            root.join(SIGNAL_DIR).display()
        ))
    })?;
    let events_path = root.join(EVENTS_FILE);

    let mut member_env = env.clone();
    // Before the graph's own block, so a graph that means to select this way
    // still can — see `invoke::PROCESS_WIDE_HARNESS_ENV`.
    member_env.remove(invoke::PROCESS_WIDE_HARNESS_ENV);
    for (key, value) in &graph.env {
        member_env.insert(key.clone(), expand(value, env));
    }
    let bounds = Bounds::from_env(&member_env).map_err(Error::InvalidConfig)?;
    export(&graph.env, env)?;

    let file = std::fs::File::create(&events_path).map_err(|err| {
        Error::InvalidConfig(format!("cannot create {}: {err}", events_path.display()))
    })?;
    let emitter = Emitter::new(run_id.to_string(), Box::new(Tee { sink, file })).with_labels(
        crate::event::Labels {
            run_id: Some(run_id.to_string()),
            extra: request
                .labels
                .iter()
                .map(|label| (label.key.clone(), Value::String(label.value.clone())))
                .collect(),
            ..crate::event::Labels::default()
        },
    );

    let mut record = Record {
        schema_version: RECORD_SCHEMA_VERSION,
        run_id: run_id.clone(),
        graph: request.graph.0.clone(),
        name: graph.name.clone(),
        started_ms: unix_millis(SystemTime::now()),
        finished_ms: None,
        exit_code: None,
        members: BTreeMap::new(),
        declared_members: graph.members.keys().cloned().collect(),
        refs: Vec::new(),
        events_path: events_path.display().to_string(),
    };
    write_record(&root, &record)?;

    // Every member's invocation is built before *anything* is launched, and
    // before the first envelope is written. The model pairing rule, a persona
    // that does not validate, and a ref that cannot be read are all refusals —
    // and a graph that refuses half way has already spent a paid turn on the
    // members it did start. Emitting `graph-started` first would also make a
    // refusal read as a graph that began, which is exactly what a caller wired
    // in early must not see.
    let mut invocations = BTreeMap::new();
    for (name, member) in &graph.members {
        let scratch = root.join("members").join(name);
        std::fs::create_dir_all(&scratch).map_err(|err| {
            Error::InvalidConfig(format!("cannot create {}: {err}", scratch.display()))
        })?;
        let context = Context {
            dir: &request.dir,
            scratch: &scratch,
            graph_dir: graph_document.base_dir.as_deref(),
            task: request.task.as_deref(),
            session: &format!("{run_id}-{name}"),
            oneharness_bin: &request.oneharness_bin,
        };
        invocations.insert(
            name.clone(),
            (invoke::build(member, &context, &mut resolver)?, scratch),
        );
    }
    record.refs = resolver.inventory();
    write_record(&root, &record)?;

    emitter.emit(
        EventKind::GraphStarted,
        [
            ("graph".to_string(), Value::String(request.graph.0.clone())),
            ("name".to_string(), Value::String(graph.name.clone())),
            (
                "dir".to_string(),
                Value::String(request.dir.display().to_string()),
            ),
        ]
        .into_iter()
        .collect::<Map<String, Value>>(),
    );

    let non_cron_live = Arc::new(AtomicUsize::new(
        graph
            .members
            .keys()
            .filter(|name| !solely_cron_descended(name, &graph, &mut BTreeMap::new()))
            .count(),
    ));
    let (cron_tx, cron_rx) = mpsc::channel();
    let mut cron_threads = Vec::new();
    let mut failed = false;
    for wave in waves {
        let mut runnable = Vec::new();
        for name in wave {
            let unsuccessful: Vec<String> = deps(&graph.members[&name])
                .iter()
                .filter(|dep| record.members.get(*dep) != Some(&MemberOutcome::Settled))
                .cloned()
                .collect();
            if unsuccessful.is_empty() {
                runnable.push(name);
            } else {
                record.members.insert(
                    name.clone(),
                    MemberOutcome::Skipped(
                        SkippedDeps::new(unsuccessful)
                            .expect("eligibility found validated graph dependencies"),
                    ),
                );
                if !solely_cron_descended(&name, &graph, &mut BTreeMap::new()) {
                    non_cron_live.fetch_sub(1, Ordering::SeqCst);
                }
            }
        }
        let outcomes = run_wave(&runnable, &invocations, &emitter, &member_env, bounds);
        for (name, outcome) in &outcomes {
            record.members.insert(name.clone(), describe(outcome));
            failed |= !outcome.is_success();
            if !solely_cron_descended(name, &graph, &mut BTreeMap::new()) {
                non_cron_live.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let successful: BTreeSet<String> = record
            .members
            .iter()
            .filter(|(_, outcome)| **outcome == MemberOutcome::Settled)
            .map(|(name, _)| name.clone())
            .collect();
        for (name, _) in outcomes {
            if let Some(schedule) = schedule(&graph.members[&name]) {
                cron_threads.push(spawn_cron(
                    schedule,
                    name,
                    graph.clone(),
                    invocations.clone(),
                    emitter.clone(),
                    member_env.clone(),
                    bounds,
                    root.to_path_buf(),
                    Arc::clone(&non_cron_live),
                    cron_tx.clone(),
                    successful.clone(),
                ));
            }
        }
    }
    drop(cron_tx);
    for thread in cron_threads {
        let _ = thread.join();
    }
    for (name, outcome) in cron_rx {
        failed |= !outcome.is_success();
        record.members.insert(name, describe(&outcome));
    }

    let exit = if failed {
        EXIT_MEMBER_FAILED
    } else {
        EXIT_SUCCESS
    };
    emitter.emit(
        EventKind::GraphSettled,
        [
            ("exit_code".to_string(), Value::from(exit)),
            (
                "members".to_string(),
                Value::Object(
                    record
                        .members
                        .iter()
                        .map(|(name, outcome)| {
                            (name.clone(), Value::String(String::from(outcome.clone())))
                        })
                        .collect(),
                ),
            ),
        ]
        .into_iter()
        .collect::<Map<String, Value>>(),
    );
    record.finished_ms = Some(unix_millis(SystemTime::now()));
    record.exit_code = Some(exit);
    write_record(&root, &record)?;
    let _ = crate::scratch::reap(&root);
    Ok(exit)
}

fn deps(member: &Member) -> &[String] {
    match member {
        Member::Oneharness(member) => &member.deps,
        Member::Onejudge(member) => &member.deps,
    }
}

fn schedule(member: &Member) -> Option<crate::config::Schedule> {
    match member {
        Member::Oneharness(member) => member.schedule,
        Member::Onejudge(_) => None,
    }
}

fn solely_cron_descended(
    name: &str,
    graph: &GraphConfig,
    memo: &mut BTreeMap<String, bool>,
) -> bool {
    if let Some(answer) = memo.get(name) {
        return *answer;
    }
    let member = &graph.members[name];
    let answer = schedule(member).is_some()
        || (!deps(member).is_empty()
            && deps(member)
                .iter()
                .all(|dep| solely_cron_descended(dep, graph, memo)));
    memo.insert(name.to_string(), answer);
    answer
}

// These are the immutable run resources a detached clock and its chain need;
// bundling them would only move the same fields into a single-use struct.
#[allow(clippy::too_many_arguments)]
fn spawn_cron(
    schedule: crate::config::Schedule,
    name: String,
    graph: GraphConfig,
    invocations: BTreeMap<String, (Invocation, PathBuf)>,
    emitter: Emitter,
    env: BTreeMap<String, String>,
    bounds: Bounds,
    root: PathBuf,
    live: Arc<AtomicUsize>,
    outcomes: mpsc::Sender<(String, Outcome)>,
    successful: BTreeSet<String>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let (invocation, scratch) = &invocations[&name];
        let member_emitter = emitter.with_labels(member::labels(
            emitter.stream(),
            &name,
            invocation.persona.as_deref(),
        ));
        let descendants = descendants_of(&name, &graph);
        let outcome = cron(
            &schedule,
            &name,
            invocation,
            &member_emitter,
            &env,
            bounds,
            scratch,
            &root.join(SIGNAL_DIR),
            &live,
            || {
                run_cron_chain(
                    &descendants,
                    &graph,
                    &invocations,
                    &emitter,
                    &env,
                    bounds,
                    &outcomes,
                    &successful,
                );
            },
        );
        if let Some(outcome) = outcome {
            let _ = outcomes.send((name, outcome));
        }
    })
}

fn descendants_of(root: &str, graph: &GraphConfig) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    loop {
        let added: Vec<String> = graph
            .members
            .iter()
            .filter(|(name, member)| {
                *name != root
                    && !found.contains(*name)
                    && deps(member)
                        .iter()
                        .any(|dep| dep == root || found.contains(dep))
            })
            .map(|(name, _)| name.clone())
            .collect();
        if added.is_empty() {
            return found;
        }
        found.extend(added);
    }
}

// The chain reuses the same immutable run resources as its clock, plus the
// descendant and prior-success sets that define this iteration's eligibility.
#[allow(clippy::too_many_arguments)]
fn run_cron_chain(
    descendants: &BTreeSet<String>,
    graph: &GraphConfig,
    invocations: &BTreeMap<String, (Invocation, PathBuf)>,
    emitter: &Emitter,
    env: &BTreeMap<String, String>,
    bounds: Bounds,
    outcomes: &mpsc::Sender<(String, Outcome)>,
    initially_successful: &BTreeSet<String>,
) {
    let Ok(waves) = ready_order(graph) else {
        return;
    };
    let mut successful = initially_successful.clone();
    for wave in waves {
        let runnable: Vec<String> = wave
            .into_iter()
            .filter(|name| descendants.contains(name))
            .filter(|name| {
                deps(&graph.members[name])
                    .iter()
                    .all(|dep| successful.contains(dep))
            })
            .collect();
        for (name, outcome) in run_wave(&runnable, invocations, emitter, env, bounds) {
            if outcome.is_success() {
                successful.insert(name.clone());
            }
            let _ = outcomes.send((name, outcome));
        }
    }
}

/// Run one wave of members concurrently.
fn run_wave(
    wave: &[String],
    invocations: &BTreeMap<String, (Invocation, PathBuf)>,
    emitter: &Emitter,
    env: &BTreeMap<String, String>,
    bounds: Bounds,
) -> Vec<(String, Outcome)> {
    let (tx, rx) = mpsc::channel();
    let mut running = 0;
    for name in wave {
        let Some((invocation, scratch)) = invocations.get(name) else {
            continue;
        };
        let member_emitter = emitter.with_labels(member::labels(
            emitter.stream(),
            name,
            invocation.persona.as_deref(),
        ));
        let (name, invocation, scratch, env, tx) = (
            name.clone(),
            invocation.clone(),
            scratch.clone(),
            env.clone(),
            tx.clone(),
        );
        running += 1;
        std::thread::spawn(move || {
            let outcome = member::run(&invocation, &member_emitter, &env, bounds, &scratch);
            let _ = tx.send((name, outcome));
        });
    }
    drop(tx);
    let mut settled = Vec::new();
    for _ in 0..running {
        if let Ok(outcome) = rx.recv() {
            settled.push(outcome);
        }
    }
    settled.sort_by(|a, b| a.0.cmp(&b.0));
    settled
}

/// Keep firing a scheduled member until its run is asked to stop.
///
/// A cron member's own settle is not the end of it: the schedule fires it again
/// after `every` seconds, `trigger` fires it now, and `reset-timer` restarts the
/// clock — the last only on a schedule that declared itself `resettable`, so an
/// operator cannot quietly defer a member whose author said its cadence is
/// fixed.
// Every parameter is one thing a firing needs and none is derivable from
// another: its schedule, identity and invocation, event sink, environment,
// bounds, scratch and signals, plus the liveness gate and successful-chain
// callback. Bundling them would name the same values twice.
#[allow(clippy::too_many_arguments)]
fn cron(
    schedule: &crate::config::Schedule,
    name: &str,
    invocation: &Invocation,
    emitter: &Emitter,
    env: &BTreeMap<String, String>,
    bounds: Bounds,
    scratch: &Path,
    signals: &Path,
    live: &AtomicUsize,
    mut on_success: impl FnMut(),
) -> Option<Outcome> {
    let stop = signals.join("stop");
    // A member-scoped `cancel` stops this member alone; the whole-run `stop`
    // above stops every one of them.
    let own_stop = signals.join(format!("{name}.stop"));
    let trigger = signals.join(format!("{name}.trigger"));
    let reset = signals.join(format!("{name}.reset"));
    let stopped = || stop.exists() || own_stop.exists();
    let mut last = None;
    let mut due = Instant::now() + Duration::from_secs(schedule.every);
    loop {
        std::thread::sleep(TICK);
        // Again, after the sleep: a cancel that landed while this member slept
        // has to beat a trigger that landed beside it. Read only at the top, a
        // `cancel` and a `trigger` arriving inside the same tick let the trigger
        // win, and the member spent a paid turn *after* an operator stopped it.
        if stopped() {
            break;
        }
        if live.load(Ordering::SeqCst) == 0 {
            break;
        }
        if schedule.resettable && reset.exists() {
            let _ = std::fs::remove_file(&reset);
            due = Instant::now() + Duration::from_secs(schedule.every);
            emitter.emit(EventKind::CronReset, Map::new());
            continue;
        }
        let fired = if trigger.exists() {
            let _ = std::fs::remove_file(&trigger);
            true
        } else {
            Instant::now() >= due
        };
        if !fired {
            continue;
        }
        emitter.emit(EventKind::CronFired, Map::new());
        let outcome = member::run(invocation, emitter, env, bounds, scratch);
        if outcome.is_success() {
            on_success();
        }
        last = Some(outcome);
        due = Instant::now() + Duration::from_secs(schedule.every);
    }
    last
}

/// One member's outcome, as the run record keeps it.
fn describe(outcome: &Outcome) -> MemberOutcome {
    match outcome {
        Outcome::Settled => MemberOutcome::Settled,
        Outcome::Incomplete => MemberOutcome::Incomplete,
        Outcome::Died(died) => MemberOutcome::Died(died.rule),
        Outcome::Unstartable(reason) => MemberOutcome::Unstartable(reason.clone()),
    }
}

/// Put the graph's `env` block into **this process's** environment.
///
/// The contract says a graph's `env` is "exported to every member process", and a
/// two-party member no longer *is* a process: it runs here, and the
/// `oneharness run` it starts inherits this environment. So the block has to be
/// on this process for that member to receive what the contract promises it —
/// `ONEHARNESS_BIN_<ID>`, a proxy, a credential path.
///
/// Once, here, before the first member thread starts, and never again: an
/// environment is process-wide, so a per-member write would race every sibling's
/// spawn. That is exactly why nothing per-member is exported at all — a member's
/// `mode` and its scratch stamp are stamped into the files `crate::invoke` writes
/// instead. A graph's block is safe to export because it is one block for the
/// whole graph, decided before anything runs.
///
/// `PROCESS_WIDE_HARNESS_ENV` is removed first for the reason that constant
/// gives, and re-added when the graph's own block asks for it.
///
/// # Errors
///
/// [`Error::InvalidConfig`] naming the pair that cannot be a variable.
/// `std::env::set_var` answers a name or value it cannot represent by
/// *panicking*, and a graph is external input, so every pair is checked against
/// the same rule [`crate::config::validate`] applies — here too, and not only
/// there, because a panic reached through a second door is still a document
/// taking the run down.
fn export(block: &BTreeMap<String, String>, env: &BTreeMap<String, String>) -> Result<(), Error> {
    let expanded: Vec<(&String, String)> = block
        .iter()
        .map(|(key, value)| (key, expand(value, env)))
        .collect();
    for (key, value) in &expanded {
        if key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0') {
            return Err(Error::InvalidConfig(format!(
                "env {key:?}: an environment variable name cannot be empty or contain '=' or a \
                 NUL, and neither a name nor a value may carry one — this pair is exported to \
                 every member"
            )));
        }
    }
    std::env::remove_var(invoke::PROCESS_WIDE_HARNESS_ENV);
    for (key, value) in expanded {
        std::env::set_var(key, value);
    }
    Ok(())
}

/// Expand `${VAR}` references in a graph's `env` value.
///
/// The contract shows `${HOME}`, so this is the shell-shaped spelling an author
/// expects. An unset variable expands to nothing rather than to its own name:
/// a path with an empty segment fails visibly where a literal `${HOME}` would be
/// created as a directory called that.
#[must_use]
pub fn expand(value: &str, env: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(open) = rest.find("${") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        match after.find('}') {
            Some(close) => {
                out.push_str(env.get(&after[..close]).map_or("", String::as_str));
                rest = &after[close + 1..];
            }
            None => {
                rest = &rest[open..];
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Write the run record, replacing whatever was there.
fn write_record(root: &Path, record: &Record) -> Result<(), Error> {
    let path = root.join(RECORD_FILE);
    let rendered = serde_json::to_string_pretty(record)
        .map_err(|err| Error::InvalidConfig(format!("cannot render the run record: {err}")))?;
    std::fs::write(&path, rendered)
        .map_err(|err| Error::InvalidConfig(format!("cannot write {}: {err}", path.display())))
}

/// The exit code a run failed to start with.
#[must_use]
pub fn refusal_exit() -> i32 {
    EXIT_INVALID_CONFIG
}

/// Writes each envelope to the caller's sink *and* the run's own events file.
///
/// The file is the run record's evidence and the sink is the view: `--detach`
/// and `history` read the first, a terminal reads the second, and neither can
/// be the only copy.
struct Tee {
    sink: Box<dyn std::io::Write + Send>,
    file: std::fs::File,
}

impl std::io::Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = self.file.write_all(buf);
        self.sink.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let _ = self.file.flush();
        self.sink.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(document: &str) -> GraphConfig {
        serde_norway::from_str(document).expect("a graph")
    }

    const TWO_MEMBERS: &str = concat!(
        "version: 1\nname: g\nmembers:\n",
        "  build:\n    kind: oneharness\n    oneharness_config: ./a.toml\n",
        "  report:\n    kind: oneharness\n    oneharness_config: ./a.toml\n    deps: [build]\n",
    );

    /// `deps` decides the waves: a dependant waits, and everything else runs at
    /// once.
    #[test]
    fn deps_decide_which_members_start_together() {
        let waves = ready_order(&graph(TWO_MEMBERS)).expect("an order");
        assert_eq!(
            waves,
            vec![vec!["build".to_string()], vec!["report".to_string()]]
        );
    }

    /// A dependency naming no member, and a cycle, both refuse the run — a graph
    /// that started nothing would otherwise settle as though it had.
    #[test]
    fn an_unsatisfiable_dependency_refuses_the_run() {
        let missing = graph(concat!(
            "version: 1\nname: g\nmembers:\n",
            "  a:\n    kind: oneharness\n    oneharness_config: ./a.toml\n    deps: [ghost]\n",
        ));
        let err = ready_order(&missing).unwrap_err();
        assert!(err.to_string().contains("no member called"), "{err}");

        let cyclic = graph(concat!(
            "version: 1\nname: g\nmembers:\n",
            "  a:\n    kind: oneharness\n    oneharness_config: ./a.toml\n    deps: [b]\n",
            "  b:\n    kind: oneharness\n    oneharness_config: ./a.toml\n    deps: [a]\n",
        ));
        let err = ready_order(&cyclic).unwrap_err();
        assert!(err.to_string().contains("form a cycle"), "{err}");
    }

    /// `--set` reaches the field it names, keeping the field's own type, and a
    /// path naming nothing refuses rather than being dropped.
    #[test]
    fn set_overrides_reach_the_field_they_name() {
        let mut document: Value = serde_json::json!({
            "version": 1,
            "name": "g",
            "members": {"worker": {
                "kind": "onejudge",
                "base_config": "base.yaml",
                "agent": {"oneharness_config": "agent.toml", "model": Value::Null, "stream": true},
                "judge": {"oneharness_config": "judge.toml"},
                "mode": "bypass",
                "max_turns": 4
            }}
        });
        let set = |raw: &str| parse_set(raw).expect("a parsed override");
        apply_overrides(
            &mut document,
            &[
                set("members.worker.agent.model=claude-opus-5"),
                set("members.worker.agent.stream=false"),
                set("members.worker.max_turns=9"),
            ],
        )
        .expect("every path exists");
        assert_eq!(
            document["members"]["worker"]["agent"]["model"],
            Value::from("claude-opus-5")
        );
        assert_eq!(
            document["members"]["worker"]["agent"]["stream"],
            Value::from(false)
        );
        assert_eq!(
            document["members"]["worker"]["max_turns"],
            Value::from(9u64)
        );

        apply_overrides(
            &mut document,
            &[
                set("members.worker.persona=engineer"),
                set("members.worker.task=true"),
            ],
        )
        .expect("schema-known optional string leaves can be absent");
        assert_eq!(document["members"]["worker"]["persona"], "engineer");
        assert_eq!(document["members"]["worker"]["task"], "true");

        for (path, expected) in [
            ("members.ghost.model", "this graph has no ghost"),
            ("members.worker.ghost", "schema has no field"),
        ] {
            let err = apply_overrides(&mut document, &[set(&format!("{path}=x"))]).unwrap_err();
            assert!(
                err.to_string().contains(path) && err.to_string().contains(expected),
                "{path}: {err}"
            );
        }
        let err =
            apply_overrides(&mut document, &[set("members.worker.max_turns=many")]).unwrap_err();
        assert!(err.to_string().contains("not a number"), "{err}");
        let err =
            apply_overrides(&mut document, &[set("members.worker.agent.stream=yes")]).unwrap_err();
        assert!(err.to_string().contains("not a boolean"), "{err}");

        let err =
            apply_overrides(&mut document, &[set("members.worker.schedule.every=3")]).unwrap_err();
        assert!(
            err.to_string().contains("members.worker.schedule.every"),
            "{err}"
        );

        let mut missing_required = document.clone();
        missing_required["members"]["worker"]
            .as_object_mut()
            .expect("member object")
            .remove("mode");
        let err = apply_overrides(&mut missing_required, &[set("members.worker.mode=bypass")])
            .unwrap_err();
        assert!(err.to_string().contains("members.worker.mode"), "{err}");
    }

    /// `--set` and `--label` say what they expected when they are given
    /// something else.
    #[test]
    fn a_malformed_set_or_label_says_what_it_expected() {
        assert!(parse_set("no-equals")
            .unwrap_err()
            .to_string()
            .contains("PATH=VALUE"));
        assert!(parse_set("=v")
            .unwrap_err()
            .to_string()
            .contains("path before `=` is empty"));
        assert_eq!(
            parse_set("a.b=c").unwrap(),
            Override {
                path: "a.b".into(),
                value: "c".into()
            }
        );
        assert!(parse_label("bare").unwrap_err().to_string().contains("k=v"));
        for bad in ["=v", "a b=v", "a-b=v"] {
            assert!(
                parse_label(bad)
                    .unwrap_err()
                    .to_string()
                    .contains("letters, digits, underscores, and dots"),
                "{bad}"
            );
        }
        // A run stamps `run_id`, `member`, and `persona` itself, so an operator
        // naming one would produce an envelope a consumer cannot tell apart.
        for reserved in RESERVED_LABELS {
            let err = parse_label(&format!("{reserved}=mine")).unwrap_err();
            assert!(
                err.to_string().contains("reserved label"),
                "{reserved}: {err}"
            );
        }
        let label = parse_label("tier=gate").unwrap();
        assert_eq!((label.key(), label.value()), ("tier", "gate"));
    }

    /// A graph's `env` block reaches this process's own environment, and a pair
    /// the platform cannot represent is refused rather than allowed to panic
    /// `set_var` — a graph is a document somebody wrote, not a reason to take
    /// the run down.
    #[test]
    fn an_env_pair_the_platform_cannot_represent_is_refused_rather_than_fatal() {
        let launching: BTreeMap<String, String> = BTreeMap::new();
        for (key, value) in [
            ("", "fine"),
            ("HAS=EQUALS", "fine"),
            ("HAS\0NUL", "fine"),
            ("FINE", "has\0nul"),
        ] {
            let block: BTreeMap<String, String> =
                [(key.to_string(), value.to_string())].into_iter().collect();
            let err = export(&block, &launching).unwrap_err();
            assert!(
                err.to_string().contains("exported to every member"),
                "{key:?}={value:?}: {err}"
            );
        }

        // And the same pairs are refused by `validate`, so `oneagentgraph
        // validate` answers for them without a run being started at all.
        for document in [
            "version: 1\nname: g\nenv:\n  \"HAS=EQUALS\": fine\nmembers:\n  a:\n    kind: oneharness\n    oneharness_config: ./a.toml\n",
            "version: 1\nname: g\nenv:\n  FINE: \"has\\0nul\"\nmembers:\n  a:\n    kind: oneharness\n    oneharness_config: ./a.toml\n",
        ] {
            let err = crate::config::validate(&graph(document)).unwrap_err();
            assert!(
                err.to_string().contains("exported to every member"),
                "{document}: {err}"
            );
        }
    }

    /// `${VAR}` expands from the launching environment; an unset one expands to
    /// nothing rather than to its own spelling.
    #[test]
    fn env_values_expand_their_references() {
        let env: BTreeMap<String, String> = [("HOME".to_string(), "/home/a".to_string())]
            .into_iter()
            .collect();
        assert_eq!(expand("${HOME}/.cache", &env), "/home/a/.cache");
        assert_eq!(expand("no references", &env), "no references");
        assert_eq!(expand("${NOTHING}/x", &env), "/x");
        assert_eq!(expand("${unterminated", &env), "${unterminated");
        assert_eq!(expand("${HOME}${HOME}", &env), "/home/a/home/a");
    }

    /// A run id sorts by time, carries the graph's name, and survives a name
    /// nobody could put in a path.
    #[test]
    fn a_run_id_is_readable_and_sortable() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let id = RunId::mint("Node Scope/2", at);
        assert!(
            id.as_str().starts_with("node-scope-2-1700000000000-"),
            "{id}"
        );
        let later = RunId::mint("Node Scope/2", at + Duration::from_millis(1));
        assert!(later > id, "{later} did not sort after {id}");

        // What the type is for: a minted id parses, and a record carrying a
        // traversal in this field is refused rather than joined onto the state
        // directory by whichever verb reads it back.
        assert_eq!(RunId::parse(id.as_str()).expect("a run id"), id);
        // The alphabet is exactly what `mint` emits — nothing it never produces
        // is a run id, however harmless it would be in a path.
        for hostile in ["../../etc", "a/b", "", "has space", "Node-Scope-1-1", "r_1"] {
            let err = RunId::parse(hostile).unwrap_err();
            assert!(err.to_string().contains("is not a run id"), "{err}");
        }
        let err = serde_json::from_str::<RunId>("\"../escape\"").unwrap_err();
        assert!(err.to_string().contains("is not a run id"), "{err}");
        assert_eq!(
            serde_json::to_string(&id).expect("json"),
            format!("\"{id}\""),
            "a run id must still serialize as the bare string a reader expects"
        );
    }

    /// Each outcome has one spelling in the record, and it round-trips — an
    /// existing reader of `record.json` parses exactly what it always did.
    #[test]
    fn each_outcome_has_one_spelling_that_round_trips() {
        let cases = [
            (describe(&Outcome::Settled), "settled"),
            (describe(&Outcome::Incomplete), "incomplete"),
            (
                describe(&Outcome::Died(crate::member::Death {
                    rule: crate::member::Rule::Activity,
                    payload: crate::event::MemberDied {
                        rule: "activity".into(),
                        cause: crate::event::Cause::Cancelled,
                        detail: String::new(),
                        truncated: false,
                        exit_code: None,
                        disposition: None,
                        stderr_tail: None,
                    },
                })),
                "died (activity)",
            ),
            (
                describe(&Outcome::Unstartable("no such".into())),
                "unstartable (no such)",
            ),
            (
                MemberOutcome::Skipped(
                    SkippedDeps::new(vec!["build".into(), "lint".into()]).expect("valid deps"),
                ),
                "skipped (build, lint)",
            ),
        ];
        for (outcome, expected) in cases {
            let rendered = serde_json::to_value(&outcome).expect("a record value");
            assert_eq!(rendered, Value::from(expected));
            assert_eq!(
                serde_json::from_value::<MemberOutcome>(rendered).expect("round-trips"),
                outcome
            );
        }
        // A record from a build that spelled an outcome differently is a record
        // this one cannot read, and says so rather than guessing.
        assert!(serde_json::from_value::<MemberOutcome>(Value::from("vanished")).is_err());
        for invalid in ["skipped ()", "skipped (../build)"] {
            assert!(serde_json::from_value::<MemberOutcome>(Value::from(invalid)).is_err());
        }
        assert_eq!(refusal_exit(), EXIT_INVALID_CONFIG);
    }
}
