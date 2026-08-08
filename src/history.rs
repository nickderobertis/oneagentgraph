//! `history`: what past runs did, read back out of the state directory.
//!
//! `oneagentgraph history [RUN] | history show ID`. A run's record is written
//! when it starts and rewritten as it settles, so a listing includes the run
//! that is still going — which is what makes `history` usable while something is
//! in flight rather than only afterwards.

use std::path::Path;

use crate::error::Error;
use crate::run::{Record, RunId, EVENTS_FILE, RECORD_FILE};

/// Every run in `state_dir`, newest first.
///
/// A directory whose record cannot be read is skipped rather than failing the
/// listing: a run killed between creating its directory and writing its record
/// leaves one behind, and it must not hide every other run from an operator.
#[must_use]
pub fn list(state_dir: &Path) -> Vec<Record> {
    let Ok(entries) = std::fs::read_dir(state_dir) else {
        return Vec::new();
    };
    let mut records: Vec<Record> = entries
        .flatten()
        .filter_map(|entry| read(&entry.path()).ok())
        .collect();
    records.sort_by(|a, b| {
        b.started_ms
            .cmp(&a.started_ms)
            .then_with(|| a.run_id.cmp(&b.run_id))
    });
    records
}

/// One run's record.
///
/// A run id arrives on the argv and becomes a *path component*, so a value
/// carrying a separator or a parent reference would read a record — and, through
/// `cancel`, write a signal — outside the state directory entirely. [`RunId`]
/// parses that shape, here and again when the record itself is read back.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when the id is not one this crate mints, when there
/// is no such run, or when its record cannot be read.
pub fn show(state_dir: &Path, run_id: &str) -> Result<Record, Error> {
    let run_id = RunId::parse(run_id)?;
    read(&state_dir.join(&run_id)).map_err(|err| {
        Error::InvalidConfig(format!(
            "no run {:?} under {}: {err}",
            run_id.as_str(),
            state_dir.display()
        ))
    })
}

/// One run's merged NDJSON, as it was written.
///
/// Located from the run's id rather than from the record's `events_path`: a run
/// always writes its stream to [`EVENTS_FILE`] inside its own directory, so the
/// two name the same file — but only one of them is a string read back off disk.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when the run, or its stream, cannot be read.
pub fn events(state_dir: &Path, run_id: &str) -> Result<String, Error> {
    let record = show(state_dir, run_id)?;
    let path = state_dir.join(&record.run_id).join(EVENTS_FILE);
    std::fs::read_to_string(&path)
        .map_err(|err| Error::InvalidConfig(format!("cannot read {}: {err}", path.display())))
}

/// Read one run directory's record.
fn read(dir: &Path) -> Result<Record, String> {
    let path = dir.join(RECORD_FILE);
    let raw = std::fs::read_to_string(&path).map_err(|err| err.to_string())?;
    serde_json::from_str(&raw).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn record(state: &Path, run_id: &str, started_ms: u64) -> Record {
        let record = Record {
            run_id: RunId::parse(run_id).expect("a run id"),
            graph: "./g.yaml".into(),
            name: "g".into(),
            started_ms,
            finished_ms: None,
            exit_code: None,
            members: BTreeMap::new(),
            declared_members: Vec::new(),
            refs: Vec::new(),
            events_path: state
                .join(run_id)
                .join("events.jsonl")
                .display()
                .to_string(),
        };
        let dir = state.join(run_id);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join(RECORD_FILE),
            serde_json::to_string(&record).expect("json"),
        )
        .expect("write");
        std::fs::write(&record.events_path, "{\"v\":1}\n").expect("events");
        record
    }

    /// A listing is newest first, and one unreadable run never hides the rest.
    #[test]
    fn a_listing_is_newest_first_and_survives_a_torn_record() {
        let state = tempfile::tempdir().expect("tempdir");
        record(state.path(), "old", 1);
        record(state.path(), "new", 2);
        std::fs::create_dir_all(state.path().join("torn")).expect("mkdir");
        std::fs::write(state.path().join("torn").join(RECORD_FILE), "{not json").expect("write");

        let listed = list(state.path());
        assert_eq!(
            listed.iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(),
            vec!["new", "old"]
        );
        assert!(list(&state.path().join("nowhere")).is_empty());
    }

    /// `show` reads one run and its stream, and names a run that is not there.
    #[test]
    fn show_reads_one_run_and_names_one_that_is_not_there() {
        let state = tempfile::tempdir().expect("tempdir");
        record(state.path(), "r-1", 1);
        assert_eq!(
            show(state.path(), "r-1").expect("a record").run_id.as_str(),
            "r-1"
        );
        assert_eq!(
            events(state.path(), "r-1").expect("a stream"),
            "{\"v\":1}\n"
        );

        let err = show(state.path(), "ghost").unwrap_err();
        assert!(err.to_string().contains("no run \"ghost\""), "{err}");
        assert!(events(state.path(), "ghost").is_err());
    }

    /// A record naming a stream that is gone says which file, not just that
    /// something failed.
    #[test]
    fn a_missing_stream_names_the_file() {
        let state = tempfile::tempdir().expect("tempdir");
        let record = record(state.path(), "r-2", 1);
        std::fs::remove_file(&record.events_path).expect("remove");
        let err = events(state.path(), "r-2").unwrap_err();
        assert!(err.to_string().contains("events.jsonl"), "{err}");
    }
}
