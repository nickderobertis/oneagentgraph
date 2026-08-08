//! Scratch ownership, and reaping what a member left behind.
//!
//! `docs/contract.md`: "scratch ownership via `owner.lock` flock +
//! pid-with-start-token, descendant reaping, successor contract for processes
//! meant to outlive their launcher." All three are ported from ai-orchestrator
//! intact, and each exists because a simpler rule destroyed live work:
//!
//! * **A recorded pid is not an identity.** This host's pid counter completes a
//!   full cycle in under a day while one member holds a recorded pid for the
//!   length of a turn, so a remembered number is a *slot*. Every pid this module
//!   acts on is carried as a [`ProcessIdentity`] — the number paired with the
//!   kernel's start token for the process holding it — and re-checked
//!   immediately before each signal.
//! * **A recorded pid cannot decide a sweep.** The pid inside a member's scratch
//!   dies the moment the member's own process does, while the run is still
//!   reaping its tree and reading its report out of that same directory. So a
//!   sweeper reclaims a directory only when a *non-blocking exclusive* `flock`
//!   on [`OWNER_LOCK_FILE`] succeeds — the kernel's own answer to "can anything
//!   still be using this?" — **and** the recorded identity no longer names a
//!   live process. Anything the proof does not clear is retained, not removed.
//! * **Ownership of a descendant is proven, not inferred.** The kernel fixes a
//!   process's environment at `exec`, so [`SCRATCH_ENV`] is a stamp no
//!   descendant can shed — including one reparented to init, which no walk from
//!   the member's own pid would ever reach.
//!
//! On a platform without those facilities the ownership claim degrades to the
//! directory's own existence, and reaping to the child this process holds. The
//! liveness journeys are Unix-only for exactly that reason.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::liveness::OWNER_LOCK_FILE;

/// The variable every member process is launched with, naming the scratch
/// directory it belongs to.
///
/// This is the ownership evidence teardown asks for: the kernel fixes an
/// environment at `exec`, so a descendant cannot shed it, and it is what reaches
/// one whose parent has already exited.
pub const SCRATCH_ENV: &str = "ONEAGENTGRAPH_SCRATCH_DIR";

/// The prefix every scratch directory this crate creates carries, so a sweep can
/// tell its own leavings from a neighbour's work.
pub const SCRATCH_PREFIX: &str = "oneagentgraph-";

/// A pid paired with the kernel's start token for the process holding it.
///
/// Two processes that reuse one number never share a start token, so this
/// answers "is that still the same process?" — the question a bare pid cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessIdentity {
    /// The process id.
    pub pid: i32,
    /// The kernel's start token for the process that held it when this identity
    /// was taken.
    pub start_token: u64,
}

/// A scratch directory this process owns for as long as it holds the handle.
///
/// The lock is released, and the directory removed, when this value is dropped —
/// which is also the instant a sweeper's non-blocking acquisition starts
/// succeeding.
#[derive(Debug)]
pub struct Owned {
    path: PathBuf,
    lock: Option<File>,
    keep: bool,
}

impl Owned {
    /// Claim `path` as scratch, creating it and recording who owns it.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidConfig`] when the directory or its lock cannot be
    /// created, or when something else already holds the lock.
    pub fn claim(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        std::fs::create_dir_all(&path).map_err(|err| {
            Error::InvalidConfig(format!("cannot create scratch {}: {err}", path.display()))
        })?;
        let lock_path = path.join(OWNER_LOCK_FILE);
        let mut lock = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&lock_path)
            .map_err(|err| {
                Error::InvalidConfig(format!("cannot open {}: {err}", lock_path.display()))
            })?;
        if !try_lock_exclusive(&lock) {
            return Err(Error::InvalidConfig(format!(
                "{} is already owned by a live process",
                path.display()
            )));
        }
        let identity = own_identity();
        let _ = writeln!(lock, "{} {}", identity.pid, identity.start_token);
        let _ = lock.flush();
        Ok(Self {
            path,
            lock: Some(lock),
            keep: false,
        })
    }

    /// Where the scratch is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Keep the directory on drop instead of removing it.
    ///
    /// A detached run hands its scratch to the process that outlives it — the
    /// successor contract — so the launcher must not take it away on the way
    /// out.
    pub fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        self.lock.take();
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Whether a scratch directory can be reclaimed, and why not when it cannot.
///
/// `Ok(())` means both proofs cleared: nothing holds the lock, and the recorded
/// identity no longer names a live process. Everything else is retained with the
/// reason, because a sweep that guesses destroys live work and its evidence.
///
/// # Errors
///
/// The reason the directory is retained, phrased for an operator reading a sweep
/// report.
pub fn reclaimable(path: &Path) -> Result<(), String> {
    let lock_path = path.join(OWNER_LOCK_FILE);
    let Ok(lock) = File::open(&lock_path) else {
        // A directory predating the lock, or one whose lock cannot be opened on
        // its own terms — symlinked, unreadable. Neither authorizes removal.
        return Err(format!(
            "{} has no readable {OWNER_LOCK_FILE}",
            path.display()
        ));
    };
    if !try_lock_exclusive(&lock) {
        return Err(format!("{} is still locked by its owner", path.display()));
    }
    match recorded_identity(&lock_path) {
        Some(identity) if is_live(identity) => Err(format!(
            "{} records pid {} which is still that process",
            path.display(),
            identity.pid
        )),
        _ => Ok(()),
    }
}

/// The identity a scratch directory's lock file records.
fn recorded_identity(lock_path: &Path) -> Option<ProcessIdentity> {
    let recorded = std::fs::read_to_string(lock_path).ok()?;
    let mut parts = recorded.split_whitespace();
    Some(ProcessIdentity {
        pid: parts.next()?.parse().ok()?,
        start_token: parts.next()?.parse().ok()?,
    })
}

#[cfg(unix)]
mod platform {
    use std::fs::File;
    use std::os::unix::io::AsRawFd as _;

    use super::ProcessIdentity;

    /// Take a non-blocking exclusive `flock`, reporting whether it was granted.
    ///
    /// `flock` is an interruptible call, so a signal delivered while it runs
    /// answers `EINTR` — "ask again", not "somebody else holds this". Reporting
    /// that as contention is how a busy host makes a free directory look owned,
    /// and this crate is one that runs many processes at once. Only a real
    /// refusal is a refusal.
    pub fn try_lock_exclusive(file: &File) -> bool {
        loop {
            // SAFETY: `flock` takes a file descriptor this process owns for the
            // lifetime of the borrow, and reports failure through its return
            // value.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return true;
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return false;
            }
        }
    }

    /// This process's own identity.
    pub fn own_identity() -> ProcessIdentity {
        let pid = std::process::id() as i32;
        ProcessIdentity {
            pid,
            start_token: start_token(pid).unwrap_or(0),
        }
    }

    /// The kernel's start token for `pid` — field 22 of `/proc/<pid>/stat`, the
    /// jiffies since boot at which that process started.
    ///
    /// Parsed from after the last `)` because field 2 is the executable name in
    /// parentheses and may itself contain spaces and parentheses.
    pub fn start_token(pid: i32) -> Option<u64> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let tail = &stat[stat.rfind(')')? + 1..];
        tail.split_whitespace().nth(19)?.parse().ok()
    }

    /// Whether `identity` still names the process it was taken from.
    pub fn is_live(identity: ProcessIdentity) -> bool {
        start_token(identity.pid) == Some(identity.start_token)
    }

    /// Every live process carrying `stamp` as its scratch directory.
    ///
    /// This is the evidence, not a heuristic: the kernel fixes an environment at
    /// `exec`, so a descendant reparented to init still answers for the member
    /// that started it.
    pub fn stamped_for(stamp: &str) -> Vec<ProcessIdentity> {
        let needle = format!("{}={stamp}", super::SCRATCH_ENV);
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return found;
        };
        for entry in entries.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            let Ok(environ) = std::fs::read(entry.path().join("environ")) else {
                continue;
            };
            if environ
                .split(|byte| *byte == 0)
                .any(|var| var == needle.as_bytes())
            {
                if let Some(start_token) = start_token(pid) {
                    found.push(ProcessIdentity { pid, start_token });
                }
            }
        }
        found.sort();
        found
    }

    /// Signal one identity, but only while it is still itself.
    ///
    /// The revalidation is not belt-and-braces: the ordinary outcome of a
    /// `SIGTERM` is that the process exits, which frees its number during the
    /// grace period before the `SIGKILL` — so a second signal sent on the
    /// strength of the first check would be aimed at a number the kernel was
    /// free to hand to somebody else.
    pub fn signal(identity: ProcessIdentity, sig: i32) -> bool {
        if !is_live(identity) {
            return false;
        }
        // SAFETY: `kill` takes a pid and a signal number and reports failure
        // through its return value; it cannot invalidate anything this process
        // holds.
        unsafe { libc::kill(identity.pid, sig) == 0 }
    }

    /// `SIGTERM`, then `SIGKILL` for whatever ignored it.
    pub const TERM: i32 = libc::SIGTERM;
    /// The signal nothing survives.
    pub const KILL: i32 = libc::SIGKILL;
}

#[cfg(not(unix))]
mod platform {
    use std::fs::File;

    use super::ProcessIdentity;

    /// Without `flock` there is no kernel answer to "is anything still using
    /// this?", so the claim degrades to the directory's own existence and a
    /// sweeper is told to keep it.
    pub fn try_lock_exclusive(_file: &File) -> bool {
        true
    }

    /// A pid with no start token: this platform has no cheap one, so an identity
    /// here is only ever compared against another taken the same way.
    pub fn own_identity() -> ProcessIdentity {
        ProcessIdentity {
            pid: std::process::id() as i32,
            start_token: 0,
        }
    }

    /// Nothing can be proven live, so nothing is reported live — which retains
    /// rather than removes, the safe direction.
    pub fn is_live(_identity: ProcessIdentity) -> bool {
        false
    }

    /// No procfs, so no stamped descendants can be enumerated. Reaping falls
    /// back to the child this process holds directly.
    pub fn stamped_for(_stamp: &str) -> Vec<ProcessIdentity> {
        Vec::new()
    }

    /// Nothing is signalled through an unproven handle.
    pub fn signal(_identity: ProcessIdentity, _sig: i32) -> bool {
        false
    }

    /// Placeholder signal numbers; nothing reaches [`signal`] on this platform.
    pub const TERM: i32 = 15;
    /// Placeholder signal numbers; nothing reaches [`signal`] on this platform.
    pub const KILL: i32 = 9;
}

pub use platform::{is_live, own_identity, stamped_for};
use platform::{signal, try_lock_exclusive, KILL, TERM};

/// How long a proven descendant is given to shut itself down before it is
/// killed. Short, because everything here has already been asked to stop.
const GRACE: std::time::Duration = std::time::Duration::from_millis(200);

/// Terminate every live process still stamped for `scratch`.
///
/// Returns how many were signalled. `SIGTERM` first, one grace period, then
/// `SIGKILL` for whatever is still itself — and each signal revalidates the
/// identity, so a member that shut itself down on the first is never sent a
/// second, and a number its exit released is never signalled at all.
pub fn reap(scratch: &Path) -> usize {
    let stamp = scratch.display().to_string();
    let owned = stamped_for(&stamp);
    let own = own_identity();
    let targets: Vec<_> = owned
        .into_iter()
        .filter(|identity| *identity != own)
        .collect();
    if targets.is_empty() {
        return 0;
    }
    let mut signalled = 0;
    for identity in &targets {
        if signal(*identity, TERM) {
            signalled += 1;
        }
    }
    std::thread::sleep(GRACE);
    for identity in &targets {
        signal(*identity, KILL);
    }
    signalled
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lock is the sweeper's proof: while a claim is held the directory is
    /// retained, and the instant it is dropped the directory is gone with it.
    #[test]
    fn a_held_claim_is_never_reclaimable() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("oneagentgraph-run-1");
        let owned = Owned::claim(&path).expect("claim");
        assert_eq!(owned.path(), path);
        let retained = reclaimable(&path).unwrap_err();
        assert!(retained.contains("still locked by its owner"), "{retained}");
        drop(owned);
        assert!(!path.exists(), "a released claim left its scratch behind");
    }

    /// A directory kept past its claim — the successor contract — survives the
    /// launcher, and its lock is free for the successor to claim in turn. That
    /// is the whole of the contract: the launcher sheds the directory, and the
    /// process meant to outlive it claims one it is then judged by.
    #[test]
    fn a_kept_claim_outlives_its_launcher_for_a_successor_to_claim() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("oneagentgraph-detached");
        let mut owned = Owned::claim(&path).expect("claim");
        owned.keep();
        drop(owned);
        assert!(path.exists(), "a kept claim was removed with its launcher");
        let successor = Owned::claim(&path).expect("a successor can claim what was kept");
        assert_eq!(successor.path(), path);
    }

    /// Two claims on one directory cannot both be granted, so a second run
    /// cannot adopt scratch a live one is working in.
    #[cfg(unix)]
    #[test]
    fn a_second_claim_on_one_directory_is_refused() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("oneagentgraph-contended");
        let _first = Owned::claim(&path).expect("claim");
        let err = Owned::claim(&path).unwrap_err();
        assert!(
            err.to_string().contains("already owned by a live process"),
            "{err}"
        );
    }

    /// A directory with no readable lock is retained rather than removed:
    /// nothing about it was proven, and proving nothing must remove nothing.
    #[test]
    fn a_directory_with_no_lock_is_retained() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("predates-the-lock");
        std::fs::create_dir_all(&path).expect("mkdir");
        let retained = reclaimable(&path).unwrap_err();
        assert!(retained.contains("no readable owner.lock"), "{retained}");
    }

    /// A lock recorded by a process that is still itself pins the directory,
    /// even once the lock has been released — the two proofs are independent.
    #[cfg(unix)]
    #[test]
    fn a_live_recorded_identity_pins_the_directory() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("recorded-live");
        std::fs::create_dir_all(&path).expect("mkdir");
        let own = own_identity();
        std::fs::write(
            path.join(OWNER_LOCK_FILE),
            format!("{} {}\n", own.pid, own.start_token),
        )
        .expect("write");
        let retained = reclaimable(&path).unwrap_err();
        assert!(retained.contains("still that process"), "{retained}");

        // The same pid with a start token nobody holds is a recycled number, and
        // recycling is exactly what the token exists to see through.
        std::fs::write(path.join(OWNER_LOCK_FILE), format!("{} 1\n", own.pid)).expect("write");
        assert_eq!(reclaimable(&path), Ok(()));

        // An unparseable record proves nothing about a live process, so the lock
        // is the only proof left — and it is free.
        std::fs::write(path.join(OWNER_LOCK_FILE), "not a record\n").expect("write");
        assert_eq!(reclaimable(&path), Ok(()));
    }

    /// A scratch that cannot be created, or whose lock cannot be opened, is a
    /// refusal naming the path — a run that proceeded without ownership would
    /// have nothing to hold against a sweep.
    #[test]
    fn a_scratch_that_cannot_be_claimed_names_its_path() {
        let root = tempfile::tempdir().expect("tempdir");
        let blocked = root.path().join("not-a-directory");
        std::fs::write(&blocked, "").expect("write");
        let err = Owned::claim(blocked.join("child")).unwrap_err();
        assert!(err.to_string().contains("cannot create scratch"), "{err}");
    }

    /// This process is stamped for nothing, so a sweep of an unstamped scratch
    /// signals nothing — and never itself.
    #[test]
    fn reaping_an_unstamped_scratch_signals_nothing() {
        let root = tempfile::tempdir().expect("tempdir");
        assert_eq!(reap(root.path()), 0);
        assert!(stamped_for("/no/such/scratch").is_empty());
    }

    /// This process's own identity is live by construction; a pid that cannot
    /// exist is not.
    #[cfg(unix)]
    #[test]
    fn an_identity_answers_for_the_process_it_was_taken_from() {
        assert!(is_live(own_identity()));
        assert!(!is_live(ProcessIdentity {
            pid: -1,
            start_token: 0
        }));
    }
}
