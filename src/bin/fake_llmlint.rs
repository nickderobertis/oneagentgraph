//! `oneagentgraph-fake-llmlint` — the `llmlint` CLI an llmlint judge runs.
//!
//! A `kind: llmlint` judge is not a model's opinion: onejudge links no llmlint
//! crate, and its whole verdict is one `<bin> lint …` run over the worker's
//! tree, read off llmlint's documented exit codes — `0` every rule holds, `1` at
//! least one violation, `2` the run could not complete (onejudge's
//! `docs/judges.md`). The `bin` is the graph's to name, so this is the program a
//! journey names there: **an input to that capability, on the terms the
//! `pre_turn` view double states**, not a stand-in for a seam of this crate.
//! Everything around it is real — real `oneagentgraph` composes the judge list
//! onto onejudge's `judges:`, the real engine builds the panel, probes this
//! binary at plan build, spawns it per decision into the member's real process
//! group, and combines what it answered with the other judges' verdicts. What a
//! real `llmlint` would add is a paid model turn per rule, which is the one thing
//! this suite does not spend.
//!
//! Scripted through the environment, which onejudge documents its own llmlint
//! judge as inheriting, so a graph's `env:` block steers it:
//!
//! | variable | what this process does |
//! | --- | --- |
//! | `FAKE_LLMLINT_ARGV_LOG` | append each invocation's argv, one JSON array per line, so a journey reads what onejudge composed |
//! | `FAKE_LLMLINT_VERDICT` | `pass` (default): exit `0` on a summary line; `fail`: exit `1` on a findings report; `fail-once`: `fail` on the first `lint`, `pass` after, remembered in `FAKE_LLMLINT_MARKER`; `broken`: exit `2` on a stderr diagnostic |
//! | `FAKE_LLMLINT_MARKER` | the file `fail-once` records its first run in |
//! | `FAKE_LLMLINT_FINGERPRINT=garbled` | `fingerprint` exits `0` having printed a line that is not a digest |
//! | `FAKE_LLMLINT_VERDICT=rules` | judge the files `lint` is handed against the config's fixture rules: exit `1` naming every rule whose `forbid` text a file contains, else `0` |
//!
//! A fixture rule is `{id, forbid}` under a config's `rules:`, and a config's
//! `plugins:` that are local paths (relative to the config naming them) are
//! configs too, read for their rules the same way — the double's stand-in for a
//! rule a real llmlint loads from a file its config includes. A plugin that is a
//! URL is a pin the double never fetches.
//!
//! A config that is not a mapping, whose `plugins` is not a list of strings,
//! whose rule lacks a string `id` or `forbid`, or whose local plugins include
//! one another in a cycle — and a `-c` naming no path — is refused with exit `2`
//! rather than read in part.
//!
//! `fingerprint` answers as the real one does, with one line of 64 lowercase hex
//! characters: a SHA-256 over every `-c` config and every local plugin it
//! reaches, by content, and over the `--model`, `--rationales` and
//! `--prompt-template` content it was given — so editing a rule in an included file, moving a pin, or
//! editing the template changes it. A config that cannot be read is exit `2`, as
//! the real one refuses. `fingerprint --help` lists the options it takes in
//! clap's layout, as the real one does.
//!
//! `--version` answers on every verdict, because that is the probe onejudge
//! makes before any turn: a `bin` that does not answer it is refused at plan
//! build, and that refusal is one of the journeys. It answers onejudge's own
//! floor — `LLMLINT_MIN_VERSION` in its `src/llmlint.rs`, 0.4.3, the first llmlint
//! whose `lint` takes the `--label` pairs onejudge 0.15.0 began passing — because
//! a probe that reads an older release refuses the judge before the journey
//! reaches what it is about. The version is the line's last word, which is
//! where onejudge reads it.

// The report IS stdout and a diagnostic IS stderr: onejudge hands the first to
// the worker verbatim and classifies a failed run off the second.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::Write as _;

/// What this run of the double was asked to conclude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Pass,
    Fail,
    FailOnce,
    Broken,
    Rules,
}

impl Verdict {
    /// Read the verdict, refusing a spelling nobody documented rather than
    /// passing on it: a journey whose graph has a typo in it would otherwise
    /// assert green against a judge that never failed anything.
    fn from_env() -> Result<Self, String> {
        match std::env::var("FAKE_LLMLINT_VERDICT").as_deref() {
            Err(_) | Ok("pass") => Ok(Verdict::Pass),
            Ok("fail") => Ok(Verdict::Fail),
            Ok("fail-once") => Ok(Verdict::FailOnce),
            Ok("broken") => Ok(Verdict::Broken),
            Ok("rules") => Ok(Verdict::Rules),
            Ok(other) => Err(format!(
                "FAKE_LLMLINT_VERDICT must be pass, fail, fail-once, broken, or rules; got \
                 {other:?}"
            )),
        }
    }
}

fn main() -> std::process::ExitCode {
    // An argument that is not text is refused rather than dropped: what onejudge
    // composed is the whole of what a journey reads off the log below, and an
    // argv with a hole in it would read as onejudge having composed less.
    let argv: Result<Vec<String>, _> = std::env::args_os()
        .skip(1)
        .map(std::ffi::OsString::into_string)
        .collect();
    let argv = match argv {
        Ok(argv) => argv,
        Err(raw) => {
            eprintln!("fake-llmlint: argument {raw:?} is not UTF-8");
            return std::process::ExitCode::from(2);
        }
    };
    if let Ok(path) = std::env::var("FAKE_LLMLINT_ARGV_LOG") {
        let line = serde_json::to_string(&argv).expect("an argv serializes");
        let appended = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut log| writeln!(log, "{line}"));
        if appended.is_err() {
            eprintln!("fake-llmlint: cannot append to {path}");
            return std::process::ExitCode::from(2);
        }
    }
    if argv.first().map(String::as_str) == Some("--version") {
        println!("llmlint 0.4.3");
        return std::process::ExitCode::SUCCESS;
    }
    if argv.first().map(String::as_str) == Some("fingerprint") {
        if argv.iter().any(|arg| arg == "--help") {
            print!("{FINGERPRINT_HELP}");
            return std::process::ExitCode::SUCCESS;
        }
        return match fingerprint(&argv) {
            // A digest that is not one: what a broken release would print, and
            // what the reader of this output has to refuse.
            Ok(_) if std::env::var("FAKE_LLMLINT_FINGERPRINT").as_deref() == Ok("garbled") => {
                println!("NOT-A-DIGEST");
                std::process::ExitCode::SUCCESS
            }
            Ok(digest) => {
                println!("{digest}");
                std::process::ExitCode::SUCCESS
            }
            Err(why) => {
                eprintln!("fake-llmlint: {why}");
                std::process::ExitCode::from(2)
            }
        };
    }
    if argv.first().map(String::as_str) != Some("lint") {
        eprintln!("fake-llmlint: expected `lint` or `--version`, got {argv:?}");
        return std::process::ExitCode::from(2);
    }
    let verdict = match Verdict::from_env() {
        Ok(verdict) => verdict,
        Err(why) => {
            eprintln!("fake-llmlint: {why}");
            return std::process::ExitCode::from(2);
        }
    };
    let verdict = match verdict {
        // The first `lint` run is remembered by creating the marker; every run
        // that finds it already there passes.
        Verdict::FailOnce => {
            let Ok(marker) = std::env::var("FAKE_LLMLINT_MARKER") else {
                eprintln!("fake-llmlint: fail-once needs FAKE_LLMLINT_MARKER");
                return std::process::ExitCode::from(2);
            };
            // Only "it is already there" means a run came before this one; a
            // marker that cannot be created for any other reason is a journey
            // steering this double through a path it cannot use, refused
            // rather than read as a second run that passes.
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
            {
                Ok(_) => Verdict::Fail,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Verdict::Pass,
                Err(err) => {
                    eprintln!("fake-llmlint: cannot create FAKE_LLMLINT_MARKER {marker}: {err}");
                    return std::process::ExitCode::from(2);
                }
            }
        }
        other => other,
    };
    match verdict {
        Verdict::Pass => {
            println!("Evaluated 3 rules over 2 files.");
            println!("0 violations, 3 rules hold");
            std::process::ExitCode::SUCCESS
        }
        Verdict::Fail => {
            println!("Evaluated 3 rules over 2 files.");
            println!();
            println!("src/lib.rs: no_todo — a TODO was left in the change");
            println!("  fake finding: the double was told to fail this run");
            println!();
            println!("1 violation, 2 rules hold");
            std::process::ExitCode::from(1)
        }
        Verdict::Broken => {
            eprintln!("fake-llmlint: the judge harness is unreachable");
            std::process::ExitCode::from(2)
        }
        Verdict::Rules => match broken_rules(&argv) {
            Ok(broken) if broken.is_empty() => {
                println!("0 violations, every fixture rule holds");
                std::process::ExitCode::SUCCESS
            }
            Ok(broken) => {
                for line in &broken {
                    println!("{line}");
                }
                println!();
                println!("{} violation(s)", broken.len());
                std::process::ExitCode::from(1)
            }
            Err(why) => {
                eprintln!("fake-llmlint: {why}");
                std::process::ExitCode::from(2)
            }
        },
        Verdict::FailOnce => unreachable!("resolved above"),
    }
}

/// Every `-c`/`--config` the argv names, refusing one that names no path.
fn configs(argv: &[String]) -> Result<Vec<std::path::PathBuf>, String> {
    let mut found = Vec::new();
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        if arg == "-c" || arg == "--config" {
            let path = args
                .next()
                .ok_or_else(|| format!("{arg} names no config path"))?;
            found.push(std::path::PathBuf::from(path));
        } else if let Some(path) = arg.strip_prefix("--config=") {
            found.push(std::path::PathBuf::from(path));
        }
    }
    Ok(found)
}

/// One fixture config, read and checked: its local plugins, resolved against
/// its own directory, and its rules as `(id, forbid)`.
struct Fixture {
    text: String,
    plugins: Vec<std::path::PathBuf>,
    rules: Vec<(String, String)>,
}

/// Read one fixture config, refusing a shape it does not document — a
/// `plugins` that is not a list of strings, or a rule without a string `id`
/// and `forbid` — rather than judging with part of it dropped.
fn fixture(path: &std::path::Path) -> Result<Fixture, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("cannot read config {}: {err}", path.display()))?;
    let document: serde_json::Value = oneagentgraph::yaml::from_str(&text)
        .map_err(|err| format!("{} is not YAML: {err}", path.display()))?;
    let refused = |what: &str| format!("{}: {what}", path.display());
    if !document.is_object() && !document.is_null() {
        return Err(refused("a config must be a mapping"));
    }
    // Absent is empty; present is a list, and `null` is not one.
    let list = |key: &str| match document.get(key) {
        None => Ok(Vec::new()),
        Some(serde_json::Value::Array(items)) => Ok(items.clone()),
        Some(_) => Err(refused(&format!("`{key}` must be a list"))),
    };
    let dir = path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default();
    let mut plugins = Vec::new();
    for plugin in list("plugins")? {
        let plugin = plugin
            .as_str()
            .ok_or_else(|| refused("every `plugins` entry must be a string"))?;
        if !plugin.starts_with("https://") && !plugin.starts_with("http://") {
            plugins.push(dir.join(plugin));
        }
    }
    let mut rules = Vec::new();
    for rule in list("rules")? {
        let field = |name: &str| rule.get(name).and_then(serde_json::Value::as_str);
        let (Some(id), Some(forbid)) = (field("id"), field("forbid")) else {
            return Err(refused("every rule must carry a string `id` and `forbid`"));
        };
        rules.push((id.to_string(), forbid.to_string()));
    }
    Ok(Fixture {
        text,
        plugins,
        rules,
    })
}

/// Each config `configs` reaches — the named ones, then every local plugin
/// they name, depth first.
///
/// A plugin that reaches a config already on its own path is refused: an
/// include cycle has no fixed set of rules to judge with.
fn reached(configs: &[std::path::PathBuf]) -> Result<Vec<Fixture>, String> {
    let mut out = Vec::new();
    // Each config still to read, with the chain of includes that reached it.
    let mut pending: Vec<(std::path::PathBuf, Vec<std::path::PathBuf>)> = configs
        .iter()
        .rev()
        .map(|path| (path.clone(), Vec::new()))
        .collect();
    while let Some((path, mut chain)) = pending.pop() {
        let identity = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if chain.contains(&identity) {
            return Err(format!("{} includes itself", path.display()));
        }
        let read = fixture(&path)?;
        chain.push(identity);
        pending.extend(
            read.plugins
                .iter()
                .rev()
                .map(|plugin| (plugin.clone(), chain.clone())),
        );
        out.push(read);
    }
    Ok(out)
}

/// The value the argv gives `flag`, as `flag VALUE` or `flag=VALUE`.
fn value_of<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    let prefix = format!("{flag}=");
    argv.iter().enumerate().find_map(|(at, arg)| {
        if arg == flag {
            argv.get(at + 1).map(String::as_str)
        } else {
            arg.strip_prefix(&prefix)
        }
    })
}

/// The double's `fingerprint`: SHA-256 over every config reached, by content,
/// and over the `--model` and the `--prompt-template`'s content when given.
fn fingerprint(argv: &[String]) -> Result<String, String> {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"fake-llmlint 0.4.3\n");
    let mut take = |part: &str| {
        hasher.update(part.len().to_le_bytes());
        hasher.update(part.as_bytes());
    };
    for read in reached(&configs(argv)?)? {
        take(&read.text);
    }
    take(value_of(argv, "--model").unwrap_or(""));
    take(if argv.iter().any(|arg| arg == "--rationales") {
        "rationales"
    } else {
        ""
    });
    if let Some(template) = value_of(argv, "--prompt-template") {
        take(
            &std::fs::read_to_string(template)
                .map_err(|err| format!("cannot read --prompt-template {template}: {err}"))?,
        );
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// What `fingerprint --help` lists: the subset of the real `llmlint
/// fingerprint --help` this double reads, in its own words and layout —
/// `tests/judged-lint/lint_llm_diff.rs` holds every option line of it to the
/// installed llmlint, so the double cannot list an option the real one lacks.
const FINGERPRINT_HELP: &str = include_str!("llmlint-fingerprint-help.txt");

/// Each finding the fixture rules make against the files `lint` was handed —
/// its trailing arguments that are files.
fn broken_rules(argv: &[String]) -> Result<Vec<String>, String> {
    let rules: Vec<(String, String)> = reached(&configs(argv)?)?
        .into_iter()
        .flat_map(|read| read.rules)
        .collect();
    let mut broken = Vec::new();
    for file in argv.iter().rev().take_while(|arg| !arg.starts_with('-')) {
        let path = std::path::Path::new(file);
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (id, forbid) in &rules {
            if text.contains(forbid.as_str()) {
                broken.push(format!("{file}: {id} — the file contains {forbid:?}"));
            }
        }
    }
    Ok(broken)
}
