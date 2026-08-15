//! Preparing one member for launch.
//!
//! This is the whole of what `docs/contract.md` means by "prepares each member's
//! launch": a member's refs are resolved, its persona is merged onto its base,
//! each side's oneharness config is written into the run's own directory, and
//! what actually starts the member is assembled — a onejudge plan this
//! process drives itself for a two-party member, an argv for the single-sided
//! one. Nothing here selects a harness, a model chain, or a fallback order —
//! those live in the oneharness config files a graph names, and this module only
//! ever *carries* them.
//!
//! # How each side is pinned, in a process shared with every other member
//!
//! onejudge routes both conversation sides through one `provider.bin`. The judge
//! side is given `oneharness run --config <judge_config>`; the agent side is
//! given none on the argv onejudge builds, and would otherwise rely on oneharness
//! discovering `oneharness.toml` upward from **the directory the harness will run
//! in** — `oneharness run --cwd`, which onejudge takes from the conversation's own
//! worktree.
//!
//! Both sides are therefore pinned by *file*, never by directory: each side's
//! resolved config is written into the member's scratch, and the path to the
//! agent side's rides [`JudgeLaunch::agent_config`] to `crate::judge`, which puts
//! it on that side's own `--config`. That is what frees
//! [`JudgeLaunch::worktree`] to be the directory the graph was told to work in.
//!
//! Naming a worktree, rather than changing directory into it: a process has one working
//! directory and this one runs every member of the graph at once, so a member
//! that pinned its side by `cd`-ing would pin its siblings too. Everything
//! per-member is therefore carried *in the files this module writes*, never in
//! process-wide state — which is also why the two settings below are stamped into
//! a side's resolved config rather than exported:
//!
//! * A `model` override, because a config's per-harness `model` beats
//!   `ONEHARNESS_MODEL`, so exporting it would be a setting that silently loses.
//!   Stamping the per-harness sections is what the contract's pairing rule makes
//!   safe — a chain of one harness family has one set of sections to stamp.
//! * The member's `mode` and its [`crate::scratch::SCRATCH_ENV`] ownership stamp,
//!   because both are *per-member* values that would otherwise have to be
//!   exported from a process shared with every other member. oneharness's config
//!   carries both — `mode` at the top level, and `[env]` as the environment every
//!   harness process it starts is given — so the stamp reaches the harness the
//!   same way it always did: fixed at `exec`, on a process no walk from this one
//!   would find once its parent is gone.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;
use toml_edit::{DocumentMut, Item};

use crate::config::{AgentSide, ConfigRef, JudgeSide, Member, OnejudgeMember, TaskText};
use crate::error::Error;
use crate::persona::{self, Persona};
use crate::resolve::{ResolvedRef, Resolver};

/// The file name oneharness discovers upward from a working directory. Writing
/// the agent side's resolved config here is what pins that side.
pub const AGENT_CONFIG_FILE: &str = "oneharness.toml";

/// The judge side's config, named on its own `oneharness run --config`.
pub const JUDGE_CONFIG_FILE: &str = "oneharness.judge.toml";

/// The effective onejudge config a `kind: onejudge` member runs.
pub const ONEJUDGE_CONFIG_FILE: &str = "onejudge.yaml";

/// Where a member's `mode` lands.
///
/// onejudge's own config schema has no approval mode — it rejects the key
/// outright — because the mode is a harness posture, and oneharness is what maps
/// it to each harness's native mechanism. oneharness reads it from this variable,
/// which beats both sides' config files, so one member's `mode` reaches the agent
/// side and the judge side alike.
///
/// It is exported only for a **single-sided** member, which is still a child
/// process of its own and so has an environment of its own. A two-party member is
/// driven in this process, shared with every other member, so its `mode` is
/// stamped into each side's resolved config instead — see this module's own
/// documentation.
pub const MODE_ENV: &str = "ONEHARNESS_MODE";

/// oneharness's process-wide identity selection — the one variable that *beats*
/// a config's own `harnesses` chain.
///
/// The mirror image of the `ONEHARNESS_MODEL` case above, and the dangerous half.
/// A graph names an oneharness config per side and this crate hands each side
/// exactly that config; if a value the launching process happened to export then
/// overrides it, the graph did not choose anything — the run silently spends a
/// different subscription than the one it was pointed at. That launcher is not
/// hypothetical: ai-orchestrator, the system this crate was extracted from,
/// *exports* this variable around everything it dispatches.
///
/// So it is dropped from what a member inherits. It is not forbidden: a graph
/// that means to select this way says so in its own `env:` block, which is
/// applied afterwards and wins.
pub const PROCESS_WIDE_HARNESS_ENV: &str = "ONEHARNESS_HARNESSES";

/// How one member is started.
///
/// The contract's two member kinds are two different things to start, and this is
/// the one place that difference lives. A `kind: onejudge` member is a onejudge
/// run driven **through onejudge's library, in this process** — there is no
/// `onejudge` binary in the chain, and so no argv, no exit status, and no stderr
/// for it. A `kind: oneharness` member is still `oneharness run`, a child process
/// of its own — no longer because oneharness's library surface could not answer
/// (since 0.7.0 `oneharness_core::io::run::run` returns the report and takes an
/// event sink), but because a *supervised* member needs two things that surface
/// still cannot give and one the contract has not yet been asked for.
/// `src/harness_process.rs` names all three at the site, with the proposal each
/// one is. See `docs/contract.md`.
#[derive(Debug, Clone)]
pub enum Launch {
    /// onejudge's own run driver, over the config written into the member's
    /// scratch, driven in this process.
    Library(Box<JudgeLaunch>),
    /// A child process: the program, its arguments, and the directory it runs
    /// in.
    Process {
        /// The program to run.
        program: String,
        /// Its arguments.
        args: Vec<String>,
        /// The child's working directory.
        cwd: PathBuf,
    },
}

/// Everything driving one two-party member in this process needs.
#[derive(Debug, Clone)]
pub struct JudgeLaunch {
    /// The effective onejudge config written into the member's scratch. Kept as
    /// a path rather than a parsed plan because it is also the run's evidence:
    /// an operator reads this file to see exactly what the member ran.
    pub config: PathBuf,
    /// The task prose this member drives to completion.
    pub task: String,
    /// The directory this member's harness works in — `member_dir`, the same
    /// value a single-sided member's `--cwd` gets.
    ///
    /// Named to oneharness rather than entered, because it is not *this
    /// process's* working directory: this one is shared with every other member.
    /// It **is** a working directory in every other sense — onejudge takes it as
    /// the conversation's skill directory and puts it on the agent side's
    /// `oneharness run --cwd` — and that is the whole of what it is for.
    ///
    /// Repointing it needs [`Self::agent_config`] set too, or the member reverts
    /// to whatever config sits above the operator's directory: until that field
    /// existed, this one was also the only thing pinning the agent side.
    pub worktree: PathBuf,
    /// The agent side's resolved oneharness config, pinned by name rather than by
    /// where the harness happens to run.
    ///
    /// onejudge gives the agent side no config key — only a worktree, from which
    /// oneharness *discovers* a project `oneharness.toml` — so `crate::judge`
    /// puts this on that side's own `--config` through onejudge's `SpawnHook`.
    /// By name and not merely by preference, because this is the *stamped* copy:
    /// discovery from an operator's own directory finds their `oneharness.toml`,
    /// whose `harnesses` chain would spend a different subscription than the
    /// graph named — [`PROCESS_WIDE_HARNESS_ENV`]'s hazard from the other side.
    ///
    /// *Proposal to onejudge, which would retire the hook:* an `agent_config` on
    /// its `kind: oneharness` provider, beside the `judge_config` it takes.
    pub agent_config: PathBuf,
    // llmlint: ignore-block[invalid_states_unrepresentable] the handle is a
    // `String` for the reason [`crate::control::Address::session`] records: what
    // a session handle may be is oneharness's rule, applied by it on both ends,
    // and a type here would either restate rules this crate does not own or
    // refuse a value oneharness accepts. It is written verbatim into the member's
    // onejudge config as `session:`, which is a YAML string either way.
    /// The session handle threaded across this member's turns, which is also
    /// how an `oneagentgraph interrupt` addresses the turn in flight — see
    /// [`crate::control`].
    pub session: String,
    // llmlint: ignore-end[invalid_states_unrepresentable]
}

/// One member, ready to start.
#[derive(Debug, Clone)]
pub struct Invocation {
    /// Which program's contract this member settles under, because the two read
    /// their outcomes differently.
    pub kind: crate::member::Kind,
    /// What starting it means, and where — a child process's working directory
    /// and an in-process member's worktree are different things, so each rides
    /// the variant it belongs to rather than one field claiming to be both.
    pub launch: Launch,
    /// The persona label to stamp on this member's events, when it has one.
    pub persona: Option<String>,
    /// What this member adds to the process environment of a child it starts,
    /// over and above the graph's own `env` block. Empty for a member with no
    /// child process of its own.
    pub env: Vec<(String, String)>,
    /// Every config ref this member read, content-addressed for the run record.
    pub refs: Vec<ResolvedRef>,
}

/// What a member's invocation is built against.
#[derive(Debug, Clone)]
pub struct Context<'a> {
    /// The directory the graph was told to work in.
    pub dir: &'a Path,
    /// Where this member's generated config files are written.
    pub scratch: &'a Path,
    /// The directory relative refs in the graph document resolve against.
    pub graph_dir: Option<&'a Path>,
    /// The graph's own persona catalog, when it named one — see
    /// [`crate::config::GraphConfig::personas`].
    pub personas: Option<&'a Path>,
    /// The task prose, when the run supplied one.
    pub task: Option<&'a str>,
    /// How a member's own task text is read, which the graph document's schema
    /// decided — see [`crate::config::TaskText`].
    pub task_text: TaskText,
    /// The session name threaded across this member's turns.
    pub session: &'a str,
    /// The `oneharness` binary onejudge shells out to, and that a single-sided
    /// member runs directly.
    pub oneharness_bin: &'a str,
}

/// Build one member's invocation, resolving everything it names.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when a ref cannot be read, a persona is invalid, the
/// merged config is incomplete, the model pairing rule is broken, or a generated
/// file cannot be written.
pub fn build(
    member: &Member,
    context: &Context<'_>,
    resolver: &mut Resolver,
) -> Result<Invocation, Error> {
    // A catalog the graph named is read whether or not this member looks in it:
    // an unreadable one is the operator's mistake wherever it is found, and a run
    // that only surfaced it once somebody wrote a name-shaped `persona:` would
    // surface it a dispatch later than the graph it was written in.
    catalog_root(context)?;
    match member {
        Member::Onejudge(member) => onejudge(member, context, resolver),
        Member::Oneharness(member) => {
            let (config, persona_label) = harness_side(
                &member.oneharness_config,
                member.persona.as_ref(),
                Side::default(),
                context,
                resolver,
            )?;
            let path = context.scratch.join(AGENT_CONFIG_FILE);
            write(&path, &config)?;
            // Every argument as computed, empty or not: there are no optional
            // flags here, so dropping an empty entry can only shift a *value* out
            // of the argv and leave oneharness reading the next flag as one.
            let args = vec![
                "run".to_string(),
                "--config".to_string(),
                path.display().to_string(),
                "--cwd".to_string(),
                member_dir(member.dir.as_deref(), context)
                    .display()
                    .to_string(),
                "--events".to_string(),
                "--stream".to_string(),
                "--prompt".to_string(),
                member_task(member.task.as_deref(), context)?,
            ];
            Ok(Invocation {
                kind: crate::member::Kind::Oneharness,
                launch: Launch::Process {
                    program: context.oneharness_bin.to_string(),
                    args,
                    cwd: context.scratch.to_path_buf(),
                },
                persona: persona_label,
                env: Vec::new(),
                refs: resolver.inventory(),
            })
        }
    }
}

/// Build a two-party member's invocation.
fn onejudge(
    member: &OnejudgeMember,
    context: &Context<'_>,
    resolver: &mut Resolver,
) -> Result<Invocation, Error> {
    let base = resolver
        .resolve(&member.base_config, context.graph_dir)?
        .clone();
    let (persona, label) = load_persona(member.persona.as_ref(), context, resolver)?;
    let mut effective = persona::merge(&base.content, &member.base_config.0, &persona)?;
    let missing = persona::missing_from_merged(&effective);
    if !missing.is_empty() {
        return Err(Error::InvalidConfig(format!(
            "the config {} merges to is incomplete: missing {}",
            member.base_config.0,
            missing.join(", ")
        )));
    }

    let side = Side {
        model: member.agent.model.as_deref(),
        mode: Some(member.mode.as_str()),
        scratch: Some(context.scratch),
    };
    let (agent_config, _) = harness_side(
        &member.agent.oneharness_config,
        None,
        side,
        context,
        resolver,
    )?;
    let agent_path = context.scratch.join(AGENT_CONFIG_FILE);
    write(&agent_path, &agent_config)?;

    let provider = provider_block(
        &member.judge,
        &member.agent,
        Some(member.mode.as_str()),
        context,
        resolver,
    )?;
    let map = effective.as_object_mut().expect("merge returns a mapping");
    anchor_skill(map, base.base_dir.as_deref());
    map.insert("provider".into(), provider);
    map.insert("session".into(), Value::String(context.session.to_string()));
    if let Some(cap) = member.max_turns {
        map.entry("user")
            .or_insert_with(|| Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .expect("user is a mapping after the merge")
            .insert("max_turns".into(), Value::Number(cap.into()));
    }

    let config_path = context.scratch.join(ONEJUDGE_CONFIG_FILE);
    let rendered = serde_norway::to_string(&effective).map_err(|err| {
        Error::InvalidConfig(format!("cannot render the effective config: {err}"))
    })?;
    write(&config_path, &rendered)?;

    let prose = member_task(member.task.as_deref(), context)?;

    Ok(Invocation {
        kind: crate::member::Kind::Onejudge,
        launch: Launch::Library(Box::new(JudgeLaunch {
            config: config_path,
            task: prose,
            // The graph's own directory, resolved exactly as a single-sided
            // member's is. `None`, not because this kind has no `dir` of its own
            // to honour but because `docs/contract.md` scopes that field to
            // `kind: oneharness`, so `OnejudgeMember` has none to read; the call
            // is written this way so the day the contract grows one, the value
            // goes here and nothing else moves.
            worktree: member_dir(None, context),
            agent_config: agent_path,
            session: context.session.to_string(),
        })),
        persona: label,
        // Nothing: this member starts no child process of its own, so it has no
        // environment to add to. Its `mode` and its ownership stamp are in the
        // resolved configs written above.
        env: Vec::new(),
        refs: resolver.inventory(),
    })
}

/// Anchor a base config's own `skill:` to the directory its author wrote it in.
///
/// onejudge resolves a config-file `skill:` against *that config's* directory,
/// and the config it is handed is the merged copy this module writes into the
/// member's scratch — a directory the author never saw. So a relative skill has
/// to be made absolute here, where the base's own directory is still known, or
/// it resolves under the scratch and the member dies having found no `SKILL.md`.
///
/// A base fetched over https has no directory for a relative path to mean
/// anything against, so its skill is left exactly as written: onejudge refuses it
/// by name, which is a better answer than a path this crate invented.
fn anchor_skill(config: &mut serde_json::Map<String, Value>, base_dir: Option<&Path>) {
    let Some(base_dir) = base_dir else {
        return;
    };
    let Some(named) = config.get("skill").and_then(Value::as_str) else {
        return;
    };
    let path = Path::new(named);
    if !path.is_relative() {
        return;
    }
    // Absolute, not merely joined: a graph named by a relative path has a
    // relative `base_dir` too, and a relative skill would then be resolved a
    // second time — against the scratch the merged copy sits in. This runs
    // before any member starts, in the directory the operator's own paths were
    // written against, so this process's own is the right one to anchor to.
    // Purely lexical: nothing is read, so a skill that is not there is still
    // onejudge's refusal to make, by the name the author wrote.
    let joined = base_dir.join(path);
    let anchored = std::path::absolute(&joined).unwrap_or(joined);
    config.insert(
        "skill".into(),
        Value::String(anchored.display().to_string()),
    );
}

/// Every two-party member's agent side asks for a controllable turn, so
/// `oneagentgraph interrupt` has a lever whenever the harness under it has one.
///
/// Unconditional rather than a graph field, because the ask costs nothing where
/// it cannot be honored: onejudge retries the same call once without `--control`
/// and records the refusal as a stated reason, so a harness with no control
/// mechanism runs exactly as it did before and `interrupt` reports the reason
/// instead of a lever that silently did nothing. Only the *agent* side asks — a
/// judge turn is short and has nothing to redirect, and giving it a socket would
/// put two runs on one address.
const ASK_FOR_CONTROL: bool = true;

/// The onejudge `provider` block for a two-party member.
///
/// A harness-backed judge is one `kind: oneharness` provider carrying both
/// sides. A command judge cannot be expressed that way, so the two sides are
/// split — which is exactly onejudge's `split` provider.
fn provider_block(
    judge: &JudgeSide,
    agent: &AgentSide,
    mode: Option<&str>,
    context: &Context<'_>,
    resolver: &mut Resolver,
) -> Result<Value, Error> {
    match judge {
        JudgeSide::Harness(harness) => {
            let (config, _) = harness_side(
                &harness.oneharness_config,
                None,
                Side {
                    model: harness.model.as_deref(),
                    mode,
                    scratch: Some(context.scratch),
                },
                context,
                resolver,
            )?;
            let path = context.scratch.join(JUDGE_CONFIG_FILE);
            write(&path, &config)?;
            Ok(serde_json::json!({
                "kind": "oneharness",
                "bin": context.oneharness_bin,
                "judge_config": path.display().to_string(),
                "stream": agent.stream,
                "control": ASK_FOR_CONTROL,
            }))
        }
        JudgeSide::Command(command) => {
            if command.command.is_empty() {
                return Err(Error::InvalidConfig(
                    "a command judge needs a command to run".to_string(),
                ));
            }
            Ok(serde_json::json!({
                "kind": "split",
                "skill": {
                    "kind": "oneharness",
                    "bin": context.oneharness_bin,
                    "stream": agent.stream,
                    "control": ASK_FOR_CONTROL,
                },
                "judge": {"kind": "command", "command": command.command},
            }))
        }
    }
}

/// What one conversation side's resolved config carries beyond what its author
/// wrote.
///
/// Every field is a value that is *this member's*, not the config file's, and
/// that therefore cannot be exported: the process this crate now drives onejudge
/// in is shared with every other member of the graph.
#[derive(Debug, Clone, Copy, Default)]
struct Side<'a> {
    /// A `model` override to pair with the config's chain.
    model: Option<&'a str>,
    /// The member's approval mode.
    mode: Option<&'a str>,
    /// The member's scratch directory, stamped as the ownership evidence every
    /// harness process this side starts carries.
    scratch: Option<&'a Path>,
}

/// Resolve one side's oneharness config, stamping this member's own values into
/// it.
///
/// Returns the config text and, when a persona was named, its label.
fn harness_side(
    config: &ConfigRef,
    persona_ref: Option<&ConfigRef>,
    side: Side<'_>,
    context: &Context<'_>,
    resolver: &mut Resolver,
) -> Result<(String, Option<String>), Error> {
    let text = resolver.resolve(config, context.graph_dir)?.content.clone();
    let label = match persona_ref {
        Some(_) => load_persona(persona_ref, context, resolver)?.1,
        None => None,
    };
    Ok((stamp_side(&text, &config.0, side)?, label))
}

/// Stamp one side's own values into its resolved config.
///
/// The rest of the operator's file — comments included — stays byte-identical,
/// and a side with nothing to stamp is returned untouched.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when the config is not TOML, or when a `model` breaks
/// the pairing rule — see [`stamp_model`].
fn stamp_side(config: &str, origin: &str, side: Side<'_>) -> Result<String, Error> {
    let stamped = match side.model {
        Some(model) => stamp_model(config, origin, model)?,
        None => config.to_string(),
    };
    if side.mode.is_none() && side.scratch.is_none() {
        return Ok(stamped);
    }
    let mut document: DocumentMut = stamped
        .parse()
        .map_err(|err| Error::InvalidConfig(format!("{origin} is not valid TOML: {err}")))?;
    if let Some(mode) = side.mode {
        document["mode"] = toml_edit::value(mode);
    }
    if let Some(scratch) = side.scratch {
        // oneharness's `[env]` is the environment it gives every harness process
        // it starts, which is where this stamp has to land: the kernel fixes an
        // environment at `exec`, so it is evidence a descendant cannot shed and
        // one that still answers for its member after its parent is gone.
        let env = document
            .entry("env")
            .or_insert(Item::Table(toml_edit::Table::new()));
        let table = env.as_table_mut().ok_or_else(|| {
            Error::InvalidConfig(format!(
                "{origin}: `env` must be a table of environment values"
            ))
        })?;
        table[crate::scratch::SCRATCH_ENV] = toml_edit::value(scratch.display().to_string());
    }
    Ok(document.to_string())
}

/// Load a member's persona, from the shipped catalog or a ref.
fn load_persona(
    reference: Option<&ConfigRef>,
    context: &Context<'_>,
    resolver: &mut Resolver,
) -> Result<(Persona, Option<String>), Error> {
    let Some(reference) = reference else {
        return Ok((Persona::default(), None));
    };
    let (document, origin) = persona_document(reference, context, resolver)?;
    let persona = Persona::parse(&document, &origin)?;
    let errors = persona.validate();
    if !errors.is_empty() {
        return Err(Error::InvalidConfig(format!(
            "{origin}: {}",
            errors.join("; ")
        )));
    }
    let label = persona.label().map(str::to_string).or_else(|| {
        Path::new(&reference.0)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
    });
    Ok((persona, label))
}

/// The document one `persona:` ref names, and the origin to report it under.
///
/// A ref that parses as a persona *name* — `engineer`, or the slash-qualified
/// `crozier/crozier-corpus` — is a catalog lookup; anything else is the path or
/// URL it has always been, and `./roles/lead.yaml` is not a name. That split is
/// what makes an operator's own catalog dispatchable without taking a path ref
/// away from anyone.
///
/// A name in the graph's catalog **and** in the shipped set is refused rather
/// than resolved. Either answer would be silent: a graph would depend on which
/// catalog this build happens to prefer, and today's preference for the shipped
/// one shadows an operator's file of the same name without a word. The explicit
/// selection is a path ref, which reaches a file whatever it is called.
fn persona_document(
    reference: &ConfigRef,
    context: &Context<'_>,
    resolver: &mut Resolver,
) -> Result<(String, String), Error> {
    let name = &reference.0;
    if persona::is_persona_name(name) {
        match (catalog_file(name, context)?, persona::shipped(name)) {
            (Some(path), Some(_)) => {
                return Err(Error::InvalidConfig(format!(
                    "persona {name:?} names both {} in this graph's catalog and one this crate \
                     ships, and which of them a member runs under must not be this build's to \
                     decide: rename yours, or name it by path (`persona: {}`)",
                    path.display(),
                    path.display()
                )))
            }
            (Some(path), None) => {
                let reference = ConfigRef(path.display().to_string());
                let resolved = resolver.resolve(&reference, None)?;
                return Ok((resolved.content.clone(), reference.0));
            }
            (None, Some(document)) => return Ok((document.to_string(), name.clone())),
            // A graph that named a catalog meant a name to be looked up in it,
            // so a miss is refused with both catalogs named rather than falling
            // through to read a file called `crozier/crozier-corpus`.
            (None, None) => {
                if let Some(root) = catalog_root(context)? {
                    return Err(Error::InvalidConfig(format!(
                        "persona {name:?}: this graph's catalog ({}) holds no {name}.yaml, and it \
                         is not one this crate ships ({}). Name a file by path to use one from \
                         anywhere else.",
                        root.display(),
                        persona::shipped_names()
                            .into_iter()
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                }
            }
        }
    }
    let resolved = resolver.resolve(reference, context.graph_dir)?;
    Ok((resolved.content.clone(), reference.0.clone()))
}

/// This graph's persona catalog, resolved against the graph document the way
/// every other relative ref in it is.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when the graph named a catalog that is not a
/// directory this run can read. Refused rather than treated as an empty catalog,
/// because an empty one is indistinguishable from a correct lookup that missed:
/// a typo in the path, or a directory this process cannot search, would send
/// every name a member gives straight back to the shipped personas, which is the
/// silent shadowing this catalog exists to end.
///
/// Opened rather than asked about: a metadata check answers what a path *is*,
/// and what this needs to know is whether the catalog can be read at all.
fn catalog_root(context: &Context<'_>) -> Result<Option<PathBuf>, Error> {
    let Some(root) = context.personas else {
        return Ok(None);
    };
    let root = match context.graph_dir {
        Some(graph_dir) if root.is_relative() => graph_dir.join(root),
        _ => root.to_path_buf(),
    };
    if let Err(err) = std::fs::read_dir(&root) {
        return Err(Error::InvalidConfig(format!(
            "this graph's persona catalog ({}) is not a directory this run can read ({err}), so \
             every name a member gives would resolve to a shipped persona or to nothing",
            root.display()
        )));
    }
    Ok(Some(root))
}

/// The catalog file a persona name points at, when this graph named a catalog
/// and the file is there.
///
/// The name has already been through [`persona::is_persona_name`], which is what
/// keeps a slash-qualified one inside the catalog it is joined onto.
///
/// # Errors
///
/// [`Error::InvalidConfig`] for a catalog root that is not readable — see
/// [`catalog_root`] — and for an entry the filesystem refused to describe. Only
/// *absence* is a miss: an entry this process may not stat is a persona that may
/// well be there, and reading it as absent would resolve the member to a shipped
/// role instead of saying what went wrong.
fn catalog_file(name: &str, context: &Context<'_>) -> Result<Option<PathBuf>, Error> {
    let Some(root) = catalog_root(context)? else {
        return Ok(None);
    };
    let file = root.join(format!("{name}.yaml"));
    match std::fs::metadata(&file) {
        Ok(found) if found.is_file() => Ok(Some(file)),
        // A directory by that name is not a persona, and neither is a device.
        Ok(_) => Ok(None),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::InvalidConfig(format!(
            "persona {name:?}: cannot read {} from this graph's catalog: {err}",
            file.display()
        ))),
    }
}

/// Stamp `model` into every per-harness section of one side's config.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when the config is not TOML, declares no chain, or
/// declares one spanning more than one harness family — the pairing rule
/// `docs/contract.md` states, checked here, before anything is launched. One
/// model applies to whichever candidate the chain settles on, and a fallback
/// chain falls through only a candidate that could not run at all — never a task
/// failure — so an unpaired model would reach a candidate of another provider
/// and kill the member on a rejection instead of degrading.
pub fn stamp_model(config: &str, origin: &str, model: &str) -> Result<String, Error> {
    let mut document: DocumentMut = config
        .parse()
        .map_err(|err| Error::InvalidConfig(format!("{origin} is not valid TOML: {err}")))?;
    let families = harness_families(&document, origin)?;
    if families.len() > 1 {
        let named: Vec<&str> = families.iter().copied().collect();
        return Err(Error::InvalidConfig(format!(
            "model {model:?}: {origin} declares a chain spanning {}, and one model cannot name \
             a model of each. A model override must be paired with a config whose chain is one \
             harness family.",
            named.join(", ")
        )));
    }
    let family = families
        .iter()
        .next()
        .copied()
        .unwrap_or_default()
        .to_string();
    let harness = document
        .entry("harness")
        .or_insert(Item::Table(toml_edit::Table::new()));
    let table = harness.as_table_mut().ok_or_else(|| {
        Error::InvalidConfig(format!(
            "{origin}: `harness` must be a table of per-harness settings"
        ))
    })?;
    table.set_implicit(true);
    let section = table
        .entry(&family)
        .or_insert(Item::Table(toml_edit::Table::new()));
    let section = section.as_table_mut().ok_or_else(|| {
        Error::InvalidConfig(format!("{origin}: `harness.{family}` must be a table"))
    })?;
    section["model"] = toml_edit::value(model);
    Ok(document.to_string())
}

/// The harness families one config's chain names — `claude-code:alternate2` is
/// the `claude-code` family.
fn harness_families<'a>(
    document: &'a DocumentMut,
    origin: &str,
) -> Result<BTreeSet<&'a str>, Error> {
    let chain = document
        .get("harnesses")
        .and_then(Item::as_array)
        .ok_or_else(|| {
            Error::InvalidConfig(format!(
                "{origin} declares no `harnesses` chain, so there is nothing a model could be \
                 paired with"
            ))
        })?;
    let mut families = BTreeSet::new();
    for value in chain {
        let identity = value.as_str().ok_or_else(|| {
            Error::InvalidConfig(format!(
                "{origin}: every entry in `harnesses` must be a string"
            ))
        })?;
        families.insert(identity.split(':').next().unwrap_or(identity));
    }
    if families.is_empty() {
        return Err(Error::InvalidConfig(format!(
            "{origin} declares an empty `harnesses` chain, so there is nothing a model could be \
             paired with"
        )));
    }
    Ok(families)
}

const TASK_TOKEN: &str = "{task}";

/// The whole escape mechanism, and this exact spelling on purpose.
///
/// A general `{{`-doubling rule would change what documents already written say;
/// this one cannot, because a document carrying `{{task}}` carries `{task}` too
/// and so is already in the set expansion touches.
const ESCAPED_TASK_TOKEN: &str = "{{task}}";

/// One member's own task prose, with the run's `--task` interpolated into it.
///
/// A run that supplied none expands the token to nothing rather than refusing: a
/// member carrying its own task is the one shape that never needed a `--task`,
/// and a token demanding one would take that away.
fn expand_task(template: &str, given: Option<&str>) -> String {
    let mut expanded = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(brace) = rest.find('{') {
        expanded.push_str(&rest[..brace]);
        rest = &rest[brace..];
        if let Some(tail) = rest.strip_prefix(ESCAPED_TASK_TOKEN) {
            expanded.push_str(TASK_TOKEN);
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix(TASK_TOKEN) {
            expanded.push_str(given.unwrap_or_default());
            rest = tail;
        } else {
            // Any other brace is a brace. `{` is one ASCII byte, so this index is
            // a character boundary.
            expanded.push('{');
            rest = &rest[1..];
        }
    }
    expanded.push_str(rest);
    expanded
}

fn member_task(own: Option<&str>, context: &Context<'_>) -> Result<String, Error> {
    match own {
        // Prose is prose: under the schema that predates the token, the six
        // characters `{task}` said themselves, and a document is entitled to keep
        // meaning what it meant.
        Some(own) if context.task_text == TaskText::Literal => Ok(own.to_string()),
        Some(own) => Ok(expand_task(own, context.task)),
        None => task(context),
    }
}

/// The task prose the run supplied, or the refusal for a member that needs one.
fn task(context: &Context<'_>) -> Result<String, Error> {
    context.task.map(str::to_string).ok_or_else(|| {
        Error::InvalidConfig(
            "no task: supply one with --task/--task-file, or give the member its own `task`"
                .to_string(),
        )
    })
}

/// The directory one member is told to work in: its own when it named one, and
/// the graph's when it did not.
///
/// **Both member kinds**, and by the same call. A single-sided member carries it
/// to `oneharness run --cwd` on the argv this module builds; a two-party one
/// carries it on [`JudgeLaunch::worktree`], which onejudge puts on the same flag.
/// Only a `kind: oneharness` member can name a `dir` of its own — that is
/// `docs/contract.md`'s scoping rather than this function's — so the two-party
/// call passes `None` and takes the graph's.
///
/// A member that named none gets `context.dir` **exactly as the run was given
/// it**, relative or not, because that is what this crate has always passed to
/// `oneharness run --cwd` and a member with no `dir` behaves as it did before.
///
/// A member's own `dir` is resolved rather than passed through, and the
/// difference is not tidiness. The value goes to `--cwd` on a child spawned with
/// its working directory set to the *member's scratch*, so a relative path left
/// as written would resolve against a generated directory the graph's author has
/// never seen — the graph-wide `--dir`'s own sharp edge, which a new field has no
/// reason to inherit. So a relative `dir` is joined onto the graph's directory —
/// `dir: ./api` is the member working one level inside the graph's own — and the
/// result is made absolute against this process's working directory. Absolute
/// rather than canonical: the directory need not exist yet, and no symlink an
/// operator wrote is rewritten under them.
fn member_dir(named: Option<&Path>, context: &Context<'_>) -> PathBuf {
    let Some(named) = named else {
        return context.dir.to_path_buf();
    };
    let joined = context.dir.join(named);
    std::path::absolute(&joined).unwrap_or(joined)
}

/// Write one generated file, creating the directory that holds it.
fn write(path: &Path, content: &str) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            Error::InvalidConfig(format!("cannot create {}: {err}", parent.display()))
        })?;
    }
    std::fs::write(path, content)
        .map_err(|err| Error::InvalidConfig(format!("cannot write {}: {err}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_FAMILY: &str = concat!(
        "# an operator's own comment\n",
        "run_mode = \"fallback\"\n",
        "harnesses = [\"claude-code:alternate\", \"claude-code:primary\"]\n",
        "\n[harness.claude-code]\nmodel = \"claude-opus-5\"\n",
    );

    /// The stamp lands on the family's own section, and the rest of the
    /// operator's file — comments included — is byte-identical.
    #[test]
    fn a_model_is_stamped_into_the_family_it_was_paired_with() {
        let stamped = stamp_model(ONE_FAMILY, "oneharness.toml", "claude-sonnet-5").unwrap();
        assert!(stamped.contains("model = \"claude-sonnet-5\""));
        assert!(!stamped.contains("claude-opus-5"));
        assert!(stamped.starts_with("# an operator's own comment\n"));
        assert!(stamped.contains("run_mode = \"fallback\""));
    }

    /// A config with no per-harness section yet gains one, so a chain that never
    /// pinned a model still takes the override.
    #[test]
    fn a_config_with_no_per_harness_section_gains_one() {
        let stamped = stamp_model("harnesses = [\"codex\"]\n", "c.toml", "gpt-5.5").unwrap();
        assert!(stamped.contains("[harness.codex]"), "{stamped}");
        assert!(stamped.contains("model = \"gpt-5.5\""), "{stamped}");
    }

    /// The pairing rule: a chain spanning two families refuses before launch,
    /// naming both.
    #[test]
    fn a_chain_spanning_two_families_refuses_the_model() {
        let err = stamp_model(
            "harnesses = [\"claude-code:alternate\", \"codex\"]\n",
            "oneharness.toml",
            "claude-opus-5",
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("claude-code, codex"), "{message}");
        assert!(message.contains("one harness family"), "{message}");
    }

    /// A config that declares no chain, an empty one, or a non-string entry has
    /// nothing to pair a model with, and says so.
    #[test]
    fn a_config_with_no_usable_chain_refuses_the_model() {
        for (config, expected) in [
            ("run_mode = \"fallback\"\n", "declares no `harnesses` chain"),
            ("harnesses = []\n", "declares an empty `harnesses` chain"),
            ("harnesses = [1]\n", "must be a string"),
            ("harnesses = [\"codex\"]\nharness = 3\n", "must be a table"),
            ("not = toml = here\n", "not valid TOML"),
        ] {
            let err = stamp_model(config, "c.toml", "m").unwrap_err();
            assert!(err.to_string().contains(expected), "{config:?}: {err}");
        }
    }

    /// A model value is forwarded unchecked — the contract's deliberate asymmetry
    /// with the identity, which the config alone selects.
    #[test]
    fn the_model_value_itself_is_never_checked() {
        let stamped = stamp_model(ONE_FAMILY, "c.toml", "no-such-model-anywhere").unwrap();
        assert!(stamped.contains("no-such-model-anywhere"));
    }

    /// One workspace with a graph's refs on disk, for the builders below.
    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("oneharness.toml"), ONE_FAMILY).expect("chain");
        std::fs::write(
            dir.path().join("base.yaml"),
            concat!(
                "provider:\n  kind: oneharness\n",
                "agent:\n  instructions: preamble\n",
                "user:\n  persona: lead\n  done_when: done\n  max_turns: 4\n",
            ),
        )
        .expect("base");
        dir
    }

    /// A [`Context`] over `dir`, under the schema this build writes.
    fn context<'a>(dir: &'a Path, scratch: &'a Path) -> Context<'a> {
        Context {
            dir,
            scratch,
            graph_dir: Some(dir),
            personas: None,
            task: Some("do the thing"),
            task_text: TaskText::Template,
            session: "s",
            oneharness_bin: "oneharness",
        }
    }

    /// A member's own `max_turns` beats the merged config's, and the effective
    /// config is written where onejudge is told to read it.
    #[test]
    fn a_members_own_turn_cap_reaches_the_effective_config() {
        let dir = workspace();
        let scratch = dir.path().join("scratch");
        let member: Member = serde_norway::from_str(concat!(
            "kind: onejudge\nbase_config: ./base.yaml\nmode: bypass\nmax_turns: 9\n",
            "agent: {oneharness_config: ./oneharness.toml}\n",
            "judge: {oneharness_config: ./oneharness.toml}\n",
        ))
        .expect("a member");
        let invocation = build(
            &member,
            &context(dir.path(), &scratch),
            &mut Resolver::new(),
        )
        .expect("built");
        assert_eq!(invocation.kind, crate::member::Kind::Onejudge);
        // Nothing exported: this member is driven in a process it shares with
        // every other one, so its own values are in its own files.
        assert!(invocation.env.is_empty(), "{:?}", invocation.env);
        let effective =
            std::fs::read_to_string(scratch.join(ONEJUDGE_CONFIG_FILE)).expect("config");
        assert!(effective.contains("max_turns: 9"), "{effective}");
        assert!(effective.contains("system_prompt"), "{effective}");
        // Both sides carry the member's mode and its ownership stamp, which is
        // what replaces exporting them.
        //
        // Read back as TOML, not searched for as text: oneharness *parses* this
        // file, and a path decides how it is spelled. A Windows scratch carries
        // backslashes, which `toml_edit` correctly renders as a literal string
        // (`'C:\…'`) rather than escaping every one of them into a basic string —
        // so a text search for `KEY = "value"` asserted this platform's rendering
        // rather than the value oneharness would read on either.
        for side in [AGENT_CONFIG_FILE, JUDGE_CONFIG_FILE] {
            let config = std::fs::read_to_string(scratch.join(side)).expect(side);
            let document: DocumentMut = config.parse().expect(side);
            assert_eq!(
                document["mode"].as_str(),
                Some("bypass"),
                "{side}: {config}"
            );
            assert_eq!(
                document["env"][crate::scratch::SCRATCH_ENV].as_str(),
                Some(scratch.display().to_string().as_str()),
                "{side}: {config}"
            );
            // And the operator's own file is otherwise untouched — comments
            // included, which is why this half stays a check on the raw text.
            assert!(
                config.starts_with("# an operator's own comment\n"),
                "{config}"
            );
        }
        // The member's own task beats the run's, because a member that carries
        // one is asking for that task rather than the graph's.
        assert_eq!(judge_launch(&invocation).task, "do the thing");
        // The member works in the directory the graph was given, and is pinned by
        // the stamped config in its scratch — two values now, because one field
        // doing both is what kept `--dir` from reaching this member's harness.
        assert_eq!(judge_launch(&invocation).worktree, dir.path());
        assert_eq!(
            judge_launch(&invocation).agent_config,
            scratch.join(AGENT_CONFIG_FILE)
        );
    }

    /// The two-party launch of `invocation`, or a panic naming what it was.
    fn judge_launch(invocation: &Invocation) -> &JudgeLaunch {
        match &invocation.launch {
            Launch::Library(launch) => launch,
            other => panic!("expected a library launch, got {other:?}"),
        }
    }

    /// A member with its own `task` uses it, and one with a persona file takes
    /// its label from the file when the persona names none.
    #[test]
    fn a_member_takes_its_own_task_and_its_files_name() {
        let dir = workspace();
        std::fs::write(
            dir.path().join("lead.yaml"),
            "agent:\n  instructions: role\nuser:\n  persona: supervisor\n",
        )
        .expect("persona");
        let scratch = dir.path().join("scratch");
        let member: Member = serde_norway::from_str(concat!(
            "kind: onejudge\nbase_config: ./base.yaml\nmode: bypass\n",
            "task: its own task\npersona: ./lead.yaml\n",
            "agent: {oneharness_config: ./oneharness.toml}\n",
            "judge: {oneharness_config: ./oneharness.toml}\n",
        ))
        .expect("a member");
        let invocation = build(
            &member,
            &context(dir.path(), &scratch),
            &mut Resolver::new(),
        )
        .expect("built");
        assert_eq!(judge_launch(&invocation).task, "its own task");
        assert_eq!(invocation.persona.as_deref(), Some("lead"));
    }

    /// A single-sided member resolves its own persona, and a run with no task at
    /// all is refused rather than launched with nothing to do.
    #[test]
    fn a_single_sided_member_carries_its_persona_and_needs_a_task() {
        let dir = workspace();
        let scratch = dir.path().join("scratch");
        let member: Member = serde_norway::from_str(
            "kind: oneharness\noneharness_config: ./oneharness.toml\npersona: reviewer\n",
        )
        .expect("a member");
        let invocation = build(
            &member,
            &context(dir.path(), &scratch),
            &mut Resolver::new(),
        )
        .expect("built");
        assert_eq!(invocation.kind, crate::member::Kind::Oneharness);
        assert_eq!(invocation.persona.as_deref(), Some("reviewer"));

        let mut taskless = context(dir.path(), &scratch);
        taskless.task = None;
        let err = build(&member, &taskless, &mut Resolver::new()).unwrap_err();
        assert!(err.to_string().contains("no task"), "{err}");
    }

    /// A graph's own catalog is what makes an operator's personas dispatchable
    /// by name — a bare one and a slash-qualified one alike — and a name that is
    /// in that catalog *and* in the shipped set is refused rather than quietly
    /// resolved to either.
    #[test]
    fn a_graph_local_catalog_is_reachable_by_name() {
        let dir = workspace();
        let catalog = dir.path().join("personas");
        std::fs::create_dir_all(catalog.join("crozier")).expect("catalog");
        std::fs::write(
            catalog.join("crozier/crozier-corpus.yaml"),
            "agent:\n  instructions: corpus role\nuser:\n  persona: corpus supervisor\n",
        )
        .expect("a catalog persona");
        let scratch = dir.path().join("scratch");
        let member = |named: &str| -> Member {
            serde_norway::from_str(&format!(
                "kind: oneharness\noneharness_config: ./oneharness.toml\npersona: {named}\n"
            ))
            .expect("a member")
        };
        let mut catalogued = context(dir.path(), &scratch);
        catalogued.personas = Some(Path::new("personas"));

        let invocation = build(
            &member("crozier/crozier-corpus"),
            &catalogued,
            &mut Resolver::new(),
        )
        .expect("the catalog persona resolves");
        assert_eq!(invocation.persona.as_deref(), Some("crozier-corpus"));
        // And it is in the run record as the file it was read from, so an audit
        // says which document the member actually ran under.
        assert!(
            invocation
                .refs
                .iter()
                .any(|resolved| resolved.origin.ends_with("crozier-corpus.yaml")),
            "{:?}",
            invocation.refs
        );

        // A shipped name still resolves through a catalog that does not hold it.
        assert_eq!(
            build(&member("reviewer"), &catalogued, &mut Resolver::new())
                .expect("a shipped persona still resolves")
                .persona
                .as_deref(),
            Some("reviewer")
        );

        // Until the catalog holds one too, and then neither wins silently.
        std::fs::write(
            catalog.join("reviewer.yaml"),
            "agent:\n  instructions: our reviewer\nuser:\n  persona: ours\n",
        )
        .expect("a colliding persona");
        let err = build(&member("reviewer"), &catalogued, &mut Resolver::new()).unwrap_err();
        assert!(err.to_string().contains("names both"), "{err}");
        assert!(err.to_string().contains("reviewer.yaml"), "{err}");

        // A name in neither is refused with both catalogs named, rather than
        // read as a file called `nobody`.
        let err = build(&member("nobody"), &catalogued, &mut Resolver::new()).unwrap_err();
        assert!(err.to_string().contains("holds no nobody.yaml"), "{err}");
        assert!(err.to_string().contains("engineer"), "{err}");

        // A path ref is how an operator names their own file whatever it
        // collides with, and it is not a catalog lookup at all.
        assert_eq!(
            build(
                &member("./personas/reviewer.yaml"),
                &catalogued,
                &mut Resolver::new()
            )
            .expect("a path ref reaches the file")
            .persona
            .as_deref(),
            Some("reviewer")
        );
    }

    /// A graph that names no catalog resolves exactly as it did before there
    /// were any: a shipped name, or a ref.
    #[test]
    fn a_graph_with_no_catalog_resolves_the_shipped_personas_and_refs() {
        let dir = workspace();
        std::fs::create_dir_all(dir.path().join("personas")).expect("catalog");
        std::fs::write(
            dir.path().join("personas/reviewer.yaml"),
            "agent:\n  instructions: our reviewer\nuser:\n  persona: ours\n",
        )
        .expect("a persona no graph named");
        let scratch = dir.path().join("scratch");
        let member: Member = serde_norway::from_str(
            "kind: oneharness\noneharness_config: ./oneharness.toml\npersona: reviewer\n",
        )
        .expect("a member");
        // The directory is there, unnamed, and so is not consulted — a graph
        // opts into its own catalog rather than acquiring one by being near it.
        let invocation = build(
            &member,
            &context(dir.path(), &scratch),
            &mut Resolver::new(),
        )
        .expect("the shipped persona resolves");
        assert_eq!(invocation.persona.as_deref(), Some("reviewer"));
    }

    /// A single-sided member's own `task` and `dir` are what oneharness is told,
    /// and a member carrying neither is told the graph's — byte for byte what it
    /// was told before either field existed.
    ///
    /// Asserted on the argv because that is the whole of what this crate decides
    /// for a single-sided member: `--prompt` is the job it is given and `--cwd`
    /// is where it does it, and a member whose job differs from its graph's is
    /// one where these two differ from the run's.
    #[test]
    fn a_single_sided_member_is_told_its_own_task_and_directory() {
        let dir = workspace();
        let scratch = dir.path().join("scratch");
        let graph_wide: Member =
            serde_norway::from_str("kind: oneharness\noneharness_config: ./oneharness.toml\n")
                .expect("a member");
        let own: Member = serde_norway::from_str(concat!(
            "kind: oneharness\noneharness_config: ./oneharness.toml\n",
            "task: write one status update\ndir: ./api\n",
        ))
        .expect("a member");

        let default = process_args(&graph_wide, &context(dir.path(), &scratch));
        assert_eq!(flag(&default, "--prompt").as_deref(), Some("do the thing"));
        assert_eq!(
            flag(&default, "--cwd").as_deref(),
            Some(dir.path().display().to_string().as_str()),
            "a member with no directory of its own must be told the graph's, unchanged"
        );

        let job = process_args(&own, &context(dir.path(), &scratch));
        assert_eq!(
            flag(&job, "--prompt").as_deref(),
            Some("write one status update"),
            "a member carrying its own task was handed the graph's"
        );
        // Joined onto the graph's directory and made absolute, so the value
        // reaches oneharness meaning the same thing wherever the child is
        // spawned — its working directory is the member's scratch, not this one.
        assert_eq!(
            flag(&job, "--cwd").map(std::path::PathBuf::from),
            Some(dir.path().join("api")),
        );

        // An absolute `dir` is used exactly as written: a member working
        // somewhere that is not below the graph's directory at all is the case a
        // scratch-dwelling member is.
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let absolute: Member = serde_norway::from_str(&format!(
            "kind: oneharness\noneharness_config: ./oneharness.toml\ndir: {}\n",
            elsewhere.path().display()
        ))
        .expect("a member");
        assert_eq!(
            flag(
                &process_args(&absolute, &context(dir.path(), &scratch)),
                "--cwd"
            )
            .map(std::path::PathBuf::from),
            Some(elsewhere.path().to_path_buf()),
        );

        // And a member with its own task needs no `--task` from the run at all,
        // which is the whole point: its job is not the graph's.
        let mut taskless = context(dir.path(), &scratch);
        taskless.task = None;
        assert_eq!(
            flag(&process_args(&own, &taskless), "--prompt").as_deref(),
            Some("write one status update"),
        );
        let err = build(&graph_wide, &taskless, &mut Resolver::new()).unwrap_err();
        assert!(err.to_string().contains("no task"), "{err}");
    }

    /// A single-sided member carrying `{task}` is handed the run's task where it
    /// named it, and everything else about the field is exactly as it was.
    ///
    /// Read off the argv, because `--prompt` *is* what this crate decides for a
    /// single-sided member: the two compatibility guarantees below — a member with
    /// no task of its own, and one whose task names no token — are only worth
    /// anything as statements about the value that reaches oneharness.
    #[test]
    fn a_members_own_task_takes_the_runs_where_it_names_it() {
        let dir = workspace();
        let scratch = dir.path().join("scratch");
        let given = context(dir.path(), &scratch);
        let mut taskless = context(dir.path(), &scratch);
        taskless.task = None;

        for (own, expanded, alone) in [
            // The token is the whole task, and the token inside prose of the
            // member's own — the shape a pacemaker's `task:` actually takes.
            ("{task}", "do the thing", ""),
            (
                "{task}\n\nReport it, and nothing else.",
                "do the thing\n\nReport it, and nothing else.",
                "\n\nReport it, and nothing else.",
            ),
            // No token: the member's task replaces the run's outright, which is
            // what the field has always meant and what every document already
            // written depends on.
            (
                "write one status update",
                "write one status update",
                "write one status update",
            ),
            // The one escape, and every other brace left alone.
            ("{{task}}", "{task}", "{task}"),
            (
                "{task} {{task}} {other} {",
                "do the thing {task} {other} {",
                " {task} {other} {",
            ),
        ] {
            assert_eq!(
                flag(&process_args(&single_sided(own), &given), "--prompt").as_deref(),
                Some(expanded),
                "{own:?}"
            );
            // And with no `--task` at all the token expands to nothing rather
            // than refusing: a member carrying its own task is the one shape that
            // never needed a run's.
            assert_eq!(
                flag(&process_args(&single_sided(own), &taskless), "--prompt").as_deref(),
                Some(alone),
                "{own:?} in a run with no task"
            );
        }

        // And under every schema that predates the token, a member's own task is
        // the prose it has always been — the six characters said themselves
        // there, and a document written then keeps meaning what it meant.
        for older in crate::config::FIRST_SCHEMA_VERSION..crate::config::FIRST_TASK_TOKEN_VERSION {
            let mut before = context(dir.path(), &scratch);
            before.task_text = TaskText::under(older);
            assert_eq!(
                flag(
                    &process_args(&single_sided("{task}\n\nand report it"), &before),
                    "--prompt"
                )
                .as_deref(),
                Some("{task}\n\nand report it"),
                "version {older} expanded a token that schema never had"
            );
        }

        // The run's own task is prose, not a template: a member that named no
        // task of its own is handed it byte for byte, token or no token.
        let mut literal = context(dir.path(), &scratch);
        literal.task = Some("mind the {task} in this sentence");
        let graph_wide: Member =
            serde_norway::from_str("kind: oneharness\noneharness_config: ./oneharness.toml\n")
                .expect("a member");
        assert_eq!(
            flag(&process_args(&graph_wide, &literal), "--prompt").as_deref(),
            Some("mind the {task} in this sentence"),
        );
    }

    /// A two-party member's own `task` takes the run's on the same terms — one
    /// field, one rule, whichever kind of member carries it.
    #[test]
    fn a_two_party_members_own_task_takes_the_runs_the_same_way() {
        let dir = workspace();
        let scratch = dir.path().join("scratch");
        let member: Member = serde_norway::from_str(concat!(
            "kind: onejudge\nbase_config: ./base.yaml\nmode: bypass\n",
            "task: \"{task}\\n\\nand judge it\"\n",
            "agent: {oneharness_config: ./oneharness.toml}\n",
            "judge: {oneharness_config: ./oneharness.toml}\n",
        ))
        .expect("a member");
        let invocation = build(
            &member,
            &context(dir.path(), &scratch),
            &mut Resolver::new(),
        )
        .expect("built");
        assert_eq!(
            judge_launch(&invocation).task,
            "do the thing\n\nand judge it"
        );
    }

    /// One single-sided member carrying `task`, built rather than parsed so a
    /// template's braces and newlines need no YAML quoting to survive.
    fn single_sided(task: &str) -> Member {
        Member::Oneharness(crate::config::OneharnessMember {
            oneharness_config: ConfigRef("./oneharness.toml".to_string()),
            persona: None,
            task: Some(task.to_string()),
            dir: None,
            schedule: None,
            deps: Vec::new(),
        })
    }

    /// The argv a single-sided member is launched with.
    fn process_args(member: &Member, context: &Context<'_>) -> Vec<String> {
        let invocation = build(member, context, &mut Resolver::new()).expect("built");
        match invocation.launch {
            Launch::Process { args, .. } => args,
            other => panic!("expected a child process, got {other:?}"),
        }
    }

    /// One argv flag's value.
    fn flag(args: &[String], name: &str) -> Option<String> {
        args.iter()
            .position(|arg| arg == name)
            .and_then(|at| args.get(at + 1))
            .cloned()
    }

    /// A command judge composes onejudge's `split` provider, and an empty
    /// command is refused rather than written into a config nothing can run.
    #[test]
    fn a_command_judge_composes_the_split_provider() {
        let dir = workspace();
        let scratch = dir.path().join("scratch");
        let agent: AgentSide =
            serde_norway::from_str("oneharness_config: ./oneharness.toml\n").expect("a side");
        let judge = JudgeSide::Command(crate::config::JudgeCommand {
            command: vec!["my-provider".into(), "--flag".into()],
        });
        let block = provider_block(
            &judge,
            &agent,
            Some("bypass"),
            &context(dir.path(), &scratch),
            &mut Resolver::new(),
        )
        .expect("a provider");
        assert_eq!(block["kind"], serde_json::json!("split"));
        assert_eq!(
            block["judge"]["command"][0],
            serde_json::json!("my-provider")
        );
        assert_eq!(block["skill"]["stream"], serde_json::json!(true));

        let empty = JudgeSide::Command(crate::config::JudgeCommand {
            command: Vec::new(),
        });
        let err = provider_block(
            &empty,
            &agent,
            Some("bypass"),
            &context(dir.path(), &scratch),
            &mut Resolver::new(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("needs a command to run"), "{err}");
    }

    /// A side with nothing of its own to carry is handed back untouched, and one
    /// whose config cannot hold the stamp says which key is wrong.
    ///
    /// The second half is what makes the refusal *pre-launch*: a member whose
    /// `mode` or ownership stamp could not be written would otherwise run with
    /// neither, and the run would only find out when a cancel reached nothing.
    #[test]
    fn a_side_with_nothing_to_carry_is_untouched_and_one_that_cannot_hold_it_says_so() {
        assert_eq!(
            stamp_side(ONE_FAMILY, "oneharness.toml", Side::default()).expect("untouched"),
            ONE_FAMILY,
            "a side with nothing to stamp must not be rewritten at all"
        );

        let err = stamp_side(
            "harnesses = [\"codex\"]\nenv = 3\n",
            "c.toml",
            Side {
                scratch: Some(Path::new("/state/run/members/worker")),
                ..Side::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("`env` must be a table"), "{err}");

        let err = stamp_side(
            "not = toml = here\n",
            "c.toml",
            Side {
                mode: Some("bypass"),
                ..Side::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("not valid TOML"), "{err}");
    }

    /// A scratch path carrying backslashes round-trips through the stamp with
    /// its value intact.
    ///
    /// The stamp is *evidence*: `crate::scratch` reads it back out of a live
    /// process's environment to decide whether that process belongs to this
    /// member, so a path that came back a single byte different would orphan
    /// every descendant it was supposed to name. A Windows scratch is exactly
    /// that path — `C:\Users\…` — and TOML has two ways to spell it, a basic
    /// string with every backslash escaped and a literal string with none.
    /// `toml_edit` picks the literal one, which is why this asserts the parsed
    /// value rather than the rendered line. Run on every platform, because the
    /// property is about the *value* and nothing here needs Windows to hold it.
    #[test]
    fn a_scratch_path_with_backslashes_round_trips_through_the_stamp() {
        let windows = r"C:\Users\RUNNER~1\AppData\Local\Temp\.tmpCuzqTi\scratch";
        let stamped = stamp_side(
            ONE_FAMILY,
            "oneharness.toml",
            Side {
                mode: Some("bypass"),
                scratch: Some(Path::new(windows)),
                ..Side::default()
            },
        )
        .expect("stamped");

        let document: DocumentMut = stamped.parse().expect("the stamped config is still TOML");
        assert_eq!(
            document["env"][crate::scratch::SCRATCH_ENV].as_str(),
            Some(windows),
            "the stamp did not survive being written and read back: {stamped}"
        );
        assert_eq!(document["mode"].as_str(), Some("bypass"), "{stamped}");
    }

    /// A config whose `harness` key is not a table of sections cannot take a
    /// stamp, and says which key is wrong.
    #[test]
    fn a_config_whose_harness_section_is_not_a_table_is_refused() {
        let err = stamp_model(
            "harnesses = [\"codex\"]\n[harness]\ncodex = 3\n",
            "c.toml",
            "gpt-5.5",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("`harness.codex` must be a table"),
            "{err}"
        );
    }

    /// A generated file that cannot be written names the path, because the run
    /// that could not write it is the one an operator has to fix.
    #[test]
    fn a_generated_file_that_cannot_be_written_names_its_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blocked = dir.path().join("not-a-directory");
        std::fs::write(&blocked, "").expect("write");
        let err = write(&blocked.join("child").join("x.toml"), "body").unwrap_err();
        assert!(err.to_string().contains("cannot create"), "{err}");
    }
}
