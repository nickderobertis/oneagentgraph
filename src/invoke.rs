//! Constructing one member's invocation.
//!
//! This is the whole of what `docs/contract.md` means by "constructs
//! onejudge/oneharness invocations": a member's refs are resolved, its persona is
//! merged onto its base, each side's oneharness config is written into the run's
//! own directory, and the argv is assembled. Nothing here selects a harness, a
//! model chain, or a fallback order — those live in the oneharness config files
//! a graph names, and this module only ever *carries* them.
//!
//! # How each side is pinned without a wrapper
//!
//! onejudge routes both conversation sides through one `provider.bin`. The judge
//! side is given `oneharness run --config <judge_config>`; the agent side is
//! given none, and relies on oneharness discovering `oneharness.toml` upward from
//! its own working directory. So the agent side is pinned by *placing* its
//! resolved config at `<member scratch>/oneharness.toml` and running `onejudge`
//! from there. The harness itself still works in the graph's `--dir`, because
//! onejudge passes that through as `oneharness run --cwd`.
//!
//! That is why a `model` override is stamped **into the resolved config** rather
//! than exported as `ONEHARNESS_MODEL`: a config's per-harness `model` beats that
//! variable, so exporting it would be a setting that silently loses. Stamping the
//! per-harness sections is also exactly what the contract's pairing rule makes
//! safe — a chain of one harness family has one set of sections to stamp.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;
use toml_edit::{DocumentMut, Item};

use crate::config::{AgentSide, ConfigRef, JudgeSide, Member, OnejudgeMember};
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
/// which beats both sides' config files, so one graph-level `mode` reaches the
/// agent side and the judge side alike without either resolved config being
/// rewritten behind its author's back.
pub const MODE_ENV: &str = "ONEHARNESS_MODE";

/// One member's invocation, ready to spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// Which program this is, because the two read their exit codes differently.
    pub kind: crate::member::Kind,
    /// The program to run.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// The directory to run it from. For a `kind: onejudge` member this is the
    /// member's own scratch directory, because that is what pins the agent side's
    /// oneharness config; the harness still works in the graph's `--dir`.
    pub cwd: PathBuf,
    /// The persona label to stamp on this member's events, when it has one.
    pub persona: Option<String>,
    /// What this member's invocation adds to the process environment, over and
    /// above the graph's own `env` block.
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
    /// The task prose, when the run supplied one.
    pub task: Option<&'a str>,
    /// The session name threaded across this member's turns.
    pub session: &'a str,
    /// The `onejudge` binary to run.
    pub onejudge_bin: &'a str,
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
    match member {
        Member::Onejudge(member) => onejudge(member, context, resolver),
        Member::Oneharness(member) => {
            let (config, persona_label) = harness_side(
                &member.oneharness_config,
                member.persona.as_ref(),
                None,
                context,
                resolver,
            )?;
            let path = context.scratch.join(AGENT_CONFIG_FILE);
            write(&path, &config)?;
            let mut args = vec![
                "run".to_string(),
                "--config".to_string(),
                path.display().to_string(),
                "--cwd".to_string(),
                context.dir.display().to_string(),
                "--events".to_string(),
                "--stream".to_string(),
                "--prompt".to_string(),
                task(context)?,
            ];
            args.retain(|arg| !arg.is_empty());
            Ok(Invocation {
                kind: crate::member::Kind::Oneharness,
                program: context.oneharness_bin.to_string(),
                args,
                cwd: context.scratch.to_path_buf(),
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

    let (agent_config, _) = harness_side(
        &member.agent.oneharness_config,
        None,
        member.agent.model.as_deref(),
        context,
        resolver,
    )?;
    let agent_path = context.scratch.join(AGENT_CONFIG_FILE);
    write(&agent_path, &agent_config)?;

    let provider = provider_block(&member.judge, &member.agent, context, resolver)?;
    let map = effective.as_object_mut().expect("merge returns a mapping");
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

    let prose = match member.task.as_deref() {
        Some(own) => own.to_string(),
        None => task(context)?,
    };

    Ok(Invocation {
        kind: crate::member::Kind::Onejudge,
        program: context.onejudge_bin.to_string(),
        args: vec![
            "run".to_string(),
            config_path.display().to_string(),
            "--format".to_string(),
            "json".to_string(),
            "--stream".to_string(),
            "--task".to_string(),
            prose,
        ],
        cwd: context.scratch.to_path_buf(),
        persona: label,
        env: vec![(MODE_ENV.to_string(), member.mode.clone())],
        refs: resolver.inventory(),
    })
}

/// The onejudge `provider` block for a two-party member.
///
/// A harness-backed judge is one `kind: oneharness` provider carrying both
/// sides. A command judge cannot be expressed that way, so the two sides are
/// split — which is exactly onejudge's `split` provider.
fn provider_block(
    judge: &JudgeSide,
    agent: &AgentSide,
    context: &Context<'_>,
    resolver: &mut Resolver,
) -> Result<Value, Error> {
    match judge {
        JudgeSide::Harness(harness) => {
            let (config, _) = harness_side(
                &harness.oneharness_config,
                None,
                harness.model.as_deref(),
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
                },
                "judge": {"kind": "command", "command": command.command},
            }))
        }
    }
}

/// Resolve one side's oneharness config, stamping a model override into it.
///
/// Returns the config text and, when a persona was named, its label.
fn harness_side(
    config: &ConfigRef,
    persona_ref: Option<&ConfigRef>,
    model: Option<&str>,
    context: &Context<'_>,
    resolver: &mut Resolver,
) -> Result<(String, Option<String>), Error> {
    let text = resolver.resolve(config, context.graph_dir)?.content.clone();
    let label = match persona_ref {
        Some(_) => load_persona(persona_ref, context, resolver)?.1,
        None => None,
    };
    let Some(model) = model else {
        return Ok((text, label));
    };
    Ok((stamp_model(&text, &config.0, model)?, label))
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
    let (document, origin) = match persona::shipped(&reference.0) {
        Some(document) => (document.to_string(), reference.0.clone()),
        None => {
            let resolved = resolver.resolve(reference, context.graph_dir)?;
            (resolved.content.clone(), reference.0.clone())
        }
    };
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

/// The task prose the run supplied, or the refusal for a member that needs one.
fn task(context: &Context<'_>) -> Result<String, Error> {
    context.task.map(str::to_string).ok_or_else(|| {
        Error::InvalidConfig(
            "no task: supply one with --task/--task-file, or give the member its own `task`"
                .to_string(),
        )
    })
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
}
