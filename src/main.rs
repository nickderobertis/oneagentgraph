//! The `oneagentgraph` binary.
//!
//! Every verb `docs/contract.md` lists, and the three exit codes it assigns: `0`
//! every member settled successfully, `1` a member failed or died, `2` invalid
//! config. Nothing here does the work — each verb parses its arguments, resolves
//! where the run state lives, and hands off to the library.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use oneagentgraph::cli::{
    CancelArgs, Cli, Command, HistoryArgs, HistoryCommand, MemberArgs, OutputFormat, PersonaArgs,
    PersonaCommand, RunArgs, SmokeArgs, ValidateArgs,
};
use oneagentgraph::config::GraphConfig;
use oneagentgraph::error::{Error, EXIT_INVALID_CONFIG, EXIT_MEMBER_FAILED, EXIT_SUCCESS};
use oneagentgraph::persona::{Persona, PERSONA_TEMPLATE};
use oneagentgraph::render::Text;
use oneagentgraph::resolve::Resolver;
use oneagentgraph::{config, health, history, run, smoke};

/// Where run state lives unless the environment says otherwise.
const STATE_DIR_ENV: &str = "ONEAGENTGRAPH_STATE_DIR";

/// The `onejudge` binary a run drives, overridable so a test — or a host with a
/// pinned install — can name its own.
const ONEJUDGE_BIN_ENV: &str = "ONEAGENTGRAPH_ONEJUDGE_BIN";

/// The `oneharness` binary, on the same terms.
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
        Command::History(args) => show_history(&args, env),
        Command::Health => {
            let report = health::read(&oneharness_bin(env))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&report).unwrap_or_default()
            );
            Ok(EXIT_SUCCESS)
        }
        Command::Smoke(args) => run_smoke(&args, env),
        Command::Persona(args) => persona(&args),
    }
}

/// `oneagentgraph run`.
fn run_graph(args: RunArgs, env: &BTreeMap<String, String>) -> Result<i32, Error> {
    let task = match (&args.task, &args.task_file) {
        (Some(_), Some(_)) => {
            return Err(Error::InvalidConfig(
                "--task and --task-file both name the task; give exactly one".into(),
            ))
        }
        (Some(text), None) => Some(text.clone()),
        (None, Some(path)) => Some(std::fs::read_to_string(path).map_err(|err| {
            Error::InvalidConfig(format!("cannot read --task-file {}: {err}", path.display()))
        })?),
        (None, None) => None,
    };
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
        onejudge_bin: onejudge_bin(env),
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
    preflight(&args.graph, &overrides, env)?;

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

/// `oneagentgraph validate`.
///
/// Everything `run` does short of launching: the graph parses, its schema holds,
/// its `deps` can be satisfied, and **every member's invocation is built** — so a
/// persona that does not satisfy the delta contract, a base that merges to an
/// incomplete config, and a model paired with a chain of two harness families are
/// all found here rather than after a paid turn has been spent on the members
/// that did start.
fn validate(args: &ValidateArgs, env: &BTreeMap<String, String>) -> Result<i32, Error> {
    let graph = preflight(&args.graph, &[], env)?;
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
    // checked is that they *can* be generated, not what they say. A stand-in task
    // stands in for the one `run` would be given, because a graph is not invalid
    // for lacking prose nobody has typed yet.
    let scratch = tempdir(env)?;
    for (name, member) in &graph.members {
        let member_scratch = scratch.join(name);
        let context = oneagentgraph::invoke::Context {
            dir: Path::new("."),
            scratch: &member_scratch,
            graph_dir: document.base_dir.as_deref(),
            task: Some("validate: no task is run"),
            session: "validate",
            onejudge_bin: &onejudge_bin(env),
            oneharness_bin: &oneharness_bin(env),
        };
        oneagentgraph::invoke::build(member, &context, &mut resolver)
            .map_err(|err| Error::InvalidConfig(format!("member {name:?}: {err}")))?;
    }
    let _ = std::fs::remove_dir_all(&scratch);
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
    let scratch;
    let dir = match &args.dir {
        Some(dir) => dir.clone(),
        None => {
            scratch = tempdir(env)?;
            scratch.clone()
        }
    };
    std::fs::create_dir_all(&dir)
        .map_err(|err| Error::InvalidConfig(format!("cannot create {}: {err}", dir.display())))?;
    let verdict = smoke::run(&oneharness_bin(env), &dir)?;
    for candidate in &verdict.fell_through {
        println!(
            "smoke: fell through {} ({}); the chain handed the turn on",
            candidate.identity,
            candidate.reason.as_str()
        );
    }
    println!("smoke: passed via {}", verdict.ran);
    Ok(EXIT_SUCCESS)
}

/// A throwaway directory for a command that needs one: a smoke that named none,
/// or the generated configs `validate` builds and discards.
fn tempdir(env: &BTreeMap<String, String>) -> Result<PathBuf, Error> {
    let base = env
        .get("TMPDIR")
        .map_or_else(std::env::temp_dir, PathBuf::from);
    let dir = base.join(format!("oneagentgraph-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&dir)
        .map_err(|err| Error::InvalidConfig(format!("cannot create {}: {err}", dir.display())))?;
    Ok(dir)
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

/// The `onejudge` binary a run drives.
fn onejudge_bin(env: &BTreeMap<String, String>) -> String {
    env.get(ONEJUDGE_BIN_ENV)
        .cloned()
        .unwrap_or_else(|| "onejudge".into())
}

/// The `oneharness` binary a run drives.
fn oneharness_bin(env: &BTreeMap<String, String>) -> String {
    env.get(ONEHARNESS_BIN_ENV)
        .cloned()
        .unwrap_or_else(|| "oneharness".into())
}
