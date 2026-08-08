//! Personas: the delta a member layers over its onejudge base config.
//!
//! `docs/contract.md` gives a member an optional `persona` ref beside its
//! `base_config`, and the CLI a `persona new NAME` / `persona validate PATH`
//! pair. This module is the schema behind all three: what a persona may say,
//! how it merges onto a base, and what a scaffolded one looks like.
//!
//! A persona authors in this crate's own vocabulary — the role goes in
//! `agent.instructions` — and the merge translates that to onejudge's wire
//! schema (`system_prompt`). Keeping the two decoupled is deliberate: a schema
//! change on onejudge's side is absorbed here, in one place, rather than
//! rippling through every persona file anybody has written.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Error;

/// The template `persona new` scaffolds from.
pub const PERSONA_TEMPLATE: &str = include_str!("../personas/_template.yaml");

/// The personas this crate ships, as `(name, document)`.
///
/// They are general cross-repo roles, so a graph can name one without authoring
/// anything: `persona: engineer`. Each is validated by the suite through the
/// same [`Persona::validate`] a user's own file goes through.
pub const SHIPPED_PERSONAS: &[(&str, &str)] = &[
    ("docs-writer", include_str!("../personas/docs-writer.yaml")),
    ("engineer", include_str!("../personas/engineer.yaml")),
    ("planner", include_str!("../personas/planner.yaml")),
    ("researcher", include_str!("../personas/researcher.yaml")),
    ("reviewer", include_str!("../personas/reviewer.yaml")),
];

/// One persona document.
///
/// `deny_unknown_fields` at every level is the trust boundary: a persona is
/// external input, and a key this crate does not know is a typo that would
/// otherwise be silently dropped — leaving a role running under instructions
/// its author believed they had changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Persona {
    /// The role this persona gives the agent side.
    #[serde(default)]
    pub agent: PersonaAgent,
    /// How the supervisor reviews this kind of work.
    #[serde(default)]
    pub user: PersonaUser,
    /// Extra transcript checks for this role, replacing the base's if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evals: Option<Vec<Value>>,
}

/// A persona's name: one lowercase segment, or several separated by `/` for a
/// catalog under a directory.
///
/// A newtype because the name decides a *path* — `persona new` writes
/// `<name>.yaml` — so a value carrying `..`, a leading `/`, or a backslash would
/// write outside the catalog. Parsing it here means a document carrying one is
/// rejected when it is read, rather than after something has been created.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PersonaName(String);

impl PersonaName {
    /// The name as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for PersonaName {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, String> {
        if is_persona_name(name) {
            return Ok(Self(name.to_string()));
        }
        Err(format!(
            "invalid persona name {name:?}: use slash-separated segments of lowercase letters, \
             digits, and hyphens"
        ))
    }
}

impl<'de> Deserialize<'de> for PersonaName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for PersonaName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The agent half of a persona.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaAgent {
    /// A label for the role, stamped on every event as the `persona` label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<PersonaName>,
    /// The role, appended after the base config's shared preamble.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// The supervisor half of a persona.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaUser {
    /// The supervisor's stance for reviewing this role's work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// A role-specific completion bar, overriding the base's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_when: Option<String>,
    /// A role-specific turn cap, overriding the base's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
}

impl Persona {
    /// Parse one persona document.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidConfig`] when the document is not YAML, is not a mapping,
    /// or carries a key the schema does not name.
    pub fn parse(document: &str, origin: &str) -> Result<Self, Error> {
        serde_norway::from_str(document)
            .map_err(|err| Error::InvalidConfig(format!("{origin}: {err}")))
    }

    /// Every way this persona falls short of the delta contract, in one pass.
    ///
    /// Collected rather than short-circuited so one `persona validate` run tells
    /// an author everything they have to fix, not the first thing.
    #[must_use]
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        match self.agent.instructions.as_deref() {
            Some(text) if !text.trim().is_empty() => {}
            _ => errors
                .push("agent.instructions is required and must be a non-empty string".to_string()),
        }
        match self.user.persona.as_deref() {
            Some(text) if !text.trim().is_empty() => {}
            _ => errors.push("user.persona is required and must be a non-empty string".to_string()),
        }
        if self.user.max_turns == Some(0) {
            errors.push("user.max_turns must be a positive integer".to_string());
        }
        errors
    }

    /// The name to stamp on this member's events, when the persona gave one.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.agent.name.as_ref().map(PersonaName::as_str)
    }
}

/// Whether `name` is a usable persona name: one lowercase segment, or several
/// separated by `/` for a catalog under a directory.
///
/// This is what keeps `persona new` from writing outside its catalog — a name
/// carrying `..`, a leading `/`, or a backslash fails here rather than at the
/// filesystem, where a partial path may already have been created.
#[must_use]
pub fn is_persona_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('/').all(|segment| {
            !segment.is_empty()
                && segment.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                && segment
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// One shipped persona's document, by name.
#[must_use]
pub fn shipped(name: &str) -> Option<&'static str> {
    SHIPPED_PERSONAS
        .iter()
        .find(|(shipped, _)| *shipped == name)
        .map(|(_, document)| *document)
}

/// Merge a persona over a onejudge base config, producing the effective config.
///
/// The merge rules the reference implementation settled on, kept intact:
///
/// * `system_prompt` — the base's shared preamble (`agent.instructions`) with
///   the persona's role appended after it. Both are kept; neither replaces the
///   other, because the preamble is the standing bar and the role is what makes
///   this member different.
/// * `user` — persona keys override base keys.
/// * `evals` — the persona replaces the base's list when it brings one.
/// * `task` — dropped. It reaches onejudge over the CLI, never through a file,
///   so a base that happens to carry one cannot leak into every member.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when the base is not a YAML mapping.
pub fn merge(base: &str, base_origin: &str, persona: &Persona) -> Result<Value, Error> {
    let mut config: Value = serde_norway::from_str(base)
        .map_err(|err| Error::InvalidConfig(format!("{base_origin}: {err}")))?;
    if config.is_null() {
        config = Value::Object(serde_json::Map::new());
    }
    let map = config.as_object_mut().ok_or_else(|| {
        Error::InvalidConfig(format!(
            "{base_origin}: a onejudge base config must be a mapping"
        ))
    })?;
    map.remove("task");

    // The base authors its shared preamble in this crate's vocabulary too, so
    // the `agent` block is consumed here rather than forwarded — onejudge has no
    // such key and would reject it.
    let preamble = map
        .remove("agent")
        .and_then(|agent| {
            agent
                .get("instructions")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    let role = persona.agent.instructions.clone().unwrap_or_default();
    let combined: Vec<&str> = [preamble.trim_end(), role.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect();
    if !combined.is_empty() {
        map.insert("system_prompt".into(), Value::String(combined.join("\n\n")));
    }

    let user = map
        .entry("user")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !user.is_object() {
        return Err(Error::InvalidConfig(format!(
            "{base_origin}: `user` must be a mapping, got {}",
            kind_of(user)
        )));
    }
    let user = user.as_object_mut().expect("checked above");
    if let Some(text) = &persona.user.persona {
        user.insert("persona".into(), Value::String(text.clone()));
    }
    if let Some(text) = &persona.user.done_when {
        user.insert("done_when".into(), Value::String(text.clone()));
    }
    if let Some(cap) = persona.user.max_turns {
        user.insert("max_turns".into(), Value::Number(cap.into()));
    }

    if let Some(evals) = &persona.evals {
        map.insert("evals".into(), Value::Array(evals.clone()));
    }
    Ok(config)
}

/// Everything a merged config still lacks to be runnable.
///
/// A base missing a shared default leaves *every* member incomplete, so this is
/// checked once at validation rather than discovered one member at a time.
#[must_use]
pub fn missing_from_merged(merged: &Value) -> Vec<String> {
    let mut missing = Vec::new();
    if merged
        .get("system_prompt")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        missing.push(
            "system_prompt (check the base config's agent.instructions preamble)".to_string(),
        );
    }
    for field in ["persona", "done_when", "max_turns"] {
        if merged
            .get("user")
            .and_then(|user| user.get(field))
            .is_none()
        {
            missing.push(format!("user.{field} (check the base config)"));
        }
    }
    missing
}

/// The JSON type name of a value, for a refusal that says what was found.
fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "a mapping",
    }
}

/// The shipped persona names, for a refusal that can name the alternatives.
#[must_use]
pub fn shipped_names() -> BTreeSet<&'static str> {
    SHIPPED_PERSONAS.iter().map(|(name, _)| *name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = concat!(
        "provider:\n  kind: oneharness\n",
        "agent:\n  instructions: |\n    Standing bar.\n",
        "user:\n  done_when: base bar\n  max_turns: 12\n",
        "task: leaked\n",
    );

    /// The preamble and the role are both kept, in that order, and the internal
    /// `agent` block never reaches onejudge.
    #[test]
    fn the_merge_keeps_both_halves_of_the_prompt() {
        let persona = Persona::parse(
            "agent:\n  name: engineer\n  instructions: |\n    Role text.\nuser:\n  persona: Lead.\n",
            "p",
        )
        .unwrap();
        let merged = merge(BASE, "base", &persona).unwrap();
        assert_eq!(
            merged["system_prompt"].as_str().unwrap(),
            "Standing bar.\n\nRole text."
        );
        assert!(
            merged.get("agent").is_none(),
            "internal vocabulary reached onejudge"
        );
        assert_eq!(merged["user"]["persona"].as_str().unwrap(), "Lead.");
        assert_eq!(merged["user"]["done_when"].as_str().unwrap(), "base bar");
        assert_eq!(merged["user"]["max_turns"].as_u64().unwrap(), 12);
        assert!(missing_from_merged(&merged).is_empty());
    }

    /// A base's `task` never reaches a member: it arrives over the CLI.
    #[test]
    fn a_base_task_is_dropped() {
        let persona =
            Persona::parse("agent:\n  instructions: r\nuser:\n  persona: p\n", "p").unwrap();
        let merged = merge(BASE, "base", &persona).unwrap();
        assert!(merged.get("task").is_none());
    }

    /// A persona's own `done_when` / `max_turns` / `evals` beat the base's.
    #[test]
    fn a_persona_overrides_what_it_brings() {
        let persona = Persona::parse(
            "agent:\n  instructions: r\nuser:\n  persona: p\n  done_when: stricter\n  max_turns: 3\nevals:\n  - criterion: c\n",
            "p",
        )
        .unwrap();
        let merged = merge(BASE, "base", &persona).unwrap();
        assert_eq!(merged["user"]["done_when"].as_str().unwrap(), "stricter");
        assert_eq!(merged["user"]["max_turns"].as_u64().unwrap(), 3);
        assert_eq!(merged["evals"].as_array().unwrap().len(), 1);
    }

    /// A base that never supplied a shared default is named at validation, not
    /// discovered one member at a time. With no persona to make up for it — the
    /// member that names none — every field the run needs is reported at once.
    #[test]
    fn a_base_missing_its_shared_defaults_is_named() {
        let missing = missing_from_merged(
            &merge(
                "provider:\n  kind: oneharness\n",
                "bare",
                &Persona::default(),
            )
            .unwrap(),
        );
        assert!(
            missing.iter().any(|m| m.starts_with("system_prompt")),
            "{missing:?}"
        );
        assert!(
            missing.iter().any(|m| m.starts_with("user.persona")),
            "{missing:?}"
        );
        assert!(
            missing.iter().any(|m| m.starts_with("user.done_when")),
            "{missing:?}"
        );
        assert!(
            missing.iter().any(|m| m.starts_with("user.max_turns")),
            "{missing:?}"
        );

        // A persona supplying the role is what makes the prompt complete, even
        // when the base carries no preamble of its own.
        let role = Persona::parse("agent:\n  instructions: r\nuser:\n  persona: p\n", "p").unwrap();
        let merged = merge("provider:\n  kind: oneharness\n", "bare", &role).unwrap();
        assert_eq!(merged["system_prompt"].as_str(), Some("r"));
        let missing = missing_from_merged(&merged);
        assert_eq!(missing.len(), 2, "{missing:?}");
    }

    /// A key the schema does not name is a typo, and fails loudly.
    #[test]
    fn an_unknown_key_is_refused() {
        let err = Persona::parse("agent:\n  instrucions: typo\n", "p.yaml").unwrap_err();
        assert!(err.to_string().contains("instrucions"), "{err}");
    }

    /// Validation collects every failure so one run says everything.
    #[test]
    fn validation_names_every_failure_at_once() {
        let persona = Persona::parse(
            "agent:\n  instructions: '  '\nuser:\n  persona: ''\n  max_turns: 0\n",
            "p",
        )
        .unwrap();
        let errors = persona.validate();
        assert_eq!(errors.len(), 3, "{errors:?}");

        // A name that would write outside its catalog is refused when the
        // document is *read*, not later: by then `persona new` has created it.
        let err = Persona::parse("agent:\n  name: ../escape\n", "p.yaml").unwrap_err();
        assert!(err.to_string().contains("invalid persona name"), "{err}");
    }

    /// A base that is not a mapping, or whose `user` is not one, is refused with
    /// what was found rather than a serde trace.
    #[test]
    fn a_base_of_the_wrong_shape_is_refused() {
        let persona =
            Persona::parse("agent:\n  instructions: r\nuser:\n  persona: p\n", "p").unwrap();
        let err = merge("- a\n- b\n", "list.yaml", &persona).unwrap_err();
        assert!(err.to_string().contains("must be a mapping"), "{err}");
        let err = merge("user: 3\n", "scalar.yaml", &persona).unwrap_err();
        assert!(
            err.to_string()
                .contains("`user` must be a mapping, got a number"),
            "{err}"
        );
        // An empty document is a base with nothing in it, not a failure.
        assert!(merge("", "empty.yaml", &persona).is_ok());
    }

    /// A `user` of any wrong shape is refused with the shape that was found, so
    /// an author fixes the key rather than reading a serde trace.
    #[test]
    fn a_user_of_any_wrong_shape_names_what_was_found() {
        let persona =
            Persona::parse("agent:\n  instructions: r\nuser:\n  persona: p\n", "p").unwrap();
        for (base, found) in [
            ("user: null\n", "a mapping"),
            ("user: true\n", "a boolean"),
            ("user: 3\n", "a number"),
            ("user: text\n", "a string"),
            ("user: [1]\n", "a list"),
        ] {
            let merged = merge(base, "b.yaml", &persona);
            match merged {
                // A null `user` is an absent one, which the merge fills in.
                Ok(config) => assert!(config["user"].is_object(), "{base}: {config}"),
                Err(err) => assert!(err.to_string().contains(found), "{base}: {err}"),
            }
        }
    }

    /// Names that would escape a catalog are refused before anything is written.
    #[test]
    fn a_name_that_escapes_its_catalog_is_refused() {
        assert!(is_persona_name("engineer"));
        assert!(is_persona_name("crozier/corpus-2"));
        for bad in [
            "", "../etc", "/abs", "Engineer", "-lead", "a//b", "a\\b", "a b",
        ] {
            assert!(!is_persona_name(bad), "{bad} was accepted");
        }
    }

    /// The catalog and the directory it is compiled from cannot drift: a file
    /// added to `personas/` and forgotten here would ship as a persona no graph
    /// can name, and one removed there would fail to compile — so only the first
    /// direction needs a gate, and this is it.
    #[test]
    fn the_catalog_names_every_persona_the_directory_holds() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("personas");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .expect("the personas directory")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            // `_`-prefixed files are scaffolding, not personas.
            .filter(|name| name.ends_with(".yaml") && !name.starts_with('_'))
            .map(|name| name.trim_end_matches(".yaml").to_string())
            .collect();
        on_disk.sort();
        let shipped: Vec<String> = SHIPPED_PERSONAS
            .iter()
            .map(|(name, _)| (*name).into())
            .collect();
        assert_eq!(
            shipped, on_disk,
            "SHIPPED_PERSONAS and personas/ disagree; a persona in one and not the other \
             either ships unnamed or is named and absent"
        );
    }

    /// Every shipped persona validates and merges to a complete config — the
    /// contract's own acceptance criterion, held here rather than only in the
    /// binary journey.
    #[test]
    fn every_shipped_persona_validates() {
        assert_eq!(shipped_names().len(), SHIPPED_PERSONAS.len());
        for (name, document) in SHIPPED_PERSONAS {
            let persona = Persona::parse(document, name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(
                persona.validate().is_empty(),
                "{name}: {:?}",
                persona.validate()
            );
            assert_eq!(
                persona.label(),
                Some(*name),
                "{name} labels itself differently"
            );
            let merged = merge(BASE, "base", &persona).unwrap();
            assert!(missing_from_merged(&merged).is_empty(), "{name}");
            assert_eq!(shipped(name), Some(*document));
        }
        assert_eq!(shipped("nobody"), None);
    }

    /// The scaffold `persona new` writes is itself a valid document — an author
    /// filling in the two placeholders gets a persona that validates.
    #[test]
    fn the_template_parses_and_names_its_two_required_keys() {
        let persona = Persona::parse(PERSONA_TEMPLATE, "_template.yaml").unwrap();
        assert!(persona.agent.instructions.is_some());
        assert!(persona.user.persona.is_some());
    }
}
