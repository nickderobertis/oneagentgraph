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
//! Both proofs are per-platform, and neither is `/proc`. Linux reads the start
//! token out of `/proc/<pid>/stat` and the stamp out of `/proc/<pid>/environ`;
//! Darwin has no `/proc` at all and answers the same two questions through
//! `libproc` and `KERN_PROCARGS2`. Reading the Linux path on Darwin is not a
//! degraded answer but a wrong one — every identity reports dead, so no
//! directory is ever pinned and a live run's scratch is free for the taking.
//!
//! On a platform that can do neither the ownership claim degrades to the
//! `flock` alone plus the directory's own existence, and reaping to the child
//! this process holds. Nothing is reported live that cannot be *proven* live,
//! which retains a directory rather than reclaiming one still in use. The
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
    // Acquiring the lock is only evidence where the kernel backs it. Where it
    // does not, taking it says nothing about who else is working here — and
    // `Owned::claim` is granted for exactly that reason, so reading its success
    // as "nobody holds this" would let a sweep delete a live run's scratch. The
    // two questions want opposite answers from the same missing facility: a
    // claim proceeds, a sweep does not.
    if !LOCK_PROVES_OWNERSHIP {
        return Err(format!(
            "{} cannot be proven unused on this platform, so it is retained",
            path.display()
        ));
    }
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
    #[cfg(target_vendor = "apple")]
    use std::ffi::c_int;
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

    /// Whether `identity` still names the process it was taken from.
    ///
    /// A platform that cannot answer for a token answers `None` here, which is
    /// never equal to a recorded token — so an identity is reported live only
    /// where it can be *proven* live, and a sweeper on such a platform falls
    /// back to the `flock`, which is the independent half of the same proof.
    pub fn is_live(identity: ProcessIdentity) -> bool {
        start_token(identity.pid).is_some_and(|token| token == identity.start_token)
    }

    /// Every live process carrying `stamp`, **or a scratch below it**, as its
    /// scratch directory.
    ///
    /// This is the evidence, not a heuristic: the kernel fixes an environment at
    /// `exec`, so a descendant reparented to init still answers for the member
    /// that started it.
    ///
    /// The "or below" is what makes a *run* answerable for its members. A member
    /// is stamped with its own scratch, `<run>/members/<name>`, and nothing is
    /// ever stamped with the run directory itself — so an exact match asked
    /// `cancel RUN --kill` to find processes that by construction do not exist,
    /// and it reported a cancelled run while every member kept going. The
    /// comparison is on path components, not bytes: `<run>-2` is not below
    /// `<run>`.
    pub fn stamped_for(stamp: &str) -> Vec<ProcessIdentity> {
        let prefix = format!("{}=", super::SCRATCH_ENV);
        let mut found: Vec<ProcessIdentity> = enumerate(&prefix, stamp);
        found.sort();
        found
    }

    /// The kernel's start token for `pid`: a value fixed when that process
    /// started, so a recycled number carries a different one.
    ///
    /// `None` means this platform cannot prove the identity — see [`is_live`]
    /// for what a caller does with that.
    #[cfg(target_os = "linux")]
    pub fn start_token(pid: i32) -> Option<u64> {
        // Field 22 of `/proc/<pid>/stat`, the jiffies since boot at which the
        // process started. Parsed from after the last `)` because field 2 is the
        // executable name in parentheses and may itself contain spaces and
        // parentheses.
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let tail = &stat[stat.rfind(')')? + 1..];
        tail.split_whitespace().nth(19)?.parse().ok()
    }

    /// Every process whose environment carries `stamp`, read from `/proc`.
    #[cfg(target_os = "linux")]
    fn enumerate(prefix: &str, stamp: &str) -> Vec<ProcessIdentity> {
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
            if carries(&environ, prefix, stamp) {
                if let Some(start_token) = start_token(pid) {
                    found.push(ProcessIdentity { pid, start_token });
                }
            }
        }
        found
    }

    /// The kernel's start token for `pid`, from `libproc`: the wall-clock time
    /// at which that process started, in microseconds.
    ///
    /// Darwin has no `/proc`, and reading the Linux path here is what reported
    /// every identity as dead — a scratch directory nobody could pin, because
    /// the proof it rests on was never available.
    #[cfg(target_vendor = "apple")]
    pub fn start_token(pid: i32) -> Option<u64> {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: `proc_pidinfo` fills at most `size` bytes of the buffer, which
        // is a live `proc_bsdinfo` this frame owns, and reports how many it
        // wrote. A short write is rejected below rather than read.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                std::ptr::from_mut(&mut info).cast(),
                size,
            )
        };
        if written != size {
            return None;
        }
        // Both halves, because the seconds alone cannot separate two runs of a
        // recycled number inside one second — which is exactly the window a
        // busy host recycles pids in.
        Some(
            info.pbi_start_tvsec
                .saturating_mul(1_000_000)
                .saturating_add(info.pbi_start_tvusec),
        )
    }

    /// Every process whose arguments-and-environment block carries `stamp`.
    ///
    /// Darwin exposes that block as one `KERN_PROCARGS2` buffer of
    /// null-separated strings rather than as a file per process. The whole
    /// buffer is scanned rather than parsed into argv and environ: the only
    /// thing being looked for is a [`super::SCRATCH_ENV`] assignment, which this
    /// crate only ever puts in an environment, and splitting the two halves
    /// correctly means replaying a layout that has no stable contract.
    #[cfg(target_vendor = "apple")]
    fn enumerate(prefix: &str, stamp: &str) -> Vec<ProcessIdentity> {
        let mut found = Vec::new();
        for pid in all_pids() {
            let Some(block) = process_args(pid) else {
                continue;
            };
            if carries(&block, prefix, stamp) {
                if let Some(start_token) = start_token(pid) {
                    found.push(ProcessIdentity { pid, start_token });
                }
            }
        }
        found
    }

    /// Every pid on the host, as `libproc` reports them.
    #[cfg(target_vendor = "apple")]
    fn all_pids() -> Vec<i32> {
        // SAFETY: a null buffer asks `proc_listallpids` for the size it needs
        // rather than writing anything.
        let bytes = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        let Ok(count) = usize::try_from(bytes) else {
            return Vec::new();
        };
        if count == 0 {
            return Vec::new();
        }
        // Room to spare, because processes start between the two calls and a
        // full buffer is indistinguishable from a truncated one.
        let mut pids = vec![0i32; count + 64];
        let Ok(size) = c_int::try_from(std::mem::size_of_val(pids.as_slice())) else {
            return Vec::new();
        };
        // SAFETY: the buffer is `size` bytes of live, owned, initialised memory,
        // and the call reports how many bytes it filled.
        let written = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), size) };
        let Ok(written) = usize::try_from(written) else {
            return Vec::new();
        };
        pids.truncate(written / std::mem::size_of::<i32>());
        pids.retain(|pid| *pid > 0);
        pids
    }

    /// One process's `KERN_PROCARGS2` block, when this process may read it.
    ///
    /// A process owned by another user answers `EPERM`, which is not an error
    /// worth reporting: it cannot be one this run stamped.
    #[cfg(target_vendor = "apple")]
    fn process_args(pid: i32) -> Option<Vec<u8>> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut needed: libc::size_t = 0;
        // SAFETY: a null output buffer asks for the size the answer needs. `mib`
        // is a live array of the length passed alongside it.
        let sized = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                std::ptr::from_mut(&mut needed),
                std::ptr::null_mut(),
                0,
            )
        };
        if sized != 0 || needed == 0 {
            return None;
        }
        let mut block = vec![0u8; needed];
        // SAFETY: `block` is `needed` bytes of live, owned, initialised memory,
        // and `needed` is updated to what was actually written.
        let read = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                block.as_mut_ptr().cast(),
                std::ptr::from_mut(&mut needed),
                std::ptr::null_mut(),
                0,
            )
        };
        if read != 0 {
            return None;
        }
        block.truncate(needed);
        Some(block)
    }

    /// No way to prove an identity here, so none is claimed. [`is_live`] reports
    /// nothing live, which retains a directory rather than reclaiming one that
    /// may still be in use.
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    pub fn start_token(_pid: i32) -> Option<u64> {
        None
    }

    /// No way to enumerate a stamped process here, so reaping falls back to the
    /// child this process holds directly.
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    fn enumerate(_prefix: &str, _stamp: &str) -> Vec<ProcessIdentity> {
        Vec::new()
    }

    /// Whether a block of null-separated `KEY=VALUE` strings stamps this scratch.
    ///
    /// Shared by the two platforms that can read one; a platform that cannot
    /// enumerate a process's environment never asks.
    #[cfg(any(target_os = "linux", target_vendor = "apple"))]
    fn carries(block: &[u8], prefix: &str, stamp: &str) -> bool {
        block
            .split(|byte| *byte == 0)
            .any(|var| at_or_below(var, prefix.as_bytes(), stamp.as_bytes()))
    }

    /// Whether one `KEY=VALUE` pair names `stamp` or a path below it.
    fn at_or_below(var: &[u8], prefix: &[u8], stamp: &[u8]) -> bool {
        let Some(value) = var.strip_prefix(prefix) else {
            return false;
        };
        value == stamp || (value.starts_with(stamp) && value.get(stamp.len()) == Some(&b'/'))
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

    /// An `flock` here is the kernel's own answer to "is anything still using
    /// this?", so acquiring one is evidence a sweeper may act on.
    pub const LOCK_PROVES_OWNERSHIP: bool = true;

    /// `SIGTERM`, then `SIGKILL` for whatever ignored it.
    pub const TERM: i32 = libc::SIGTERM;
    /// The signal nothing survives.
    pub const KILL: i32 = libc::SIGKILL;

    #[cfg(test)]
    mod tests {
        use super::at_or_below;

        /// The comparison is on path components. A sibling run whose name merely
        /// starts with this one's is not below it — and what this answers is
        /// which processes to kill, so the cost of getting it wrong is reaping
        /// another run's members.
        #[test]
        fn a_stamp_below_this_scratch_matches_and_a_sibling_does_not() {
            let key = b"ONEAGENTGRAPH_SCRATCH_DIR=";
            let run = b"/state/node-1";
            for below in [
                &b"ONEAGENTGRAPH_SCRATCH_DIR=/state/node-1"[..],
                &b"ONEAGENTGRAPH_SCRATCH_DIR=/state/node-1/members/worker"[..],
            ] {
                assert!(at_or_below(below, key, run), "{below:?}");
            }
            for outside in [
                &b"ONEAGENTGRAPH_SCRATCH_DIR=/state/node-12"[..],
                &b"ONEAGENTGRAPH_SCRATCH_DIR=/state/node-1x/members/worker"[..],
                &b"ONEAGENTGRAPH_SCRATCH_DIR=/state/node"[..],
                &b"ONEAGENTGRAPH_SCRATCH=/state/node-1"[..],
                &b"PATH=/state/node-1"[..],
            ] {
                assert!(!at_or_below(outside, key, run), "{outside:?}");
            }
        }
    }
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

    /// Taking the lock here proves nothing: [`try_lock_exclusive`] always
    /// grants it, so a sweeper reading that as "nobody holds this" would
    /// reclaim a directory a live run is working in. Ownership degrades to the
    /// directory's own existence, and a sweeper is told to keep it.
    pub const LOCK_PROVES_OWNERSHIP: bool = false;

    /// Placeholder signal numbers; nothing reaches [`signal`] on this platform.
    pub const TERM: i32 = 15;
    /// Placeholder signal numbers; nothing reaches [`signal`] on this platform.
    pub const KILL: i32 = 9;
}

pub use platform::{is_live, own_identity, stamped_for};
use platform::{signal, try_lock_exclusive, KILL, LOCK_PROVES_OWNERSHIP, TERM};

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
        assert!(retained.contains(&path.display().to_string()), "{retained}");
        // Retained on every platform, for reasons that are not the same one: a
        // kernel lock is proof a sweeper can act on, and a platform without one
        // has none to offer — so it keeps the directory rather than guessing.
        #[cfg(unix)]
        assert!(retained.contains("still locked by its owner"), "{retained}");
        #[cfg(not(unix))]
        assert!(retained.contains("cannot be proven unused"), "{retained}");
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
        let own = own_identity();

        // Each case gets its own directory rather than rewriting one lock file
        // in place. `flock` is held by an *open file description*, and rewriting
        // a file this process has already opened and closed makes the second
        // acquisition depend on the first's close having been observed — which
        // is a race the test would lose occasionally and the code never runs.
        let recorded = |name: &str, record: &str| {
            let path = root.path().join(name);
            std::fs::create_dir_all(&path).expect("mkdir");
            std::fs::write(path.join(OWNER_LOCK_FILE), record).expect("write");
            path
        };

        let live = recorded("live", &format!("{} {}\n", own.pid, own.start_token));
        let retained = reclaimable(&live).unwrap_err();
        assert!(retained.contains("still that process"), "{retained}");

        // The same pid with a start token nobody holds is a recycled number, and
        // recycling is exactly what the token exists to see through.
        let recycled = recorded("recycled", &format!("{} 1\n", own.pid));
        assert_eq!(reclaimable(&recycled), Ok(()));

        // An unparseable record proves nothing about a live process, so the lock
        // is the only proof left — and it is free.
        let torn = recorded("torn", "not a record\n");
        assert_eq!(reclaimable(&torn), Ok(()));
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
