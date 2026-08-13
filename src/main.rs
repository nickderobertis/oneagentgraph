//! The `oneagentgraph` binary.
//!
//! Every verb `docs/contract.md` lists, and the exit codes it assigns them: `0`
//! every member settled successfully, `1` a member failed or died, `2` invalid
//! config — plus `3`, which only `interrupt` answers with and which is a *fact*
//! rather than a failure (there was no controllable turn in flight). Nothing here
//! does the work — each verb parses its arguments, resolves where the run state
//! lives, and hands off to the library.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use oneagentgraph::cli::{
    CancelArgs, Cli, Command, HistoryArgs, HistoryCommand, InterruptArgs, MemberArgs, OutputFormat,
    PersonaArgs, PersonaCommand, RunArgs, SmokeArgs, SweepArgs, ValidateArgs,
};
use oneagentgraph::config::GraphConfig;
use oneagentgraph::control::{Delivery, Turn};
use oneagentgraph::error::{
    Error, EXIT_INVALID_CONFIG, EXIT_MEMBER_FAILED, EXIT_NO_CONTROLLABLE_TURN, EXIT_SUCCESS,
};
use oneagentgraph::event::{Emitter, EventKind, Labels, TurnInterrupted};
use oneagentgraph::persona::{Persona, PERSONA_TEMPLATE};
use oneagentgraph::render::Text;
use oneagentgraph::resolve::Resolver;
use oneagentgraph::scratch::{Owned, SCRATCH_PREFIX};
use oneagentgraph::{config, health, history, run, smoke, sweep};

/// Where run state lives unless the environment says otherwise.
const STATE_DIR_ENV: &str = "ONEAGENTGRAPH_STATE_DIR";

/// The `oneharness` binary a run drives, overridable so a test — or a host with
/// a pinned install — can name its own.
///
/// There is no companion for `onejudge`: `docs/contract.md` runs that one through
/// its library, so no build of this crate has a `onejudge` binary to point at.
const ONEHARNESS_BIN_ENV: &str = "ONEAGENTGRAPH_ONEHARNESS_BIN";

fn main() -> ExitCode {
    let cli = Cli::parse();
    // `vars()` panics on an environment this process did not choose: one
    // variable that is not UTF-8 would take down every verb, including the ones
    // that never read the environment. Every consumer here wants a `String`, so
    // a name or value that is not one is a variable this crate cannot act on
    // and drops, rather than a reason to abort.
    let env: BTreeMap<String, String> = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect();
    match dispatch(cli.command, &env) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            eprintln!("oneagentgraph: {err}");
            ExitCode::from(u8::try_from(exit_for(&err)).unwrap_or(1))
        }
    }
}

/// The exit code one failure carries.
fn exit_for(err: &Error) -> i32 {
    match err {
        Error::InvalidConfig(_) => EXIT_INVALID_CONFIG,
        Error::MemberFailed { .. } => EXIT_MEMBER_FAILED,
        // `Error` is `#[non_exhaustive]`, so a variant added later still exits
        // with a code rather than failing to compile this dispatch — and it
        // exits `1`, the contract's "a member failed or died", because a failure
        // this build cannot name is not one it can call an invalid config.
        _ => EXIT_MEMBER_FAILED,
    }
}

/// Run one command, returning the code it exits with.
fn dispatch(command: Command, env: &BTreeMap<String, String>) -> Result<i32, Error> {
    match command {
        Command::Run(args) => run_graph(args, env),
        Command::Validate(args) => validate(&args, env),
        Command::Trigger(args) => signal(&args, env, Signal::Trigger),
        Command::ResetTimer(args) => signal(&args, env, Signal::Reset),
        Command::Cancel(args) => cancel(&args, env),
        Command::Interrupt(args) => interrupt(&args, env),
        Command::History(args) => show_history(&args, env),
        Command::Health => {
            let report = health::read()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&report).unwrap_or_default()
            );
            Ok(EXIT_SUCCESS)
        }
        Command::Smoke(args) => run_smoke(&args, env),
        Command::Sweep(args) => sweep_scratch(&args, env),
        Command::Persona(args) => persona(&args),
    }
}

/// The task this invocation names, from whichever flag carries it.
///
/// One reader for both the foreground run and the `--detach` preflight: the
/// detached child is handed these same flags, so a preflight that resolved the
/// task differently would be checking a different invocation than the one it
/// goes on to launch.
fn requested_task(args: &RunArgs) -> Result<Option<String>, Error> {
    match (&args.task, &args.task_file) {
        (Some(_), Some(_)) => Err(Error::InvalidConfig(
            "--task and --task-file both name the task; give exactly one".into(),
        )),
        (Some(text), None) => Ok(Some(text.clone())),
        (None, Some(path)) => Ok(Some(std::fs::read_to_string(path).map_err(|err| {
            Error::InvalidConfig(format!("cannot read --task-file {}: {err}", path.display()))
        })?)),
        (None, None) => Ok(None),
    }
}

/// `oneagentgraph run`.
fn run_graph(args: RunArgs, env: &BTreeMap<String, String>) -> Result<i32, Error> {
    let task = requested_task(&args)?;
    let request = run::Request {
        graph: config::ConfigRef(args.graph.clone()),
        task,
        dir: args.dir.clone().unwrap_or_else(|| PathBuf::from(".")),
        labels: args
            .label
            .iter()
            .map(|raw| run::parse_label(raw))
            .collect::<Result<Vec<_>, _>>()?,
        overrides: args
            .set
            .iter()
            .map(|raw| run::parse_set(raw))
            .collect::<Result<Vec<_>, _>>()?,
        state_dir: state_dir(env),
        oneharness_bin: oneharness_bin(env),
    };
    if args.detach {
        return detach(&args, env);
    }
    let sink: Box<dyn std::io::Write + Send> = match args.output {
        OutputFormat::Json => Box::new(std::io::stdout()),
        OutputFormat::Text => Box::new(Text::new(std::io::stdout())),
    };
    run::run(&request, sink, env)
}

/// `oneagentgraph run --detach`: relaunch this same binary without `--detach`,
/// print `{run_id, events_path, pid}`, and exit 0.
///
/// The child is the run; this process only reports where to watch it. It is
/// spawned with its stream discarded rather than inherited, because the caller
/// has been handed a path to the same events and a terminal that closes must not
/// take the run with it.
fn detach(args: &RunArgs, env: &BTreeMap<String, String>) -> Result<i32, Error> {
    // Before anything is spawned. The child writes its record and *then* builds
    // every member's invocation, so a graph that cannot run — an unpairable
    // model, a persona that will not load — produces a record and dies. Waiting
    // only for a record to appear would report that as a started run: `{run_id,
    // …}` on stdout and exit 0, for a config the contract gives exit 2.
    let overrides = args
        .set
        .iter()
        .map(|raw| run::parse_set(raw))
        .collect::<Result<Vec<_>, _>>()?;
    // Against the task this invocation actually carries, not a stand-in: the
    // child is handed the same flags, so a taskless `--detach` is the same
    // invalid config the foreground run exits 2 for — and reporting it as a
    // started run on stdout is the exact failure this preflight exists for.
    let task = requested_task(args)?;
    preflight(&args.graph, &overrides, env, task.as_deref())?;

    let state = state_dir(env);
    let before: Vec<run::RunId> = history::list(&state)
        .into_iter()
        .map(|r| r.run_id)
        .collect();
    let executable = std::env::current_exe()
        .map_err(|err| Error::InvalidConfig(format!("cannot find this binary: {err}")))?;
    let mut command = std::process::Command::new(executable);
    command.arg("run").arg(&args.graph);
    if let Some(task) = &args.task {
        command.args(["--task", task]);
    }
    if let Some(path) = &args.task_file {
        command.arg("--task-file").arg(path);
    }
    if let Some(dir) = &args.dir {
        command.arg("--dir").arg(dir);
    }
    for label in &args.label {
        command.args(["--label", label]);
    }
    for set in &args.set {
        command.args(["--set", set]);
    }
    let child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|err| Error::InvalidConfig(format!("cannot detach: {err}")))?;

    // The run id is the child's to mint, so it is read back from the state
    // directory rather than guessed here — a guess would be a second source for
    // an identifier the record already owns.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if let Some(record) = history::list(&state)
            .into_iter()
            .find(|record| !before.contains(&record.run_id))
        {
            println!(
                "{}",
                serde_json::to_string(&run::Started {
                    run_id: record.run_id,
                    events_path: record.events_path,
                    pid: child.id(),
                })
                .unwrap_or_default()
            );
            return Ok(EXIT_SUCCESS);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Err(Error::InvalidConfig(
        "the detached run did not record itself; check the graph with `oneagentgraph validate`"
            .into(),
    ))
}

/// The task `validate` checks a member's invocation against.
///
/// `validate` is given a graph and nothing else, and a graph is not invalid for
/// lacking prose nobody has typed yet — so a stand-in stands in for the one a
/// run would carry. This is the *only* caller entitled to one: a `--detach`
/// preflight knows the real task, and substituting here is how a taskless run
/// was reported as started.
const VALIDATE_TASK: &str = "validate: no task is run";

/// `oneagentgraph validate`.
///
/// Everything `run` does short of launching: the graph parses, its schema holds,
/// its `deps` can be satisfied, and **every member's invocation is built** — so a
/// persona that does not satisfy the delta contract, a base that merges to an
/// incomplete config, and a model paired with a chain of two harness families are
/// all found here rather than after a paid turn has been spent on the members
/// that did start.
fn validate(args: &ValidateArgs, env: &BTreeMap<String, String>) -> Result<i32, Error> {
    let graph = preflight(&args.graph, &[], env, Some(VALIDATE_TASK))?;
    println!("{}: {} member(s) OK", graph.name, graph.members.len());
    Ok(EXIT_SUCCESS)
}

/// Everything `run` does short of launching, without reporting anything.
///
/// Shared with `--detach`, which must not answer `{run_id, …}` and exit 0 for a
/// graph that cannot run: the child writes its record *before* it builds member
/// invocations, so a failure past that point would otherwise reach the caller as
/// a started run. The contract gives that case exit 2, and this is where the
/// parent earns it.
fn preflight(
    graph_ref: &str,
    overrides: &[run::Override],
    env: &BTreeMap<String, String>,
    task: Option<&str>,
) -> Result<GraphConfig, Error> {
    let mut resolver = Resolver::new();
    let reference = config::ConfigRef(graph_ref.to_string());
    let document = resolver.resolve(&reference, None)?.clone();
    // Parsed loosely, overridden, and only then read as a graph — the order
    // `run` itself uses. Checking the document as written would pass a `--set`
    // that names nothing, and `--detach` forwards those to the child.
    let mut parsed: serde_json::Value = serde_norway::from_str(&document.content)
        .map_err(|err| Error::InvalidConfig(format!("{graph_ref}: {err}")))?;
    run::apply_overrides(&mut parsed, overrides)?;
    let graph: GraphConfig = serde_norway::from_value(
        serde_norway::to_value(&parsed)
            .map_err(|err| Error::InvalidConfig(format!("{graph_ref}: {err}")))?,
    )
    .map_err(|err| Error::InvalidConfig(format!("{graph_ref}: {err}")))?;
    config::validate(&graph)?;
    run::ready_order(&graph)?;

    // The generated configs go to a directory that is thrown away: what is being
    // checked is that they *can* be generated, not what they say. The task is
    // the caller's to supply, because the two callers mean different things by
    // its absence — see [`VALIDATE_TASK`].
    // Removed when this claim drops, on the way out of *every* path rather than
    // only the one that reaches the end: a refusal is the common case here, and
    // a `validate` that leaked a directory per failed attempt would be one of
    // the things a sweep exists to clean up after.
    let scratch = tempdir(env, "validate")?;
    for (name, member) in &graph.members {
        let member_scratch = scratch.path().join(name);
        let context = oneagentgraph::invoke::Context {
            dir: Path::new("."),
            scratch: &member_scratch,
            graph_dir: document.base_dir.as_deref(),
            task,
            session: "validate",
            oneharness_bin: &oneharness_bin(env),
        };
        oneagentgraph::invoke::build(member, &context, &mut resolver)
            .map_err(|err| Error::InvalidConfig(format!("member {name:?}: {err}")))?;
    }
    Ok(graph)
}

/// The two out-of-band signals the contract gives an operator.
///
/// A closed set, because the run watches for a file named after one: a third
/// spelling would be a file nothing ever reads, and the command that wrote it
/// would still have reported success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signal {
    /// Fire a scheduled member now.
    Trigger,
    /// Restart a resettable schedule's clock.
    Reset,
}

impl Signal {
    /// The suffix the run watches for.
    const fn as_str(self) -> &'static str {
        match self {
            Signal::Trigger => "trigger",
            Signal::Reset => "reset",
        }
    }
}

/// Refuse a member this run does not have, naming the ones it does.
///
/// The names come from the graph, written into the record before anything
/// launches, because `members` fills in only as members *settle* — so during a
/// live run, which is when these verbs are used, it is empty. A record from
/// before that field existed falls back to the outcomes, and one that carries
/// neither is not second-guessed: refusing then would refuse a member that is
/// really there.
fn belongs_to_run(record: &run::Record, run: &str, member: &str) -> Result<(), Error> {
    let mut known: Vec<&str> = record.declared_members.iter().map(String::as_str).collect();
    if known.is_empty() {
        known = record.members.keys().map(String::as_str).collect();
    }
    if known.is_empty() || known.contains(&member) {
        return Ok(());
    }
    Err(Error::InvalidConfig(format!(
        "run {run:?} has no member {member:?}; it has {}",
        known.join(", ")
    )))
}

/// `oneagentgraph trigger` / `reset-timer`: leave the run a signal to pick up.
fn signal(args: &MemberArgs, env: &BTreeMap<String, String>, kind: Signal) -> Result<i32, Error> {
    let member = member_name(&args.member)?;
    let state = state_dir(env);
    let record = history::show(&state, &args.run)?;
    // From the run's *id*, the way `cancel` derives the same directory — not
    // from the record's `events_path`. That field is a string this crate wrote
    // into a file it later reads back, and a signal is a write: deriving a write
    // path from it would let a record place one anywhere the process can reach.
    let dir = state.join(&record.run_id).join(run::SIGNAL_DIR);
    belongs_to_run(&record, &args.run, &args.member)?;
    std::fs::create_dir_all(&dir)
        .map_err(|err| Error::InvalidConfig(format!("cannot create {}: {err}", dir.display())))?;
    let path = dir.join(format!("{member}.{}", kind.as_str()));
    std::fs::write(&path, kind.as_str())
        .map_err(|err| Error::InvalidConfig(format!("cannot write {}: {err}", path.display())))?;
    println!("{}: {} {}", args.run, args.member, kind.as_str());
    Ok(EXIT_SUCCESS)
}

use std::path::Path;

/// `oneagentgraph cancel`.
fn cancel(args: &CancelArgs, env: &BTreeMap<String, String>) -> Result<i32, Error> {
    let member = args.member.as_deref().map(member_name).transpose()?;
    let state = state_dir(env);
    let record = history::show(&state, &args.run)?;
    if let Some(named) = &args.member {
        belongs_to_run(&record, &args.run, named)?;
    }
    let root = state.join(&record.run_id);
    std::fs::create_dir_all(root.join(run::SIGNAL_DIR))
        .map_err(|err| Error::InvalidConfig(format!("cannot create {}: {err}", root.display())))?;
    // Scoped to the member when one is named. The whole-run `stop` is what every
    // scheduled member watches, so writing it for a member-scoped cancel stopped
    // the rest of the run along with the one the operator named.
    let stop = match &member {
        Some(member) => root.join(run::SIGNAL_DIR).join(format!("{member}.stop")),
        None => root.join(run::SIGNAL_DIR).join("stop"),
    };
    std::fs::write(&stop, "stop")
        .map_err(|err| Error::InvalidConfig(format!("cannot signal run {:?}: {err}", args.run)))?;
    let reaped = if args.kill {
        // Only a proven process is signalled: every live process still carrying
        // this run's scratch stamp, and nothing derived from a remembered number.
        match member {
            Some(member) => oneagentgraph::scratch::reap(&root.join("members").join(member)),
            None => oneagentgraph::scratch::reap(&root),
        }
    } else {
        0
    };
    println!(
        "{}: cancelled{}{}",
        args.run,
        args.member
            .as_ref()
            .map(|m| format!(" member {m}"))
            .unwrap_or_default(),
        if args.kill {
            format!(", {reaped} process(es) signalled")
        } else {
            String::new()
        }
    );
    Ok(EXIT_SUCCESS)
}

/// `oneagentgraph interrupt`: redirect a member's in-flight turn.
///
/// `cancel`'s sibling, addressed the same way and through the same plumbing — the
/// run record says which members exist, and the member's own scratch is where the
/// run wrote down where its turn is addressed. What differs is the intent: `cancel`
/// ends a turn and the worker's accumulated context goes with it, and this
/// redirects one that keeps running.
///
/// Every outcome publishes one `turn-interrupted`, including the ones that did not
/// land: a verb that stayed quiet unless it worked would leave "the lever was
/// pulled and nothing happened" visible only in an exit code.
fn interrupt(args: &InterruptArgs, env: &BTreeMap<String, String>) -> Result<i32, Error> {
    let member = member_name(&args.member)?;
    let input = redirection(args)?;
    let state = state_dir(env);
    let record = history::show(&state, &args.run)?;
    belongs_to_run(&record, &args.run, &args.member)?;
    // Derived from the run's *id*, exactly as `cancel` derives the directory it
    // reaps: `events_path` is a string this crate wrote into a file it reads
    // back, and following it would let a record point this verb at any directory
    // the process can reach.
    let scratch = state.join(&record.run_id).join("members").join(member);

    let bytes = input.as_ref().map_or(0, |text| text.len() as u64);
    let delivery = match oneagentgraph::control::read(&scratch) {
        // An ask that was refused is the permanent fact and beats every other
        // answer: a member on a harness with no lever has none whether it is
        // running, between turns, or long settled.
        Ok(Turn::Unavailable { reason }) => Delivery::NoTurn(reason),
        Ok(Turn::Open { address }) => {
            if settled(&record, member) {
                Delivery::NoTurn("the member has already settled, so its turn is over".to_string())
            } else {
                oneagentgraph::control::deliver(&oneharness_bin(env), &address, input.as_deref())
            }
        }
        Err(reason) => Delivery::NoTurn(reason),
    };

    let (code, reason) = match delivery {
        Delivery::Delivered => (EXIT_SUCCESS, None),
        Delivery::NoTurn(reason) => (EXIT_NO_CONTROLLABLE_TURN, Some(reason)),
        Delivery::Failed(reason) => (EXIT_MEMBER_FAILED, Some(reason)),
        // A redirection oneharness would not take is an argument this caller got
        // wrong, and nothing was ever delivered — so it refuses like every other
        // bad argument, before any event claims a lever was pulled.
        Delivery::Invalid(reason) => {
            return Err(Error::InvalidConfig(format!("--input: {reason}")))
        }
    };
    publish(&record.run_id, member, bytes, reason.clone());
    // Exit 3 is a fact rather than an error, so it says so on stdout with the
    // event; only a delivery that was attempted and failed is a diagnostic.
    if code == EXIT_MEMBER_FAILED {
        eprintln!(
            "oneagentgraph: {}: could not interrupt member {member}: {}",
            args.run,
            reason.unwrap_or_default()
        );
    }
    Ok(code)
}

/// The redirection this invocation carries, from whichever flag names it.
fn redirection(args: &InterruptArgs) -> Result<Option<String>, Error> {
    let text = match (&args.input, &args.input_file) {
        (Some(_), Some(_)) => {
            return Err(Error::InvalidConfig(
                "--input and --input-file both name the redirection; give at most one".into(),
            ))
        }
        (Some(text), None) => text.clone(),
        (None, Some(path)) => std::fs::read_to_string(path).map_err(|err| {
            Error::InvalidConfig(format!(
                "cannot read --input-file {}: {err}",
                path.display()
            ))
        })?,
        (None, None) => return Ok(None),
    };
    // Refused here rather than passed on: a blank redirection stops the turn and
    // says nothing, which is `cancel` spelled the long way round and not what the
    // caller asked for.
    if text.trim().is_empty() {
        return Err(Error::InvalidConfig(
            "--input is blank, so it would redirect the turn at nothing; send no --input to only \
             stop it"
                .into(),
        ));
    }
    Ok(Some(text))
}

/// Whether this run already recorded an outcome for `member`.
///
/// The record is the run's own evidence, so a member it has settled needs no
/// socket asked: `members` fills in as members settle, and a finished run has an
/// exit code of its own.
fn settled(record: &run::Record, member: &str) -> bool {
    record.exit_code.is_some() || record.members.contains_key(member)
}

/// Publish this interrupt as the contract's event, on this process's own stream.
///
/// Its own stream id and its own `seq`, because the envelope's `stream` is "a
/// unique id per producing process" and this *is* one — the run holds its events
/// file open and writes at its own offset, so a second writer appending there
/// would overwrite the line the run wrote next.
fn publish(run_id: &run::RunId, member: &str, input_bytes: u64, reason: Option<String>) {
    let emitter = Emitter::new(
        format!("{run_id}-interrupt-{}", std::process::id()),
        Box::new(std::io::stdout()),
    )
    .with_labels(Labels {
        run_id: Some(run_id.to_string()),
        member: Some(member.to_string()),
        ..Labels::default()
    });
    let payload = TurnInterrupted {
        member: member.to_string(),
        delivered: reason.is_none(),
        input_bytes,
        reason,
    };
    emitter.emit(
        EventKind::TurnInterrupted,
        match serde_json::to_value(&payload) {
            Ok(serde_json::Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        },
    );
}

/// One `MEMBER` argument, checked against the shape a graph's own member names
/// are checked against.
///
/// The argument becomes a path — the signal file a run watches for, and the
/// member scratch `cancel --kill` reaps — so a value carrying a separator or a
/// parent reference would write or reap outside the run's own directory.
fn member_name(member: &str) -> Result<&str, Error> {
    if config::is_member_name(member) {
        return Ok(member);
    }
    Err(Error::InvalidConfig(format!(
        "member {member:?}: a member name is letters, digits, hyphens, and underscores — this \
         one would name a path outside the run's own directory"
    )))
}

/// `oneagentgraph history`.
fn show_history(args: &HistoryArgs, env: &BTreeMap<String, String>) -> Result<i32, Error> {
    let state = state_dir(env);
    match &args.command {
        Some(HistoryCommand::Show { id }) => {
            let record = history::show(&state, id)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&record).unwrap_or_default()
            );
        }
        None => {
            let records = history::list(&state);
            let selected: Vec<_> = match &args.run {
                Some(run) => records
                    .into_iter()
                    .filter(|r| r.run_id.as_str() == run)
                    .collect(),
                None => records,
            };
            for record in &selected {
                println!(
                    "{}\t{}\t{}",
                    record.run_id,
                    record
                        .exit_code
                        .map_or_else(|| "running".into(), |code| code.to_string()),
                    record.name
                );
            }
            if selected.is_empty() {
                if let Some(run) = &args.run {
                    return Err(Error::InvalidConfig(format!("no run {run:?}")));
                }
            }
        }
    }
    Ok(EXIT_SUCCESS)
}

/// `oneagentgraph smoke`.
fn run_smoke(args: &SmokeArgs, env: &BTreeMap<String, String>) -> Result<i32, Error> {
    // Kept rather than removed on the way out: a smoke that failed is one an
    // operator reads the harness's own leavings out of, and taking them away
    // would leave them the exit code alone. It is claimed scratch, so `sweep`
    // can prove it dead and reclaim it later — which is what a throwaway
    // directory nobody is left holding needs to be.
    let mut scratch;
    let dir = match &args.dir {
        Some(dir) => dir.clone(),
        None => {
            scratch = tempdir(env, "smoke")?;
            scratch.keep();
            scratch.path().to_path_buf()
        }
    };
    std::fs::create_dir_all(&dir)
        .map_err(|err| Error::InvalidConfig(format!("cannot create {}: {err}", dir.display())))?;
    ensure_git_worktree(&dir)?;
    let verdict = smoke::run(&oneharness_bin(env), &dir)?;
    for candidate in &verdict.fell_through {
        println!(
            "smoke: fell through {} ({}); the chain handed the turn on",
            candidate.identity,
            candidate.reason.as_str()
        );
    }
    // The retry is reported rather than smoothed over: a host that needed two
    // starts to record one turn is healthy now and worth knowing about, and the
    // operator is the only one who can act on it.
    if verdict.attempts > 1 {
        println!(
            "smoke: passed via {} after {} attempts",
            verdict.ran, verdict.attempts
        );
    } else {
        println!("smoke: passed via {}", verdict.ran);
    }
    Ok(EXIT_SUCCESS)
}

/// Make the smoke's working directory acceptable to harnesses which require a
/// trusted repository.
///
/// An existing worktree, including a caller-supplied subdirectory of one, is
/// left alone. A standalone directory becomes its own empty repository. Doing
/// this at the shared working-directory boundary keeps harness-specific flags
/// out of a command whose chain may select any identity.
fn ensure_git_worktree(dir: &std::path::Path) -> Result<(), Error> {
    let inside = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|err| {
            Error::InvalidConfig(format!("cannot inspect {} with git: {err}", dir.display()))
        })?;
    if inside.status.success() && inside.stdout == b"true\n" {
        return Ok(());
    }

    let initialized = std::process::Command::new("git")
        .arg("init")
        .arg("--quiet")
        .arg(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|err| {
            Error::InvalidConfig(format!(
                "cannot initialize {} as a git repository: {err}",
                dir.display()
            ))
        })?;
    if initialized.status.success() {
        Ok(())
    } else {
        Err(Error::InvalidConfig(format!(
            "cannot initialize {} as a git repository: {}",
            dir.display(),
            String::from_utf8_lossy(&initialized.stderr).trim()
        )))
    }
}

/// `oneagentgraph sweep`.
///
/// The roots come from the same environment every other verb reads, so a sweep
/// answers for the run state this install actually uses rather than for a host's
/// defaults. Exit 0 whatever it finds: a family it could not examine is a
/// *reported* fact, not a refusal — the report names it, and an operator acting
/// on that is the whole point of the verb.
fn sweep_scratch(args: &SweepArgs, env: &BTreeMap<String, String>) -> Result<i32, Error> {
    let families = sweep::families(state_dir(env), temp_root(env));
    let mode = if args.dry_run {
        sweep::Mode::Report
    } else {
        sweep::Mode::Reclaim
    };
    let report = sweep::sweep(
        &families,
        mode,
        std::time::Duration::from_secs(args.min_age_hours.saturating_mul(3600)),
        std::time::SystemTime::now(),
    );
    for line in report.lines() {
        println!("{line}");
    }
    Ok(EXIT_SUCCESS)
}

/// The directory this crate leaves its throwaway scratch in.
///
/// `TMPDIR` first, so a caller can point one invocation somewhere with room on
/// it — and so `sweep` looks in the same place the invocation that made the
/// scratch did.
fn temp_root(env: &BTreeMap<String, String>) -> PathBuf {
    env.get("TMPDIR")
        .map_or_else(std::env::temp_dir, PathBuf::from)
}

/// A throwaway directory for a command that needs one: a smoke that named none,
/// or the generated configs `validate` builds and discards.
///
/// `verb` is the caller's own, because the two callers leave different things
/// behind and one of them leaves it for a person to read — a directory called
/// `smoke` holding a `validate`'s discarded configs is a name that lies to
/// whoever goes looking. It also means the two can never be handed the same
/// path.
///
/// Claimed rather than merely created, and that is what makes it sweepable: an
/// `owner.lock` is the only thing that ever proves a directory is done with, so
/// scratch this crate leaves without one is scratch its own `sweep` has to
/// retain forever. The claim is released when this process exits, and the
/// identity it records is dead from that moment.
fn tempdir(env: &BTreeMap<String, String>, verb: &str) -> Result<Owned, Error> {
    let dir = temp_root(env).join(format!("{SCRATCH_PREFIX}{verb}-{}", std::process::id()));
    Owned::claim(dir)
}

/// `oneagentgraph persona`.
fn persona(args: &PersonaArgs) -> Result<i32, Error> {
    match &args.command {
        PersonaCommand::New { name } => {
            // Parsed before anything is created: the name decides the path.
            let name: oneagentgraph::persona::PersonaName =
                name.parse().map_err(Error::InvalidConfig)?;
            let path = PathBuf::from(format!("{name}.yaml"));
            if path.exists() {
                return Err(Error::InvalidConfig(format!(
                    "{} already exists",
                    path.display()
                )));
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    Error::InvalidConfig(format!("cannot create {}: {err}", parent.display()))
                })?;
            }
            std::fs::write(&path, PERSONA_TEMPLATE).map_err(|err| {
                Error::InvalidConfig(format!("cannot write {}: {err}", path.display()))
            })?;
            println!(
                "Created {} — fill in agent.instructions and user.persona, then run \
                 `oneagentgraph persona validate {}`.",
                path.display(),
                path.display()
            );
            Ok(EXIT_SUCCESS)
        }
        PersonaCommand::Validate { path } => {
            let mut failures = 0;
            for file in personas_under(path)? {
                let document = std::fs::read_to_string(&file).map_err(|err| {
                    Error::InvalidConfig(format!("cannot read {}: {err}", file.display()))
                })?;
                let name = file.display().to_string();
                match Persona::parse(&document, &name) {
                    Ok(persona) => {
                        let errors = persona.validate();
                        for error in &errors {
                            eprintln!("{name}: {error}");
                        }
                        failures += usize::from(!errors.is_empty());
                    }
                    Err(err) => {
                        eprintln!("{err}");
                        failures += 1;
                    }
                }
            }
            if failures > 0 {
                return Err(Error::InvalidConfig(format!(
                    "{failures} persona(s) invalid"
                )));
            }
            println!("{}: OK", path.display());
            Ok(EXIT_SUCCESS)
        }
    }
}

/// Every persona a `validate` argument names: one file, or every `.yaml` under a
/// directory, skipping the `_`-prefixed template a catalog scaffolds from.
fn personas_under(path: &Path) -> Result<Vec<PathBuf>, Error> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        return Err(Error::InvalidConfig(format!(
            "no persona at {}",
            path.display()
        )));
    }
    let mut found = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|err| Error::InvalidConfig(format!("cannot read {}: {err}", dir.display())))?;
        for entry in entries {
            // A catalog walk that skipped what it could not read would report
            // OK for a directory it never finished — the one answer `persona
            // validate` must never give.
            let entry = entry
                .map_err(|err| {
                    Error::InvalidConfig(format!(
                        "cannot read an entry of {}: {err}",
                        dir.display()
                    ))
                })?
                .path();
            let name = entry
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            if name.starts_with('_') {
                continue;
            }
            if entry.is_dir() {
                stack.push(entry);
            } else if entry.extension().and_then(|e| e.to_str()) == Some("yaml") {
                found.push(entry);
            }
        }
    }
    found.sort();
    if found.is_empty() {
        return Err(Error::InvalidConfig(format!(
            "no personas under {}",
            path.display()
        )));
    }
    Ok(found)
}

/// Where run state lives.
fn state_dir(env: &BTreeMap<String, String>) -> PathBuf {
    env.get(STATE_DIR_ENV).map_or_else(
        || {
            env.get("HOME")
                .map_or_else(std::env::temp_dir, PathBuf::from)
                .join(".local/state/oneagentgraph/runs")
        },
        PathBuf::from,
    )
}

/// The `oneharness` binary a run drives.
fn oneharness_bin(env: &BTreeMap<String, String>) -> String {
    env.get(ONEHARNESS_BIN_ENV)
        .cloned()
        .unwrap_or_else(|| "oneharness".into())
}
