//! `run --judge-only`: one graph member's judge side, alone, over an input.
//!
//! A repository that wants a piece of work reviewed — a plan before launch, a
//! correction to a running node — names the graph member whose reviewer it
//! wants, and this runs that member's **judge side only**: its whole `judge:`
//! panel, under its own harness routing, against the bar a dispatch of it would
//! apply — its persona, or one named in its place — through onejudge's own
//! judge-only mode ([`onejudge::cli::Plan::with_judge_input`]). No agent side
//! starts.
//!
//! Nothing about the member is re-derived here. The graph is read, overridden
//! and validated by the reader [`crate::run`] uses; the member is prepared by
//! [`crate::invoke::build`] exactly as a dispatch prepares it; and it runs on the
//! same in-process driver, watchdogs and stream as any two-party member
//! ([`crate::judge::run`]). What this module adds is the selection — one
//! `kind: onejudge` member, the rest of the graph left unlaunched — the
//! material, and the reading of the settle as a verdict on the bar:
//! [`Verdict::Pass`] (`0`), [`Verdict::Fail`] with the findings (`1`), or
//! [`Verdict::CouldNotJudge`] (`2`).
//!
//! # The bar fingerprint
//!
//! [`bar_fingerprint`] digests everything such a verdict depends on besides the
//! material: the member's judge configuration (its judge list, `judge_settings`
//! and each harness judge's oneharness config), each llmlint judge's own
//! `llmlint fingerprint`, and the persona the member resolves to, merged into its
//! base config. It runs nothing but those llmlint probes. A judge-only run
//! computes the same digest before it launches and publishes it on the member's
//! `member-settled` as `bar_fingerprint`, so a host can record what a pass was
//! judged against and later ask whether that bar has moved.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::config::{ConfigRef, GraphConfig, Member, OnejudgeMember, TaskText};
use crate::error::Error;
use crate::invoke::{Context, Invocation, Launch};
use crate::member::{Outcome, REPORT_FILE};
use crate::resolve::Resolver;
use crate::run::{Label, MemberName, Override};

/// A judge-only run passed its bar.
pub const EXIT_PASS: i32 = 0;

/// The judge side judged the material and found it short of the bar.
pub const EXIT_FAIL: i32 = 1;

/// The judge side could not judge the material: a judge that never answered,
/// failed, or answered nothing usable — or a request that could not be run.
pub const EXIT_COULD_NOT_JUDGE: i32 = 2;

/// The version of the material a [`BarFingerprint`] digests. Raised whenever
/// what is digested changes shape, so a digest recorded before the change reads
/// as a bar that moved rather than one that silently means something else.
const BAR_FORMAT: &str = "oneagentgraph.bar/1";

/// Which member's bar, in which graph — what [`bar_fingerprint`] reads and a
/// judge-only [`Request`] runs.
#[derive(Debug, Clone)]
pub struct Target {
    /// The graph config, by path or URL.
    pub graph: ConfigRef,
    /// The `kind: onejudge` member whose judge side is run.
    pub member: MemberName,
    /// The persona to judge under in place of the member's own: a persona
    /// **name** — looked up in the graph's catalog and the shipped roles exactly
    /// as a member's `persona:` is — or a path or URL to a persona file. A
    /// relative path is the caller's, so it is read against this process's
    /// working directory. `None` keeps the member's own.
    pub persona: Option<String>,
    /// `--set`-style overrides, applied to the graph as a run applies them.
    pub overrides: Vec<Override>,
    /// The directory the member works in, as `run --dir` gives it.
    pub dir: PathBuf,
}

/// What a judge-only run judges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A file, judged where it lies when it is under the member's worktree.
    File(PathBuf),
    /// Material held in memory — what `--input -` reads from stdin.
    Text(String),
}

/// One judge-only run.
#[derive(Debug, Clone)]
pub struct Request {
    /// The member, and the bar it judges against.
    pub target: Target,
    /// The material judged.
    pub input: Input,
    /// Files or directories every judge is named to read, replacing the bar's
    /// own `user.artifacts` as onejudge's `--artifact` does. A relative path is
    /// read against this process's working directory.
    pub artifacts: Vec<PathBuf>,
    /// The task the material answers; `None` takes the member's own `task`, else
    /// onejudge's judge-only default ([`onejudge::cli::JUDGE_ONLY_TASK`]).
    pub task: Option<String>,
    /// Extra labels stamped on every event.
    pub labels: Vec<Label>,
    /// Which envelopes reach the stream, as `run --event-filter` says.
    pub filter: Option<crate::event::EventFilter>,
    /// Where run state lives.
    pub state_dir: PathBuf,
    /// The `oneharness` binary.
    pub oneharness_bin: String,
}

/// One digest of everything a judge-only verdict's bar depends on — 64
/// lowercase hex characters of SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct BarFingerprint(String);

impl BarFingerprint {
    /// The digest, as printed and published.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BarFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the judge side made of the material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The judge side declared the bar met.
    Pass,
    /// The judge side judged the material and found it short.
    Fail {
        /// What each judge that did not pass said, then each boolean verdict
        /// that failed.
        findings: Findings,
    },
    /// No judgement was reached.
    CouldNotJudge {
        /// Why.
        reason: String,
    },
}

/// What a [`Verdict::Fail`] found: at least one finding, by construction — a
/// fail with nothing to act on is not one this module can produce, because it
/// is read as [`Verdict::CouldNotJudge`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Findings {
    first: String,
    rest: Vec<String>,
}

impl Findings {
    /// The findings, in order: `None` for an empty list, which is no fail.
    fn of(mut all: Vec<String>) -> Option<Self> {
        if all.is_empty() {
            return None;
        }
        let first = all.remove(0);
        Some(Self { first, rest: all })
    }

    /// Each finding, in the order the report holds them.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.first.as_str()).chain(self.rest.iter().map(String::as_str))
    }
}

impl Verdict {
    /// The exit status this verdict is: [`EXIT_PASS`], [`EXIT_FAIL`] or
    /// [`EXIT_COULD_NOT_JUDGE`].
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            Verdict::Pass => EXIT_PASS,
            Verdict::Fail { .. } => EXIT_FAIL,
            Verdict::CouldNotJudge { .. } => EXIT_COULD_NOT_JUDGE,
        }
    }
}

/// A finished judge-only run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judged {
    /// What the judge side made of the material.
    pub verdict: Verdict,
    /// The bar it was judged against — the digest [`bar_fingerprint`] prints for
    /// the same target, and the one the stream's `member-settled` carries.
    pub bar_fingerprint: BarFingerprint,
    /// The member's onejudge report, where the run stored it, when it settled
    /// on one.
    pub report_path: Option<PathBuf>,
}

/// The material a judge-only member is launched with — what
/// [`crate::invoke::JudgeLaunch::judge_only`] carries.
#[derive(Debug, Clone)]
pub struct JudgeOnly {
    /// What the judge side judges, as onejudge takes it.
    pub input: onejudge::JudgeInput,
    /// The `--artifact`s, absolute; empty keeps the bar's own.
    pub artifacts: Vec<String>,
    /// The digest of the bar this run judges against.
    pub bar_fingerprint: BarFingerprint,
}

/// What [`crate::run`] is told to judge, prepared before the graph is read so a
/// request that cannot be honoured is refused before anything is created.
pub(crate) struct Judging {
    pub(crate) member: MemberName,
    persona: Option<ConfigRef>,
    input: onejudge::JudgeInput,
    artifacts: Vec<String>,
}

/// Print one digest of `target`'s bar, running nothing but its llmlint judges'
/// `llmlint fingerprint`.
///
/// # Errors
///
/// [`Error::InvalidConfig`] for a graph that does not read or validate, a member
/// that is absent or has no judge side, a persona or config that cannot be
/// read, and an llmlint judge whose `fingerprint` fails.
pub fn bar_fingerprint(
    target: &Target,
    env: &BTreeMap<String, String>,
) -> Result<BarFingerprint, Error> {
    let mut resolver = Resolver::new();
    let document = resolver.resolve(&target.graph, None)?.clone();
    let mut graph = crate::run::read_graph(&document.content, &target.graph.0, |parsed| {
        crate::run::apply_background_env(parsed, env)?;
        crate::run::apply_overrides(parsed, &target.overrides)
    })?;
    crate::config::validate(&graph)?;
    let persona = persona_ref(target.persona.as_deref())?;
    let member = select(&mut graph, target.member.as_str(), persona)?.clone();
    let scratch = std::env::temp_dir();
    let context = Context {
        dir: &target.dir,
        scratch: &scratch,
        graph_dir: document.base_dir.as_deref(),
        personas: graph.personas.as_deref(),
        task: None,
        task_text: TaskText::under(graph.version),
        session: "bar-fingerprint",
        oneharness_bin: "",
        background: false,
    };
    fingerprint(&member, &context, &mut resolver, &graph_env(&graph, env))
}

/// Run `request`'s member's judge side over its input, streaming envelopes to
/// `sink` as a run does.
///
/// # Errors
///
/// [`Error::InvalidConfig`] for everything that stops the judgement starting —
/// an input that cannot be read, a member that is absent or has no judge side,
/// and every refusal [`crate::run::run`] makes. A judgement that starts and
/// cannot finish is a [`Verdict::CouldNotJudge`], not an error.
pub fn run(
    request: &Request,
    sink: Box<dyn std::io::Write + Send>,
    env: &BTreeMap<String, String>,
) -> Result<Judged, Error> {
    let input = match &request.input {
        Input::File(path) => onejudge::JudgeInput::from_path(path).map_err(|err| {
            Error::InvalidConfig(format!("cannot read --input {}: {err}", path.display()))
        })?,
        Input::Text(text) => onejudge::JudgeInput::text(text.clone()),
    };
    let artifacts = request
        .artifacts
        .iter()
        .map(|path| {
            std::path::absolute(path)
                .map(|absolute| absolute.display().to_string())
                .map_err(|err| {
                    Error::InvalidConfig(format!(
                        "cannot resolve --artifact {}: {err}",
                        path.display()
                    ))
                })
        })
        .collect::<Result<_, _>>()?;
    let judging = Judging {
        member: request.target.member.clone(),
        persona: persona_ref(request.target.persona.as_deref())?,
        input,
        artifacts,
    };
    let run = crate::run::Request {
        graph: request.target.graph.clone(),
        task: request
            .task
            .clone()
            .or_else(|| Some(onejudge::cli::JUDGE_ONLY_TASK.to_string())),
        dir: request.target.dir.clone(),
        labels: request.labels.clone(),
        overrides: request.target.overrides.clone(),
        filter: request.filter.clone(),
        state_dir: request.state_dir.clone(),
        oneharness_bin: request.oneharness_bin.clone(),
        follow_events: None,
    };
    crate::run::judge(&run, sink, env, &judging)
}

/// A `--persona` as the ref a member's `persona:` would carry: a name stays a
/// name, a URL stays a URL, and a path is made absolute against the caller's
/// working directory — a relative ref in a graph means the graph's directory,
/// and the caller who typed this one meant their own.
fn persona_ref(given: Option<&str>) -> Result<Option<ConfigRef>, Error> {
    let Some(given) = given else {
        return Ok(None);
    };
    if crate::persona::is_persona_name(given) || given.starts_with("https://") {
        return Ok(Some(ConfigRef(given.to_string())));
    }
    let absolute = std::path::absolute(given)
        .map_err(|err| Error::InvalidConfig(format!("cannot resolve --persona {given}: {err}")))?;
    Ok(Some(ConfigRef(absolute.display().to_string())))
}

/// Narrow `graph` to the one member being judged, which runs once, now: its
/// dependencies, schedule, scheduled notes and background flag belong to a
/// dispatch of the whole graph, not to one judgement of one member.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when the graph has no such member, or it has no
/// judge side to run.
fn select<'a>(
    graph: &'a mut GraphConfig,
    name: &str,
    persona: Option<ConfigRef>,
) -> Result<&'a OnejudgeMember, Error> {
    let Some(member) = graph.members.remove(name) else {
        return Err(Error::InvalidConfig(format!(
            "--member {name:?}: graph {:?} has no such member (it has: {})",
            graph.name,
            graph.members.keys().cloned().collect::<Vec<_>>().join(", ")
        )));
    };
    let Member::Onejudge(mut member) = member else {
        return Err(Error::InvalidConfig(format!(
            "--member {name:?} is a single-sided member (kind: oneharness): it has no judge side \
             to run judge-only"
        )));
    };
    member.deps.clear();
    member.schedule = None;
    member.scheduled_notes.clear();
    member.background = None;
    if persona.is_some() {
        member.persona = persona;
    }
    graph.members.clear();
    graph
        .members
        .insert(name.to_string(), Member::Onejudge(member));
    match &graph.members[name] {
        Member::Onejudge(member) => Ok(member),
        Member::Oneharness(_) => unreachable!("inserted as a two-party member above"),
    }
}

/// [`select`] for a run: the one place [`crate::run`] narrows a graph.
pub(crate) fn narrow(graph: &mut GraphConfig, judging: &Judging) -> Result<(), Error> {
    select(graph, judging.member.as_str(), judging.persona.clone()).map(|_| ())
}

/// Arm `invocation` — the judged member's, as [`crate::invoke::build`] made it
/// — with the material and the bar it is judged against, returning that bar.
pub(crate) fn arm(
    invocation: &mut Invocation,
    member: &Member,
    context: &Context<'_>,
    resolver: &mut Resolver,
    env: &BTreeMap<String, String>,
    judging: &Judging,
) -> Result<BarFingerprint, Error> {
    let (Member::Onejudge(member), Launch::Judge(launch)) = (member, &mut invocation.launch) else {
        unreachable!("`narrow` keeps only a two-party member, and `build` launches one as such")
    };
    let bar = fingerprint(member, context, resolver, env)?;
    launch.judge_only = Some(JudgeOnly {
        input: judging.input.clone(),
        artifacts: judging.artifacts.clone(),
        bar_fingerprint: bar.clone(),
    });
    Ok(bar)
}

/// Read the judged member's settle as a verdict on its bar.
///
/// A settle that met the bar is a pass. One that did not is a fail only when the
/// judge side said *why* — a judge that did not pass, or a boolean verdict that
/// failed — because a fail without findings gives its caller nothing to act on;
/// a settle onejudge records as having no decision at all (`settled_reason`: an
/// answer that never parsed, or no instruction even asked again) could not
/// judge, and neither could a member that died or never started.
pub(crate) fn judged(outcome: Option<&Outcome>, scratch: &Path, bar: BarFingerprint) -> Judged {
    let path = scratch.join(REPORT_FILE);
    // onejudge's own `Report`, read back through its own type from the copy
    // the settle stored: what a decision is called, and where a verdict's value
    // lives, are onejudge's to say.
    let report: Option<onejudge::Report> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let report_path = report.as_ref().map(|_| path);
    let verdict = match outcome {
        Some(Outcome::Settled) => Verdict::Pass,
        Some(Outcome::Incomplete) => incomplete(report.as_ref()),
        Some(Outcome::Died(death)) => Verdict::CouldNotJudge {
            reason: format!(
                "the judge side died ({}): {}",
                death.rule.as_str(),
                death.payload.detail
            ),
        },
        Some(Outcome::Unstartable(why)) => Verdict::CouldNotJudge {
            reason: format!("the judge side could not start: {why}"),
        },
        // llmlint: ignore-block[changed_behavior_has_e2e] no input reaches this
        // arm: `narrow` leaves the judged member alone in the graph with no
        // dependency and no schedule, so the first wave always runs it and records
        // its outcome. It exists so the absence a bug elsewhere would cause is a
        // could-not-judge, never a pass.
        None => Verdict::CouldNotJudge {
            reason: "the judged member never ran".to_string(),
        },
        // llmlint: ignore-end[changed_behavior_has_e2e]
    };
    Judged {
        verdict,
        bar_fingerprint: bar,
        report_path,
    }
}

/// The verdict on a report that did not meet its bar — see [`judged`].
fn incomplete(report: Option<&onejudge::Report>) -> Verdict {
    // llmlint: ignore-block[changed_behavior_has_e2e] an incomplete settle is one
    // `settle_report` wrote a report for moments before, so this arm is reached
    // only when the host refused that write or the read back — a host failure no
    // graph, input or flag asks for. Its direction is the safe one: no report is
    // no judgement, never a fail with nothing in it.
    let Some(report) = report else {
        return Verdict::CouldNotJudge {
            reason: "the judge side settled without a report".to_string(),
        };
    };
    // llmlint: ignore-end[changed_behavior_has_e2e]
    if let Some(why) = &report.settled_reason {
        return Verdict::CouldNotJudge {
            reason: why.clone(),
        };
    }
    let judges = report
        .judge_decisions
        .iter()
        .flat_map(|turn| &turn.decisions)
        .filter(|decision| decision.decision != onejudge::Decision::Done)
        .map(|decision| {
            format!(
                "{} ({}) {}: {}",
                decision.judge,
                decision.kind,
                decision.decision.as_str(),
                decision.reason.trim()
            )
        });
    let verdicts = report
        .verdicts
        .iter()
        .filter(|named| named.verdict.value == onejudge::JudgeValue::Bool(false))
        .map(|named| format!("{}: {}", named.criterion, named.verdict.reason.trim()));
    match Findings::of(judges.chain(verdicts).collect()) {
        Some(findings) => Verdict::Fail { findings },
        // llmlint: ignore-block[changed_behavior_has_e2e] onejudge records a
        // judge-only run's bar as a boolean verdict whose value is whether it was
        // met, so a settle that did not meet it and carries no `settled_reason`
        // always holds that `false` verdict — a finding. This arm is what keeps
        // `Findings` non-empty should a later onejudge stop recording it, and it
        // reads that change as no judgement rather than an empty fail.
        None => Verdict::CouldNotJudge {
            reason: "the judge side neither passed the material nor said what it lacks".to_string(),
        },
        // llmlint: ignore-end[changed_behavior_has_e2e]
    }
}

/// The environment a run gives its members: the graph's `env:` block over this
/// process's, with the process-wide harness selection removed first — the same
/// composition [`crate::run`] exports, given here to an llmlint probe alone.
fn graph_env(graph: &GraphConfig, env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut composed = env.clone();
    composed.remove(crate::invoke::PROCESS_WIDE_HARNESS_ENV);
    for (key, value) in &graph.env {
        composed.insert(key.clone(), crate::run::expand(value, env));
    }
    composed
}

/// `member`'s bar, digested — see the module docs for what goes in.
pub(crate) fn fingerprint(
    member: &OnejudgeMember,
    context: &Context<'_>,
    resolver: &mut Resolver,
    env: &BTreeMap<String, String>,
) -> Result<BarFingerprint, Error> {
    let bar = crate::invoke::bar(member, context, resolver)?;
    let worktree = crate::invoke::worktree(member, context);
    let llmlint = bar
        .llmlint
        .iter()
        .map(|probe| llmlint_fingerprint(probe, &worktree, env))
        .collect::<Result<Vec<_>, _>>()?;
    let material = serde_json::json!({
        "format": BAR_FORMAT,
        "member": bar.document,
        "llmlint": llmlint,
    });
    let canonical = serde_json::to_vec(&canonicalize(material)).map_err(|err| {
        Error::InvalidConfig(format!("cannot serialize the bar to digest it: {err}"))
    })?;
    let digest = Sha256::digest(&canonical);
    Ok(BarFingerprint(
        digest.iter().map(|byte| format!("{byte:02x}")).collect(),
    ))
}

/// `value` with every mapping's keys sorted, so the digest does not depend on
/// the order a document happened to write them in.
fn canonicalize(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> = map
                .into_iter()
                .map(|(key, value)| (key, canonicalize(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonicalize).collect()),
        other => other,
    }
}

/// The options `<bin> fingerprint` takes, as its own `--help` lists them —
/// each spelling paired with whether it takes a value. llmlint is the one
/// source of which options decide a verdict, so this asks it rather than
/// keeping a copy of its command line here: an option a later llmlint adds is
/// forwarded the day it ships, and one it drops is never passed to a release
/// that would refuse it.
fn fingerprint_options(
    bin: &str,
    env: &BTreeMap<String, String>,
) -> Result<Vec<(String, bool)>, String> {
    let output = std::process::Command::new(bin)
        .args(["fingerprint", "--help"])
        .env_clear()
        .envs(env)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|err| format!("could not run `{bin} fingerprint --help`: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "`{bin} fingerprint --help` failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let options = options_listed(&String::from_utf8_lossy(&output.stdout));
    // A help that lists nothing is one this reader no longer understands, not
    // a command that takes no options: forwarding nothing would digest a bar
    // without the options that decide it.
    if options.is_empty() {
        return Err(format!(
            "`{bin} fingerprint --help` listed no option this can read, so which of the \
             judge's args decide its verdict is unknown"
        ));
    }
    Ok(options)
}

/// Every option a clap `--help` lists: each line that opens with `-` names
/// one or two spellings, and takes a value when a `<VALUE>` follows them.
fn options_listed(help: &str) -> Vec<(String, bool)> {
    let mut options = Vec::new();
    for line in help.lines().map(str::trim_start) {
        if !line.starts_with('-') {
            continue;
        }
        let head = line.split("  ").next().unwrap_or(line);
        let takes_value = head.contains('<');
        for word in head.split_whitespace() {
            let spelling = word.trim_end_matches(',');
            if spelling.starts_with('-') && spelling != "-h" && spelling != "--help" {
                options.push((spelling.to_string(), takes_value));
            }
        }
    }
    options
}

/// The part of a judge's `args` its `fingerprint` takes, by `options` — see
/// [`fingerprint_options`]. The rest (`--label`, `--diff`, …) shape a run
/// rather than its bar; they are in the digest as the graph wrote them.
fn fingerprint_args(args: &[String], options: &[(String, bool)]) -> Vec<String> {
    let mut kept = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, _)) if flag.starts_with("--") => (flag, true),
            _ => (arg.as_str(), false),
        };
        let Some((_, takes_value)) = options.iter().find(|(name, _)| name == flag) else {
            continue;
        };
        kept.push(arg.clone());
        if *takes_value && !inline {
            if let Some(value) = args.next() {
                kept.push(value.clone());
            }
        }
    }
    kept
}

/// One llmlint judge's own `<bin> fingerprint`, run as onejudge would run its
/// `lint`: the same config, from the member's worktree, in the member's
/// environment.
fn llmlint_fingerprint(
    probe: &crate::invoke::LlmlintBar,
    worktree: &Path,
    env: &BTreeMap<String, String>,
) -> Result<String, Error> {
    let refused = |why: String| {
        Error::InvalidConfig(format!(
            "judge entry {}: `{} fingerprint` {why}",
            probe.entry, probe.bin
        ))
    };
    let options = if probe.args.is_empty() {
        Vec::new()
    } else {
        fingerprint_options(&probe.bin, env)
            .map_err(|why| Error::InvalidConfig(format!("judge entry {}: {why}", probe.entry)))?
    };
    let mut command = std::process::Command::new(&probe.bin);
    command.arg("fingerprint");
    if let Some(config) = &probe.config {
        command.arg("-c").arg(config);
    }
    command
        .arg("--cwd")
        .arg(worktree)
        .args(fingerprint_args(&probe.args, &options))
        .env_clear()
        .envs(env)
        .stdin(std::process::Stdio::null());
    let output = command
        .output()
        .map_err(|err| refused(format!("could not run: {err}")))?;
    if !output.status.success() {
        return Err(refused(format!(
            "failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let digest = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if digest.len() != 64 || !digest.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
        return Err(refused(format!(
            "printed {digest:?}, which is not one SHA-256 digest"
        )));
    }
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The subset of llmlint's own `fingerprint --help` the llmlint double
    /// answers with, which `tests/judged-lint/lint_llm_diff.rs` holds to the
    /// installed llmlint.
    const HELP: &str = include_str!("bin/llmlint-fingerprint-help.txt");

    #[test]
    fn the_options_forwarded_are_the_ones_fingerprint_lists() {
        let options = options_listed(HELP);
        assert_eq!(
            options,
            [
                ("-c".to_string(), true),
                ("--config".to_string(), true),
                ("--model".to_string(), true),
                ("--prompt-template".to_string(), true),
                ("--rationales".to_string(), false),
                ("--cwd".to_string(), true),
            ]
        );
        let args: Vec<String> = [
            "--label",
            "k=v",
            "--model",
            "m",
            "--no-ignore-check",
            "--config=x.yml",
            "--rationales",
            "-c",
            "extra.yml",
        ]
        .map(str::to_string)
        .to_vec();
        assert_eq!(
            fingerprint_args(&args, &options),
            [
                "--model",
                "m",
                "--config=x.yml",
                "--rationales",
                "-c",
                "extra.yml"
            ]
        );
    }

    #[test]
    fn a_digest_does_not_depend_on_key_order() {
        let one = serde_json::json!({"b": 1, "a": {"y": 2, "x": [ {"q": 1, "p": 2} ]}});
        let two = serde_json::json!({"a": {"x": [ {"p": 2, "q": 1} ], "y": 2}, "b": 1});
        assert_eq!(
            serde_json::to_string(&canonicalize(one)).unwrap(),
            serde_json::to_string(&canonicalize(two)).unwrap()
        );
    }

    #[test]
    fn a_fail_is_never_empty() {
        assert!(Findings::of(Vec::new()).is_none());
        let findings = Findings::of(vec!["a".into(), "b".into()]).expect("two findings");
        assert_eq!(findings.iter().collect::<Vec<_>>(), ["a", "b"]);
        assert!(matches!(incomplete(None), Verdict::CouldNotJudge { .. }));
    }
}
