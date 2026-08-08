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

/// How often a run notices a `trigger` / `reset-timer` signal, and re-checks a
/// schedule's clock.
const TICK: Duration = Duration::from_millis(100);

/// One run, as `history` keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// The run's id.
    pub run_id: String,
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
    /// Every config ref the run read, content-addressed, so replay and audit
    /// never depend on a URL staying stable.
    #[serde(default)]
    pub refs: Vec<ResolvedRef>,
    /// Where the run's merged NDJSON is.
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
}

impl From<MemberOutcome> for String {
    fn from(outcome: MemberOutcome) -> Self {
        match outcome {
            MemberOutcome::Settled => "settled".into(),
            MemberOutcome::Incomplete => "incomplete".into(),
            MemberOutcome::Died(rule) => format!("died ({})", rule.as_str()),
            MemberOutcome::Unstartable(reason) => format!("unstartable ({reason})"),
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
    pub labels: Vec<(String, String)>,
    /// `members.worker.agent.model=NAME`-style overrides, already parsed.
    pub overrides: Vec<(String, String)>,
    /// Where run state lives.
    pub state_dir: PathBuf,
    /// The `onejudge` binary.
    pub onejudge_bin: String,
    /// The `oneharness` binary.
    pub oneharness_bin: String,
}

/// A run that has started: what a caller needs to watch or cancel it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Started {
    /// The run's id.
    pub run_id: String,
    /// Where its merged NDJSON is being written.
    pub events_path: String,
    /// The process producing it.
    pub pid: u32,
}

/// A run id: sortable, unique on one host, and readable in a directory listing.
#[must_use]
pub fn new_run_id(name: &str, at: SystemTime) -> String {
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
    format!(
        "{}-{}-{}",
        slug.trim_matches('-'),
        unix_millis(at),
        std::process::id()
    )
}

/// Parse one `--set members.worker.agent.model=NAME` override.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when the argument carries no `=`, or an empty path.
pub fn parse_set(raw: &str) -> Result<(String, String), Error> {
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
    Ok((path.to_string(), value.to_string()))
}

/// Parse one `--label k=v`.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when the argument carries no `=`, or an empty key.
pub fn parse_label(raw: &str) -> Result<(String, String), Error> {
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
    Ok((key.to_string(), value.to_string()))
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
/// [`Error::InvalidConfig`] when a path names nothing the document has.
pub fn apply_overrides(document: &mut Value, overrides: &[(String, String)]) -> Result<(), Error> {
    for (path, value) in overrides {
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
        let slot = cursor
            .as_object_mut()
            .and_then(|map| map.get_mut(*last))
            .ok_or_else(|| {
                Error::InvalidConfig(format!("--set {path}=…: this graph has no {last}"))
            })?;
        *slot = match slot {
            Value::Number(_) => value.parse::<u64>().map(Value::from).map_err(|_| {
                Error::InvalidConfig(format!("--set {path}={value:?}: not a number"))
            })?,
            Value::Bool(_) => value.parse::<bool>().map(Value::from).map_err(|_| {
                Error::InvalidConfig(format!("--set {path}={value:?}: not a boolean"))
            })?,
            _ => Value::String(value.clone()),
        };
    }
    Ok(())
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
            Member::Onejudge(_) => &[],
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

    let run_id = new_run_id(&graph.name, SystemTime::now());
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
    for (key, value) in &graph.env {
        member_env.insert(key.clone(), expand(value, env));
    }
    let bounds = Bounds::from_env(&member_env).map_err(Error::InvalidConfig)?;

    let file = std::fs::File::create(&events_path).map_err(|err| {
        Error::InvalidConfig(format!("cannot create {}: {err}", events_path.display()))
    })?;
    let emitter = Emitter::new(run_id.clone(), Box::new(Tee { sink, file })).with_labels(
        crate::event::Labels {
            run_id: Some(run_id.clone()),
            extra: request
                .labels
                .iter()
                .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                .collect(),
            ..crate::event::Labels::default()
        },
    );

    let mut record = Record {
        run_id: run_id.clone(),
        graph: request.graph.0.clone(),
        name: graph.name.clone(),
        started_ms: unix_millis(SystemTime::now()),
        finished_ms: None,
        exit_code: None,
        members: BTreeMap::new(),
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
            onejudge_bin: &request.onejudge_bin,
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

    let mut failed = false;
    for wave in waves {
        let outcomes = run_wave(
            &wave,
            &graph,
            &invocations,
            &emitter,
            &member_env,
            bounds,
            &root,
        );
        for (name, outcome) in outcomes {
            record.members.insert(name, describe(&outcome));
            failed |= !outcome.is_success();
        }
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

/// Run one wave of members concurrently, and every cron member's later firings.
fn run_wave(
    wave: &[String],
    graph: &GraphConfig,
    invocations: &BTreeMap<String, (Invocation, PathBuf)>,
    emitter: &Emitter,
    env: &BTreeMap<String, String>,
    bounds: Bounds,
    root: &Path,
) -> Vec<(String, Outcome)> {
    let (tx, rx) = mpsc::channel();
    let mut running = 0;
    for name in wave {
        let Some((invocation, scratch)) = invocations.get(name) else {
            continue;
        };
        let schedule = match graph.members.get(name) {
            Some(Member::Oneharness(member)) => member.schedule,
            _ => None,
        };
        let member_emitter = emitter.with_labels(member::labels(
            emitter.stream(),
            name,
            invocation.persona.as_deref(),
        ));
        let (name, invocation, scratch, env, tx, signals) = (
            name.clone(),
            invocation.clone(),
            scratch.clone(),
            env.clone(),
            tx.clone(),
            root.join(SIGNAL_DIR),
        );
        running += 1;
        std::thread::spawn(move || {
            let mut outcome = member::run(&invocation, &member_emitter, &env, bounds, &scratch);
            if let Some(schedule) = schedule {
                outcome = cron(
                    &schedule,
                    &name,
                    &invocation,
                    &member_emitter,
                    &env,
                    bounds,
                    &scratch,
                    &signals,
                    outcome,
                );
            }
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
// another: the schedule, the member's name and invocation, where its events go,
// its environment and bounds, its scratch, the signal directory, and the outcome
// so far. Bundling them into a struct would name the same nine values twice.
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
    first: Outcome,
) -> Outcome {
    let stop = signals.join("stop");
    let trigger = signals.join(format!("{name}.trigger"));
    let reset = signals.join(format!("{name}.reset"));
    let mut last = first;
    let mut due = Instant::now() + Duration::from_secs(schedule.every);
    while !stop.exists() {
        std::thread::sleep(TICK);
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
        last = member::run(invocation, emitter, env, bounds, scratch);
        due = Instant::now() + Duration::from_secs(schedule.every);
    }
    last
}

/// One member's outcome, as the run record keeps it.
fn describe(outcome: &Outcome) -> MemberOutcome {
    match outcome {
        Outcome::Settled { completed: true } => MemberOutcome::Settled,
        Outcome::Settled { completed: false } => MemberOutcome::Incomplete,
        Outcome::Died(died) => MemberOutcome::Died(died.rule),
        Outcome::Unstartable(reason) => MemberOutcome::Unstartable(reason.clone()),
    }
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
            "members": {"worker": {"agent": {"model": Value::Null, "stream": true}, "max_turns": 4}}
        });
        apply_overrides(
            &mut document,
            &[
                ("members.worker.agent.model".into(), "claude-opus-5".into()),
                ("members.worker.agent.stream".into(), "false".into()),
                ("members.worker.max_turns".into(), "9".into()),
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

        for (path, expected) in [
            ("members.ghost.model", "this graph has no ghost"),
            ("members.worker.ghost", "this graph has no ghost"),
        ] {
            let err = apply_overrides(&mut document, &[(path.into(), "x".into())]).unwrap_err();
            assert!(err.to_string().contains(expected), "{path}: {err}");
        }
        let err = apply_overrides(
            &mut document,
            &[("members.worker.max_turns".into(), "many".into())],
        )
        .unwrap_err();
        assert!(err.to_string().contains("not a number"), "{err}");
        let err = apply_overrides(
            &mut document,
            &[("members.worker.agent.stream".into(), "yes".into())],
        )
        .unwrap_err();
        assert!(err.to_string().contains("not a boolean"), "{err}");
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
        assert_eq!(parse_set("a.b=c").unwrap(), ("a.b".into(), "c".into()));
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
        assert_eq!(
            parse_label("tier=gate").unwrap(),
            ("tier".into(), "gate".into())
        );
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
        let id = new_run_id("Node Scope/2", at);
        assert!(id.starts_with("node-scope-2-1700000000000-"), "{id}");
        let later = new_run_id("Node Scope/2", at + Duration::from_millis(1));
        assert!(later > id, "{later} did not sort after {id}");
    }

    /// Each outcome has one spelling in the record, and it round-trips — an
    /// existing reader of `record.json` parses exactly what it always did.
    #[test]
    fn each_outcome_has_one_spelling_that_round_trips() {
        let cases = [
            (describe(&Outcome::Settled { completed: true }), "settled"),
            (
                describe(&Outcome::Settled { completed: false }),
                "incomplete",
            ),
            (
                describe(&Outcome::Died(crate::member::Death {
                    rule: crate::member::Rule::Activity,
                    payload: crate::event::MemberDied {
                        rule: "activity".into(),
                        exit_code: None,
                        disposition: crate::event::Disposition::Signaled,
                        stderr_tail: String::new(),
                        truncated: false,
                    },
                })),
                "died (activity)",
            ),
            (
                describe(&Outcome::Unstartable("no such".into())),
                "unstartable (no such)",
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
        assert_eq!(refusal_exit(), EXIT_INVALID_CONFIG);
    }
}
