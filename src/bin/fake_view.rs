//! `oneagentgraph-fake-view` — a program a graph declares as a `pre_turn` view.
//!
//! **Not a seam of this crate, and not a stand-in for one.** A pre-turn command
//! is an *input*: whatever program the operator names in their graph document is
//! the one this engine spawns, and there is nothing between the two to fake. What
//! the journeys need is a program that behaves the same way on every host — one
//! that prints a known view, one that fails, one that never finishes — and that
//! is what this is. The one thing the e2e suite genuinely fakes is still the paid
//! harness process, at oneharness's own `ONEHARNESS_BIN_<ID>` seam, and this does
//! not widen it: real `oneagentgraph` validates the declared argv, spawns it into
//! the member's real process group, bounds it, drains its real pipes, and folds
//! what it printed into the real prompt the real member's turn answers.
//!
//! Steered entirely by its own argv, so one graph document decides one view:
//!
//! | argument | what this process does |
//! | --- | --- |
//! | `--say TEXT` | print `TEXT` and a newline on standard output |
//! | `--bulk N` | print `N` bytes of view rows on standard output |
//! | `--complain TEXT` | print `TEXT` and a newline on standard error |
//! | `--fail CODE` | exit `CODE` rather than `0` |
//! | `--hang` | never finish, so the caller's own bound is what ends this |
//!
//! Keep it deterministic and dependency-free: it is spawned as a subprocess, once
//! per view per turn.

// This binary's whole product IS its stdout and stderr: it stands in for whatever
// program an operator points a view at, and such a program answers on those two.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::Write as _;

/// Refuse the argv rather than run something nobody asked for.
///
/// The same bar this file already holds an unknown argument to, applied to a
/// value that is missing or is not the number the flag takes: a journey whose
/// document has a typo in it would otherwise get a view that ran, printed
/// something plausible, and asserted green for the wrong reason.
fn refuse(why: &str) -> ! {
    eprintln!("oneagentgraph-fake-view: {why}");
    std::process::exit(2);
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut exit = 0;
    let mut hang = false;
    let mut at = 0;
    while at < argv.len() {
        let value = |at: usize| -> &str {
            argv.get(at + 1)
                .unwrap_or_else(|| refuse(&format!("{} takes a value", argv[at])))
        };
        // Each flag's own type, and neither is `parse::<i32>()` for both: a
        // negative `--bulk` would print nothing at all, which reads exactly like a
        // view that ran and had nothing to say — the one thing these journeys
        // must be able to tell apart.
        let count = |at: usize| -> usize {
            value(at).parse().unwrap_or_else(|_| {
                refuse(&format!(
                    "{} takes a count of bytes, not {:?}",
                    argv[at],
                    value(at)
                ))
            })
        };
        let code = |at: usize| -> i32 {
            value(at).parse().unwrap_or_else(|_| {
                refuse(&format!(
                    "{} takes an exit code, not {:?}",
                    argv[at],
                    value(at)
                ))
            })
        };
        match argv[at].as_str() {
            "--say" => {
                println!("{}", value(at));
                at += 2;
            }
            // Rows rather than one long line, because what a view prints is rows
            // and a bound that lands mid-row is what a reader has to notice.
            "--bulk" => {
                let bytes = count(at);
                let mut written = 0;
                let mut row = 0;
                while written < bytes {
                    let line = format!("row {row} of the prepared view\n");
                    print!("{line}");
                    written += line.len();
                    row += 1;
                }
                at += 2;
            }
            "--complain" => {
                eprintln!("{}", value(at));
                at += 2;
            }
            "--fail" => {
                exit = code(at);
                at += 2;
            }
            "--hang" => {
                hang = true;
                at += 1;
            }
            // An argument nobody taught this about is a journey asserting against
            // a view it never configured, which passes for the wrong reason.
            other => refuse(&format!("unknown argument {other:?}")),
        }
    }
    // Before the hang, so a journey that reads what a wedged view printed reads
    // it rather than waiting on a buffer nobody flushed.
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    if hang {
        // Deliberately never answered: what ends this process is the caller's own
        // bound, which is the whole of what the journey drives.
        loop {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    std::process::exit(exit);
}
