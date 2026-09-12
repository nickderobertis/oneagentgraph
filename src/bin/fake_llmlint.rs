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
//!
//! `--version` answers on every verdict, because that is the probe onejudge
//! makes before any turn: a `bin` that does not answer it is refused at plan
//! build, and that refusal is one of the journeys.

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
            Ok(other) => Err(format!(
                "FAKE_LLMLINT_VERDICT must be pass, fail, fail-once, or broken; got {other:?}"
            )),
        }
    }
}

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args_os()
        .skip(1)
        .filter_map(|arg| arg.into_string().ok())
        .collect();
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
        println!("llmlint 0.0.0 (oneagentgraph double)");
        return std::process::ExitCode::SUCCESS;
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
            let first = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
                .is_ok();
            if first {
                Verdict::Fail
            } else {
                Verdict::Pass
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
        Verdict::FailOnce => unreachable!("resolved above"),
    }
}
