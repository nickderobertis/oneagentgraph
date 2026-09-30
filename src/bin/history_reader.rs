//! `oneagentgraph-history-reader` — a consumer of the history pointer a graph
//! published, as its own process.
//!
//! **Not a double of anything.** It is what a consumer of a `kind: oneharness`
//! member's turn does with the pointer file the graph's `env:` block named: read
//! the one pointer line through `oneharness_core::io::history::read_pointers`,
//! resolve the session file its three path fields name through the same library,
//! and read that one session back. It is a process of its own for one reason —
//! so a journey can trace the file-system calls that read makes, which a read
//! inside the test's own process cannot be traced for.
//!
//! One argument, the pointer file. On success prints one JSON object —
//! `history_id`, `session_file` and `prompt` of the record the pointer names —
//! and exits 0; anything short of exactly that record is a message on standard
//! error and exit 1.

// This binary's whole product IS its stdout and stderr: the journey that spawns
// it reads the record it found on one and the refusal on the other.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::path::Path;

use oneharness_core::domain::history::HistoryShowEntry;
use oneharness_core::io::history;

fn refuse(why: &str) -> ! {
    eprintln!("oneagentgraph-history-reader: {why}");
    std::process::exit(1)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(pointer_file), None) = (args.next(), args.next()) else {
        refuse("usage: oneagentgraph-history-reader POINTER_FILE")
    };
    let read = history::read_pointers(Path::new(&pointer_file))
        .unwrap_or_else(|err| refuse(&format!("{pointer_file} did not read: {err}")));
    let [pointer] = read.pointers.as_slice() else {
        refuse(&format!(
            "{pointer_file} holds {} pointer lines and {} unreadable ones, not one",
            read.pointers.len(),
            read.skipped
        ))
    };
    let dir = history::resolve_dir(Some(pointer.history_dir()))
        .unwrap_or_else(|| refuse("the pointer's store did not resolve to a directory"));
    let session_file = history::find_session_path(
        &dir,
        Some(pointer.history_project()),
        pointer.history_session(),
    )
    .unwrap_or_else(|err| refuse(&format!("the store did not read: {err}")))
    .unwrap_or_else(|| {
        refuse(&format!(
            "no session file under {} for {}/{}",
            dir.display(),
            pointer.history_project(),
            pointer.history_session()
        ))
    });
    let entries = history::read_session_display(&session_file)
        .unwrap_or_else(|err| refuse(&format!("{} did not read: {err}", session_file.display())));
    let record = entries
        .into_iter()
        .find_map(|entry| match entry {
            HistoryShowEntry::Record(record) if record.history_id == pointer.history_id() => {
                Some(record)
            }
            _ => None,
        })
        .unwrap_or_else(|| {
            refuse(&format!(
                "{} holds no record {}",
                session_file.display(),
                pointer.history_id()
            ))
        });
    println!(
        "{}",
        serde_json::json!({
            "history_id": record.history_id.to_string(),
            "session_file": session_file,
            "prompt": record.prompt,
        })
    );
}
