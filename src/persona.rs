//! Personas: the onejudge config fragment a member layers over its base config.
//!
//! `docs/contract.md` gives a member an optional `persona` ref beside its
//! `base_config`, and the CLI a `persona new NAME` / `persona validate PATH`
//! pair. This module is what stands behind all three: how a persona is read, how
//! it merges onto a base, and what a scaffolded one looks like.
//!
//! A persona is written in **onejudge's own field names** — the role is
//! `system_prompt`, the supervisor is `user:` — and what it may say is decided by
//! onejudge's own `Config`, which every persona is validated through. This crate
//! keeps no second copy of that schema, so there is nothing here to drift from
//! it: onejudge relaxing a field relaxes it for a persona too, in the release
//! that relaxes it.
//!
//! Two keys are this crate's own and never reach onejudge — the top-level `name`,
//! which labels the member's events, and `user.done_when_replaces_base`, which
//! decides how a role's bar composes with the base's. `docs/persona-format.md`
//! documents both for an author.
//!
//! The `agent:` block this crate used to define is **refused**, naming the field
//! to write instead. There is no alias, no flag, and no deprecation period: a
//! persona written in that spelling does not load, anywhere.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::Error;

/// The template `persona new` scaffolds from.
pub const PERSONA_TEMPLATE: &str = include_str!("../personas/_template.yaml");

/// The personas this crate ships, as `(name, document)`.
///
/// They are general cross-repo roles, so a graph can name one without authoring
/// anything: `persona: engineer`. Each is read by the suite through the same
/// [`Persona::parse`] a user's own file goes through.
pub const SHIPPED_PERSONAS: &[(&str, &str)] = &[
    ("docs-writer", include_str!("../personas/docs-writer.yaml")),
    ("engineer", include_str!("../personas/engineer.yaml")),
    ("planner", include_str!("../personas/planner.yaml")),
    ("researcher", include_str!("../personas/researcher.yaml")),
    ("reviewer", include_str!("../personas/reviewer.yaml")),
];

/// The onejudge fields a persona may not carry, and what owns each instead.
///
/// Every one of them is decided by the member's own launch and written over the
/// merged config on the way out, so a persona naming one would be dropped
/// without a word — the silent drop this crate refuses everywhere else. They are
/// named here rather than left to onejudge because onejudge, reading a whole
/// config, is right to accept all four.
const MEMBER_OWNED: &[(&str, &str)] = &[
    (
        "provider",
        "the member's own `agent:` and `judge:` in the graph decide its provider",
    ),
    (
        "session",
        "the run names the session, and every member of it shares one",
    ),
    (
        "task",
        "the task comes from the member's own `task:`, or from the run's `--task`",
    ),
    (
        "skill",
        "a relative skill path is resolved against the directory of the config naming it, which \
         for a member is its base config — so name it there",
    ),
];

/// One persona document: a onejudge config fragment, plus the two keys this
/// crate owns.
///
/// Built only by [`Persona::parse`], which is the trust boundary: a persona is
/// external input, and every key it carries is either onejudge's — checked
/// against onejudge's own schema, so a typo fails loudly instead of being
/// silently dropped — or one of this crate's two, checked here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Persona {
    /// This crate's own `name`: the label stamped on this member's events.
    name: Option<PersonaName>,
    /// This crate's own `user.done_when_replaces_base` — see [`merge`].
    done_when_replaces_base: bool,
    /// Everything onejudge's, exactly as written, ready to layer onto a base.
    fragment: Map<String, Value>,
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

impl std::fmt::Display for PersonaName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Persona {
    /// Parse one persona document.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidConfig`] when the document is not YAML, is not a mapping,
    /// carries the refused `agent:` block, carries a key the member itself owns,
    /// or is not a onejudge config fragment. Every message is prefixed with
    /// `origin`, so the answer names the file that has to be repaired.
    pub fn parse(document: &str, origin: &str) -> Result<Self, Error> {
        let value: Value = serde_norway::from_str(document)
            .map_err(|err| Error::InvalidConfig(format!("{origin}: {err}")))?;
        Self::read(value).map_err(|err| Error::InvalidConfig(format!("{origin}: {err}")))
    }

    /// One parsed document, read as a persona.
    fn read(value: Value) -> Result<Self, String> {
        let mut fragment = match value {
            // An empty file is a persona that layers nothing, not a failure:
            // what a persona must carry is onejudge's to say, and onejudge asks
            // for none of it.
            Value::Null => Map::new(),
            Value::Object(map) => map,
            other => {
                return Err(format!(
                    "a persona must be a mapping, got {}",
                    kind_of(&other)
                ))
            }
        };
        if let Some(agent) = fragment.get("agent") {
            return Err(refused_agent_block(agent));
        }
        for (key, owner) in MEMBER_OWNED {
            if fragment.contains_key(*key) {
                return Err(format!("`{key}` is not a persona key: {owner}"));
            }
        }
        let name = match fragment.remove("name") {
            None | Some(Value::Null) => None,
            Some(Value::String(name)) => Some(name.parse()?),
            Some(other) => return Err(format!("`name` must be a string, got {}", kind_of(&other))),
        };
        let done_when_replaces_base = take_replaces_base(&mut fragment)?;
        // What is left is onejudge's, and onejudge's own schema is what says
        // whether each key is a field at all and what it has to hold. Validated
        // by construction rather than by a copy of the rules kept here, so this
        // crate cannot be stricter than the config it produces.
        serde_json::from_value::<onejudge::cli::Config>(Value::Object(fragment.clone())).map_err(
            |err| format!("a persona is a onejudge config fragment, and this one is not: {err}"),
        )?;
        // The one pair this crate's own flat key allows and must not honour:
        // `done_when_replaces_base` with nothing to replace the base's bar with
        // is the shared review bar removed and nothing put in its place, which is
        // the silent drop spelled out loud.
        if done_when_replaces_base && role_bar(&fragment).trim().is_empty() {
            return Err(
                "user.done_when_replaces_base names nothing to replace the base's bar with: give \
                 this persona its own user.done_when, or drop the key"
                    .to_string(),
            );
        }
        Ok(Self {
            name,
            done_when_replaces_base,
            fragment,
        })
    }

    /// The name to stamp on this member's events, when the persona gave one.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.name.as_ref().map(PersonaName::as_str)
    }
}

/// The refusal one `agent:` block earns, naming what to write in its place.
///
/// This message is the whole migration interface — there is no alias behind it,
/// and nothing that keeps such a file loading — so it names each key it refused
/// beside the field that replaces it, and an author repairs the file from the
/// message without reading a release note.
fn refused_agent_block(agent: &Value) -> String {
    let mut rewrites = Vec::new();
    if let Some(block) = agent.as_object() {
        if block.contains_key("instructions") {
            rewrites.push(
                "write `agent.instructions` as the top-level `system_prompt`, which is onejudge's \
                 own field for it",
            );
        }
        if block.contains_key("name") {
            rewrites.push(
                "write `agent.name` as the top-level `name`, which stays this crate's own key for \
                 the label on a member's events",
            );
        }
    }
    if rewrites.is_empty() {
        rewrites.push(
            "write the role as the top-level `system_prompt`, which is onejudge's own field for it",
        );
    }
    format!(
        "`agent` is not a persona key: a persona is a onejudge config fragment, and onejudge has \
         no `agent` field. To migrate this file, {}. There is no alias and no deprecation period \
         — see docs/persona-format.md.",
        rewrites.join(", and ")
    )
}

/// Take this crate's own `user.done_when_replaces_base` out of the fragment, so
/// what is left is onejudge's alone.
///
/// A `user` of any other shape is left exactly as written: it is onejudge's key,
/// and onejudge's own validation is where it earns its refusal.
fn take_replaces_base(fragment: &mut Map<String, Value>) -> Result<bool, String> {
    let Some(user) = fragment.get_mut("user").and_then(Value::as_object_mut) else {
        return Ok(false);
    };
    match user.remove("done_when_replaces_base") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(flag)) => Ok(flag),
        Some(other) => Err(format!(
            "`user.done_when_replaces_base` must be true or false, got {}",
            kind_of(&other)
        )),
    }
}

/// The completion bar a fragment brings, as the string onejudge's schema has
/// already proved it to be.
fn role_bar(fragment: &Map<String, Value>) -> &str {
    fragment
        .get("user")
        .and_then(|user| user.get("done_when"))
        .and_then(Value::as_str)
        .unwrap_or_default()
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
/// Both documents speak onejudge's schema, so most of this is one layered over
/// the other. Three fields are composed rather than replaced, and the reasons
/// are the ones the reference implementation settled on:
///
/// * `system_prompt` — the base's shared preamble with the persona's role
///   appended after it. Both are kept; neither replaces the other, because the
///   preamble is the standing bar and the role is what makes this member
///   different.
/// * `user.done_when` — the base's bar with the persona's added to it, and both
///   enforced. A base's `done_when` is where an operator centralizes the review
///   bar for *every* dispatch, so a role carrying one of its own is asking for a
///   second bar rather than for the shared one to go away — which is what
///   inserting over it silently did. A persona that genuinely must stand in for
///   the shared bar says `user.done_when_replaces_base`, and then only its own is
///   enforced.
/// * `task` — dropped. It reaches onejudge over the CLI, never through a file,
///   so a base that happens to carry one cannot leak into every member.
///
/// Everything else the persona brings — `user`'s other keys, `evals`,
/// `assessment` — is layered over the base's.
///
/// A base with no simulated user, merged with a persona that brings none, keeps
/// having none: no empty `user:` is invented, because to onejudge an empty one
/// is a supervisor with an empty persona rather than the single-turn run the
/// base asked for.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when the base is not a YAML mapping, carries the
/// refused `agent:` block, or spells `system_prompt`, `user`, or
/// `user.done_when` as something other than what the composition reads.
pub fn merge(base: &str, base_origin: &str, persona: &Persona) -> Result<Value, Error> {
    let mut config: Value = serde_norway::from_str(base)
        .map_err(|err| Error::InvalidConfig(format!("{base_origin}: {err}")))?;
    if config.is_null() {
        config = Value::Object(Map::new());
    }
    let map = config.as_object_mut().ok_or_else(|| {
        Error::InvalidConfig(format!(
            "{base_origin}: a onejudge base config must be a mapping"
        ))
    })?;
    if map.contains_key("agent") {
        return Err(Error::InvalidConfig(format!(
            "{base_origin}: `agent` is not a onejudge field: a base config is a onejudge config, \
             so write its shared preamble as the top-level `system_prompt`. There is no alias and \
             no deprecation period — see docs/persona-format.md."
        )));
    }
    map.remove("task");

    // The preamble and the role, in that order, both kept.
    let preamble = match map.get("system_prompt") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => {
            return Err(Error::InvalidConfig(format!(
                "{base_origin}: `system_prompt` must be a string, got {}",
                kind_of(other)
            )))
        }
    };
    let role = persona
        .fragment
        .get("system_prompt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // A half of whitespace is a half nobody wrote, and one that survived would
    // be pasted into the prompt as a blank paragraph.
    let combined: Vec<&str> = [preamble.trim_end(), role.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect();
    if !combined.is_empty() {
        map.insert("system_prompt".into(), Value::String(combined.join("\n\n")));
    }

    // The base's own bar, read as the string it has to be. A base is external
    // input and this is the value the whole composition rests on, so a
    // `done_when:` of some other shape is refused with what was found rather
    // than read as no bar at all — which is the shared review bar going missing
    // silently, the very thing composing them is here to stop. Checked whether
    // or not this persona layers anything onto it, because a base nobody can
    // compose with is broken wherever it is read.
    let base_bar = match map.get("user") {
        None | Some(Value::Null) => String::new(),
        Some(Value::Object(user)) => match user.get("done_when") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(text)) => text.clone(),
            Some(other) => {
                return Err(Error::InvalidConfig(format!(
                    "{base_origin}: `user.done_when` must be a string, got {}",
                    kind_of(other)
                )))
            }
        },
        Some(other) => {
            return Err(Error::InvalidConfig(format!(
                "{base_origin}: `user` must be a mapping, got {}",
                kind_of(other)
            )))
        }
    };

    let role_user = persona
        .fragment
        .get("user")
        .and_then(Value::as_object)
        .cloned();
    if role_user.is_some() || persona.done_when_replaces_base {
        let user = map
            .entry("user")
            .or_insert_with(|| Value::Object(Map::new()));
        if user.is_null() {
            *user = Value::Object(Map::new());
        }
        let user = user.as_object_mut().expect("the shape checked above");
        for (key, value) in role_user.unwrap_or_default() {
            if key == "done_when" {
                continue;
            }
            user.insert(key, value);
        }
        let role_bar = role_bar(&persona.fragment);
        let kept = match persona.done_when_replaces_base {
            true => "",
            false => base_bar.trim(),
        };
        let bars: Vec<&str> = [kept, role_bar.trim()]
            .into_iter()
            .filter(|bar| !bar.is_empty())
            .collect();
        if !role_bar.trim().is_empty() {
            user.insert("done_when".into(), Value::String(one_bar_from(&bars)));
        }
    }

    for (key, value) in &persona.fragment {
        if key == "system_prompt" || key == "user" {
            continue;
        }
        map.insert(key.clone(), value.clone());
    }
    Ok(config)
}

/// One completion bar out of every bar the merge kept.
///
/// Numbered and paragraph-separated rather than joined by a conjunction, because
/// either bar may itself be several sentences: what a supervisor is handed has
/// to say plainly that there is more than one and that neither is optional. One
/// bar is handed over as it was written — there is nothing to conjoin.
fn one_bar_from(bars: &[&str]) -> String {
    if let [only] = bars {
        return (*only).to_string();
    }
    let numbered: Vec<String> = bars
        .iter()
        .enumerate()
        .map(|(index, bar)| format!("{}. {bar}", index + 1))
        .collect();
    format!("Both of these must hold:\n\n{}", numbered.join("\n\n"))
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
        "system_prompt: |\n  Standing bar.\n",
        "user:\n  done_when: base bar\n  max_turns: 12\n",
        "task: leaked\n",
    );

    /// The preamble and the role are both kept, in that order, and everything
    /// the merge produces is onejudge's own vocabulary.
    #[test]
    fn the_merge_keeps_both_halves_of_the_prompt() {
        let persona = Persona::parse(
            "name: engineer\nsystem_prompt: |\n  Role text.\nuser:\n  persona: Lead.\n",
            "p",
        )
        .unwrap();
        let merged = merge(BASE, "base", &persona).unwrap();
        assert_eq!(
            merged["system_prompt"].as_str().unwrap(),
            "Standing bar.\n\nRole text."
        );
        assert_eq!(merged["user"]["persona"].as_str().unwrap(), "Lead.");
        assert_eq!(merged["user"]["done_when"].as_str().unwrap(), "base bar");
        assert_eq!(merged["user"]["max_turns"].as_u64().unwrap(), 12);
        assert_eq!(persona.label(), Some("engineer"));
        // And the effective config is a onejudge config: no key of this crate's
        // own survived the merge.
        assert!(merged.get("name").is_none(), "{merged}");
        assert!(
            serde_json::from_value::<onejudge::cli::Config>(merged.clone()).is_ok(),
            "{merged}"
        );
    }

    /// A base's `task` never reaches a member: it arrives over the CLI.
    #[test]
    fn a_base_task_is_dropped() {
        let persona = Persona::parse("system_prompt: r\nuser:\n  persona: p\n", "p").unwrap();
        let merged = merge(BASE, "base", &persona).unwrap();
        assert!(merged.get("task").is_none());
    }

    /// A persona's own `max_turns` / `evals` / `assessment` beat the base's, and
    /// its own bar is added to the base's rather than standing in for it.
    #[test]
    fn a_persona_overrides_what_it_brings() {
        let persona = Persona::parse(
            "system_prompt: r\nuser:\n  persona: p\n  done_when: stricter\n  max_turns: 3\nevals:\n  - criterion: c\nassessment: judge it\n",
            "p",
        )
        .unwrap();
        let merged = merge(BASE, "base", &persona).unwrap();
        let bar = merged["user"]["done_when"].as_str().unwrap();
        assert!(
            bar.contains("base bar"),
            "the base's bar was dropped: {bar}"
        );
        assert!(bar.contains("stricter"), "{bar}");
        assert_eq!(merged["user"]["max_turns"].as_u64().unwrap(), 3);
        assert_eq!(merged["evals"].as_array().unwrap().len(), 1);
        assert_eq!(merged["assessment"].as_str().unwrap(), "judge it");
    }

    /// The base's review bar survives a persona that brings its own, because the
    /// base is where an operator centralizes the bar for *every* dispatch — and a
    /// persona that must stand in for it says so, visibly, in the merged config.
    #[test]
    fn a_persona_adds_to_the_base_bar_unless_it_says_it_replaces_it() {
        let composing = Persona::parse(
            "system_prompt: r\nuser:\n  persona: p\n  done_when: the review is signed off\n",
            "p",
        )
        .unwrap();
        let bar = merge(BASE, "base", &composing).unwrap()["user"]["done_when"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(bar.contains("base bar"), "{bar}");
        assert!(bar.contains("the review is signed off"), "{bar}");
        // And the composition says plainly that neither half is optional.
        assert!(bar.contains("Both of these must hold"), "{bar}");

        let replacing = Persona::parse(
            concat!(
                "system_prompt: r\nuser:\n  persona: p\n",
                "  done_when: the review is signed off\n  done_when_replaces_base: true\n",
            ),
            "p",
        )
        .unwrap();
        let replaced = merge(BASE, "base", &replacing).unwrap();
        assert_eq!(
            replaced["user"]["done_when"].as_str().unwrap(),
            "the review is signed off"
        );
        // The flag is this crate's own, so it never reaches onejudge.
        assert!(
            replaced["user"].get("done_when_replaces_base").is_none(),
            "{replaced}"
        );

        // A base with no bar of its own leaves the persona's standing alone
        // rather than composed with nothing.
        let bare = merge("user:\n  done_when: '  '\n", "bare", &composing).unwrap();
        assert_eq!(
            bare["user"]["done_when"].as_str().unwrap(),
            "the review is signed off"
        );

        // Replacing without a bar to replace it with is the one invalid pair the
        // flat key allows, and it is refused at load rather than silently
        // dropping the shared bar for nothing.
        let err = Persona::parse(
            "system_prompt: r\nuser:\n  persona: p\n  done_when_replaces_base: true\n",
            "p",
        )
        .unwrap_err();
        assert!(err.to_string().contains("done_when_replaces_base"), "{err}");
        // And the flag is this crate's own key, so its own shape is checked here.
        let err =
            Persona::parse("user:\n  done_when_replaces_base: yes please\n", "p").unwrap_err();
        assert!(err.to_string().contains("must be true or false"), "{err}");
    }

    /// A persona carrying nothing but the role is a persona: what a config must
    /// hold is onejudge's to say, and onejudge asks for none of it. The member it
    /// is layered onto keeps the single-turn shape its base asked for.
    #[test]
    fn a_role_only_persona_is_accepted_and_invents_no_supervisor() {
        let persona = Persona::parse("system_prompt: just the role\n", "p.yaml").unwrap();
        let merged = merge("provider:\n  kind: command\n", "bare", &persona).unwrap();
        assert_eq!(merged["system_prompt"].as_str(), Some("just the role"));
        assert!(
            merged.get("user").is_none(),
            "an empty supervisor was invented: {merged}"
        );
        // An empty document layers nothing at all, and is not a failure either.
        let nothing = Persona::parse("", "empty.yaml").unwrap();
        assert_eq!(nothing, Persona::default());
        assert_eq!(nothing.label(), None);
        assert_eq!(
            merge("system_prompt: base only\n", "bare", &nothing).unwrap()["system_prompt"],
            Value::String("base only".into())
        );
    }

    /// The spelling this crate used to define does not load, and the refusal is
    /// the whole migration interface: it names the key and the field to write
    /// instead, per key.
    #[test]
    fn the_previous_spelling_is_refused_by_name() {
        let err = Persona::parse(
            "agent:\n  name: lead\n  instructions: r\nuser:\n  persona: p\n",
            "roles/lead.yaml",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("roles/lead.yaml"), "{err}");
        assert!(err.contains("`agent.instructions`"), "{err}");
        assert!(err.contains("`system_prompt`"), "{err}");
        assert!(err.contains("`agent.name`"), "{err}");
        assert!(err.contains("top-level `name`"), "{err}");
        assert!(err.contains("no deprecation period"), "{err}");

        // A block carrying neither still names the field that replaces it.
        for document in ["agent:\n  instrucions: typo\n", "agent: []\n"] {
            let err = Persona::parse(document, "p.yaml").unwrap_err().to_string();
            assert!(err.contains("`system_prompt`"), "{document}: {err}");
        }

        // And a base config is a onejudge config for the same reason, so the
        // preamble this crate used to read out of an `agent:` block there is
        // refused rather than translated a second time.
        let err = merge(
            "agent:\n  instructions: preamble\n",
            "base.yaml",
            &Persona::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("base.yaml"), "{err}");
        assert!(err.contains("`system_prompt`"), "{err}");
    }

    /// A key onejudge does not have is a typo, and onejudge's own schema is what
    /// says so — this crate keeps no second copy to be stricter than.
    #[test]
    fn a_key_onejudge_does_not_have_is_refused() {
        let err = Persona::parse("system_promt: typo\n", "p.yaml").unwrap_err();
        assert!(err.to_string().contains("system_promt"), "{err}");
        assert!(
            err.to_string().contains("onejudge config fragment"),
            "{err}"
        );

        // Including inside a block onejudge owns, and for a value of the wrong
        // type as much as for a name it does not know.
        for document in [
            "user:\n  persona: p\n  done_wen: typo\n",
            "user:\n  max_turns: many\n",
            "evals:\n  - kind: boolean\n",
        ] {
            assert!(
                Persona::parse(document, "p.yaml").is_err(),
                "{document} was accepted"
            );
        }
    }

    /// A key the member itself decides is refused where it is written, rather
    /// than dropped without a word on the way to onejudge.
    #[test]
    fn a_key_the_member_owns_is_refused_where_it_is_written() {
        for (document, expected) in [
            ("provider:\n  kind: command\n", "decide its provider"),
            ("session: mine\n", "the run names the session"),
            ("task: do the thing\n", "the run's `--task`"),
            ("skill: ./skills/lead\n", "name it there"),
        ] {
            let err = Persona::parse(document, "p.yaml").unwrap_err().to_string();
            assert!(err.contains(expected), "{document}: {err}");
        }
    }

    /// A document that is not a mapping, and a `name` that is not a name, are
    /// refused when the document is *read* — by then `persona new` has created
    /// it, and a name deciding a path must never escape its catalog.
    #[test]
    fn a_document_of_the_wrong_shape_is_refused() {
        let err = Persona::parse("- a\n- b\n", "p.yaml").unwrap_err();
        assert!(err.to_string().contains("must be a mapping"), "{err}");
        let err = Persona::parse("name: ../escape\n", "p.yaml").unwrap_err();
        assert!(err.to_string().contains("invalid persona name"), "{err}");
        let err = Persona::parse("name: [lead]\n", "p.yaml").unwrap_err();
        assert!(
            err.to_string()
                .contains("`name` must be a string, got a list"),
            "{err}"
        );
        // A `name:` nobody set is no name at all, not an empty one.
        assert_eq!(Persona::parse("name:\n", "p.yaml").unwrap().label(), None);
    }

    /// A base that is not a mapping, or whose `user` is not one, is refused with
    /// what was found rather than a serde trace.
    #[test]
    fn a_base_of_the_wrong_shape_is_refused() {
        let persona = Persona::parse("system_prompt: r\nuser:\n  persona: p\n", "p").unwrap();
        let err = merge("- a\n- b\n", "list.yaml", &persona).unwrap_err();
        assert!(err.to_string().contains("must be a mapping"), "{err}");
        let err = merge("user: 3\n", "scalar.yaml", &persona).unwrap_err();
        assert!(
            err.to_string()
                .contains("`user` must be a mapping, got a number"),
            "{err}"
        );
        // And the two values the composition rests on: either of another shape is
        // refused rather than read as a base that set none, which would drop the
        // shared preamble or the shared review bar without saying so.
        let err = merge("user:\n  done_when: [a]\n", "list.yaml", &persona).unwrap_err();
        assert!(
            err.to_string()
                .contains("`user.done_when` must be a string, got a list"),
            "{err}"
        );
        let err = merge("system_prompt: [a]\n", "list.yaml", &persona).unwrap_err();
        assert!(
            err.to_string()
                .contains("`system_prompt` must be a string, got a list"),
            "{err}"
        );
        // An empty document is a base with nothing in it, not a failure.
        assert!(merge("", "empty.yaml", &persona).is_ok());
    }

    /// A `user` of any wrong shape is refused with the shape that was found, so
    /// an author fixes the key rather than reading a serde trace.
    #[test]
    fn a_user_of_any_wrong_shape_names_what_was_found() {
        let persona = Persona::parse("system_prompt: r\nuser:\n  persona: p\n", "p").unwrap();
        for (base, found) in [
            ("user: null\n", "a mapping"),
            ("user: true\n", "a boolean"),
            ("user: 3\n", "a number"),
            ("user: text\n", "a string"),
            ("user: [1]\n", "a list"),
        ] {
            match merge(base, "b.yaml", &persona) {
                // A null `user` is an absent one, which the persona fills in.
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

    /// Every shipped persona loads, labels itself by its own file name, and
    /// merges to a config onejudge accepts.
    #[test]
    fn every_shipped_persona_loads() {
        assert_eq!(shipped_names().len(), SHIPPED_PERSONAS.len());
        for (name, document) in SHIPPED_PERSONAS {
            let persona = Persona::parse(document, name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(
                persona.label(),
                Some(*name),
                "{name} labels itself differently"
            );
            let merged = merge(BASE, "base", &persona).unwrap();
            assert!(
                serde_json::from_value::<onejudge::cli::Config>(merged).is_ok(),
                "{name} merges to something onejudge would refuse"
            );
            assert_eq!(shipped(name), Some(*document));
        }
        assert_eq!(shipped("nobody"), None);
    }

    /// The scaffold `persona new` writes is itself a valid document, in the
    /// spelling that ships.
    #[test]
    fn the_template_parses_in_the_spelling_that_ships() {
        let persona = Persona::parse(PERSONA_TEMPLATE, "_template.yaml").unwrap();
        assert!(persona.fragment.contains_key("system_prompt"));
        assert!(persona.fragment["user"].get("persona").is_some());
    }
}
