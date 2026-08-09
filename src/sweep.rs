//! `sweep`: what scratch exists, what can be reclaimed, and what could not be
//! examined.
//!
//! This owns no reclamation rule of its own. Whether one directory may be taken
//! is [`crate::scratch::reclaimable`]'s answer — the non-blocking kernel lock on
//! `owner.lock` plus the pid-with-start-token beside it — and what is still
//! running under one is [`crate::scratch::stamped_for`]'s. What this module adds
//! is the *interface*: which roots hold this crate's scratch, and a report an
//! operator under disk pressure can act on.
//!
//! # Why a family is the unit
//!
//! Ported from ai-orchestrator's `sweep-scratch`, and from the property that
//! made it trustworthy: **every sweep names the families it examined and the
//! families it could not.** A sweep that silently skips one is worse than no
//! sweep at all, because `reclaimed 0 bytes` then reads as "there was nothing to
//! reclaim" when it may mean "the place it would have been was never looked at".
//! So a family lands in exactly one of two lists, always, and the reason a
//! skipped one was skipped travels with it — the host filled to 98% once, and
//! the sweep that ran answered zero because the large families were all live.
//!
//! [`Examination`] is that pair of lists in the type, so the two cannot drift:
//! there is no third answer for a family, and no way to produce a report that
//! omits one.
//!
//! # What is never taken
//!
//! Three proofs, in this order, and any one of them retains the directory:
//!
//! 1. **The ownership proof** — [`crate::scratch::reclaimable`]. A live run
//!    holds its scratch against the sweep, and a directory whose lock cannot be
//!    read proves nothing and is kept.
//! 2. **The age floor** — [`DEFAULT_MIN_AGE`]. Not a safety proof; the proofs
//!    above are. This is what stops a sweep run in anger from taking the run
//!    records an operator is about to read, and it is why the floor is checked
//!    *before* the scan below rather than after: on a host with many fresh runs,
//!    the cheap answer comes first.
//!    The order is also what keeps a sweep of a full disk quick: the scan below
//!    answers for one directory at a time, so a host with hundreds of them pays
//!    a process enumeration per candidate that reaches it, and the two cheap
//!    proofs above turn most of those away first.
//! 3. **The stamp** — [`crate::scratch::stamped_for`]. A directory whose owner
//!    has exited can still have descendants that never did: a paid harness
//!    reparented to init, stamped for scratch below this one, which is precisely
//!    what the environment stamp exists to reach. Removing the tree it is
//!    writing into would destroy live work *and* leave the harness billing, so a
//!    named directory is retained rather than swept — ending it is
//!    `cancel --kill`'s job, and a sweep that killed on its own would be a
//!    teardown wearing a report's name.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::scratch::{reclaimable, stamped_for, SCRATCH_PREFIX};

/// The run state directory: one directory per run, each claimed by the run that
/// made it.
pub const RUNS_FAMILY: &str = "runs";

/// The throwaway directories this crate leaves in the host's temp directory —
/// what `smoke` spends a turn in, and what `validate` builds its configs in.
pub const TEMP_FAMILY: &str = "temp";

/// How long a directory is left alone after its last write unless the operator
/// asks for less.
///
/// A day, which is ai-orchestrator's own floor for scratch it cannot prove
/// anything about. Here the proofs *are* available, so this buys something
/// narrower: the run records and event streams of everything that ran today,
/// which is what an operator reaches for after the failure that sent them
/// looking at disk in the first place.
pub const DEFAULT_MIN_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Which directories inside a family's root belong to this crate.
///
/// The distinction is not cosmetic: one of these roots is this crate's own and
/// one is shared with every other program on the host, and sweeping the second
/// as if it were the first is how a sweep removes somebody else's work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Membership {
    /// Every directory in the root, because the root itself is this crate's.
    EveryDirectory,
    /// Only directories carrying [`SCRATCH_PREFIX`], because the root is the
    /// host's and this crate is one tenant of it.
    Prefixed,
}

/// Which family a report is talking about.
///
/// Closed, because the set is: a family is a place *this crate* writes scratch,
/// and there are two. A free-form name would let a caller report on a family the
/// crate does not have — and the promise here is that every family is in one of
/// two lists, which is only worth something if "every family" is knowable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FamilyName {
    /// [`RUNS_FAMILY`].
    Runs,
    /// [`TEMP_FAMILY`].
    Temp,
}

impl FamilyName {
    /// What a report calls it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            FamilyName::Runs => RUNS_FAMILY,
            FamilyName::Temp => TEMP_FAMILY,
        }
    }
}

/// One family of scratch: a root, and the rule for what in it is this crate's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Family {
    /// What the report calls it. The name is the whole point of a family — an
    /// unnamed one cannot be reported as unexamined.
    pub name: FamilyName,
    /// Where it lives.
    pub root: PathBuf,
    /// What in it belongs to this crate.
    pub membership: Membership,
}

/// Every family this crate creates scratch in.
///
/// The authoritative list, and the extension point: a new place this crate
/// writes scratch is a new entry here, and it is reported from the moment it is
/// added — including as unexamined, which is the answer that cannot be reached
/// by forgetting.
///
/// The roots are the caller's to supply because they are the caller's to
/// resolve: the binary reads them from the same environment the rest of its
/// verbs do, and a library that went looking for them itself would answer for a
/// host rather than for the run state its caller actually uses.
#[must_use]
pub fn families(state_dir: PathBuf, temp_root: PathBuf) -> Vec<Family> {
    vec![
        Family {
            name: FamilyName::Runs,
            root: state_dir,
            membership: Membership::EveryDirectory,
        },
        Family {
            name: FamilyName::Temp,
            root: temp_root,
            membership: Membership::Prefixed,
        },
    ]
}

/// Whether a sweep removes what it proves reclaimable, or only reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Report only — `--dry-run`. Nothing is removed.
    Report,
    /// Remove what all three proofs clear.
    Reclaim,
}

/// What became of one directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Removed, and its bytes are back.
    Reclaimed,
    /// Provably reclaimable, and left alone because this was a [`Mode::Report`]
    /// sweep.
    Reclaimable,
    /// Kept, for the reason given — phrased for an operator reading a report.
    Retained(String),
}

/// One directory a sweep looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Where it is.
    pub path: PathBuf,
    /// How big its tree was when the sweep measured it. Best effort: a size is
    /// an estimate, where the disposition beside it is a claim.
    pub bytes: u64,
    /// What became of it.
    pub disposition: Disposition,
}

/// A family's two possible answers, and no third.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Examination {
    /// Every directory in the family, with what became of it. An empty list is
    /// an honest zero: the root was read and held nothing of this crate's.
    Examined(Vec<Entry>),
    /// The family could not be examined, and this is why. A report carrying one
    /// of these is a report whose totals are incomplete, and says so.
    Unexamined(String),
}

/// One family's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamilySweep {
    /// The family's name.
    pub name: FamilyName,
    /// Where it was looked for.
    pub root: PathBuf,
    /// What was found, or why nothing could be.
    pub examination: Examination,
}

/// What one sweep did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Whether this sweep removed anything.
    pub mode: Mode,
    /// Every family, in the order [`families`] lists them.
    pub families: Vec<FamilySweep>,
}

impl Report {
    /// Every entry the sweep took, or would have taken.
    fn taken(&self) -> impl Iterator<Item = &Entry> {
        self.families
            .iter()
            .filter_map(|family| match &family.examination {
                Examination::Examined(entries) => Some(entries),
                Examination::Unexamined(_) => None,
            })
            .flatten()
            .filter(|entry| {
                matches!(
                    entry.disposition,
                    Disposition::Reclaimed | Disposition::Reclaimable
                )
            })
    }

    /// How many bytes this sweep proved reclaimable.
    ///
    /// Reclaimable rather than reclaimed, because it answers for both modes and
    /// only one of them removes anything: a directory that was taken was
    /// reclaimable, and one a [`Mode::Report`] sweep left alone is reclaimable
    /// too. What separates them is [`Report::mode`], which the rendering reads.
    #[must_use]
    pub fn reclaimable_bytes(&self) -> u64 {
        self.taken().map(|entry| entry.bytes).sum()
    }

    /// How many directories this sweep proved reclaimable, in both modes and for
    /// the reason [`Report::reclaimable_bytes`] gives.
    #[must_use]
    pub fn reclaimable_count(&self) -> usize {
        self.taken().count()
    }

    /// Every family that was examined, and every family that was not with the
    /// reason it was not.
    ///
    /// Rendered together, always, and both named even when one is empty: a
    /// reader who has to infer the second list from the absence of a line
    /// cannot tell it from a sweep that never had one.
    fn family_lists(&self) -> (Vec<String>, Vec<String>) {
        let mut examined = Vec::new();
        let mut unexamined = Vec::new();
        for family in &self.families {
            match &family.examination {
                Examination::Examined(_) => examined.push(family.name.as_str().to_string()),
                Examination::Unexamined(reason) => {
                    unexamined.push(format!("{} ({reason})", family.name.as_str()));
                }
            }
        }
        (examined, unexamined)
    }

    /// The report an operator reads, one line at a time.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for family in &self.families {
            match &family.examination {
                Examination::Examined(entries) => {
                    lines.push(format!(
                        "sweep: examined family {:?} at {} — {} director{}",
                        family.name.as_str(),
                        family.root.display(),
                        entries.len(),
                        if entries.len() == 1 { "y" } else { "ies" }
                    ));
                    for entry in entries {
                        lines.push(match &entry.disposition {
                            Disposition::Reclaimed => format!(
                                "sweep:   reclaimed {} ({})",
                                entry.path.display(),
                                human(entry.bytes)
                            ),
                            Disposition::Reclaimable => format!(
                                "sweep:   would reclaim {} ({})",
                                entry.path.display(),
                                human(entry.bytes)
                            ),
                            // The reason already names the path — every one of
                            // them comes from a proof that was taken against it.
                            Disposition::Retained(reason) => {
                                format!("sweep:   retained ({}): {reason}", human(entry.bytes))
                            }
                        });
                    }
                }
                Examination::Unexamined(reason) => lines.push(format!(
                    "sweep: could not examine family {:?} at {}: {reason}",
                    family.name.as_str(),
                    family.root.display()
                )),
            }
        }
        let (examined, unexamined) = self.family_lists();
        lines.push(format!(
            "sweep: {} {} from {} director{}; examined families: {}; unexamined families: {}",
            match self.mode {
                Mode::Report => "would reclaim",
                Mode::Reclaim => "reclaimed",
            },
            human(self.reclaimable_bytes()),
            self.reclaimable_count(),
            if self.reclaimable_count() == 1 {
                "y"
            } else {
                "ies"
            },
            join_or_none(&examined),
            join_or_none(&unexamined),
        ));
        lines
    }
}

/// One list, or the word that says it was empty rather than forgotten.
fn join_or_none(names: &[String]) -> String {
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

/// A byte count as an operator reads one.
///
/// Binary units, because this answers a question about a filesystem, and one
/// decimal place, because the number is an estimate of a tree's size and a
/// second digit would suggest otherwise.
#[must_use]
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        // Whole bytes below the first unit: "0 B" is exact, where "0.0 KiB"
        // would read as a rounding of something.
        return format!("{bytes} B");
    }
    // Rendered to one decimal place, so what an `f64` loses on a count this
    // large is far below what the rendering shows.
    #[allow(clippy::cast_precision_loss)]
    let mut size = bytes as f64 / 1024.0;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

/// Sweep every family, reclaiming what all three proofs clear.
///
/// `now` and `min_age` are parameters rather than read here so that a caller can
/// say what "recent" means — the CLI's `--min-age-hours 0` is an operator
/// declaring they want everything provably dead, whatever it was written.
#[must_use]
pub fn sweep(families: &[Family], mode: Mode, min_age: Duration, now: SystemTime) -> Report {
    Report {
        mode,
        families: families
            .iter()
            .map(|family| FamilySweep {
                name: family.name,
                root: family.root.clone(),
                examination: examine(family, mode, min_age, now),
            })
            .collect(),
    }
}

/// Look at one family's root, or say why it could not be.
fn examine(family: &Family, mode: Mode, min_age: Duration, now: SystemTime) -> Examination {
    let listing = match std::fs::read_dir(&family.root) {
        Ok(listing) => listing,
        // A root that is not there holds nothing, and a sweep can say so
        // honestly: there is no run state until a run makes some. Every other
        // failure — a file where a directory was expected, a directory this
        // process may not read — is a family whose contents are unknown, and
        // that is the answer a zero must never be confused with.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Examination::Examined(Vec::new())
        }
        Err(err) => return Examination::Unexamined(err.to_string()),
    };
    /// One listed name, with the kind that decides whether it is scratch.
    fn identify(
        found: std::io::Result<std::fs::DirEntry>,
    ) -> std::io::Result<(std::fs::DirEntry, std::fs::FileType)> {
        let found = found?;
        let kind = found.file_type()?;
        Ok((found, kind))
    }

    let mut paths = Vec::new();
    for found in listing {
        // A walk that skipped what it could not read would report the family as
        // examined while a directory in it went unnamed — the one answer this
        // verb exists to make impossible.
        // llmlint: ignore-block[changed_behavior_has_e2e] no journey reaches the
        // failure arm: the root has just been opened, so a listing that then
        // fails part way through is the filesystem going out from under this
        // process — an unmounted device, a revoked handle — which no flag,
        // directory layout, or fake can ask for. The reachable half of this
        // answer, a root that cannot be listed at all, is driven through the CLI
        // in tests/e2e/verbs.rs. What the arm decides is the direction of an
        // unreachable failure, and it makes it the honest one: the family is
        // reported unexamined rather than as a zero somebody would read as
        // "nothing there".
        let (found, kind) = match identify(found) {
            Ok(identified) => identified,
            Err(err) => {
                return Examination::Unexamined(format!("it could not be read to its end: {err}"))
            }
        };
        // llmlint: ignore-end[changed_behavior_has_e2e]
        // Directories only, and `file_type` does not follow a symlink — so a
        // link pointing into somebody else's tree is not this crate's scratch
        // and is never removed as if it were.
        if !kind.is_dir() {
            continue;
        }
        if family.membership == Membership::Prefixed
            && !found
                .file_name()
                .to_string_lossy()
                .starts_with(SCRATCH_PREFIX)
        {
            continue;
        }
        paths.push(found.path());
    }
    // Sorted, so two sweeps of one host report the same order and a diff between
    // them is a change in the scratch rather than in the directory listing.
    paths.sort();
    Examination::Examined(
        paths
            .into_iter()
            .map(|path| Entry {
                // Measured before it is judged, and the field order is what says
                // so: a reclaim removes the tree, and a size taken afterwards is
                // zero for every directory the sweep actually took.
                bytes: tree_bytes(&path),
                disposition: judge(&path, mode, min_age, now),
                path,
            })
            .collect(),
    )
}

/// What becomes of one directory: the three proofs, and the removal if they all
/// clear.
fn judge(path: &Path, mode: Mode, min_age: Duration, now: SystemTime) -> Disposition {
    if let Err(reason) = reclaimable(path) {
        return Disposition::Retained(reason);
    }
    if let Some(reason) = too_young(path, min_age, now) {
        return Disposition::Retained(reason);
    }
    let live = stamped_for(&path.display().to_string());
    if !live.is_empty() {
        return Disposition::Retained(format!(
            "{} is still named by {} live process(es) started under it; end them with \
             `oneagentgraph cancel RUN --kill` first",
            path.display(),
            live.len()
        ));
    }
    match mode {
        Mode::Report => Disposition::Reclaimable,
        // llmlint: ignore-block[changed_behavior_has_e2e] the failure arm has no
        // journey because no input reaches it: every directory here has just
        // been proven unlocked, unclaimed, and unnamed by any live process, so
        // what is left is the filesystem refusing a removal this process is
        // entitled to make — a permission or device failure no graph, flag, or
        // fake can ask for. What the arm decides is the *direction* of that
        // failure, and it makes it the honest one: a removal that did not happen
        // is reported as a retention naming the error, never counted into the
        // bytes this sweep claims to have reclaimed.
        Mode::Reclaim => match std::fs::remove_dir_all(path) {
            Ok(()) => Disposition::Reclaimed,
            Err(err) => {
                Disposition::Retained(format!("{} could not be removed: {err}", path.display()))
            }
        },
        // llmlint: ignore-end[changed_behavior_has_e2e]
    }
}

/// Whether a directory is inside the floor, and how to say so.
///
/// An unreadable timestamp retains, like every other unanswerable question here:
/// the sweep only ever acts on what it can prove.
fn too_young(path: &Path, min_age: Duration, now: SystemTime) -> Option<String> {
    if min_age.is_zero() {
        return None;
    }
    // llmlint: ignore-block[changed_behavior_has_e2e] neither arm below is
    // reachable from a command line. A timestamp that cannot be read belongs to
    // a directory `reclaimable` opened a lock inside a moment earlier, so it is
    // the filesystem failing between two calls; a timestamp in the *future* is a
    // clock that disagrees with the one this process reads, which a journey
    // could only produce by forging an mtime — state no invocation of this
    // binary creates, and a forgery that would make the journey a unit test with
    // a subprocess around it. Both are held below, deterministically and on
    // every platform, by passing the clock in. What an operator can reach — a
    // directory inside the floor, and the same directory once it is past it — is
    // driven through the CLI in tests/e2e/verbs.rs.
    let age = match std::fs::metadata(path).and_then(|meta| meta.modified()) {
        // A directory stamped in the future is not old, and reading it as
        // enormously old is how a clock skew reclaims a live run's scratch.
        Ok(modified) => now.duration_since(modified).unwrap_or(Duration::ZERO),
        Err(err) => {
            return Some(format!(
                "{}: its age could not be read ({err}), so it is retained",
                path.display()
            ))
        }
    };
    // llmlint: ignore-end[changed_behavior_has_e2e]
    if age >= min_age {
        return None;
    }
    Some(format!(
        "{} was written {}s ago, inside this sweep's {}s floor; pass `--min-age-hours 0` to \
         sweep whatever is provably dead",
        path.display(),
        age.as_secs(),
        min_age.as_secs()
    ))
}

/// How many bytes one tree holds.
///
/// Symlinks are counted as the links they are rather than followed: a tree's
/// size is what removing it gives back, and removing a link gives back the link.
/// What cannot be read is left out of the total rather than failing the sweep —
/// the number beside a directory is an estimate of its size, and the disposition
/// beside it is the claim that has to be exact.
fn tree_bytes(path: &Path) -> u64 {
    let mut total = 0;
    let mut walking = vec![path.to_path_buf()];
    while let Some(dir) = walking.pop() {
        let Ok(listing) = std::fs::read_dir(&dir) else {
            continue;
        };
        for found in listing.flatten() {
            // `DirEntry::metadata` does not traverse a symlink.
            let Ok(meta) = found.metadata() else {
                continue;
            };
            if meta.is_dir() {
                walking.push(found.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;
    use crate::liveness::OWNER_LOCK_FILE;
    use crate::scratch::{own_identity, Owned};

    /// The two families the binary sweeps, rooted where a test can see them.
    fn two_families(state: &Path, temp: &Path) -> Vec<Family> {
        families(state.to_path_buf(), temp.to_path_buf())
    }

    /// One directory left the way a finished run leaves its own: the lock file
    /// still there, and its record naming a process that is no longer that
    /// process.
    ///
    /// Written rather than claimed-and-dropped, because a claim taken *here* is
    /// recorded against this test process — which is very much alive, and would
    /// pin the directory by the identity half of the proof. The pid is this
    /// one's with a start token nobody holds, which is exactly the recycled
    /// number the token exists to see through.
    fn abandoned(path: &Path) {
        std::fs::create_dir_all(path).expect("mkdir");
        std::fs::write(
            path.join(OWNER_LOCK_FILE),
            format!("{} 1\n", own_identity().pid),
        )
        .expect("write");
    }

    /// A directory nothing holds is reported in [`Mode::Report`] and gone in
    /// [`Mode::Reclaim`] — and the report names both families either way.
    ///
    /// Held wherever the kernel backs the lock, which is where a sweep may act
    /// on it at all: a platform with no lock to take retains everything, and
    /// `a_held_claim_is_retained_with_its_reason` is what covers it there.
    #[cfg(any(unix, windows))]
    #[test]
    fn a_released_claim_is_reported_then_reclaimed() {
        let root = tempfile::tempdir().expect("tempdir");
        let state = root.path().join("state");
        let temp = root.path().join("temp");
        std::fs::create_dir_all(&temp).expect("mkdir");
        let dead = state.join("node-scope-1");
        abandoned(&dead);
        std::fs::write(dead.join("events.ndjson"), vec![b'x'; 2048]).expect("write");

        let families = two_families(&state, &temp);
        let reported = sweep(&families, Mode::Report, Duration::ZERO, SystemTime::now());
        assert_eq!(reported.reclaimable_count(), 1);
        assert!(reported.reclaimable_bytes() >= 2048);
        assert!(dead.is_dir(), "a report removed a directory");
        let rendered = reported.lines().join("\n");
        assert!(rendered.contains("would reclaim"), "{rendered}");
        // Both families named, every time: that is what makes a zero honest.
        assert!(
            rendered.contains("examined families: runs, temp"),
            "{rendered}"
        );
        assert!(rendered.contains("unexamined families: none"), "{rendered}");

        let swept = sweep(&families, Mode::Reclaim, Duration::ZERO, SystemTime::now());
        assert_eq!(swept.reclaimable_count(), 1);
        assert!(!dead.exists(), "a sweep left what it reported reclaimed");
        assert!(swept.lines().join("\n").contains("sweep:   reclaimed"));
    }

    /// A directory a live claim holds is retained, with the proof that retained
    /// it — the sweep never takes a directory a run is working in.
    #[test]
    fn a_held_claim_is_retained_with_its_reason() {
        let root = tempfile::tempdir().expect("tempdir");
        let state = root.path().join("state");
        let live = state.join("node-scope-live");
        let _owned = Owned::claim(&live).expect("claim");

        let report = sweep(
            &two_families(&state, root.path()),
            Mode::Reclaim,
            Duration::ZERO,
            SystemTime::now(),
        );
        assert_eq!(report.reclaimable_count(), 0);
        assert_eq!(report.reclaimable_bytes(), 0);
        assert!(live.is_dir(), "a sweep took a live run's scratch");
        let rendered = report.lines().join("\n");
        assert!(rendered.contains("sweep:   retained"), "{rendered}");
        assert!(
            rendered.contains(&live.display().to_string()),
            "a retention that does not name what it kept: {rendered}"
        );
    }

    /// A root that cannot be listed is a family the sweep names as unexamined,
    /// with the reason — never a family whose zero is read as "nothing here".
    #[test]
    fn a_root_that_cannot_be_listed_is_named_as_unexamined() {
        let root = tempfile::tempdir().expect("tempdir");
        let blocked = root.path().join("not-a-directory");
        std::fs::write(&blocked, "").expect("write");

        let report = sweep(
            &two_families(&blocked, root.path()),
            Mode::Reclaim,
            Duration::ZERO,
            SystemTime::now(),
        );
        let rendered = report.lines().join("\n");
        assert!(
            rendered.contains("could not examine family \"runs\""),
            "{rendered}"
        );
        assert!(
            rendered.contains("examined families: temp"),
            "the families that *were* examined must still be named: {rendered}"
        );
        assert!(
            rendered.contains("unexamined families: runs ("),
            "an unexamined family must carry its reason: {rendered}"
        );
    }

    /// A root that is not there yet is an honest zero rather than a skip: there
    /// is no run state until a run makes some, and that is knowable.
    #[test]
    fn a_root_that_does_not_exist_is_an_examined_zero() {
        let root = tempfile::tempdir().expect("tempdir");
        let report = sweep(
            &two_families(&root.path().join("never-ran"), root.path()),
            Mode::Reclaim,
            Duration::ZERO,
            SystemTime::now(),
        );
        assert_eq!(report.reclaimable_count(), 0);
        let rendered = report.lines().join("\n");
        assert!(
            rendered.contains("examined families: runs, temp"),
            "{rendered}"
        );
        assert!(rendered.contains("unexamined families: none"), "{rendered}");
    }

    /// The temp family is one tenant of a root the whole host shares, so only
    /// this crate's own directories are ever candidates there — and a file, or a
    /// link, is not a scratch directory whatever it is called.
    #[cfg(any(unix, windows))]
    #[test]
    fn a_shared_root_offers_up_only_this_crate_s_own_directories() {
        let root = tempfile::tempdir().expect("tempdir");
        let temp = root.path().join("temp");
        let ours = temp.join(format!("{SCRATCH_PREFIX}smoke-4242"));
        let theirs = temp.join("someone-elses-work");
        abandoned(&ours);
        abandoned(&theirs);
        // A file wearing the prefix is not a directory this crate created, and
        // is passed over rather than removed.
        std::fs::write(temp.join(format!("{SCRATCH_PREFIX}not-a-directory")), "").expect("write");

        let report = sweep(
            &two_families(&root.path().join("state"), &temp),
            Mode::Reclaim,
            Duration::ZERO,
            SystemTime::now(),
        );
        assert_eq!(report.reclaimable_count(), 1);
        assert!(!ours.exists(), "this crate's own scratch was not reclaimed");
        assert!(theirs.is_dir(), "a sweep took a directory that is not ours");
        assert!(temp
            .join(format!("{SCRATCH_PREFIX}not-a-directory"))
            .is_file());
    }

    /// The age floor retains what is inside it, and says so in terms an operator
    /// can act on — with the flag that overrides it.
    #[cfg(any(unix, windows))]
    #[test]
    fn the_age_floor_retains_a_fresh_directory_and_says_how_to_override_it() {
        let root = tempfile::tempdir().expect("tempdir");
        let state = root.path().join("state");
        let fresh = state.join("node-scope-fresh");
        abandoned(&fresh);

        let report = sweep(
            &two_families(&state, root.path()),
            Mode::Reclaim,
            DEFAULT_MIN_AGE,
            SystemTime::now(),
        );
        assert_eq!(report.reclaimable_count(), 0);
        assert!(fresh.is_dir());
        let rendered = report.lines().join("\n");
        assert!(rendered.contains("--min-age-hours 0"), "{rendered}");

        // And the floor is a floor, not a refusal: the same directory, judged
        // from far enough in the future, is taken.
        let later = SystemTime::now() + DEFAULT_MIN_AGE + Duration::from_secs(60);
        let swept = sweep(
            &two_families(&state, root.path()),
            Mode::Reclaim,
            DEFAULT_MIN_AGE,
            later,
        );
        assert_eq!(swept.reclaimable_count(), 1);
        assert!(!fresh.exists());
    }

    /// A directory stamped in the future is not "very old": a clock skew read
    /// that way would hand a live run's scratch to the sweep.
    #[test]
    fn a_directory_written_in_the_future_is_inside_the_floor() {
        let root = tempfile::tempdir().expect("tempdir");
        let state = root.path().join("state");
        let fresh = state.join("node-scope-skewed");
        abandoned(&fresh);

        let report = sweep(
            &two_families(&state, root.path()),
            Mode::Reclaim,
            DEFAULT_MIN_AGE,
            UNIX_EPOCH,
        );
        assert_eq!(report.reclaimable_count(), 0);
        assert!(fresh.is_dir());
    }

    /// Sizes are rendered the way a filesystem is read, and the boundaries are
    /// where a unit turns over.
    #[test]
    fn a_byte_count_renders_in_the_units_a_filesystem_is_read_in() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1023), "1023 B");
        assert_eq!(human(1024), "1.0 KiB");
        assert_eq!(human(1024 * 1024 * 3 / 2), "1.5 MiB");
        assert_eq!(human(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(human(5 * 1024 * 1024 * 1024 * 1024), "5.0 TiB");
        // Past the last unit it keeps counting rather than inventing one.
        assert_eq!(human(u64::MAX), "16777216.0 TiB");
    }
}
